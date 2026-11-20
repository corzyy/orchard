//! Window tiling, macOS Sequoia style.
//!
//! * Drag a window against the left / right screen edge: it fills that half.
//!   The top edge fills the screen; the corners give quarters. A translucent
//!   preview shows the target while the pointer rests there.
//! * ⌥⌘←/→/↑/↓ tile to a half, ⌥⌘C centers, ⌥⌘R returns to the previous size.
//! * Dragging a tiled (or zoomed) window away restores its previous size, with
//!   the pointer keeping its relative spot in the title bar.

use smithay::{
    desktop::{layer_map_for_output, space::SpaceElement, WindowSurface},
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
const EDGE_ZONE: f64 = 32.0;
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

pub fn tile_rect(area: Rectangle<i32, Logical>, tile: Tile) -> Rectangle<i32, Logical> {
    if tile == Tile::Fill {
        return area;
    }
    // margin to the screen edges, then GAP between the halves
    let (x, y) = (area.loc.x + GAP, area.loc.y + GAP);
    let (w, h) = ((area.size.w - 2 * GAP).max(2), (area.size.h - 2 * GAP).max(2));
    let (lw, th) = ((w - GAP) / 2, (h - GAP) / 2);
    let (rw, bh) = (w - GAP - lw, h - GAP - th);
    let (x2, y2) = (x + lw + GAP, y + th + GAP);
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
pub fn edge_tile(
    outputs: &[(Rectangle<i32, Logical>, Rectangle<i32, Logical>)],
    pos: Point<f64, Logical>,
    window: Option<DragWindow>,
    current: Option<(Tile, Rectangle<i32, Logical>)>,
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
    let zone = |seam: bool| if seam { SEAM_ZONE } else { EDGE_ZONE };
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
    let near_top = pos.y <= a.loc.y + cy;
    let near_bottom = pos.y >= a.loc.y + a.size.h - cy;
    let near_left = pos.x <= g.loc.x + cx;
    let near_right = pos.x >= g.loc.x + g.size.w - cx;
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
        self.apply_frame(window, tile_rect(area, tile), Some(tile));
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
        let hit = edge_tile(&self.output_areas(), pos, drag, current);
        let target = hit.map(|(t, a)| tile_rect(a, t));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    #[test]
    fn halves_cover_the_area() {
        let area = rect(0, 24, 1921, 1000);
        let l = tile_rect(area, Tile::Left);
        let r = tile_rect(area, Tile::Right);
        // margin, left half, gap, right half, margin
        assert_eq!(l.loc.x, GAP);
        assert_eq!(GAP + l.size.w + GAP + r.size.w + GAP, 1921);
        assert_eq!(r.loc.x, l.loc.x + l.size.w + GAP);
        assert_eq!(l.loc.y, 24 + GAP);
        assert_eq!(l.size.h, 1000 - 2 * GAP);
        let tl = tile_rect(area, Tile::TopLeft);
        let bl = tile_rect(area, Tile::BottomLeft);
        assert_eq!(bl.loc.y, tl.loc.y + tl.size.h + GAP);
        assert_eq!(bl.loc.y + bl.size.h, 24 + 1000 - GAP);
        assert_eq!(tile_rect(area, Tile::Fill), area);
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
        let r = tile_rect(outs[1].1, Tile::Left);
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
}
