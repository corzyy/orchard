//! Mission Control and App Exposé.
//!
//! Opens on every display at once. The windows of each display's current
//! Space spring into a non-overlapping grid; a strip at the top shows that
//! display's Spaces plus "+". Windows can be dragged onto a Space, onto "+"
//! (a new Space) or onto another display. Hovering a Space shows a remove
//! button. The arrow keys select a window, Return opens it, and ⌃⌘←/→/1…9
//! switch Spaces without leaving Mission Control.

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

pub const STRIP_H: f64 = 132.0;
const THUMB_GAP: f64 = 24.0;
const MARGIN: f64 = 48.0;
const CELL_PAD: f64 = 24.0;
/// pointer travel before a press on a window becomes a drag
const DRAG_THRESHOLD: f64 = 6.0;
/// radius of a Space's remove button (centered on the thumbnail's corner)
pub const REMOVE_R: f64 = 10.0;
/// a dragged window shrinks to this width over the Spaces strip
const DRAG_SMALL_W: f64 = 180.0;

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
    /// is already open (Space switched, Space removed)
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
    /// bottom to top
    pub wins: Vec<McWin>,
    /// thumbnails of the Spaces strip (global logical), last one is "+"
    pub thumbs: Vec<Rectangle<f64, Logical>>,
    /// miniature of each Space (wallpaper + its windows)
    pub thumb_textures: Vec<Option<ThumbTexture>>,
}

impl McScreen {
    /// Number of Spaces shown in the strip.
    pub fn space_count(&self) -> usize {
        self.thumbs.len().saturating_sub(1)
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

impl MissionControl {
    pub fn progress(&self, now: Duration) -> f64 {
        self.spring.value_at(now)
    }

    pub fn is_done_closing(&self, now: Duration) -> bool {
        self.closing && self.spring.is_done_at(now)
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
    pub fn thumb_at(&self, pos: Point<f64, Logical>) -> Option<(usize, usize)> {
        self.screens.iter().enumerate().find_map(|(si, s)| {
            let count = s.space_count();
            s.thumbs
                .iter()
                .enumerate()
                .find(|(i, r)| if *i < count { grow(**r, REMOVE_R).contains(pos) } else { r.contains(pos) })
                .map(|(i, _)| (si, i))
        })
    }

    /// The window drawn with a highlight: chosen with the keyboard, else hovered.
    pub fn highlighted(&self) -> Option<&WindowElement> {
        if self.drag.as_ref().map(|d| d.active).unwrap_or(false) {
            return None;
        }
        self.selected.as_ref().or(self.hovered.as_ref())
    }
}

/// Spaces strip of a display: one thumbnail per Space + "+", shrunk to fit.
fn thumb_rects(geo: Rectangle<i32, Logical>, count: usize) -> Vec<Rectangle<f64, Logical>> {
    let mut th = STRIP_H - 36.0;
    let mut tw = th * geo.size.w as f64 / geo.size.h.max(1) as f64;
    let mut gap = THUMB_GAP;
    let n = count + 1;
    let total = n as f64 * tw + (n - 1) as f64 * gap;
    let avail = geo.size.w as f64 - 2.0 * MARGIN;
    if total > avail {
        let k = avail / total;
        th *= k;
        tw *= k;
        gap *= k;
    }
    let total = n as f64 * tw + (n - 1) as f64 * gap;
    let x0 = geo.loc.x as f64 + (geo.size.w as f64 - total) / 2.0;
    // clear of the menu bar, with room for the shelf around the thumbnails
    let top = geo.loc.y as f64 + 36.0;
    (0..n)
        .map(|i| {
            let (w, h) = if i == count {
                let s = (th * 0.6).min(tw);
                (s, s)
            } else {
                (tw, th)
            };
            let x = x0 + i as f64 * (tw + gap) + (tw - w) / 2.0;
            let y = top + (STRIP_H - 36.0 - h) / 2.0;
            Rectangle::new((x, y).into(), (w, h).into())
        })
        .collect()
}

/// Where the window grid goes: below the Spaces strip, clear of the Dock on
/// whichever edge it is (`insets`: left, right, bottom space that panels
/// reserve on this display).
fn grid_area(geo: Rectangle<i32, Logical>, strip: bool, insets: (f64, f64, f64)) -> Rectangle<f64, Logical> {
    let strip = if strip { STRIP_H } else { 0.0 };
    let (l, r, b) = (insets.0.max(0.0), insets.1.max(0.0), insets.2.max(0.0));
    Rectangle::new(
        (geo.loc.x as f64 + MARGIN + l, geo.loc.y as f64 + strip + MARGIN).into(),
        (
            (geo.size.w as f64 - 2.0 * MARGIN - l - r).max(1.0),
            (geo.size.h as f64 - strip - 2.0 * MARGIN - b).max(1.0),
        )
            .into(),
    )
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
    // choose the column count that gives the largest average scale
    let mut best = (0.0, 1usize);
    for cols in 1..=n {
        let rows = n.div_ceil(cols);
        let cw = area.size.w / cols as f64;
        let ch = area.size.h / rows as f64;
        let total: f64 = items
            .iter()
            .map(|(_, s)| {
                let k = ((cw - CELL_PAD) / s.w).min((ch - CELL_PAD) / s.h).min(1.0);
                k * k * s.w * s.h
            })
            .sum();
        if total > best.0 {
            best = (total, cols);
        }
    }
    let cols = best.1;
    let rows = n.div_ceil(cols);
    // order: by center y into rows, then by x within a row
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|a, b| items[*a].0.y.total_cmp(&items[*b].0.y));
    let mut out = vec![Rectangle::default(); n];
    let ch = area.size.h / rows as f64;
    for (r, chunk) in order.chunks(cols).enumerate() {
        let mut row: Vec<usize> = chunk.to_vec();
        row.sort_by(|a, b| items[*a].0.x.total_cmp(&items[*b].0.x));
        // center partially filled rows
        let cw = area.size.w / cols as f64;
        let row_w = cw * row.len() as f64;
        let x0 = area.loc.x + (area.size.w - row_w) / 2.0;
        for (c, idx) in row.into_iter().enumerate() {
            let s = items[idx].1;
            let k = ((cw - CELL_PAD) / s.w).min((ch - CELL_PAD) / s.h).min(1.0);
            let w = s.w * k;
            let h = s.h * k;
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
            })
            .collect()
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
            let area = grid_area(geo, !app_only, insets);
            let windows = self.mc_windows(&output, app.as_deref());
            let items: Vec<_> = windows.iter().map(|(_, r)| (center(*r), r.size)).collect();
            let wins = windows
                .into_iter()
                .zip(grid_layout(area, &items))
                .map(|((w, real), t)| McWin::new(w, real, t))
                .collect();
            let thumbs = if app_only {
                Vec::new()
            } else {
                thumb_rects(geo, self.spaces.info(&output.name()).1)
            };
            screens.push(McScreen {
                output,
                geo,
                area,
                wins,
                thumbs,
                thumb_textures: Vec::new(),
            });
        }
        if screens.is_empty() {
            return;
        }

        let now = self.clock.now().into();
        self.mc = Some(MissionControl {
            app_only: app,
            spring: Spring::new(0.0, 1.0, SpringParams::SMOOTH, now),
            closing: false,
            screens,
            hovered: None,
            hovered_thumb: None,
            hovered_remove: false,
            selected: None,
            drag: None,
            pick: None,
        });
        self.render_space_thumbs();
        self.mc_log_layout();
        // pointer focus must not stay on a client while MC is open
        let pos = self.pointer.current_location();
        self.inject_pointer_motion(pos, 0);
    }

    pub fn close_mission_control(&mut self, pick: Option<WindowElement>) {
        let now = self.clock.now().into();
        if let Some(mc) = self.mc.as_mut() {
            mc.drag = None;
            if let Some(w) = &pick {
                // the picked window ends up on top
                self.space.raise_element(w, true);
                // and is drawn last (on top) during the animation
                if let Some((si, i)) = mc.find(w) {
                    let item = mc.screens[si].wins.remove(i);
                    mc.screens[si].wins.push(item);
                }
            }
            mc.pick = pick;
            mc.closing = true;
            mc.spring.retarget(0.0, now);
        }
    }

    /// Called every frame; removes Mission Control once it has closed.
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
                win.enter = Some(Spring::new(0.0, 1.0, SpringParams::SMOOTH, now));
            }
            wins.push(win);
        }
        screen.wins = wins;
        if let Some(sel) = mc.selected.clone() {
            if mc.find(&sel).is_none() {
                mc.selected = None;
            }
        }
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
    fn mc_rebuild_strip(&mut self, si: usize) {
        let Some(mc) = self.mc.as_ref() else { return };
        if mc.app_only.is_some() {
            return;
        }
        let Some(screen) = mc.screens.get(si) else { return };
        let count = self.spaces.info(&screen.output.name()).1;
        let geo = screen.geo;
        if let Some(mc) = self.mc.as_mut() {
            mc.screens[si].thumbs = thumb_rects(geo, count);
            mc.hovered_thumb = None;
            mc.hovered_remove = false;
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
        mc.hovered_thumb = mc.thumb_at(pos);
        mc.hovered_remove = mc
            .hovered_thumb
            .map(|(si, i)| {
                let s = &mc.screens[si];
                let dragging = mc.drag.as_ref().map(|d| d.active).unwrap_or(false);
                !dragging && i < s.space_count() && s.space_count() > 1 && remove_button(s.thumbs[i]).contains(pos)
            })
            .unwrap_or(false);
    }

    /// Left button pressed while Mission Control is open.
    pub fn mc_press(&mut self, pos: Point<f64, Logical>) {
        let now: Duration = self.clock.now().into();
        self.mc_motion(pos);
        let Some(mc) = self.mc.as_ref() else { return };
        if mc.closing {
            return;
        }
        if let Some((si, i)) = mc.thumb_at(pos) {
            let screen = &mc.screens[si];
            let output = screen.output.clone();
            let count = screen.space_count();
            let (active, _) = self.spaces.info(&output.name());
            if mc.hovered_remove {
                self.mc_remove_space(si, i);
            } else if i == count {
                // "+" adds a Space and stays in Mission Control
                self.add_space(&output);
                self.mc_rebuild_strip(si);
                self.render_space_thumbs();
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
        self.mc_rebuild_strip(si);
        self.render_space_thumbs();
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
        let thumb = mc.thumb_at(pos).filter(|(s, _)| *s == si).map(|(_, i)| i);
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
        st.mc_rebuild_strip(dst_si);
    }
    st.render_space_thumbs();
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
    fn single_small_window_keeps_size() {
        let area = Rectangle::<f64, Logical>::new((0.0, 0.0).into(), (1400.0, 800.0).into());
        let rects = grid_layout(area, &[(Point::from((10.0, 10.0)), Size::from((400.0, 300.0)))]);
        assert_eq!(rects[0].size, Size::from((400.0, 300.0)));
    }
}


impl<BackendData: Backend> OrchardState<BackendData> {
    /// Render a miniature of every Space for the strips at the top of Mission
    /// Control: the wallpaper with that Space's windows where they are.
    pub(crate) fn render_space_thumbs(&mut self) {
        let Some(mc) = self.mc.as_ref() else { return };
        if mc.app_only.is_some() {
            return;
        }
        let screens: Vec<(Output, Vec<Rectangle<f64, Logical>>)> =
            mc.screens.iter().map(|s| (s.output.clone(), s.thumbs.clone())).collect();
        for (si, (output, thumbs)) in screens.into_iter().enumerate() {
            let textures = self.render_thumbs_for(&output, &thumbs);
            if let Some(screen) = self.mc.as_mut().and_then(|m| m.screens.get_mut(si)) {
                screen.thumb_textures = textures;
            }
        }
    }

    fn render_thumbs_for(&mut self, output: &Output, thumbs: &[Rectangle<f64, Logical>]) -> Vec<Option<ThumbTexture>> {
        let Some(geo) = self.space.output_geometry(output) else { return Vec::new() };
        let Some(mode) = output.current_mode() else { return Vec::new() };
        let scale = output.current_scale().fractional_scale();
        let name = output.name();
        let (active, count) = self.spaces.info(&name);
        let thumbs: Vec<Rectangle<f64, Logical>> = thumbs.iter().take(count).copied().collect();

        // windows of each Space, bottom to top, with their locations
        let mut per_space: Vec<Vec<(WindowElement, Point<i32, Logical>)>> = Vec::new();
        for i in 0..count {
            if i == active {
                per_space.push(self.windows_on_output(output));
            } else {
                let slot = self.spaces.outputs.get(&name).and_then(|o| o.slots.get(i));
                let mut v: Vec<_> = slot.map(|s| s.windows.clone()).unwrap_or_default();
                if let Some(fs) = slot.and_then(|s| s.fullscreen.clone()) {
                    v.push((fs, geo.loc));
                }
                per_space.push(v);
            }
        }

        let full = output.current_transform().transform_size(mode.size);
        let fx = &mut self.fx;
        self.backend_data
            .with_renderer(|renderer: &mut GlesRenderer| {
                let wall = fx.wallpaper.textures(renderer, full);
                let mut out = Vec::new();
                for (thumb, windows) in thumbs.iter().zip(&per_space) {
                    let size: Size<i32, Physical> = thumb.size.to_physical_precise_ceil(scale);
                    if size.w <= 0 || size.h <= 0 {
                        out.push(None);
                        continue;
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
                    if let Some(wall) = wall.map(|w| w.element(scale)) {
                        elements.push(ThumbElement::Wall(RescaleRenderElement::from_element(wall, origin, k)));
                    }
                    let texture: Option<GlesTexture> = (|| {
                        use smithay::backend::renderer::{Frame, Renderer};
                        let mut tex: GlesTexture =
                            renderer.create_buffer(Fourcc::Abgr8888, (size.w, size.h).into()).ok()?;
                        {
                            let mut target = renderer.bind(&mut tex).ok()?;
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
                        Some(tex)
                    })();
                    out.push(texture.map(|texture| ThumbTexture { texture, size, id: Id::new() }));
                }
                out
            })
            .unwrap_or_default()
    }
}
