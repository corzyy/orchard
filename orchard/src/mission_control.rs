//! Mission Control and App Exposé.
//!
//! Opens on every display at once. The windows of each display's current
//! Space spring into a tidy grid, under a translucent Spaces bar that shows a
//! miniature of every Space plus "+". Window and Space previews fade in with a
//! slight stagger, the hovered window lifts under a blue outline and shows its
//! title, and Space thumbnails grow under the pointer. Windows can be dragged
//! onto a Space, onto "+" (a new Space) or onto another display. The arrow keys
//! or Tab select a window, Return opens it, the scroll wheel steps through
//! windows, and ⌃⌘←/→/1…9 switch Spaces without leaving Mission Control.
//!
//! Space previews are rendered lazily after the first frame (a few per tick)
//! and kept for the session, so opening Mission Control never waits for
//! offscreen renders. Only previews whose Space actually changed are drawn
//! again.

use std::time::Duration;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker,
            element::{
                texture::TextureRenderElement,
                utils::RescaleRenderElement,
                AsRenderElements, Id,
            },
            gles::{GlesRenderer, GlesTexture},
            Bind, Color32F, Offscreen,
        },
    },
    desktop::space::SpaceElement,
    output::Output,
    utils::{IsAlive, Logical, Physical, Point, Rectangle, Scale, Size, Transform},
};

use crate::{
    effects::{
        lerp_rect,
        spring::{Spring, SpringParams},
    },
    shell::{WindowElement, WindowRenderElement},
    state::{Backend, OrchardState},
};

/// Height of the Spaces bar at the top of a display.
pub const STRIP_H: f64 = 164.0;
/// Space thumbnails' height (logical px); width follows the display's aspect.
const THUMB_H: f64 = 86.0;
const THUMB_GAP: f64 = 20.0;
/// gap between the last Space thumbnail and the "+" button
const ADD_GAP: f64 = 14.0;
const ADD_D: f64 = 30.0;
/// top of the Space thumbnails below the menu bar
const THUMB_TOP: f64 = 22.0;
const MARGIN: f64 = 44.0;
/// area reserved above the grid in App Exposé for the app name
const APP_HEADER_H: f64 = 76.0;
/// pointer travel before a press on a window becomes a drag
const DRAG_THRESHOLD: f64 = 6.0;
/// radius of a Space's remove button (centered on the thumbnail's corner)
pub const REMOVE_R: f64 = 10.0;
/// a dragged window shrinks to this width over the Spaces strip
const DRAG_SMALL_W: f64 = 180.0;
/// windows may grow up to this factor when the grid has room (macOS does)
const MAX_CELL_SCALE: f64 = 1.7;
/// how much the highlighted window grows under the pointer
pub(crate) const HOVER_GROW: f64 = 0.014;
/// how much a hovered Space thumbnail grows
pub(crate) const THUMB_HOVER_GROW: f64 = 0.05;
/// Space previews rendered per tick while Mission Control is open
const THUMB_BUDGET: usize = 3;
/// scroll distance (logical px) that steps to the next window
const SCROLL_STEP: f64 = 24.0;

/// One window in the grid.
#[derive(Debug)]
pub struct McWin {
    pub window: WindowElement,
    /// where the window really is (incl. title bar), global logical
    pub real: Rectangle<f64, Logical>,
    /// its cell in the grid
    pub target: Rectangle<f64, Logical>,
    /// grid position it is moving from after a re-layout
    move_from: Rectangle<f64, Logical>,
    move_spring: Option<Spring>,
    /// own opening animation, for windows that appear while Mission Control
    /// is already open (Space switched, Space removed) or their staggered
    /// entrance when it opens
    enter: Option<Spring>,
}

impl McWin {
    fn new(window: WindowElement, real: Rectangle<f64, Logical>, target: Rectangle<f64, Logical>) -> Self {
        McWin {
            window,
            real,
            target,
            move_from: target,
            move_spring: None,
            enter: None,
        }
    }

    /// The window's cell at `now` (moves smoothly after a re-layout).
    pub fn grid_at(&self, now: Duration) -> Rectangle<f64, Logical> {
        match &self.move_spring {
            Some(s) => lerp_rect(self.move_from, self.target, s.value_at(now)),
            None => self.target,
        }
    }

    /// Where the window is drawn, at overall progress `p`.
    pub fn rect_at(&self, p: f64, closing: bool, now: Duration) -> Rectangle<f64, Logical> {
        let p = match (&self.enter, closing) {
            (Some(e), false) => e.value_at(now).min(p.max(0.0)),
            _ => p,
        };
        lerp_rect(self.real, self.grid_at(now), p)
    }
}

/// Mission Control on one display.
#[derive(Debug)]
pub struct McScreen {
    pub output: Output,
    pub geo: Rectangle<i32, Logical>,
    /// area the grid fills (global logical)
    pub area: Rectangle<f64, Logical>,
    /// height the shell reserves at the top (menu bar) in logical px
    pub top_inset: f64,
    /// bottom to top
    pub wins: Vec<McWin>,
    /// thumbnails of the Spaces strip (global logical), last one is "+"
    pub thumbs: Vec<Rectangle<f64, Logical>>,
    /// miniature of each Space (wallpaper + its windows), rendered lazily
    pub thumb_textures: Vec<Option<ThumbTexture>>,
    /// where each thumbnail came from when the strip was last re-laid out
    thumb_from: Vec<Rectangle<f64, Logical>>,
    /// thumbnail glides after a Space was added or removed
    thumb_move: Option<Spring>,
    /// 0..1 hover growth of each thumbnail (and "+")
    pub thumb_hover: Vec<Spring>,
}

impl McScreen {
    /// Number of Spaces shown in the strip.
    pub fn space_count(&self) -> usize {
        self.thumbs.len().saturating_sub(1)
    }

    /// A thumbnail's shown rect at `now` (slides after a re-layout).
    pub fn thumb_rect_at(&self, i: usize, now: Duration) -> Rectangle<f64, Logical> {
        let Some(target) = self.thumbs.get(i).copied() else {
            return Rectangle::default();
        };
        match &self.thumb_move {
            Some(s) if !s.is_done_at(now) => {
                let from = self.thumb_from.get(i).copied().unwrap_or(target);
                lerp_rect(from, target, s.value_at(now))
            }
            _ => target,
        }
    }

    /// The drawn rect of a thumbnail, grown under the pointer.
    pub fn thumb_drawn_at(&self, i: usize, now: Duration) -> Rectangle<f64, Logical> {
        let target = self.thumb_rect_at(i, now);
        let hover = self
            .thumb_hover
            .get(i)
            .map(|s| s.value_at(now))
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        scale_rect(target, 1.0 + THUMB_HOVER_GROW * hover)
    }
}

/// A window being dragged in Mission Control.
#[derive(Debug)]
pub struct McDrag {
    pub window: WindowElement,
    pub press: Point<f64, Logical>,
    pub pos: Point<f64, Logical>,
    /// where the pointer grabbed the window, as a fraction of its size
    pub grab: (f64, f64),
    /// size of its cell when the drag started
    pub size: Size<f64, Logical>,
    /// moved far enough to count as a drag
    pub active: bool,
    /// 0 = cell size, 1 = small (over the Spaces strip)
    pub shrink: Spring,
}

impl McDrag {
    pub fn rect_at(&self, now: Duration) -> Rectangle<f64, Logical> {
        let small = (DRAG_SMALL_W / self.size.w.max(1.0)).min(1.0);
        let k = 1.0 + (small - 1.0) * self.shrink.value_at(now).clamp(0.0, 1.0);
        let (w, h) = (self.size.w * k, self.size.h * k);
        Rectangle::new(
            (self.pos.x - self.grab.0 * w, self.pos.y - self.grab.1 * h).into(),
            (w, h).into(),
        )
    }
}

#[derive(Debug)]
pub struct MissionControl {
    /// `Some(app_id)` for App Exposé
    pub app_only: Option<String>,
    /// display name for App Exposé's header
    pub app_name: Option<String>,
    pub spring: Spring,
    pub closing: bool,
    pub screens: Vec<McScreen>,
    pub hovered: Option<WindowElement>,
    /// (screen, thumbnail) under the pointer
    pub hovered_thumb: Option<(usize, usize)>,
    /// the pointer is on the hovered thumbnail's remove button
    pub hovered_remove: bool,
    /// chosen with the arrow keys
    pub selected: Option<WindowElement>,
    pub drag: Option<McDrag>,
    /// focus this window once the closing animation ends
    pub pick: Option<WindowElement>,
    /// 0..1: ring, lift and title of the highlighted window
    pub hover: Spring,
    /// vertical scroll accumulated for window navigation
    scroll_acc: f64,
    /// Space previews waiting for the renderer (screen, space)
    pending_thumbs: Vec<(usize, usize)>,
}

#[derive(Debug, Clone)]
pub struct ThumbTexture {
    pub texture: GlesTexture,
    pub size: Size<i32, Physical>,
    pub id: Id,
}

smithay::backend::renderer::element::render_elements! {
    ThumbElement<=GlesRenderer>;
    Window=RescaleRenderElement<WindowRenderElement>,
    Wall=RescaleRenderElement<TextureRenderElement<GlesTexture>>,
}

/// The remove button of a Space thumbnail: a circle on its top-left corner.
pub fn remove_button(thumb: Rectangle<f64, Logical>) -> Rectangle<f64, Logical> {
    Rectangle::new(
        (thumb.loc.x - REMOVE_R, thumb.loc.y - REMOVE_R).into(),
        (2.0 * REMOVE_R, 2.0 * REMOVE_R).into(),
    )
}

fn grow(r: Rectangle<f64, Logical>, d: f64) -> Rectangle<f64, Logical> {
    Rectangle::new((r.loc.x - d, r.loc.y - d).into(), (r.size.w + 2.0 * d, r.size.h + 2.0 * d).into())
}

fn center(r: Rectangle<f64, Logical>) -> Point<f64, Logical> {
    (r.loc.x + r.size.w / 2.0, r.loc.y + r.size.h / 2.0).into()
}

/// Scale a rect around its center.
pub(crate) fn scale_rect(r: Rectangle<f64, Logical>, k: f64) -> Rectangle<f64, Logical> {
    if (k - 1.0).abs() < 1e-9 {
        return r;
    }
    let c = center(r);
    Rectangle::new(
        (c.x - r.size.w * k / 2.0, c.y - r.size.h * k / 2.0).into(),
        (r.size.w * k, r.size.h * k).into(),
    )
}

/// "org.kde.kcalc" → "Kcalc", "firefox" → "Firefox".
fn pretty_app_name(id: &str) -> String {
    let last = id.rsplit(|c| c == '.' || c == '/').next().unwrap_or(id);
    let mut out = String::with_capacity(last.len());
    let mut upper = true;
    for c in last.chars() {
        if c == '-' || c == '_' || c == ' ' {
            out.push(' ');
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    if out.is_empty() {
        "App Exposé".to_string()
    } else {
        out
    }
}

impl MissionControl {
    pub fn progress(&self, now: Duration) -> f64 {
        self.spring.value_at(now)
    }

    pub fn is_done_closing(&self, now: Duration) -> bool {
        self.closing && self.spring.is_done_at(now)
    }

    /// Whether anything still moves: open/close spring, window cell glides,
    /// windows flying in, thumbnail hovers/slides, the highlighted window's
    /// lift, or previews still waiting for the renderer.
    pub fn is_animating(&self, now: Duration) -> bool {
        !self.spring.is_done_at(now)
            || !self.hover.is_done_at(now)
            || !self.pending_thumbs.is_empty()
            || self.screens.iter().any(|s| {
                s.thumb_move.as_ref().is_some_and(|sp| !sp.is_done_at(now))
                    || s.thumb_hover.iter().any(|sp| !sp.is_done_at(now))
                    || s.wins.iter().any(|w| {
                        w.move_spring.as_ref().is_some_and(|sp| !sp.is_done_at(now))
                            || w.enter.as_ref().is_some_and(|sp| !sp.is_done_at(now))
                    })
            })
            || self.drag.as_ref().is_some_and(|d| !d.shrink.is_done_at(now))
    }

    pub fn screen_of(&self, output: &Output) -> Option<&McScreen> {
        self.screens.iter().find(|s| s.output == *output)
    }

    fn screen_index_at(&self, pos: Point<f64, Logical>) -> Option<usize> {
        self.screens.iter().position(|s| s.geo.to_f64().contains(pos))
    }

    /// (screen index, entry index) of a window in the grid.
    fn find(&self, window: &WindowElement) -> Option<(usize, usize)> {
        self.screens
            .iter()
            .enumerate()
            .find_map(|(si, s)| s.wins.iter().position(|w| w.window == *window).map(|i| (si, i)))
    }

    pub fn window_at(&self, pos: Point<f64, Logical>, now: Duration) -> Option<WindowElement> {
        let dragged = self.drag.as_ref().filter(|d| d.active).map(|d| &d.window);
        self.screens.iter().find_map(|s| {
            s.wins
                .iter()
                .rev()
                .filter(|w| Some(&w.window) != dragged)
                .find(|w| w.grid_at(now).contains(pos))
                .map(|w| w.window.clone())
        })
    }

    /// Thumbnail under the pointer. Space thumbnails count a little larger so
    /// their remove button (which sticks out) belongs to them.
    pub fn thumb_at(&self, pos: Point<f64, Logical>, now: Duration) -> Option<(usize, usize)> {
        self.screens.iter().enumerate().find_map(|(si, s)| {
            let count = s.space_count();
            (0..s.thumbs.len())
                .find(|i| {
                    let r = s.thumb_drawn_at(*i, now);
                    if *i < count {
                        grow(r, REMOVE_R).contains(pos)
                    } else {
                        r.contains(pos)
                    }
                })
                .map(|i| (si, i))
        })
    }

    /// The window drawn with a highlight: chosen with the keyboard, else hovered.
    pub fn highlighted(&self) -> Option<&WindowElement> {
        if self.drag.as_ref().map(|d| d.active).unwrap_or(false) {
            return None;
        }
        self.selected.as_ref().or(self.hovered.as_ref())
    }

    /// Retarget the highlight spring to match the current selection.
    fn sync_hover(&mut self, now: Duration) {
        let dragging = self.drag.as_ref().map(|d| d.active).unwrap_or(false);
        let to = if !dragging && (self.selected.is_some() || self.hovered.is_some()) {
            1.0
        } else {
            0.0
        };
        if self.hover.to != to {
            self.hover.retarget(to, now);
        }
    }

    fn queue_thumb(&mut self, si: usize, idx: usize) {
        let Some(count) = self.screens.get(si).map(|s| s.space_count()) else {
            return;
        };
        if idx < count && !self.pending_thumbs.contains(&(si, idx)) {
            self.pending_thumbs.push((si, idx));
        }
    }

    fn queue_screen_thumbs(&mut self, si: usize) {
        let Some(count) = self.screens.get(si).map(|s| s.space_count()) else {
            return;
        };
        for i in 0..count {
            self.queue_thumb(si, i);
        }
    }

    fn queue_all_thumbs(&mut self) {
        for si in 0..self.screens.len() {
            self.queue_screen_thumbs(si);
        }
    }
}

/// Spaces strip of a display: one thumbnail per Space + "+", shrunk to fit.
/// `top_inset` is the menu bar's height, so thumbnails (and their remove
/// buttons) sit clear of it.
fn thumb_rects(
    geo: Rectangle<i32, Logical>,
    count: usize,
    top_inset: f64,
) -> Vec<Rectangle<f64, Logical>> {
    let aspect = geo.size.w as f64 / geo.size.h.max(1) as f64;
    let mut th = THUMB_H;
    let mut tw = th * aspect;
    let mut gap = THUMB_GAP;
    let mut add = ADD_D;
    let n = count.max(1);
    let avail = (geo.size.w as f64 - 2.0 * MARGIN - ADD_GAP).max(1.0);
    let total = n as f64 * tw + (n - 1) as f64 * gap + add;
    if total > avail {
        let k = (avail / total).clamp(0.35, 1.0);
        th *= k;
        tw *= k;
        gap *= k;
        add *= k;
    }
    let total = n as f64 * tw + (n - 1) as f64 * gap + ADD_GAP + add;
    let x0 = geo.loc.x as f64 + (geo.size.w as f64 - total) / 2.0;
    let top = geo.loc.y as f64 + top_inset.max(0.0) + THUMB_TOP;
    let mut out = Vec::with_capacity(count + 1);
    for i in 0..count {
        out.push(Rectangle::new(
            (x0 + i as f64 * (tw + gap), top).into(),
            (tw, th).into(),
        ));
    }
    let last_right = if count == 0 {
        x0
    } else {
        x0 + (count - 1) as f64 * (tw + gap) + tw
    };
    out.push(Rectangle::new(
        (last_right + ADD_GAP, top + (th - add) / 2.0).into(),
        (add, add).into(),
    ));
    out
}

/// Where the window grid goes: below the Spaces bar (or App Exposé's header),
/// clear of the Dock on whichever edge it is (`insets`: left, right, bottom
/// space that panels reserve on this display).
fn grid_area(
    geo: Rectangle<i32, Logical>,
    strip: bool,
    header: bool,
    insets: (f64, f64, f64),
    top_inset: f64,
) -> Rectangle<f64, Logical> {
    let top = if strip {
        STRIP_H
    } else if header {
        top_inset.max(0.0) + APP_HEADER_H
    } else {
        top_inset.max(0.0)
    };
    let (l, r, b) = (insets.0.max(0.0), insets.1.max(0.0), insets.2.max(0.0));
    Rectangle::new(
        (geo.loc.x as f64 + MARGIN + l, geo.loc.y as f64 + top + MARGIN).into(),
        (
            (geo.size.w as f64 - 2.0 * MARGIN - l - r).max(1.0),
            (geo.size.h as f64 - top - 2.0 * MARGIN - b).max(1.0),
        )
            .into(),
    )
}

/// Gap between grid cells, a little smaller on small displays.
fn cell_pad(area: Rectangle<f64, Logical>) -> f64 {
    (area.size.w.min(area.size.h) * 0.04).clamp(16.0, 38.0)
}

/// Grid layout that keeps windows roughly in their spatial order and
/// maximises their size. Returns target rects in the same order as `sizes`.
pub fn grid_layout(
    area: Rectangle<f64, Logical>,
    items: &[(Point<f64, Logical>, Size<f64, Logical>)],
) -> Vec<Rectangle<f64, Logical>> {
    let n = items.len();
    if n == 0 {
        return Vec::new();
    }
    let pad = cell_pad(area);
    // choose the column count that gives the largest average scale
    let mut best = (f64::MIN, 1usize);
    for cols in 1..=n {
        let rows = n.div_ceil(cols);
        let cw = area.size.w / cols as f64;
        let ch = area.size.h / rows as f64;
        if cw <= pad || ch <= pad {
            continue;
        }
        let total: f64 = items
            .iter()
            .map(|(_, s)| {
                let k = ((cw - pad) / s.w.max(1.0))
                    .min((ch - pad) / s.h.max(1.0))
                    .clamp(0.02, MAX_CELL_SCALE);
                k * k * s.w * s.h
            })
            .sum();
        if total > best.0 {
            best = (total, cols);
        }
    }
    let cols = best.1.max(1);
    let rows = n.div_ceil(cols);
    // order: by center y into rows, then by x within a row
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|a, b| {
        items[*a]
            .0
            .y
            .total_cmp(&items[*b].0.y)
            .then_with(|| items[*a].0.x.total_cmp(&items[*b].0.x))
    });
    let mut out = vec![Rectangle::default(); n];
    let cw = area.size.w / cols as f64;
    let ch = area.size.h / rows as f64;
    for (r, chunk) in order.chunks(cols).enumerate() {
        let mut row: Vec<usize> = chunk.to_vec();
        row.sort_by(|a, b| items[*a].0.x.total_cmp(&items[*b].0.x));
        // center partially filled rows
        let row_w = cw * row.len() as f64;
        let x0 = area.loc.x + (area.size.w - row_w) / 2.0;
        for (c, idx) in row.into_iter().enumerate() {
            let s = items[idx].1;
            let k = ((cw - pad) / s.w.max(1.0))
                .min((ch - pad) / s.h.max(1.0))
                .clamp(0.02, MAX_CELL_SCALE);
            let (w, h) = (s.w * k, s.h * k);
            let cx = x0 + cw * c as f64 + cw / 2.0;
            let cy = area.loc.y + ch * r as f64 + ch / 2.0;
            out[idx] = Rectangle::new((cx - w / 2.0, cy - h / 2.0).into(), (w, h).into());
        }
    }
    out
}

impl<BackendData: Backend> OrchardState<BackendData> {
    /// Windows Mission Control shows on a display: its current Space (only
    /// one app's windows for App Exposé), bottom to top, with their rects.
    fn mc_windows(&self, output: &Output, app: Option<&str>) -> Vec<(WindowElement, Rectangle<f64, Logical>)> {
        let mut out: Vec<(WindowElement, Rectangle<f64, Logical>)> = Vec::new();
        out.extend(
            self.space
                .elements()
                .filter(|w| w.alive())
                .filter(|w| app.map(|a| w.app_id() == a).unwrap_or(true))
                .filter(|w| {
                    self.window_output(w).as_ref()
                        .map(|o| o == output)
                        .unwrap_or_else(|| self.space.outputs().next() == Some(output))
                })
                .filter_map(|w| {
                    let loc = self.space.element_location(w)?;
                    Some((w.clone(), Rectangle::new(loc, w.geometry().size).to_f64()))
                }),
        );
        out
    }

    pub fn toggle_mission_control(&mut self, app_only: bool) {
        if let Some(mc) = self.mc.as_ref() {
            if !mc.closing {
                self.close_mission_control(None);
                return;
            }
        }
        let app = if app_only {
            match self.focused_window() {
                Some(w) => Some(w.app_id()),
                None => return,
            }
        } else {
            None
        };
        let app_name = app.as_deref().map(pretty_app_name);
        let now: Duration = self.clock.now().into();

        let outputs: Vec<Output> = self.space.outputs().cloned().collect();
        let mut screens = Vec::new();
        for output in outputs {
            let Some(geo) = self.space.output_geometry(&output) else { continue };
            // keep clear of what panels reserve (the Dock, on any edge)
            let zone = smithay::desktop::layer_map_for_output(&output).non_exclusive_zone();
            let insets = (
                zone.loc.x as f64,
                (geo.size.w - zone.loc.x - zone.size.w) as f64,
                (geo.size.h - zone.loc.y - zone.size.h) as f64,
            );
            let is_app = app.is_some();
            // assume at least a slim menu bar, even if the shell didn't reserve one
            let top_inset = (zone.loc.y as f64).max(24.0);
            let area = grid_area(geo, !is_app, is_app, insets, top_inset);
            let windows = self.mc_windows(&output, app.as_deref());
            let items: Vec<_> = windows.iter().map(|(_, r)| (center(*r), r.size)).collect();
            let wins = windows
                .into_iter()
                .zip(grid_layout(area, &items))
                .enumerate()
                .map(|(i, ((w, real), target))| {
                    let mut win = McWin::new(w, real, target);
                    // stagger the entrance a little, like macOS
                    let delay = Duration::from_millis((i.min(8) as u64) * 9);
                    win.enter = Some(Spring::new(0.0, 1.0, SpringParams::OVERVIEW, now + delay));
                    win
                })
                .collect();
            let (thumbs, textures, hover) = if is_app {
                (Vec::new(), Vec::new(), Vec::new())
            } else {
                let count = self.spaces.info(&output.name()).1;
                let thumbs = thumb_rects(geo, count, top_inset);
                let textures = vec![None; count];
                let hover = (0..thumbs.len())
                    .map(|_| Spring::new(0.0, 0.0, SpringParams::HOVER, now))
                    .collect();
                (thumbs, textures, hover)
            };
            screens.push(McScreen {
                output,
                geo,
                area,
                top_inset,
                wins,
                thumbs,
                thumb_textures: textures,
                thumb_from: Vec::new(),
                thumb_move: None,
                thumb_hover: hover,
            });
        }
        if screens.is_empty() {
            return;
        }

        let mut mc = MissionControl {
            app_only: app,
            app_name,
            spring: Spring::new(0.0, 1.0, SpringParams::OVERVIEW, now),
            closing: false,
            screens,
            hovered: None,
            hovered_thumb: None,
            hovered_remove: false,
            selected: None,
            drag: None,
            pick: None,
            hover: Spring::new(0.0, 0.0, SpringParams::HOVER, now),
            scroll_acc: 0.0,
            pending_thumbs: Vec::new(),
        };
        for s in &mut mc.screens {
            s.thumb_from = s.thumbs.clone();
        }
        mc.queue_all_thumbs();
        self.mc = Some(mc);
        self.mc_log_layout();
        // pointer focus must not stay on a client while MC is open
        let pos = self.pointer.current_location();
        self.inject_pointer_motion(pos, 0);
    }

    pub fn close_mission_control(&mut self, pick: Option<WindowElement>) {
        let now: Duration = self.clock.now().into();
        if let Some(mc) = self.mc.as_mut() {
            mc.drag = None;
            mc.pending_thumbs.clear();
            if let Some(w) = &pick {
                // the picked window ends up on top (if currently mapped in space)
                if self.space.element_location(w).is_some() {
                    self.space.raise_element(w, true);
                }
                // and is drawn last (on top) during the animation
                if let Some((si, i)) = mc.find(w) {
                    let item = mc.screens[si].wins.remove(i);
                    mc.screens[si].wins.push(item);
                }
            }
            mc.pick = pick;
            mc.closing = true;
            // the close is a touch quicker and never overshoots
            mc.spring.params = SpringParams::SMOOTH;
            mc.spring.retarget(0.0, now);
        }
    }

    /// Called every frame; removes Mission Control once it has closed and
    /// renders a few pending Space previews while it is open.
    pub fn tick_mission_control(&mut self) {
        let now: Duration = self.clock.now().into();
        let done = self.mc.as_ref().map(|m| m.is_done_closing(now)).unwrap_or(false);
        if done {
            let mc = self.mc.take().unwrap();
            if let Some(w) = mc.pick {
                self.focus_window(&w);
            }
            let pos = self.pointer.current_location();
            self.inject_pointer_motion(pos, 0);
            return;
        }
        self.render_pending_thumbs(THUMB_BUDGET);
    }

    /// Render up to `budget` Space previews. Runs from `tick_effects`, before
    /// the frame is drawn, so opening Mission Control never blocks on them.
    fn render_pending_thumbs(&mut self, budget: usize) {
        for _ in 0..budget {
            let Some((si, idx)) = self.mc.as_mut().and_then(|m| {
                if m.closing {
                    None
                } else if m.pending_thumbs.is_empty() {
                    None
                } else {
                    Some(m.pending_thumbs.remove(0))
                }
            }) else {
                return;
            };
            self.render_thumb(si, idx);
        }
    }

    /// Render one Space's miniature (wallpaper + its windows) into a texture.
    fn render_thumb(&mut self, si: usize, idx: usize) {
        let (output, geo, thumb) = {
            let Some(mc) = self.mc.as_ref() else { return };
            let Some(screen) = mc.screens.get(si) else { return };
            let Some(thumb) = screen.thumbs.get(idx).copied() else { return };
            (screen.output.clone(), screen.geo, thumb)
        };
        let scale = output.current_scale().fractional_scale();
        let Some(mode) = output.current_mode() else { return };
        let full = output.current_transform().transform_size(mode.size);
        let name = output.name();
        let (active, count) = self.spaces.info(&name);
        if idx >= count {
            return;
        }
        // windows of that Space, bottom to top
        let windows: Vec<(WindowElement, Point<i32, Logical>)> = if idx == active {
            self.windows_on_output(&output)
        } else {
            let mut v = Vec::new();
            if let Some(slot) = self.spaces.outputs.get(&name).and_then(|o| o.slots.get(idx)) {
                v.extend(slot.windows.clone());
                if let Some(fs) = slot.fullscreen.clone() {
                    v.push((fs, geo.loc));
                }
            }
            v
        };

        let fx = &mut self.fx;
        let rendered = self
            .backend_data
            .with_renderer(|renderer| {
                render_thumbnail_texture(renderer, fx, full, geo, thumb, scale, &windows)
            })
            .flatten();
        let Some(tex) = rendered else { return };
        if let Some(screen) = self.mc.as_mut().and_then(|m| m.screens.get_mut(si)) {
            if screen.output == output && idx < screen.thumb_textures.len() {
                screen.thumb_textures[idx] = Some(tex);
            }
        }
    }

    /// Lay out one display's grid again after windows came or went. Windows
    /// that stay glide to their new cell, new ones fly in from where they are
    /// (or from `dropped`, where a dragged window was let go).
    fn mc_relayout(&mut self, si: usize, dropped: Option<(&WindowElement, Rectangle<f64, Logical>)>) {
        let now: Duration = self.clock.now().into();
        // windows just mapped or moved: update which output they are on
        self.space.refresh();
        let Some(mc) = self.mc.as_ref() else { return };
        let Some(screen) = mc.screens.get(si) else { return };
        let output = screen.output.clone();
        let area = screen.area;
        let app = mc.app_only.clone();
        let windows = self.mc_windows(&output, app.as_deref());
        let items: Vec<_> = windows.iter().map(|(_, r)| (center(*r), r.size)).collect();
        let targets = grid_layout(area, &items);
        let Some(mc) = self.mc.as_mut() else { return };
        let screen = &mut mc.screens[si];
        let mut wins = Vec::with_capacity(windows.len());
        for ((w, real), target) in windows.into_iter().zip(targets) {
            let mut win = McWin::new(w.clone(), real, target);
            if let Some(old) = screen.wins.iter().find(|x| x.window == w) {
                win.move_from = old.grid_at(now);
                win.enter = old.enter;
                win.move_spring = Some(Spring::new(0.0, 1.0, SpringParams::SMOOTH, now));
            } else if let Some((_, r)) = dropped.filter(|(d, _)| **d == w) {
                win.move_from = r;
                win.move_spring = Some(Spring::new(0.0, 1.0, SpringParams::SMOOTH, now));
            } else {
                win.enter = Some(Spring::new(0.0, 1.0, SpringParams::OVERVIEW, now));
            }
            wins.push(win);
        }
        screen.wins = wins;
        if let Some(sel) = mc.selected.clone() {
            if mc.find(&sel).is_none() {
                mc.selected = None;
            }
        }
        mc.sync_hover(now);
        // the active Space's miniature changed with the grid
        let active = self.spaces.info(&output.name()).0;
        mc.queue_thumb(si, active);
        self.mc_log_layout();
    }

    /// Debug log of the grid and strips (used by the headless tests).
    fn mc_log_layout(&self) {
        let Some(mc) = self.mc.as_ref() else { return };
        for s in &mc.screens {
            for w in &s.wins {
                let t = w.target;
                tracing::debug!(output = %s.output.name(), title = %w.window.title(), x = t.loc.x, y = t.loc.y, w = t.size.w, h = t.size.h, "mc cell");
            }
            for (i, t) in s.thumbs.iter().enumerate() {
                tracing::debug!(output = %s.output.name(), i, x = t.loc.x, y = t.loc.y, w = t.size.w, h = t.size.h, "mc thumb");
            }
        }
    }

    /// The Spaces strip of one display changed (Space added or removed).
    /// `removed` is the index of a Space that no longer exists.
    fn mc_rebuild_strip(&mut self, si: usize, removed: Option<usize>) {
        let now: Duration = self.clock.now().into();
        let Some(mc) = self.mc.as_ref() else { return };
        if mc.app_only.is_some() {
            return;
        }
        let Some(screen) = mc.screens.get(si) else { return };
        let name = screen.output.name();
        let geo = screen.geo;
        let old_count = screen.space_count();
        let old_displayed: Vec<Rectangle<f64, Logical>> = (0..screen.thumbs.len())
            .map(|i| screen.thumb_rect_at(i, now))
            .collect();
        let old_textures = screen.thumb_textures.clone();
        let mut thumb_hover = screen.thumb_hover.clone();

        let count = self.spaces.info(&name).1;
        let thumbs = thumb_rects(geo, count, screen.top_inset);
        let mut from = Vec::with_capacity(thumbs.len());
        for i in 0..thumbs.len() {
            if let Some(old) = old_displayed.get(i).copied() {
                from.push(old);
            } else if let Some(plus) = old_displayed.last().copied() {
                // a new thumbnail grows out of the "+" button
                let c = center(plus);
                let t = thumbs[i];
                from.push(Rectangle::new(
                    (c.x - t.size.w * 0.18, c.y - t.size.h * 0.18).into(),
                    (t.size.w * 0.36, t.size.h * 0.36).into(),
                ));
            } else {
                from.push(thumbs[i]);
            }
        }
        thumb_hover.resize_with(thumbs.len(), || Spring::new(0.0, 0.0, SpringParams::HOVER, now));
        thumb_hover.truncate(thumbs.len());

        let mut textures = old_textures;
        if let Some(idx) = removed {
            if idx < textures.len() {
                textures.remove(idx);
            }
        }
        while textures.len() < count {
            textures.push(None);
        }
        textures.truncate(count);

        if let Some(mc) = self.mc.as_mut() {
            let screen = &mut mc.screens[si];
            screen.thumbs = thumbs;
            screen.thumb_from = from;
            screen.thumb_move = Some(Spring::new(0.0, 1.0, SpringParams::SMOOTH, now));
            screen.thumb_textures = textures;
            screen.thumb_hover = thumb_hover;
            mc.hovered_thumb = None;
            mc.hovered_remove = false;
        }
        // a new Space has no miniature yet; a removed Space's windows moved
        // into the one on its left, whose miniature must be redrawn
        if let Some(mc) = self.mc.as_mut() {
            if removed.is_none() && count > old_count {
                mc.queue_thumb(si, count - 1);
            } else if removed.is_some() && count > 0 {
                let target = removed.unwrap().saturating_sub(1).min(count - 1);
                mc.queue_thumb(si, target);
            }
        }
        self.mc_log_layout();
        let pos = self.pointer.current_location();
        self.mc_motion(pos);
    }

    /// ⌃⌘←/→/1…9 while Mission Control is open: switch the Space of that
    /// display and show its windows, without leaving Mission Control.
    pub fn mc_switch_space(&mut self, output: &Output, idx: usize) {
        let Some(si) = self.mc.as_ref().and_then(|m| m.screens.iter().position(|s| s.output == *output)) else {
            return;
        };
        let (active, count) = self.spaces.info(&output.name());
        if idx >= count || idx == active {
            return;
        }
        if self.mc.as_ref().map(|m| m.closing).unwrap_or(true) {
            return;
        }
        self.switch_space(output, idx);
        // the grid replaces the slide
        self.fx.space_transition = None;
        if let Some(mc) = self.mc.as_mut() {
            mc.screens[si].wins.clear();
        }
        self.mc_relayout(si, None);
        if let Some(mc) = self.mc.as_mut() {
            mc.queue_thumb(si, idx);
        }
    }

    /// Pointer moved while Mission Control is open.
    pub fn mc_motion(&mut self, pos: Point<f64, Logical>) {
        let now: Duration = self.clock.now().into();
        let Some(mc) = self.mc.as_mut() else { return };
        let moved = mc.drag.as_ref().map(|d| d.pos != pos).unwrap_or(true);
        if let Some(drag) = mc.drag.as_mut() {
            drag.pos = pos;
            if !drag.active {
                let d = (pos.x - drag.press.x).hypot(pos.y - drag.press.y);
                drag.active = d > DRAG_THRESHOLD;
            }
        }
        // over a Spaces strip: the dragged window shrinks to thumbnail size
        let over_strip = mc
            .screens
            .iter()
            .any(|s| !s.thumbs.is_empty() && s.geo.to_f64().contains(pos) && pos.y < s.geo.loc.y as f64 + STRIP_H);
        if let Some(drag) = mc.drag.as_mut() {
            let to = if over_strip && drag.active { 1.0 } else { 0.0 };
            if drag.shrink.to != to {
                drag.shrink.retarget(to, now);
            }
        }
        let hovered = mc.window_at(pos, now);
        if moved && hovered.is_some() && hovered != mc.hovered {
            // the mouse takes over from the keyboard
            mc.selected = None;
        }
        mc.hovered = hovered;
        mc.hovered_thumb = mc.thumb_at(pos, now);
        mc.hovered_remove = mc
            .hovered_thumb
            .map(|(si, i)| {
                let s = &mc.screens[si];
                let dragging = mc.drag.as_ref().map(|d| d.active).unwrap_or(false);
                !dragging
                    && i < s.space_count()
                    && s.space_count() > 1
                    && remove_button(s.thumb_drawn_at(i, now)).contains(pos)
            })
            .unwrap_or(false);
        mc.sync_hover(now);
        // grow the thumbnail (or "+") under the pointer
        let hovered_thumb = mc.hovered_thumb;
        for (si, s) in mc.screens.iter_mut().enumerate() {
            for (i, spring) in s.thumb_hover.iter_mut().enumerate() {
                let to = if hovered_thumb == Some((si, i)) { 1.0 } else { 0.0 };
                if spring.to != to {
                    spring.retarget(to, now);
                }
            }
        }
    }

    /// Left button pressed while Mission Control is open.
    pub fn mc_press(&mut self, pos: Point<f64, Logical>) {
        let now: Duration = self.clock.now().into();
        self.mc_motion(pos);
        let Some(mc) = self.mc.as_ref() else { return };
        if mc.closing {
            return;
        }
        if let Some((si, i)) = mc.thumb_at(pos, now) {
            let screen = &mc.screens[si];
            let output = screen.output.clone();
            let count = screen.space_count();
            let (active, _) = self.spaces.info(&output.name());
            if mc.hovered_remove {
                self.mc_remove_space(si, i);
            } else if i == count {
                // "+" adds a Space and stays in Mission Control
                self.add_space(&output);
                self.mc_rebuild_strip(si, None);
            } else if i != active {
                // slide to the Space right away, while Mission Control closes:
                // the old Space's windows fly back from the grid and slide out
                self.switch_space(&output, i);
                self.close_mission_control(None);
            } else {
                self.close_mission_control(None);
            }
            return;
        }
        if let Some(window) = mc.window_at(pos, now) {
            let Some((si, i)) = mc.find(&window) else { return };
            let cell = mc.screens[si].wins[i].grid_at(now);
            let grab = (
                ((pos.x - cell.loc.x) / cell.size.w.max(1.0)).clamp(0.0, 1.0),
                ((pos.y - cell.loc.y) / cell.size.h.max(1.0)).clamp(0.0, 1.0),
            );
            if let Some(mc) = self.mc.as_mut() {
                mc.drag = Some(McDrag {
                    window,
                    press: pos,
                    pos,
                    grab,
                    size: cell.size,
                    active: false,
                    shrink: Spring::new(0.0, 0.0, SpringParams::PREVIEW, now),
                });
            }
            return;
        }
        // empty space: back to the desktop
        self.close_mission_control(None);
    }

    /// Left button released while Mission Control is open.
    pub fn mc_release(&mut self, pos: Point<f64, Logical>) {
        let Some(drag) = self.mc.as_mut().and_then(|m| m.drag.take()) else { return };
        if !drag.active {
            // a click: open the window
            self.close_mission_control(Some(drag.window));
            return;
        }
        drag_drop(self, drag, pos);
        let pos = self.pointer.current_location();
        self.mc_motion(pos);
    }

    /// The remove button of a Space was clicked: its windows move to the
    /// Space on its left (the right one for the first Space).
    fn mc_remove_space(&mut self, si: usize, idx: usize) {
        let Some(output) = self.mc.as_ref().and_then(|m| m.screens.get(si)).map(|s| s.output.clone()) else {
            return;
        };
        self.remove_space(&output, idx);
        self.fx.space_transition = None;
        self.mc_relayout(si, None);
        self.mc_rebuild_strip(si, Some(idx));
    }

    /// Arrow keys in Mission Control: select the nearest window in that direction.
    pub fn mc_nav(&mut self, dx: i32, dy: i32) {
        let now: Duration = self.clock.now().into();
        let pointer_output = self.active_output();
        let Some(mc) = self.mc.as_mut() else { return };
        if mc.closing {
            return;
        }
        let all: Vec<(WindowElement, Point<f64, Logical>)> = mc
            .screens
            .iter()
            .flat_map(|s| s.wins.iter().map(|w| (w.window.clone(), center(w.grid_at(now)))))
            .collect();
        let current = mc.selected.clone().or_else(|| mc.hovered.clone());
        let Some(from) = current.as_ref().and_then(|c| all.iter().find(|(w, _)| w == c)).map(|(_, p)| *p) else {
            // nothing chosen yet: start with the frontmost window of the display under the pointer
            let screen = mc
                .screens
                .iter()
                .find(|s| Some(&s.output) == pointer_output.as_ref() && !s.wins.is_empty())
                .or_else(|| mc.screens.iter().find(|s| !s.wins.is_empty()));
            mc.selected = screen.and_then(|s| s.wins.last()).map(|w| w.window.clone());
            mc.sync_hover(now);
            return;
        };
        let (dx, dy) = (dx as f64, dy as f64);
        let best = all
            .iter()
            .filter(|(w, _)| Some(w) != current.as_ref())
            .filter_map(|(w, p)| {
                let (vx, vy) = (p.x - from.x, p.y - from.y);
                let along = vx * dx + vy * dy;
                let across = (vx * dy - vy * dx).abs();
                (along > 1.0).then_some((w, along + 2.0 * across))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(w, _)| w.clone());
        if let Some(w) = best {
            mc.selected = Some(w);
            mc.sync_hover(now);
        }
    }

    /// Tab / ⇧Tab: step through the windows in reading order, wrapping around.
    pub fn mc_cycle(&mut self, step: i32) {
        let now: Duration = self.clock.now().into();
        let Some(mc) = self.mc.as_mut() else { return };
        if mc.closing {
            return;
        }
        let mut order: Vec<WindowElement> = Vec::new();
        for s in &mc.screens {
            let mut wins: Vec<&McWin> = s.wins.iter().collect();
            wins.sort_by(|a, b| {
                let pa = center(a.grid_at(now));
                let pb = center(b.grid_at(now));
                pa.y.total_cmp(&pb.y).then_with(|| pa.x.total_cmp(&pb.x))
            });
            order.extend(wins.into_iter().map(|w| w.window.clone()));
        }
        if order.is_empty() {
            return;
        }
        let current = mc.selected.clone().or_else(|| mc.hovered.clone());
        let idx = current.as_ref().and_then(|c| order.iter().position(|w| w == c));
        let next = match idx {
            Some(i) => (i as i32 + step).rem_euclid(order.len() as i32) as usize,
            None if step >= 0 => 0,
            None => order.len() - 1,
        };
        mc.selected = Some(order[next].clone());
        mc.hovered = None;
        mc.sync_hover(now);
    }

    /// Scroll wheel / two-finger scroll: step through windows.
    pub fn mc_scroll(&mut self, delta: f64) {
        if delta == 0.0 {
            return;
        }
        let Some(mc) = self.mc.as_mut() else { return };
        if mc.closing {
            return;
        }
        mc.scroll_acc += delta;
        let mut steps = 0i32;
        while mc.scroll_acc >= SCROLL_STEP {
            mc.scroll_acc -= SCROLL_STEP;
            steps += 1;
        }
        while mc.scroll_acc <= -SCROLL_STEP {
            mc.scroll_acc += SCROLL_STEP;
            steps -= 1;
        }
        // a fast flick should not jump across half the grid
        steps = steps.clamp(-2, 2);
        mc.scroll_acc = mc.scroll_acc.clamp(-SCROLL_STEP, SCROLL_STEP);
        for _ in 0..steps.abs() {
            self.mc_cycle(steps.signum());
        }
    }

    /// Return / Space in Mission Control: open the chosen window.
    pub fn mc_pick(&mut self) {
        let Some(mc) = self.mc.as_ref() else { return };
        let pick = mc.selected.clone().or_else(|| mc.hovered.clone());
        if pick.is_some() {
            self.close_mission_control(pick);
        }
    }
}

/// A dragged window was let go.
fn drag_drop<B: Backend>(st: &mut OrchardState<B>, drag: McDrag, pos: Point<f64, Logical>) {
    let now: Duration = st.clock.now().into();
    let drop_rect = drag.rect_at(now);
    let window = drag.window;
    let Some(mc) = st.mc.as_ref() else { return };
    let Some((src_si, _)) = mc.find(&window) else { return };
    let target = mc.screen_index_at(pos).map(|si| {
        let thumb = mc.thumb_at(pos, now).filter(|(s, _)| *s == si).map(|(_, i)| i);
        (si, mc.screens[si].output.clone(), mc.screens[si].space_count(), thumb)
    });

    let snap_back = |st: &mut OrchardState<B>| {
        if let Some(mc) = st.mc.as_mut() {
            if let Some((si, i)) = mc.find(&window) {
                let w = &mut mc.screens[si].wins[i];
                w.move_from = drop_rect;
                w.move_spring = Some(Spring::new(0.0, 1.0, SpringParams::SMOOTH, now));
            }
        }
    };
    let Some((dst_si, dst_output, count, thumb)) = target else {
        snap_back(st);
        return;
    };
    let (dst_active, _) = st.spaces.info(&dst_output.name());
    let (idx, added) = match thumb {
        Some(i) if i == count => (st.add_space(&dst_output), true),
        Some(i) => (i, false),
        None if dst_si != src_si => (dst_active, false),
        None => {
            snap_back(st);
            return;
        }
    };
    if !st.move_window_to_space(&window, &dst_output, idx) {
        snap_back(st);
        return;
    }
    tracing::info!(window = %window.title(), output = %dst_output.name(), space = idx + 1, "moved window in Mission Control");
    if st.focused_window().is_none() || st.focused_window().as_ref() == Some(&window) {
        st.focus_topmost();
    }
    st.mc_relayout(src_si, None);
    if dst_si != src_si && idx == dst_active {
        st.mc_relayout(dst_si, Some((&window, drop_rect)));
    }
    if added {
        st.mc_rebuild_strip(dst_si, None);
    }
    // Space previews of both displays changed; keep the old ones on screen
    // until their replacements are ready
    if let Some(mc) = st.mc.as_mut() {
        mc.queue_screen_thumbs(src_si);
        mc.queue_screen_thumbs(dst_si);
    }
}

/// Render a miniature of one Space: the wallpaper with that Space's windows
/// where they are, at the size of its strip thumbnail.
fn render_thumbnail_texture(
    renderer: &mut GlesRenderer,
    fx: &mut crate::effects::Effects,
    full: Size<i32, Physical>,
    geo: Rectangle<i32, Logical>,
    thumb: Rectangle<f64, Logical>,
    scale: f64,
    windows: &[(WindowElement, Point<i32, Logical>)],
) -> Option<ThumbTexture> {
    use smithay::backend::renderer::{Frame, Renderer};
    let size: Size<i32, Physical> = thumb.size.to_physical_precise_ceil(scale);
    if size.w <= 0 || size.h <= 0 {
        return None;
    }
    let k = size.w as f64 / full.w.max(1) as f64;
    let origin = Point::<i32, Physical>::from((0, 0));
    let mut elements: Vec<ThumbElement> = Vec::new();
    for (w, loc) in windows.iter().rev().filter(|(w, _)| w.alive()) {
        let render_loc = (*loc - w.geometry().loc - geo.loc).to_physical_precise_round(scale);
        let elems: Vec<WindowRenderElement> = AsRenderElements::<GlesRenderer>::render_elements(
            w,
            renderer,
            render_loc,
            Scale::from(scale),
            1.0,
        );
        elements.extend(
            elems
                .into_iter()
                .map(|e| ThumbElement::Window(RescaleRenderElement::from_element(e, origin, k))),
        );
    }
    if let Some(wall) = fx.wallpaper.textures(renderer, full).map(|w| w.element(scale)) {
        elements.push(ThumbElement::Wall(RescaleRenderElement::from_element(wall, origin, k)));
    }
    let mut texture: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, (size.w, size.h).into()).ok()?;
    {
        let mut target = renderer.bind(&mut texture).ok()?;
        // Clear freshly allocated VRAM (prevents NVIDIA speckles).
        if let Ok(mut frame) = renderer.render(&mut target, size, Transform::Normal) {
            let _ = frame.clear(Color32F::new(0.1, 0.1, 0.1, 1.0), &[Rectangle::from_size(size)]);
            let _ = frame.finish();
        }
        let mut tracker = OutputDamageTracker::new(size, scale, Transform::Normal);
        tracker
            .render_output(renderer, &mut target, 0, &elements, Color32F::new(0.1, 0.1, 0.1, 1.0))
            .ok()?;
    }
    Some(ThumbTexture {
        texture,
        size,
        id: Id::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_never_overlaps_and_fits() {
        let area = Rectangle::<f64, Logical>::new((0.0, 0.0).into(), (1400.0, 800.0).into());
        let items: Vec<_> = (0..7)
            .map(|i| {
                (
                    Point::from((i as f64 * 100.0, (i % 3) as f64 * 200.0)),
                    Size::from((900.0, 600.0)),
                )
            })
            .collect();
        let rects = grid_layout(area, &items);
        for (i, a) in rects.iter().enumerate() {
            assert!(a.loc.x >= 0.0 && a.loc.y >= 0.0);
            assert!(a.loc.x + a.size.w <= 1400.0 + 1e-6 && a.loc.y + a.size.h <= 800.0 + 1e-6);
            for b in rects.iter().skip(i + 1) {
                assert!(!a.overlaps(*b), "{a:?} overlaps {b:?}");
            }
            // aspect ratio preserved
            assert!((a.size.w / a.size.h - 1.5).abs() < 1e-6);
        }
    }

    #[test]
    fn single_window_fills_the_grid_up_to_the_cap() {
        let area = Rectangle::<f64, Logical>::new((0.0, 0.0).into(), (1400.0, 800.0).into());
        let rects = grid_layout(area, &[(Point::from((10.0, 10.0)), Size::from((400.0, 300.0)))]);
        let r = rects[0];
        assert!(r.size.w > 400.0);
        assert!(r.size.w <= 400.0 * MAX_CELL_SCALE + 1e-6);
        assert!((r.size.w / r.size.h - 4.0 / 3.0).abs() < 1e-6);
        assert!((r.loc.x + r.size.w / 2.0 - 700.0).abs() < 1e-6);
        assert!((r.loc.y + r.size.h / 2.0 - 400.0).abs() < 1e-6);
    }

    #[test]
    fn strip_fits_and_ends_with_plus() {
        let geo = Rectangle::<i32, Logical>::new((0, 0).into(), (1440, 900).into());
        for count in 1..=9 {
            let thumbs = thumb_rects(geo, count, 30.0);
            assert_eq!(thumbs.len(), count + 1);
            let first = thumbs[0];
            let last = thumbs[count];
            assert!(first.loc.x >= MARGIN as f64 - 1e-6);
            assert!(last.loc.x + last.size.w <= 1440.0 - MARGIN as f64 + 1e-6);
            for pair in thumbs.windows(2) {
                assert!(pair[0].loc.x + pair[0].size.w < pair[1].loc.x);
            }
        }
    }
}
