//! Window tiling, macOS Sequoia style.
//!
//! * Drag a window against the left / right screen edge: it fills that half.
//!   The top edge fills the screen; the corners give quarters. A translucent
//!   preview shows the target while the pointer rests there.
//! * ⌥⌘←/→/↑/↓ tile to a half, ⌥⌘C centers, ⌥⌘R returns to the previous size.
//! * Dragging a tiled (or zoomed) window away restores its previous size, with
//!   the pointer keeping its relative spot in the title bar.

use std::collections::HashMap;

use orchard_config::{NewWindowPlacement, SplitDirection, TilingMode, TilingSettings};
use smithay::{
    desktop::{layer_map_for_output, space::SpaceElement, WindowSurface},
    output::Output,
    reexports::wayland_protocols::xdg::shell::server::xdg_toplevel,
    utils::{IsAlive, Logical, Point, Rectangle, Size},
};

use crate::{
    shell::{ssd::HEADER_BAR_HEIGHT, WindowElement},
    state::{Backend, OrchardState},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tile {
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    /// the whole usable area (same as zoom)
    Fill,
}

/// How far (logical px) from a left / right screen edge the pointer may be
/// for the drop target to appear. Generous on purpose: aiming for a 2 px
/// strip while dragging a whole window feels fiddly.
pub const EDGE_ZONE: f64 = 32.0;
/// Once a target shows, it stays until the pointer is this much further out
/// (no flicker while the hand wobbles at the boundary).
const STICKY: f64 = 48.0;
/// The pointer zone at the seam between two displays: thin, since the pointer
/// cannot be pushed against it, it just crosses over.
const SEAM_ZONE: f64 = 14.0;
/// A window may stick this far over a seam and still tile on its own display.
const SEAM_OVERHANG: f64 = 64.0;

/// The dragged window counts as "at the edge" once its side is this close to
/// (or past) the screen edge, wherever the pointer is in its title bar.
const WINDOW_TOUCH: f64 = 4.0;

/// The window being dragged, for the window-edge trigger.
#[derive(Debug, Clone, Copy)]
pub struct DragWindow {
    /// current frame (global logical)
    pub rect: Rectangle<f64, Logical>,
    /// frame x at the start of the drag: a window that starts flush against
    /// an edge must not tile just because it is moved up or down
    pub start_x: f64,
}

/// Size of the corner zones (quarters) along an edge: a share of the screen,
/// so they are easy to hit on large displays too.
fn corner_zone(len: f64) -> f64 {
    (len * 0.16).clamp(80.0, 220.0)
}

/// Margin around tiled windows and between two tiles (logical px), like
/// macOS Sequoia's "tiled windows have margins".
pub const GAP: i32 = 8;

/// The two margins of a layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gaps {
    /// to the screen edges
    pub outer: i32,
    /// between two tiles
    pub inner: i32,
}

impl Gaps {
    pub const fn uniform(g: i32) -> Self {
        Self { outer: g, inner: g }
    }
    pub const NONE: Gaps = Gaps::uniform(0);
}

impl From<&TilingSettings> for Gaps {
    fn from(t: &TilingSettings) -> Self {
        Self { outer: t.outer_gap, inner: t.inner_gap }
    }
}

/// How far the pointer must be from an edge to snap, and whether corners count.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snap {
    pub zone: f64,
    pub corners: bool,
}

impl Default for Snap {
    fn default() -> Self {
        Self { zone: EDGE_ZONE, corners: true }
    }
}

impl From<&TilingSettings> for Snap {
    fn from(t: &TilingSettings) -> Self {
        Self { zone: t.edge_zone as f64, corners: t.snap_corners }
    }
}

pub fn tile_rect_with(area: Rectangle<i32, Logical>, tile: Tile, gaps: Gaps) -> Rectangle<i32, Logical> {
    if tile == Tile::Fill {
        return area;
    }
    let (og, g) = (gaps.outer, gaps.inner);
    // margin to the screen edges, then the inner gap between the halves
    let (x, y) = (area.loc.x + og, area.loc.y + og);
    let (w, h) = ((area.size.w - 2 * og).max(2), (area.size.h - 2 * og).max(2));
    let (lw, th) = ((w - g) / 2, (h - g) / 2);
    let (rw, bh) = (w - g - lw, h - g - th);
    let (x2, y2) = (x + lw + g, y + th + g);
    let r = |x, y, w, h| Rectangle::new((x, y).into(), (w, h).into());
    match tile {
        Tile::Left => r(x, y, lw, h),
        Tile::Right => r(x2, y, rw, h),
        Tile::Top => r(x, y, w, th),
        Tile::Bottom => r(x, y2, w, bh),
        Tile::TopLeft => r(x, y, lw, th),
        Tile::TopRight => r(x2, y, rw, th),
        Tile::BottomLeft => r(x, y2, lw, bh),
        Tile::BottomRight => r(x2, y2, rw, bh),
        Tile::Fill => area,
    }
}

fn tiled_states(tile: Tile) -> [bool; 4] {
    // left, right, top, bottom edges touching the area border
    match tile {
        Tile::Left => [true, false, true, true],
        Tile::Right => [false, true, true, true],
        Tile::Top => [true, true, true, false],
        Tile::Bottom => [true, true, false, true],
        Tile::TopLeft => [true, false, true, false],
        Tile::TopRight => [false, true, true, false],
        Tile::BottomLeft => [true, false, false, true],
        Tile::BottomRight => [false, true, false, true],
        Tile::Fill => [true, true, true, true],
    }
}

/// Which tile the pointer at `pos` asks for while dragging, if any, and the
/// usable area of the display it belongs to. Every display has its own two
/// halves: outer screen edges get a generous zone, the seam between two
/// displays a thin strip on each side (so moving a window across stays easy).
/// `current` is the target shown right now; it gets a wider zone so it does
/// not flicker.
#[cfg(test)]
pub fn edge_tile(
    outputs: &[(Rectangle<i32, Logical>, Rectangle<i32, Logical>)],
    pos: Point<f64, Logical>,
    window: Option<DragWindow>,
    current: Option<(Tile, Rectangle<i32, Logical>)>,
) -> Option<(Tile, Rectangle<i32, Logical>)> {
    edge_tile_with(outputs, pos, window, current, Snap::default())
}

pub fn edge_tile_with(
    outputs: &[(Rectangle<i32, Logical>, Rectangle<i32, Logical>)],
    pos: Point<f64, Logical>,
    window: Option<DragWindow>,
    current: Option<(Tile, Rectangle<i32, Logical>)>,
    snap: Snap,
) -> Option<(Tile, Rectangle<i32, Logical>)> {
    let contains = |p: Point<f64, Logical>| outputs.iter().any(|(g, _)| g.to_f64().contains(p));
    let (geo, area) = outputs.iter().find(|(g, _)| g.to_f64().contains(pos))?;
    let g = geo.to_f64();
    let a = area.to_f64();
    // hysteresis only for the target on this display
    let current = current.filter(|(_, r)| r == area).map(|(t, _)| t);
    let extra = |t: &[Tile]| if current.map(|c| t.contains(&c)).unwrap_or(false) { STICKY } else { 0.0 };
    use Tile::*;

    let (gl, gr) = (g.loc.x, g.loc.x + g.size.w);
    // another display on that side: a seam, not an outer edge
    let seam_l = contains((gl - 1.0, pos.y).into());
    let seam_r = contains((gr + 1.0, pos.y).into());
    let seam_t = contains((pos.x, g.loc.y - 1.0).into());
    let zone = |seam: bool| if seam { SEAM_ZONE } else { snap.zone };
    let sticky_l = extra(&[Left, TopLeft, BottomLeft]);
    let sticky_r = extra(&[Right, TopRight, BottomRight]);
    // the window pushed against the side (pointer anywhere in its title bar);
    // not for windows nearly as wide as the screen, they touch both sides. At
    // a seam it must not stick out far: then it is on its way to the next display.
    let win = window.filter(|w| w.rect.size.w < g.size.w * 0.9);
    let win_left = win
        .map(|w| {
            w.rect.loc.x <= gl + WINDOW_TOUCH + sticky_l
                && w.start_x > gl + WINDOW_TOUCH
                && (!seam_l || w.rect.loc.x >= gl - SEAM_OVERHANG)
        })
        .unwrap_or(false);
    let win_right = win
        .map(|w| {
            let w_right = w.rect.loc.x + w.rect.size.w;
            let start_right = w.start_x + w.rect.size.w;
            w_right >= gr - WINDOW_TOUCH - sticky_r
                && start_right < gr - WINDOW_TOUCH
                && (!seam_r || w_right <= gr + SEAM_OVERHANG)
        })
        .unwrap_or(false);
    let left = pos.x - gl <= zone(seam_l) + sticky_l || win_left;
    let right = gr - pos.x <= zone(seam_r) + sticky_r || win_right;
    let (left, right) = if left && right { (pos.x - gl <= gr - pos.x, pos.x - gl > gr - pos.x) } else { (left, right) };
    // the top edge: the whole menu bar strip, plus a little below it
    let sticky_t = extra(&[Fill, TopLeft, TopRight]);
    let menu_bar = (a.loc.y - g.loc.y).max(0.0);
    let top = if seam_t {
        pos.y - g.loc.y <= menu_bar + SEAM_ZONE + sticky_t
    } else {
        pos.y - g.loc.y <= menu_bar.max(12.0) + 6.0 + sticky_t
    };

    let cy = corner_zone(a.size.h);
    let cx = corner_zone(g.size.w);
    let near_top = snap.corners && pos.y <= a.loc.y + cy;
    let near_bottom = snap.corners && pos.y >= a.loc.y + a.size.h - cy;
    let near_left = snap.corners && pos.x <= g.loc.x + cx;
    let near_right = snap.corners && pos.x >= g.loc.x + g.size.w - cx;
    let tile = if left {
        if near_top {
            TopLeft
        } else if near_bottom {
            BottomLeft
        } else {
            Left
        }
    } else if right {
        if near_top {
            TopRight
        } else if near_bottom {
            BottomRight
        } else {
            Right
        }
    } else if top {
        if near_left {
            TopLeft
        } else if near_right {
            TopRight
        } else {
            Fill
        }
    } else {
        return None;
    };
    Some((tile, *area))
}

/// The display next to `from` on the left (`dir < 0`) or right (`dir > 0`)
/// that shares some height with it, nearest first.
fn neighbour(
    outputs: &[(Rectangle<i32, Logical>, Rectangle<i32, Logical>)],
    from: Rectangle<i32, Logical>,
    dir: i32,
) -> Option<Rectangle<i32, Logical>> {
    let overlaps = |g: &Rectangle<i32, Logical>| g.loc.y < from.loc.y + from.size.h && from.loc.y < g.loc.y + g.size.h;
    outputs
        .iter()
        .filter(|(g, _)| *g != from && overlaps(g))
        .filter_map(|(g, a)| {
            let dist = if dir < 0 {
                from.loc.x - (g.loc.x + g.size.w)
            } else {
                g.loc.x - (from.loc.x + from.size.w)
            };
            (dist >= 0).then_some((dist, *a))
        })
        .min_by_key(|(d, _)| *d)
        .map(|(_, a)| a)
}

impl<BackendData: Backend> OrchardState<BackendData> {
    /// (output geometry, usable area) of every output.
    pub fn output_areas(&self) -> Vec<(Rectangle<i32, Logical>, Rectangle<i32, Logical>)> {
        self.space
            .outputs()
            .filter_map(|o| {
                let geo = self.space.output_geometry(o)?;
                let zone = layer_map_for_output(o).non_exclusive_zone();
                Some((geo, Rectangle::new(geo.loc + zone.loc, zone.size)))
            })
            .collect()
    }

    /// The margins in effect right now.
    pub fn gaps(&self) -> Gaps {
        let t = crate::settings::get().tiling.clamped();
        Gaps::from(&t)
    }

    pub fn window_tile(&self, window: &WindowElement) -> Option<Tile> {
        window.decoration_state().tile
    }

    /// Give `window` the outer frame `rect` (title bar included).
    fn apply_frame(&mut self, window: &WindowElement, rect: Rectangle<i32, Logical>, tile: Option<Tile>) {
        let header = if window.is_ssd() { HEADER_BAR_HEIGHT } else { 0 };
        let client: Size<i32, Logical> = (rect.size.w, (rect.size.h - header).max(1)).into();
        match window.0.underlying_surface() {
            WindowSurface::Wayland(toplevel) => {
                let edges = tile.map(tiled_states).unwrap_or([false; 4]);
                toplevel.with_pending_state(|s| {
                    s.states.unset(xdg_toplevel::State::Maximized);
                    for (state, on) in [
                        (xdg_toplevel::State::TiledLeft, edges[0]),
                        (xdg_toplevel::State::TiledRight, edges[1]),
                        (xdg_toplevel::State::TiledTop, edges[2]),
                        (xdg_toplevel::State::TiledBottom, edges[3]),
                    ] {
                        if on {
                            s.states.set(state);
                        } else {
                            s.states.unset(state);
                        }
                    }
                    s.size = Some(client);
                });
                toplevel.send_pending_configure();
            }
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(x) => {
                let _ = x.set_maximized(false);
                let _ = x.configure(Rectangle::new(rect.loc + Point::from((0, header)), client));
            }
        }
        window.decoration_state().tile = tile;
        self.animate_frame(window, rect);
        self.space.map_element(window.clone(), rect.loc, true);
    }

    /// Remember the frame to go back to, unless already tiled / zoomed.
    fn save_restore_geometry(&mut self, window: &WindowElement) {
        if window.decoration_state().tile.is_some() || self.is_zoomed(window) {
            return;
        }
        let loc = self.space.element_location(window).unwrap_or_default();
        let client = window.client_geometry().size;
        window.decoration_state().restore_geometry = Some(Rectangle::new(loc, client));
    }

    /// Keyboard tiling on the window's display. Asking for the half it is
    /// already in moves it on to the facing half of the next display that
    /// way (⌥⌘← from the left half lands in the right half of the display to
    /// the left), so every display's two halves are reachable.
    pub fn tile_window(&mut self, window: &WindowElement, tile: Tile) {
        let Some(area) = self.usable_area_for(window) else { return };
        let (tile, area) = match (tile, window.decoration_state().tile) {
            (Tile::Left, Some(Tile::Left)) | (Tile::Right, Some(Tile::Right)) => {
                let outputs = self.output_areas();
                let next = outputs
                    .iter()
                    .find(|(_, a)| *a == area)
                    .and_then(|(g, _)| neighbour(&outputs, *g, if tile == Tile::Left { -1 } else { 1 }));
                match next {
                    Some(a) => (if tile == Tile::Left { Tile::Right } else { Tile::Left }, a),
                    None => (tile, area),
                }
            }
            _ => (tile, area),
        };
        self.tile_window_in(window, tile, area);
    }

    /// Tile `window` into `tile` of the usable area `area` (one display's).
    pub fn tile_window_in(&mut self, window: &WindowElement, tile: Tile, area: Rectangle<i32, Logical>) {
        if !window.alive() || window.decoration_state().fullscreen {
            return;
        }
        if tile == Tile::Fill {
            // zoom fills the display the window is on: bring it over first
            if self.usable_area_for(window) != Some(area) {
                if self.is_zoomed(window) {
                    self.toggle_zoom(window);
                }
                let size = window.geometry().size;
                let loc: Point<i32, Logical> = (
                    area.loc.x + (area.size.w - size.w).max(0) / 2,
                    area.loc.y + (area.size.h - size.h).max(0) / 2,
                )
                    .into();
                self.space.map_element(window.clone(), loc, true);
            }
            if !self.is_zoomed(window) {
                if window.decoration_state().tile.is_some() {
                    // zoom restores to the pre-tile size later
                    let restore = window.decoration_state().restore_geometry;
                    window.decoration_state().tile = None;
                    self.toggle_zoom(window);
                    window.decoration_state().restore_geometry = restore;
                } else {
                    self.toggle_zoom(window);
                }
            }
            return;
        }
        self.save_restore_geometry(window);
        let gaps = self.gaps();
        self.apply_frame(window, tile_rect_with(area, tile, gaps), Some(tile));
    }

    /// ⌥⌘R: back to the size and place from before tiling / zooming.
    pub fn untile_window(&mut self, window: &WindowElement) {
        if self.is_zoomed(window) {
            self.toggle_zoom(window);
            return;
        }
        if window.decoration_state().tile.is_none() {
            return;
        }
        let restore = window.decoration_state().restore_geometry.take();
        if let Some(r) = restore {
            let header = if window.is_ssd() { HEADER_BAR_HEIGHT } else { 0 };
            let frame = Rectangle::new(r.loc, (r.size.w, r.size.h + header).into());
            self.apply_frame(window, frame, None);
        } else {
            window.decoration_state().tile = None;
        }
    }

    /// ⌥⌘C: center in the usable area (keeps the size).
    pub fn center_window(&mut self, window: &WindowElement) {
        let Some(area) = self.usable_area_for(window) else { return };
        if self.is_zoomed(window) {
            return;
        }
        let size = window.geometry().size;
        let loc: Point<i32, Logical> = (
            area.loc.x + (area.size.w - size.w).max(0) / 2,
            area.loc.y + (area.size.h - size.h).max(0) / 2,
        )
            .into();
        self.animate_frame(window, Rectangle::new(loc, size));
        self.space.map_element(window.clone(), loc, true);
    }

    /// The user started dragging a tiled or zoomed window: give it back its
    /// previous size under the pointer. Returns the new window location.
    pub fn restore_for_drag(&mut self, window: &WindowElement, pointer: Point<f64, Logical>) -> Option<Point<i32, Logical>> {
        let zoomed = self.is_zoomed(window);
        let tiled = window.decoration_state().tile.is_some();
        if !zoomed && !tiled {
            return None;
        }
        let loc = self.space.element_location(window)?;
        let cur = window.geometry();
        let restore = window.decoration_state().restore_geometry.take();
        let header = if window.is_ssd() { HEADER_BAR_HEIGHT } else { 0 };
        let size: Size<i32, Logical> = match restore {
            Some(r) => (r.size.w, r.size.h + header).into(),
            // nothing saved (e.g. the client maximized itself): 2/3 of the current size
            None => (cur.size.w * 2 / 3, cur.size.h * 2 / 3).into(),
        };
        // keep the pointer at the same fraction of the width, same offset from the top
        let fx = ((pointer.x - loc.x as f64) / cur.size.w.max(1) as f64).clamp(0.0, 1.0);
        let dy = (pointer.y - loc.y as f64).clamp(0.0, size.h as f64);
        let new_loc: Point<i32, Logical> =
            ((pointer.x - fx * size.w as f64).round() as i32, (pointer.y - dy).round() as i32).into();
        self.apply_frame(window, Rectangle::new(new_loc, size), None);
        Some(new_loc)
    }

    /// While dragging `window`: show, move or hide the drop preview for the
    /// pointer position. The preview grows out of the window and glides
    /// between targets.
    pub fn update_tile_preview(
        &mut self,
        window: &WindowElement,
        pos: Point<f64, Logical>,
        start: Point<i32, Logical>,
        current: Option<(Tile, Rectangle<i32, Logical>)>,
    ) -> Option<(Tile, Rectangle<i32, Logical>)> {
        let drag = self.space.element_location(window).map(|loc| DragWindow {
            rect: Rectangle::new(loc, window.geometry().size).to_f64(),
            start_x: start.x as f64,
        });
        let cfg = crate::settings::get().tiling.clamped();
        if !cfg.snap_to_edges {
            self.fx.tile_preview = None;
            self.fx.tile_preview_anim = None;
            return None;
        }
        let hit = edge_tile_with(&self.output_areas(), pos, drag, current, Snap::from(&cfg));
        let target = hit
            .filter(|_| cfg.snap_preview)
            .map(|(t, a)| tile_rect_with(a, t, Gaps::from(&cfg)));
        if target != self.fx.tile_preview {
            let now: std::time::Duration = self.clock.now().into();
            if let Some(to) = target {
                // already showing one: glide over at full opacity; else grow out of the window
                let shown = self.fx.tile_preview_at(now);
                let fade_in = shown.is_none();
                let from = shown
                    .map(|(r, _)| r)
                    .or_else(|| {
                        let loc = self.space.element_location(window)?;
                        Some(Rectangle::new(loc, window.geometry().size).to_f64())
                    })
                    .unwrap_or_else(|| to.to_f64());
                let spring = crate::effects::spring::Spring::new(
                    0.0,
                    1.0,
                    crate::effects::spring::SpringParams::PREVIEW,
                    now,
                );
                self.fx.tile_preview_anim = (!crate::settings::get().appearance.reduce_motion).then_some((from, spring, fade_in));
            } else {
                self.fx.tile_preview_anim = None;
            }
            self.fx.tile_preview = target;
        }
        hit
    }

    pub fn clear_tile_preview(&mut self) {
        self.fx.tile_preview = None;
        self.fx.tile_preview_anim = None;
    }
}

// ---- Hyprland-style dwindle layout (BSP tree) -------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDir {
    /// Side-by-side (left | right).
    Horizontal,
    /// Stacked (top / bottom).
    Vertical,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DwindleNode {
    Leaf(u64),
    Split {
        dir: SplitDir,
        ratio: f32,
        first: Box<DwindleNode>,
        second: Box<DwindleNode>,
    },
}

impl DwindleNode {
    pub fn contains(&self, id: u64) -> bool {
        match self {
            DwindleNode::Leaf(w) => *w == id,
            DwindleNode::Split { first, second, .. } => first.contains(id) || second.contains(id),
        }
    }

    pub fn ids(&self, out: &mut Vec<u64>) {
        match self {
            DwindleNode::Leaf(w) => out.push(*w),
            DwindleNode::Split { first, second, .. } => {
                first.ids(out);
                second.ids(out);
            }
        }
    }

    /// Deepest rightmost leaf (the tail of the dwindle spiral).
    pub fn tail_leaf(&self) -> u64 {
        match self {
            DwindleNode::Leaf(w) => *w,
            DwindleNode::Split { second, .. } => second.tail_leaf(),
        }
    }

    /// Compute `(window_id, rect)` for every leaf inside `rect` with `gap` between panes.
    fn layout_in(&self, rect: Rectangle<i32, Logical>, gap: i32, out: &mut Vec<(u64, Rectangle<i32, Logical>)>) {
        match self {
            DwindleNode::Leaf(id) => out.push((*id, rect)),
            DwindleNode::Split {
                dir,
                ratio,
                first,
                second,
            } => {
                let (r1, r2) = split_rect(rect, *dir, *ratio, gap);
                first.layout_in(r1, gap, out);
                second.layout_in(r2, gap, out);
            }
        }
    }

    /// Split leaf `target` into `(target, new_id)` choosing Horizontal if the
    /// leaf's rect is wider than or equal to its height, else Vertical.
    fn split_leaf(&mut self, target: u64, new_id: u64, rect: Rectangle<i32, Logical>, gap: i32, opts: SplitOpts) -> bool {
        match self {
            DwindleNode::Leaf(id) if *id == target => {
                let dir = opts.dir.unwrap_or(if rect.size.w >= rect.size.h {
                    SplitDir::Horizontal
                } else {
                    SplitDir::Vertical
                });
                *self = DwindleNode::Split {
                    dir,
                    ratio: opts.ratio,
                    first: Box::new(DwindleNode::Leaf(*id)),
                    second: Box::new(DwindleNode::Leaf(new_id)),
                };
                true
            }
            DwindleNode::Leaf(_) => false,
            DwindleNode::Split {
                dir,
                ratio,
                first,
                second,
            } => {
                let (r1, r2) = split_rect(rect, *dir, *ratio, gap);
                first.split_leaf(target, new_id, r1, gap, opts) || second.split_leaf(target, new_id, r2, gap, opts)
            }
        }
    }

    /// Swap two leaves in place.
    pub fn swap(&mut self, a: u64, b: u64) {
        match self {
            DwindleNode::Leaf(id) => {
                if *id == a {
                    *id = b;
                } else if *id == b {
                    *id = a;
                }
            }
            DwindleNode::Split { first, second, .. } => {
                first.swap(a, b);
                second.swap(a, b);
            }
        }
    }
}

fn inset_area(area: Rectangle<i32, Logical>, gap: i32) -> Rectangle<i32, Logical> {
    Rectangle::new(
        (area.loc.x + gap, area.loc.y + gap).into(),
        ((area.size.w - 2 * gap).max(2), (area.size.h - 2 * gap).max(2)).into(),
    )
}

fn split_rect(
    rect: Rectangle<i32, Logical>,
    dir: SplitDir,
    ratio: f32,
    gap: i32,
) -> (Rectangle<i32, Logical>, Rectangle<i32, Logical>) {
    let ratio = ratio.clamp(0.15, 0.85);
    match dir {
        SplitDir::Horizontal => {
            let avail = (rect.size.w - gap).max(2);
            let w1 = ((avail as f32 * ratio).round() as i32).clamp(1, avail - 1);
            let w2 = (avail - w1).max(1);
            (
                Rectangle::new(rect.loc, (w1, rect.size.h).into()),
                Rectangle::new((rect.loc.x + w1 + gap, rect.loc.y).into(), (w2, rect.size.h).into()),
            )
        }
        SplitDir::Vertical => {
            let avail = (rect.size.h - gap).max(2);
            let h1 = ((avail as f32 * ratio).round() as i32).clamp(1, avail - 1);
            let h2 = (avail - h1).max(1);
            (
                Rectangle::new(rect.loc, (rect.size.w, h1).into()),
                Rectangle::new((rect.loc.x, rect.loc.y + h1 + gap).into(), (rect.size.w, h2).into()),
            )
        }
    }
}

/// How a new window splits its pane.
#[derive(Debug, Clone, Copy)]
pub struct SplitOpts {
    /// fixed direction; `None` = along the pane's longer side
    pub dir: Option<SplitDir>,
    /// share the existing window keeps
    pub ratio: f32,
}

impl Default for SplitOpts {
    fn default() -> Self {
        Self { dir: None, ratio: 0.5 }
    }
}

impl From<&TilingSettings> for SplitOpts {
    fn from(t: &TilingSettings) -> Self {
        Self {
            dir: match t.split_direction {
                SplitDirection::Auto => None,
                SplitDirection::Horizontal => Some(SplitDir::Horizontal),
                SplitDirection::Vertical => Some(SplitDir::Vertical),
            },
            ratio: t.split_ratio,
        }
    }
}

#[cfg(test)]
pub fn dwindle_insert(
    root: &mut Option<DwindleNode>,
    id: u64,
    target: Option<u64>,
    area: Rectangle<i32, Logical>,
    gaps: Gaps,
) {
    dwindle_insert_with(root, id, target, area, gaps, SplitOpts::default());
}

pub fn dwindle_insert_with(
    root: &mut Option<DwindleNode>,
    id: u64,
    target: Option<u64>,
    area: Rectangle<i32, Logical>,
    gaps: Gaps,
    opts: SplitOpts,
) {
    let Some(node) = root.as_mut() else {
        *root = Some(DwindleNode::Leaf(id));
        return;
    };
    if node.contains(id) {
        return;
    }
    let split_target = target.filter(|t| node.contains(*t)).unwrap_or_else(|| node.tail_leaf());
    let inner = inset_area(area, gaps.outer);
    node.split_leaf(split_target, id, inner, gaps.inner, opts);
}

pub fn dwindle_remove(root: &mut Option<DwindleNode>, id: u64) -> bool {
    fn remove_rec(node: DwindleNode, id: u64) -> (Option<DwindleNode>, bool) {
        match node {
            DwindleNode::Leaf(w) if w == id => (None, true),
            DwindleNode::Leaf(w) => (Some(DwindleNode::Leaf(w)), false),
            DwindleNode::Split {
                dir,
                ratio,
                first,
                second,
            } => {
                let (f, removed_f) = remove_rec(*first, id);
                if removed_f {
                    return match f {
                        Some(f) => (
                            Some(DwindleNode::Split {
                                dir,
                                ratio,
                                first: Box::new(f),
                                second,
                            }),
                            true,
                        ),
                        None => (Some(*second), true),
                    };
                }
                let (s, removed_s) = remove_rec(*second, id);
                if removed_s {
                    return match s {
                        Some(s) => (
                            Some(DwindleNode::Split {
                                dir,
                                ratio,
                                first: Box::new(f.unwrap()),
                                second: Box::new(s),
                            }),
                            true,
                        ),
                        None => (f, true),
                    };
                }
                (
                    Some(DwindleNode::Split {
                        dir,
                        ratio,
                        first: Box::new(f.unwrap()),
                        second: Box::new(s.unwrap()),
                    }),
                    false,
                )
            }
        }
    }
    let Some(node) = root.take() else { return false };
    let (next, removed) = remove_rec(node, id);
    *root = next;
    removed
}

pub fn dwindle_layout(
    root: &Option<DwindleNode>,
    area: Rectangle<i32, Logical>,
    gaps: Gaps,
) -> Vec<(u64, Rectangle<i32, Logical>)> {
    let mut out = Vec::new();
    if let Some(node) = root {
        node.layout_in(inset_area(area, gaps.outer), gaps.inner, &mut out);
    }
    out
}

/// Per-(output, space_index) dwindle trees and last-applied mode.
#[derive(Debug, Default)]
pub struct DwindleState {
    pub mode: TilingMode,
    /// the tiling settings last applied (to notice edits)
    pub cfg: TilingSettings,
    pub trees: HashMap<(String, usize), Option<DwindleNode>>,
}

impl DwindleState {
    pub fn new(mode: TilingMode) -> Self {
        Self {
            mode,
            cfg: crate::settings::get().tiling.clone(),
            trees: HashMap::new(),
        }
    }

    pub fn remove_everywhere(&mut self, id: u64) {
        for tree in self.trees.values_mut() {
            dwindle_remove(tree, id);
        }
    }
}

fn is_tileable(window: &WindowElement) -> bool {
    if !window.alive() || window.is_fullscreen() {
        return false;
    }
    #[cfg(feature = "xwayland")]
    if let Some(x11) = window.0.x11_surface() {
        if x11.is_override_redirect() {
            return false;
        }
    }
    // Wait until initial placement has run on the first sized commit.
    let awaiting_initial = window
        .user_data()
        .get::<std::cell::RefCell<Option<crate::wm::NeedsInitialPlacement>>>()
        .map(|c| c.borrow().is_some())
        .unwrap_or(false);
    !awaiting_initial
}

impl<BackendData: Backend> OrchardState<BackendData> {
    pub fn is_tiling_mode(&self) -> bool {
        self.dwindle.mode == TilingMode::Tiling
    }

    /// Move `window` to `loc` without changing its stacking order or focus.
    fn move_window_preserving_stack(&mut self, window: &WindowElement, loc: Point<i32, Logical>) {
        let above: Vec<WindowElement> = self
            .space
            .elements()
            .skip_while(|w| **w != *window)
            .skip(1)
            .cloned()
            .collect();
        self.space.map_element(window.clone(), loc, false);
        for w in &above {
            self.space.raise_element(w, false);
        }
    }

    /// Resize a maximized (zoomed) or snap-tiled window to fit `area` without
    /// changing its stacking order, focus, or saved restore geometry.
    fn refit_zoomed_or_tiled(&mut self, window: &WindowElement, area: Rectangle<i32, Logical>) {
        if !is_tileable(window) {
            return;
        }
        let zoomed = self.is_zoomed(window);
        let tile = window.decoration_state().tile;
        if !zoomed && tile.is_none() {
            return;
        }
        let rect = if zoomed {
            area
        } else {
            tile_rect_with(area, tile.unwrap(), self.gaps())
        };
        let header = if window.is_ssd() { HEADER_BAR_HEIGHT } else { 0 };
        let client: Size<i32, Logical> = (rect.size.w, (rect.size.h - header).max(1)).into();

        // Skip redundant configures if the window already matches `rect`.
        let cur_loc = self.space.element_location(window);
        let cur_size = window.client_geometry().size;
        if cur_loc == Some(rect.loc) && cur_size == client {
            return;
        }

        match window.0.underlying_surface() {
            WindowSurface::Wayland(toplevel) => {
                toplevel.with_pending_state(|s| {
                    s.bounds = Some(area.size);
                    if zoomed {
                        s.states.set(xdg_toplevel::State::Maximized);
                    } else if let Some(tile) = tile {
                        let edges = tiled_states(tile);
                        s.states.unset(xdg_toplevel::State::Maximized);
                        for (state, on) in [
                            (xdg_toplevel::State::TiledLeft, edges[0]),
                            (xdg_toplevel::State::TiledRight, edges[1]),
                            (xdg_toplevel::State::TiledTop, edges[2]),
                            (xdg_toplevel::State::TiledBottom, edges[3]),
                        ] {
                            if on {
                                s.states.set(state);
                            } else {
                                s.states.unset(state);
                            }
                        }
                    }
                    s.size = Some(client);
                });
                toplevel.send_pending_configure();
            }
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(x) => {
                let _ = x.set_maximized(zoomed);
                let _ = x.configure(Rectangle::new(rect.loc + Point::from((0, header)), client));
            }
        }
        self.animate_frame(window, rect);
        self.move_window_preserving_stack(window, rect.loc);
    }

    /// Refit all tiled (dwindle or snap-tiled) and maximized windows on `output`
    /// to its current usable area (e.g. after the Dock hides/unhides or moves).
    pub fn refit_windows_on_output(&mut self, output: &Output) {
        if self.is_tiling_mode() {
            self.retile_output(output);
            return;
        }
        let Some(geo) = self.space.output_geometry(output) else { return };
        let zone = layer_map_for_output(output).non_exclusive_zone();
        let area = Rectangle::new(geo.loc + zone.loc, zone.size);
        if area.size.w <= 2 * GAP || area.size.h <= 2 * GAP {
            return;
        }
        let windows: Vec<WindowElement> = self
            .windows_on_output(output)
            .into_iter()
            .map(|(w, _)| w)
            .collect();
        for window in windows {
            self.refit_zoomed_or_tiled(&window, area);
        }
    }

    /// Refit all tiled and maximized windows across every output.
    pub fn refit_all_windows(&mut self) {
        let outputs: Vec<Output> = self.space.outputs().cloned().collect();
        for output in outputs {
            self.refit_windows_on_output(&output);
        }
    }

    /// Apply a dwindle tile frame without altering stacking order or focus.
    fn apply_dwindle_frame(&mut self, window: &WindowElement, rect: Rectangle<i32, Logical>, animate: bool) {
        let header = if window.is_ssd() { HEADER_BAR_HEIGHT } else { 0 };
        let client: Size<i32, Logical> = (rect.size.w, (rect.size.h - header).max(1)).into();

        // Skip redundant configures if the window is already at `rect` and tiled.
        let cur_loc = self.space.element_location(window);
        let cur_size = window.client_geometry().size;
        let already_dwindled = window.decoration_state().dwindled;
        if already_dwindled && cur_loc == Some(rect.loc) && cur_size == client && !self.is_zoomed(window) {
            return;
        }

        match window.0.underlying_surface() {
            WindowSurface::Wayland(toplevel) => {
                toplevel.with_pending_state(|s| {
                    s.states.unset(xdg_toplevel::State::Maximized);
                    s.states.set(xdg_toplevel::State::TiledLeft);
                    s.states.set(xdg_toplevel::State::TiledRight);
                    s.states.set(xdg_toplevel::State::TiledTop);
                    s.states.set(xdg_toplevel::State::TiledBottom);
                    s.size = Some(client);
                });
                toplevel.send_pending_configure();
            }
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(x) => {
                let _ = x.set_maximized(false);
                let _ = x.configure(Rectangle::new(rect.loc + Point::from((0, header)), client));
            }
        }

        window.decoration_state().tile = None;
        window.decoration_state().dwindled = true;
        if animate {
            self.animate_frame(window, rect);
        }
        // Preserve stacking order (Space::map_element raises on top).
        self.move_window_preserving_stack(window, rect.loc);
    }

    /// Reconcile and tile all tileable windows on `output`'s active Space.
    /// `newly_placed` is a freshly opened window that should appear directly
    /// in its tile (using its open animation rather than sliding from spawn).
    pub fn retile_output_with(&mut self, output: &Output, newly_placed: Option<u64>, split_target: Option<u64>) {
        if !self.is_tiling_mode() {
            return;
        }
        let Some(geo) = self.space.output_geometry(output) else { return };
        let zone = layer_map_for_output(output).non_exclusive_zone();
        let area = Rectangle::new(geo.loc + zone.loc, zone.size);
        if area.size.w <= 2 * GAP || area.size.h <= 2 * GAP {
            return;
        }

        let name = output.name();
        let (active_space, _) = self.spaces.info(&name);
        let key = (name, active_space);

        let windows: Vec<WindowElement> = self
            .windows_on_output(output)
            .into_iter()
            .map(|(w, _)| w)
            .filter(is_tileable)
            .collect();

        let live_ids: Vec<u64> = windows.iter().map(|w| w.ipc_id().0).collect();

        // Prune leaves that are no longer on this output/Space.
        let tree = self.dwindle.trees.entry(key.clone()).or_default();
        let mut stale = Vec::new();
        if let Some(node) = tree.as_ref() {
            let mut in_tree = Vec::new();
            node.ids(&mut in_tree);
            for id in in_tree {
                if !live_ids.contains(&id) {
                    stale.push(id);
                }
            }
        }
        for id in stale {
            dwindle_remove(tree, id);
        }

        // Insert any live windows not yet in the tree.
        let cfg = crate::settings::get().tiling.clamped();
        let opts = SplitOpts::from(&cfg);
        let gaps = if cfg.smart_gaps && live_ids.len() <= 1 { Gaps::NONE } else { Gaps::from(&cfg) };
        let tree = self.dwindle.trees.entry(key.clone()).or_default();
        for &id in &live_ids {
            let target = if Some(id) == newly_placed {
                split_target
            } else {
                None
            };
            dwindle_insert_with(tree, id, target, area, gaps, opts);
        }

        let rects = dwindle_layout(tree, area, gaps);
        for (id, rect) in rects {
            let Some(window) = windows.iter().find(|w| w.ipc_id().0 == id) else { continue };
            let is_new = newly_placed == Some(id);
            if !is_new && !window.decoration_state().dwindled {
                self.save_restore_geometry(window);
            }
            self.apply_dwindle_frame(window, rect, !is_new);
        }
    }

    pub fn retile_output(&mut self, output: &Output) {
        self.retile_output_with(output, None, None);
    }

    pub fn retile_all(&mut self) {
        if !self.is_tiling_mode() {
            return;
        }
        let outputs: Vec<Output> = self.space.outputs().cloned().collect();
        for output in outputs {
            self.retile_output(&output);
        }
    }

    /// Place a newly sized window into the dwindle tree on its output.
    pub fn dwindle_place_new(&mut self, window: &WindowElement) {
        let Some(output) = self
            .window_output(window)
            .or_else(|| self.active_output())
            .or_else(|| self.primary_output())
        else {
            return;
        };
        let target = self
            .focused_window()
            .filter(|w| w != window && self.window_output(w).as_ref() == Some(&output))
            .filter(|_| crate::settings::get().tiling.new_window == NewWindowPlacement::Focused)
            .map(|w| w.ipc_id().0);
        self.retile_output_with(&output, Some(window.ipc_id().0), target);
        window.start_open_animation(self.clock.now().into());
        self.focus_window(window);
    }

    /// Restore all dwindle-tiled windows on `output` back to floating geometry.
    pub fn untile_dwindled_on_output(&mut self, output: &Output) {
        let windows: Vec<WindowElement> = self
            .windows_on_output(output)
            .into_iter()
            .map(|(w, _)| w)
            .filter(|w| w.alive() && w.decoration_state().dwindled)
            .collect();
        for window in windows {
            self.untile_single_dwindled(&window);
        }
    }

    /// Leave dwindle tiling mode: clear trees and restore all tiled windows to floating.
    pub fn untile_all_dwindle(&mut self) {
        self.dwindle.trees.clear();
        let windows: Vec<WindowElement> = self
            .space
            .elements()
            .filter(|w| w.alive() && w.decoration_state().dwindled)
            .cloned()
            .collect();
        for window in windows {
            self.untile_single_dwindled(&window);
        }
    }

    fn untile_single_dwindled(&mut self, window: &WindowElement) {
        window.decoration_state().dwindled = false;
        let restore = window.decoration_state().restore_geometry.take();
        let header = if window.is_ssd() { HEADER_BAR_HEIGHT } else { 0 };
        let frame = if let Some(r) = restore {
            Rectangle::new(r.loc, (r.size.w, r.size.h + header).into())
        } else if let Some(area) = self.usable_area_for(window) {
            // Opened while in Tiling mode: give it a comfortable 2/3 centered floating frame.
            let w = (area.size.w * 2 / 3).max(400).min(area.size.w);
            let h = (area.size.h * 2 / 3).max(300).min(area.size.h);
            let loc: Point<i32, Logical> = (
                area.loc.x + (area.size.w - w) / 2,
                area.loc.y + (area.size.h - h) / 2,
            )
                .into();
            Rectangle::new(loc, (w, h).into())
        } else {
            return;
        };
        self.apply_frame(window, frame, None);
    }

    /// Handle releasing a dragged window in Tiling mode: swap with the window
    /// under `pointer` (or move to another output), then retile.
    pub fn dwindle_drop_drag(&mut self, window: &WindowElement, pointer: Point<f64, Logical>) {
        if !self.is_tiling_mode() {
            return;
        }
        let id = window.ipc_id().0;
        // Find topmost other tileable window whose frame contains `pointer`.
        let other = self
            .space
            .elements()
            .rev()
            .find(|w| {
                **w != *window
                    && is_tileable(w)
                    && self
                        .space
                        .element_location(w)
                        .map(|loc| {
                            let r = Rectangle::new(loc, SpaceElement::geometry(*w).size).to_f64();
                            r.contains(pointer)
                        })
                        .unwrap_or(false)
            })
            .cloned();

        let src_out = self.window_output(window);
        let dst_out = other
            .as_ref()
            .and_then(|o| self.window_output(o))
            .or_else(|| self.space.output_under(pointer).next().cloned());

        match (src_out, dst_out, other) {
            (Some(src), Some(dst), Some(other)) if src == dst => {
                let (active, _) = self.spaces.info(&src.name());
                let key = (src.name(), active);
                if let Some(Some(node)) = self.dwindle.trees.get_mut(&key) {
                    node.swap(id, other.ipc_id().0);
                }
            }
            (Some(src), Some(dst), _) if src != dst => {
                // Move window into the destination output's tree.
                self.dwindle.remove_everywhere(id);
                if let Some(geo) = self.space.output_geometry(&dst) {
                    self.space.map_element(window.clone(), geo.loc + Point::from((self.gaps().outer, self.gaps().outer)), false);
                }
            }
            _ => {}
        }
        self.retile_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: Gaps = Gaps::uniform(GAP);

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    #[test]
    fn halves_cover_the_area() {
        let area = rect(0, 24, 1921, 1000);
        let l = tile_rect_with(area, Tile::Left, G);
        let r = tile_rect_with(area, Tile::Right, G);
        // margin, left half, gap, right half, margin
        assert_eq!(l.loc.x, GAP);
        assert_eq!(GAP + l.size.w + GAP + r.size.w + GAP, 1921);
        assert_eq!(r.loc.x, l.loc.x + l.size.w + GAP);
        assert_eq!(l.loc.y, 24 + GAP);
        assert_eq!(l.size.h, 1000 - 2 * GAP);
        let tl = tile_rect_with(area, Tile::TopLeft, G);
        let bl = tile_rect_with(area, Tile::BottomLeft, G);
        assert_eq!(bl.loc.y, tl.loc.y + tl.size.h + GAP);
        assert_eq!(bl.loc.y + bl.size.h, 24 + 1000 - GAP);
        assert_eq!(tile_rect_with(area, Tile::Fill, G), area);
    }

    #[test]
    fn edges() {
        let outs = vec![(rect(0, 0, 1920, 1080), rect(0, 24, 1920, 980))];
        assert_eq!(edge_tile(&outs, (0.0, 500.0).into(), None, None).unwrap().0, Tile::Left);
        assert_eq!(edge_tile(&outs, (1919.0, 500.0).into(), None, None).unwrap().0, Tile::Right);
        assert_eq!(edge_tile(&outs, (900.0, 0.0).into(), None, None).unwrap().0, Tile::Fill);
        assert_eq!(edge_tile(&outs, (0.0, 30.0).into(), None, None).unwrap().0, Tile::TopLeft);
        assert_eq!(edge_tile(&outs, (1919.0, 1000.0).into(), None, None).unwrap().0, Tile::BottomRight);
        assert!(edge_tile(&outs, (900.0, 500.0).into(), None, None).is_none());
        // generous zones: 20 px off the edge still counts, the menu bar is "top"
        assert_eq!(edge_tile(&outs, (20.0, 500.0).into(), None, None).unwrap().0, Tile::Left);
        assert_eq!(edge_tile(&outs, (900.0, 20.0).into(), None, None).unwrap().0, Tile::Fill);
        assert_eq!(edge_tile(&outs, (20.0, 150.0).into(), None, None).unwrap().0, Tile::TopLeft);
        // hysteresis: a shown target survives a small wobble outwards
        assert!(edge_tile(&outs, (60.0, 500.0).into(), None, None).is_none());
        assert_eq!(edge_tile(&outs, (60.0, 500.0).into(), None, Some((Tile::Left, outs[0].1))).unwrap().0, Tile::Left);
        assert!(edge_tile(&outs, (120.0, 500.0).into(), None, Some((Tile::Left, outs[0].1))).is_none());
    }

    #[test]
    fn window_pushed_against_the_side() {
        let outs = vec![(rect(0, 0, 1920, 1080), rect(0, 24, 1920, 980))];
        let drag = |x: f64, start_x: f64| {
            Some(DragWindow {
                rect: Rectangle::new((x, 200.0).into(), (800.0, 600.0).into()),
                start_x,
            })
        };
        // pointer 300 px into the title bar, window flush left: tiles left
        assert_eq!(edge_tile(&outs, (300.0, 500.0).into(), drag(0.0, 600.0), None).unwrap().0, Tile::Left);
        assert_eq!(edge_tile(&outs, (300.0, 500.0).into(), drag(-50.0, 600.0), None).unwrap().0, Tile::Left);
        // right side
        assert_eq!(edge_tile(&outs, (1500.0, 500.0).into(), drag(1120.0, 600.0), None).unwrap().0, Tile::Right);
        // not yet touching
        assert!(edge_tile(&outs, (320.0, 500.0).into(), drag(20.0, 600.0), None).is_none());
        // started flush left (moving it up / down): no tile
        assert!(edge_tile(&outs, (300.0, 500.0).into(), drag(0.0, 0.0), None).is_none());
        // pointer height still picks the quarter
        assert_eq!(edge_tile(&outs, (300.0, 60.0).into(), drag(0.0, 600.0), None).unwrap().0, Tile::TopLeft);
    }

    fn two_side_by_side() -> Vec<(Rectangle<i32, Logical>, Rectangle<i32, Logical>)> {
        vec![
            (rect(0, 0, 1920, 1080), rect(0, 24, 1920, 980)),
            (rect(1920, 0, 2560, 1440), rect(1920, 0, 2560, 1440)),
        ]
    }

    #[test]
    fn every_display_has_two_halves() {
        let outs = two_side_by_side();
        let hit = |x: f64| edge_tile(&outs, (x, 500.0).into(), None, None);
        // outer edges
        assert_eq!(hit(0.0), Some((Tile::Left, outs[0].1)));
        assert_eq!(hit(4479.0), Some((Tile::Right, outs[1].1)));
        // the seam: each side tiles on its own display
        assert_eq!(hit(1919.0), Some((Tile::Right, outs[0].1)));
        assert_eq!(hit(1920.0), Some((Tile::Left, outs[1].1)));
        assert_eq!(hit(1910.0), Some((Tile::Right, outs[0].1)));
        // but only a thin strip, so crossing over stays easy
        assert!(hit(1890.0).is_none());
        assert!(hit(1950.0).is_none());
        // the tile lands in that display's half
        let r = tile_rect_with(outs[1].1, Tile::Left, G);
        assert_eq!(r.loc.x, 1920 + GAP);
        assert_eq!(r.size.w, (2560 - 3 * GAP) / 2);
    }

    #[test]
    fn hysteresis_is_per_display() {
        let outs = two_side_by_side();
        // shown on the left display: a wobble keeps it
        let cur = Some((Tile::Right, outs[0].1));
        assert_eq!(edge_tile(&outs, (1880.0, 500.0).into(), None, cur).unwrap(), (Tile::Right, outs[0].1));
        // over on the next display the old target gives no extra room
        assert!(edge_tile(&outs, (1960.0, 500.0).into(), None, cur).is_none());
    }

    #[test]
    fn window_against_a_seam() {
        let outs = two_side_by_side();
        let d = |x: f64| Some(DragWindow { rect: Rectangle::new((x, 200.0).into(), (800.0, 600.0).into()), start_x: 600.0 });
        // flush against the seam, pointer in the middle of the title bar: right half
        assert_eq!(edge_tile(&outs, (1500.0, 500.0).into(), d(1120.0), None).unwrap(), (Tile::Right, outs[0].1));
        // sticking out a little still counts
        assert_eq!(edge_tile(&outs, (1540.0, 500.0).into(), d(1160.0), None).unwrap(), (Tile::Right, outs[0].1));
        // well over: on its way to the next display
        assert!(edge_tile(&outs, (1700.0, 500.0).into(), d(1320.0), None).is_none());
    }

    #[test]
    fn stacked_displays_get_top_at_the_seam() {
        let outs = vec![
            (rect(0, 0, 1920, 1080), rect(0, 0, 1920, 1080)),
            (rect(0, 1080, 1920, 1080), rect(0, 1104, 1920, 956)),
        ];
        assert_eq!(edge_tile(&outs, (900.0, 1090.0).into(), None, None).unwrap(), (Tile::Fill, outs[1].1));
        assert!(edge_tile(&outs, (900.0, 1200.0).into(), None, None).is_none());
    }

    #[test]
    fn neighbours() {
        let outs = two_side_by_side();
        assert_eq!(neighbour(&outs, outs[0].0, 1), Some(outs[1].1));
        assert_eq!(neighbour(&outs, outs[1].0, -1), Some(outs[0].1));
        assert_eq!(neighbour(&outs, outs[0].0, -1), None);
    }

    #[test]
    fn dwindle_spiral_layout_matches_hyprland() {
        let area = rect(0, 24, 1920, 1056);
        let mut root = None;

        // 1 window: fills usable area with outer gap
        dwindle_insert(&mut root, 1, None, area, G);
        let r1 = dwindle_layout(&root, area, G);
        assert_eq!(r1.len(), 1);
        assert_eq!(r1[0], (1, rect(GAP, 24 + GAP, 1920 - 2 * GAP, 1056 - 2 * GAP)));

        // 2nd window: container is wider than tall -> splits horizontally (left | right)
        dwindle_insert(&mut root, 2, Some(1), area, G);
        let r2 = dwindle_layout(&root, area, G);
        assert_eq!(r2.len(), 2);
        assert_eq!(r2[0].1.loc.y, r2[1].1.loc.y);
        assert_eq!(r2[0].1.size.h, r2[1].1.size.h);
        assert_eq!(r2[1].1.loc.x, r2[0].1.loc.x + r2[0].1.size.w + GAP);

        // 3rd window splitting 2: right half (948x1040) is taller than wide -> splits vertically (2 over 3)
        dwindle_insert(&mut root, 3, Some(2), area, G);
        let r3 = dwindle_layout(&root, area, G);
        assert_eq!(r3.len(), 3);
        let (_, b1) = r3.iter().find(|(id, _)| *id == 1).unwrap();
        let (_, b2) = r3.iter().find(|(id, _)| *id == 2).unwrap();
        let (_, b3) = r3.iter().find(|(id, _)| *id == 3).unwrap();
        assert_eq!(b1.size.h, 1056 - 2 * GAP);
        assert_eq!(b2.loc.x, b3.loc.x);
        assert_eq!(b3.loc.y, b2.loc.y + b2.size.h + GAP);

        // 4th window splitting 3: bottom-right (948x516) is wider than tall -> splits horizontally (3 | 4)
        dwindle_insert(&mut root, 4, Some(3), area, G);
        let r4 = dwindle_layout(&root, area, G);
        assert_eq!(r4.len(), 4);
        let (_, c3) = r4.iter().find(|(id, _)| *id == 3).unwrap();
        let (_, c4) = r4.iter().find(|(id, _)| *id == 4).unwrap();
        assert_eq!(c3.loc.y, c4.loc.y);
        assert_eq!(c4.loc.x, c3.loc.x + c3.size.w + GAP);

        // Removing window 3 promotes window 4 to fill bottom-right
        assert!(dwindle_remove(&mut root, 3));
        let r_after = dwindle_layout(&root, area, G);
        assert_eq!(r_after.len(), 3);
        let (_, d4) = r_after.iter().find(|(id, _)| *id == 4).unwrap();
        assert_eq!(*d4, *b3);

        // Swapping 1 and 4 exchanges their rects
        root.as_mut().unwrap().swap(1, 4);
        let r_swapped = dwindle_layout(&root, area, G);
        let (_, s1) = r_swapped.iter().find(|(id, _)| *id == 1).unwrap();
        let (_, s4) = r_swapped.iter().find(|(id, _)| *id == 4).unwrap();
        assert_eq!(*s1, *b3);
        assert_eq!(*s4, *b1);
    }
}
