//! macOS-style interactive Screenshot tool (`Print` / ⌘⇧5).
//!
//! Renders on a full-screen transparent `Layer::Overlay` surface (`orchard-screenshot`):
//! - Floating frosted-glass pill toolbar near the bottom center
//! - Three capture modes: Entire Screen, Selected Window, Selected Portion
//! - Resizable & movable selection rectangle with dimmed scrim and 8 handles
//! - Window hover highlight with camera badge
//! - Options popover menu: Save To (Pictures / Desktop / Documents / Clipboard),
//!   Timer (None / 5s / 10s), Show Mouse Pointer, Remember Last Selection
//! - Countdown timer when a delay is configured

use std::time::{Duration, Instant};

use tiny_skia::{FillRule, LineCap, LineJoin, PathBuilder, Pixmap, Stroke, StrokeDash, Transform};

use crate::{
    draw::{fill_circle, fill_rounded, paint, stroke_rounded},
    text::{Fonts, Rgba, Weight},
    theme::Theme,
};

const TOOLBAR_H: f32 = 52.0;
const TOOLBAR_RADIUS: f32 = 14.0;
const TOOLBAR_BOTTOM_MARGIN: f32 = 88.0;
const BTN_SIZE: f32 = 38.0;
const BTN_GAP: f32 = 4.0;
const HANDLE_HIT: f32 = 10.0;
const MIN_SEL: f32 = 16.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Screen,
    Window,
    Area,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Screen => "screen",
            Mode::Window => "window",
            Mode::Area => "area",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveTo {
    Pictures,
    Desktop,
    Documents,
    Clipboard,
}

impl SaveTo {
    pub fn as_str(self) -> &'static str {
        match self {
            SaveTo::Pictures => "pictures",
            SaveTo::Desktop => "desktop",
            SaveTo::Documents => "documents",
            SaveTo::Clipboard => "clipboard",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerDelay {
    None,
    Sec5,
    Sec10,
}

impl TimerDelay {
    pub fn secs(self) -> u64 {
        match self {
            TimerDelay::None => 0,
            TimerDelay::Sec5 => 5,
            TimerDelay::Sec10 => 10,
        }
    }
}

/// Persisted in-session preferences (remembered while orchard-shell runs).
#[derive(Debug, Clone)]
pub struct Prefs {
    pub mode: Mode,
    pub save_to: SaveTo,
    pub timer: TimerDelay,
    pub show_cursor: bool,
    pub remember_selection: bool,
    pub last_selection: Option<(f32, f32, f32, f32)>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            mode: Mode::Area,
            save_to: SaveTo::Pictures,
            timer: TimerDelay::None,
            show_cursor: false,
            remember_selection: true,
            last_selection: None,
        }
    }
}

/// Visible window on the active output (output-relative logical px, top-to-bottom).
#[derive(Debug, Clone)]
pub struct ShotWindow {
    pub id: u64,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    #[allow(dead_code)]
    pub title: String,
}

/// Capture request emitted when the user confirms a screenshot.
#[derive(Debug, Clone)]
pub struct CaptureSpec {
    pub mode: Mode,
    pub output: String,
    pub rect: Option<(f32, f32, f32, f32)>,
    pub window_id: Option<u64>,
    pub save_to: SaveTo,
    pub show_cursor: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Close,
    Mode(Mode),
    Options,
    Capture,
    OptSave(SaveTo),
    OptTimer(TimerDelay),
    OptCursor,
    OptRemember,
    ToolbarBg,
    MenuBg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Edges {
    left: bool,
    right: bool,
    top: bool,
    bottom: bool,
}

impl Edges {
    fn any(self) -> bool {
        self.left || self.right || self.top || self.bottom
    }
}

#[derive(Debug, Clone, Copy)]
enum Drag {
    Move {
        offset_x: f32,
        offset_y: f32,
    },
    Resize {
        edges: Edges,
    },
    NewRect {
        start_x: f32,
        start_y: f32,
    },
}

/// Outcome of a pointer or keyboard interaction.
#[derive(Debug, Clone)]
pub enum Action {
    None,
    Redraw,
    Close,
    CaptureNow(CaptureSpec),
    StartCountdown,
}

pub struct ScreenshotTool {
    pub output_name: String,
    pub windows: Vec<ShotWindow>,
    pub mode: Mode,
    pub save_to: SaveTo,
    pub timer: TimerDelay,
    pub show_cursor: bool,
    pub remember_selection: bool,
    pub options_open: bool,
    /// Selection rect in output-relative logical px: (x, y, w, h).
    pub selection: (f32, f32, f32, f32),
    pub selection_initialized: bool,
    pub hover: Option<(f32, f32)>,
    pub hovered_window: Option<u64>,
    drag: Option<Drag>,
    /// Active timer countdown: (fire_at, pending spec)
    pub countdown: Option<(Instant, CaptureSpec)>,
    // rebuilt on paint, used for hit tests and compositor backdrops
    targets: Vec<((f32, f32, f32, f32), Hit)>,
    toolbar_rect: (f32, f32, f32, f32),
    menu_rect: Option<(f32, f32, f32, f32)>,
    last_size: (f32, f32),
}

impl ScreenshotTool {
    pub fn new(output_name: String, windows: Vec<ShotWindow>, prefs: &Prefs) -> Self {
        let (selection, selection_initialized) = match (prefs.remember_selection, prefs.last_selection) {
            (true, Some(r)) if r.2 >= MIN_SEL && r.3 >= MIN_SEL => (r, true),
            _ => ((0.0, 0.0, 0.0, 0.0), false),
        };
        Self {
            output_name,
            windows,
            mode: prefs.mode,
            save_to: prefs.save_to,
            timer: prefs.timer,
            show_cursor: prefs.show_cursor,
            remember_selection: prefs.remember_selection,
            options_open: false,
            selection,
            selection_initialized,
            hover: None,
            hovered_window: None,
            drag: None,
            countdown: None,
            targets: Vec::new(),
            toolbar_rect: (0.0, 0.0, 0.0, 0.0),
            menu_rect: None,
            last_size: (0.0, 0.0),
        }
    }

    pub fn save_prefs(&self, prefs: &mut Prefs) {
        prefs.mode = self.mode;
        prefs.save_to = self.save_to;
        prefs.timer = self.timer;
        prefs.show_cursor = self.show_cursor;
        prefs.remember_selection = self.remember_selection;
        if self.remember_selection && self.selection_initialized {
            prefs.last_selection = Some(self.selection);
        }
    }

    pub fn ensure_selection(&mut self, sw: f32, sh: f32) {
        if sw <= 0.0 || sh <= 0.0 {
            return;
        }
        self.last_size = (sw, sh);
        if !self.selection_initialized {
            // a tiny/degenerate configure must not make clamp(min > max) panic
            let w = (sw * 0.5).clamp(MIN_SEL, (sw - 40.0).max(MIN_SEL)).round();
            let h = (sh * 0.5).clamp(MIN_SEL, (sh - 160.0).max(MIN_SEL)).round();
            let x = ((sw - w) * 0.5).round();
            let y = ((sh - h - 40.0) * 0.5).max(36.0).round();
            self.selection = (x, y, w, h);
            self.selection_initialized = true;
        } else {
            self.clamp_selection(sw, sh);
        }
    }

    fn clamp_selection(&mut self, sw: f32, sh: f32) {
        let (mut x, mut y, mut w, mut h) = self.selection;
        w = w.clamp(MIN_SEL, sw.max(MIN_SEL));
        h = h.clamp(MIN_SEL, sh.max(MIN_SEL));
        x = x.clamp(0.0, (sw - w).max(0.0));
        y = y.clamp(0.0, (sh - h).max(0.0));
        self.selection = (x.round(), y.round(), w.round(), h.round());
    }

    /// Frosted-glass backdrop rects in surface-relative logical px: `(x, y, w, h, radius)`.
    pub fn backdrop_rects(&self) -> Vec<(f32, f32, f32, f32, f32)> {
        let mut out = Vec::with_capacity(2);
        let (tx, ty, tw, th) = self.toolbar_rect;
        if tw > 0.0 && th > 0.0 {
            out.push((tx, ty, tw, th, TOOLBAR_RADIUS));
        }
        if let Some((mx, my, mw, mh)) = self.menu_rect {
            if mw > 0.0 && mh > 0.0 {
                out.push((mx, my, mw, mh, 10.0));
            }
        }
        out
    }

    pub fn countdown_remaining(&self) -> Option<u64> {
        let (fire_at, _) = self.countdown.as_ref()?;
        let now = Instant::now();
        if *fire_at <= now {
            Some(0)
        } else {
            Some(fire_at.duration_since(now).as_secs_f32().ceil() as u64)
        }
    }

    pub fn take_ready_capture(&mut self) -> Option<CaptureSpec> {
        if self.countdown.as_ref().is_some_and(|(t, _)| Instant::now() >= *t) {
            return self.countdown.take().map(|(_, spec)| spec);
        }
        None
    }

    fn hit_ui(&self, x: f32, y: f32) -> Option<Hit> {
        // Check menu items first when menu is open
        for ((rx, ry, rw, rh), hit) in self.targets.iter().rev() {
            if x >= *rx && x < *rx + *rw && y >= *ry && y < *ry + *rh {
                return Some(*hit);
            }
        }
        if let Some((mx, my, mw, mh)) = self.menu_rect {
            if x >= mx && x < mx + mw && y >= my && y < my + mh {
                return Some(Hit::MenuBg);
            }
        }
        let (tx, ty, tw, th) = self.toolbar_rect;
        if x >= tx && x < tx + tw && y >= ty && y < ty + th {
            return Some(Hit::ToolbarBg);
        }
        None
    }

    fn hit_edges(&self, x: f32, y: f32) -> Edges {
        let (sx, sy, sw, sh) = self.selection;
        let near_x = x >= sx - HANDLE_HIT && x <= sx + sw + HANDLE_HIT;
        let near_y = y >= sy - HANDLE_HIT && y <= sy + sh + HANDLE_HIT;
        Edges {
            left: near_y && (x - sx).abs() <= HANDLE_HIT,
            right: near_y && (x - (sx + sw)).abs() <= HANDLE_HIT,
            top: near_x && (y - sy).abs() <= HANDLE_HIT,
            bottom: near_x && (y - (sy + sh)).abs() <= HANDLE_HIT,
        }
    }

    fn inside_selection(&self, x: f32, y: f32) -> bool {
        let (sx, sy, sw, sh) = self.selection;
        x >= sx && x <= sx + sw && y >= sy && y <= sy + sh
    }

    fn window_at(&self, x: f32, y: f32) -> Option<u64> {
        self.windows
            .iter()
            .find(|w| x >= w.x && x < w.x + w.w && y >= w.y && y < w.y + w.h)
            .map(|w| w.id)
    }

    fn build_capture_spec(&self, window_override: Option<u64>) -> CaptureSpec {
        CaptureSpec {
            mode: self.mode,
            output: self.output_name.clone(),
            rect: (self.mode == Mode::Area).then_some(self.selection),
            window_id: if self.mode == Mode::Window {
                window_override.or(self.hovered_window).or_else(|| self.windows.first().map(|w| w.id))
            } else {
                None
            },
            save_to: self.save_to,
            show_cursor: self.show_cursor,
        }
    }

    fn trigger_capture(&mut self, window_override: Option<u64>) -> Action {
        self.options_open = false;
        let spec = self.build_capture_spec(window_override);
        let delay = self.timer.secs();
        if delay > 0 {
            self.countdown = Some((Instant::now() + Duration::from_secs(delay), spec));
            Action::StartCountdown
        } else {
            Action::CaptureNow(spec)
        }
    }

    pub fn pointer_motion(&mut self, x: f32, y: f32) -> Action {
        self.hover = Some((x, y));
        if self.countdown.is_some() {
            return Action::Redraw;
        }
        if let Some(drag) = self.drag {
            let (sw, sh) = self.last_size;
            match drag {
                Drag::Move { offset_x, offset_y } => {
                    let (_, _, w, h) = self.selection;
                    let nx = (x - offset_x).clamp(0.0, (sw - w).max(0.0));
                    let ny = (y - offset_y).clamp(0.0, (sh - h).max(0.0));
                    self.selection = (nx.round(), ny.round(), w, h);
                }
                Drag::Resize { edges } => {
                    let (mut x0, mut y0, w, h) = self.selection;
                    let mut x1 = x0 + w;
                    let mut y1 = y0 + h;
                    if edges.left {
                        x0 = x.clamp(0.0, x1 - MIN_SEL);
                    }
                    if edges.right {
                        x1 = x.clamp(x0 + MIN_SEL, sw);
                    }
                    if edges.top {
                        y0 = y.clamp(0.0, y1 - MIN_SEL);
                    }
                    if edges.bottom {
                        y1 = y.clamp(y0 + MIN_SEL, sh);
                    }
                    self.selection = (x0.round(), y0.round(), (x1 - x0).round(), (y1 - y0).round());
                }
                Drag::NewRect { start_x, start_y } => {
                    let x0 = start_x.min(x).clamp(0.0, sw);
                    let y0 = start_y.min(y).clamp(0.0, sh);
                    let x1 = start_x.max(x).clamp(0.0, sw);
                    let y1 = start_y.max(y).clamp(0.0, sh);
                    self.selection = (
                        x0.round(),
                        y0.round(),
                        (x1 - x0).max(MIN_SEL).round(),
                        (y1 - y0).max(MIN_SEL).round(),
                    );
                    self.clamp_selection(sw, sh);
                }
            }
            return Action::Redraw;
        }

        if self.mode == Mode::Window && self.hit_ui(x, y).is_none() {
            let win = self.window_at(x, y);
            if win != self.hovered_window {
                self.hovered_window = win;
                return Action::Redraw;
            }
        } else if self.hovered_window.is_some() && self.mode != Mode::Window {
            self.hovered_window = None;
            return Action::Redraw;
        }
        Action::Redraw
    }

    pub fn pointer_press(&mut self, x: f32, y: f32) -> Action {
        self.hover = Some((x, y));

        // Clicking Capture during a countdown cancels it; clicking Close closes the tool.
        if self.countdown.is_some() {
            match self.hit_ui(x, y) {
                Some(Hit::Close) => return Action::Close,
                Some(Hit::Capture) => {
                    self.countdown = None;
                    return Action::Redraw;
                }
                _ => return Action::None,
            }
        }

        if let Some(hit) = self.hit_ui(x, y) {
            return match hit {
                Hit::Close => Action::Close,
                Hit::Mode(m) => {
                    self.mode = m;
                    self.options_open = false;
                    self.drag = None;
                    Action::Redraw
                }
                Hit::Options => {
                    self.options_open = !self.options_open;
                    Action::Redraw
                }
                Hit::Capture => self.trigger_capture(None),
                Hit::OptSave(s) => {
                    self.save_to = s;
                    self.options_open = false;
                    Action::Redraw
                }
                Hit::OptTimer(t) => {
                    self.timer = t;
                    self.options_open = false;
                    Action::Redraw
                }
                Hit::OptCursor => {
                    self.show_cursor = !self.show_cursor;
                    Action::Redraw
                }
                Hit::OptRemember => {
                    self.remember_selection = !self.remember_selection;
                    Action::Redraw
                }
                Hit::ToolbarBg | Hit::MenuBg => Action::None,
            };
        }

        // Click outside the Options popover closes it without triggering a capture
        if self.options_open {
            self.options_open = false;
            return Action::Redraw;
        }

        match self.mode {
            Mode::Screen => self.trigger_capture(None),
            Mode::Window => {
                if let Some(id) = self.window_at(x, y) {
                    self.hovered_window = Some(id);
                    self.trigger_capture(Some(id))
                } else {
                    Action::None
                }
            }
            Mode::Area => {
                let edges = self.hit_edges(x, y);
                if edges.any() {
                    self.drag = Some(Drag::Resize { edges });
                } else if self.inside_selection(x, y) {
                    let (sx, sy, _, _) = self.selection;
                    self.drag = Some(Drag::Move {
                        offset_x: x - sx,
                        offset_y: y - sy,
                    });
                } else {
                    self.drag = Some(Drag::NewRect { start_x: x, start_y: y });
                }
                Action::Redraw
            }
        }
    }

    pub fn pointer_release(&mut self) -> Action {
        if self.drag.take().is_some() {
            return Action::Redraw;
        }
        Action::None
    }

    pub fn key_press(&mut self, sym: smithay_client_toolkit::seat::keyboard::Keysym, shift: bool) -> Action {
        use smithay_client_toolkit::seat::keyboard::Keysym;
        match sym {
            Keysym::Escape | Keysym::Print => {
                if self.countdown.take().is_some() {
                    return Action::Redraw;
                }
                if self.options_open {
                    self.options_open = false;
                    return Action::Redraw;
                }
                Action::Close
            }
            Keysym::Return | Keysym::KP_Enter => {
                if self.countdown.is_some() {
                    return Action::None;
                }
                self.trigger_capture(None)
            }
            Keysym::_1 => {
                self.mode = Mode::Screen;
                self.options_open = false;
                Action::Redraw
            }
            Keysym::_2 => {
                self.mode = Mode::Window;
                self.options_open = false;
                Action::Redraw
            }
            Keysym::_3 => {
                self.mode = Mode::Area;
                self.options_open = false;
                Action::Redraw
            }
            Keysym::Left | Keysym::Right | Keysym::Up | Keysym::Down if self.mode == Mode::Area => {
                let step = if shift { 10.0 } else { 1.0 };
                let (mut x, mut y, w, h) = self.selection;
                match sym {
                    Keysym::Left => x -= step,
                    Keysym::Right => x += step,
                    Keysym::Up => y -= step,
                    Keysym::Down => y += step,
                    _ => {}
                }
                self.selection = (x, y, w, h);
                let (sw, sh) = self.last_size;
                if sw > 0.0 && sh > 0.0 {
                    self.clamp_selection(sw, sh);
                }
                Action::Redraw
            }
            _ => Action::None,
        }
    }

    pub fn paint(&mut self, pm: &mut Pixmap, scale: f32, fonts: &Fonts, theme: &Theme, german: bool) {
        let s = scale;
        let sw = pm.width() as f32 / s;
        let sh = pm.height() as f32 / s;
        self.ensure_selection(sw, sh);
        self.targets.clear();
        self.menu_rect = None;

        // 1. Mode-specific full-screen overlay
        match self.mode {
            Mode::Area => self.paint_area_overlay(pm, s, sw, sh, fonts),
            Mode::Window => self.paint_window_overlay(pm, s),
            Mode::Screen => {}
        }

        // 2. Floating macOS Screenshot toolbar pill
        self.paint_toolbar(pm, s, sw, sh, fonts, theme, german);

        // 3. Options popover (above the Options button)
        if self.options_open {
            self.paint_options_menu(pm, s, sw, fonts, theme, german);
        }

        // 4. Hover tooltip above toolbar mode buttons (when options menu is closed)
        if !self.options_open {
            self.paint_tooltip(pm, s, fonts, theme, german);
        }
    }

    fn paint_area_overlay(&self, pm: &mut Pixmap, s: f32, sw: f32, sh: f32, fonts: &Fonts) {
        let (x, y, w, h) = self.selection;
        let scrim: Rgba = [0, 0, 0, 112];

        // 4 dimmed rectangles outside the selection box
        let rects = [
            (0.0, 0.0, sw, y),                        // top
            (0.0, y + h, sw, (sh - (y + h)).max(0.0)), // bottom
            (0.0, y, x, h),                           // left
            (x + w, y, (sw - (x + w)).max(0.0), h),   // right
        ];
        for (rx, ry, rw, rh) in rects {
            if rw > 0.0 && rh > 0.0 {
                if let Some(r) = tiny_skia::Rect::from_xywh(rx * s, ry * s, rw * s, rh * s) {
                    pm.fill_rect(r, &paint(scrim), Transform::identity(), None);
                }
            }
        }

        // Subtle dark outer stroke + crisp white dashed border
        if let Some(path) = rect_path(x * s, y * s, w * s, h * s) {
            let dark_st = Stroke {
                width: 2.0 * s,
                ..Default::default()
            };
            pm.stroke_path(&path, &paint([0, 0, 0, 140]), &dark_st, Transform::identity(), None);

            let mut dash_st = Stroke {
                width: 1.5 * s,
                ..Default::default()
            };
            dash_st.dash = StrokeDash::new(vec![6.0 * s, 4.0 * s], 0.0);
            pm.stroke_path(&path, &paint([255, 255, 255, 240]), &dash_st, Transform::identity(), None);
        }

        // 8 resize handles (4 corners + 4 edge midpoints)
        if self.countdown.is_none() {
            let handles = [
                (x, y),
                (x + w * 0.5, y),
                (x + w, y),
                (x, y + h * 0.5),
                (x + w, y + h * 0.5),
                (x, y + h),
                (x + w * 0.5, y + h),
                (x + w, y + h),
            ];
            for (hx, hy) in handles {
                fill_circle(pm, hx * s, hy * s, 4.5 * s, [0, 0, 0, 140]);
                fill_circle(pm, hx * s, hy * s, 3.5 * s, [255, 255, 255, 250]);
            }
        }

        // Live dimension badge (e.g. "960 × 540")
        let dim = format!("{} × {}", w.round() as i32, h.round() as i32);
        let fpx = 11.5 * s;
        let tw = fonts.measure(&dim, fpx, Weight::Bold) / s;
        let pad_x = 8.0;
        let pill_w = tw + pad_x * 2.0;
        let pill_h = 22.0;
        let px = (x + w - pill_w - 8.0).max(x + 8.0).min(sw - pill_w - 8.0);
        let py = if y + h + pill_h + 10.0 < sh - TOOLBAR_BOTTOM_MARGIN - TOOLBAR_H - 12.0 {
            y + h + 8.0
        } else {
            (y + h - pill_h - 8.0).max(y + 8.0)
        };
        fill_rounded(pm, px * s, py * s, pill_w * s, pill_h * s, 6.0 * s, [24, 24, 28, 200]);
        stroke_rounded(pm, px * s, py * s, pill_w * s, pill_h * s, 6.0 * s, 1.0 * s, [255, 255, 255, 45]);
        let base = fonts.baseline_centered(py * s, pill_h * s, fpx, Weight::Bold);
        fonts.draw(pm, &dim, (px + pad_x) * s, base, fpx, Weight::Bold, [245, 245, 248, 255]);
    }

    fn paint_window_overlay(&self, pm: &mut Pixmap, s: f32) {
        let Some(id) = self.hovered_window else { return };
        let Some(w) = self.windows.iter().find(|w| w.id == id) else { return };
        fill_rounded(pm, w.x * s, w.y * s, w.w * s, w.h * s, 10.0 * s, [24, 122, 248, 64]);
        stroke_rounded(pm, w.x * s, w.y * s, w.w * s, w.h * s, 10.0 * s, 2.0 * s, [90, 170, 255, 210]);

        // Centered macOS camera badge over the hovered window
        let cx = (w.x + w.w * 0.5) * s;
        let cy = (w.y + w.h * 0.5) * s;
        fill_circle(pm, cx, cy, 26.0 * s, [28, 28, 32, 210]);
        stroke_rounded(pm, cx - 26.0 * s, cy - 26.0 * s, 52.0 * s, 52.0 * s, 26.0 * s, 1.0 * s, [255, 255, 255, 65]);
        draw_camera_icon(pm, cx, cy, 13.0 * s, [245, 245, 248, 255]);
    }

    fn paint_toolbar(&mut self, pm: &mut Pixmap, s: f32, sw: f32, sh: f32, fonts: &Fonts, theme: &Theme, german: bool) {
        let dark = theme.bar_text[0] > 128;
        let solid = theme.menu_bg[3] == 255;
        let bg: Rgba = if dark {
            [38, 38, 42, if solid { 255 } else { 216 }]
        } else {
            [244, 244, 248, if solid { 255 } else { 224 }]
        };
        let border: Rgba = if dark { [255, 255, 255, 48] } else { [0, 0, 0, 38] };
        let fg: Rgba = if dark { [242, 242, 246, 255] } else { [32, 32, 36, 255] };
        let sep: Rgba = if dark { [255, 255, 255, 42] } else { [0, 0, 0, 32] };
        let hover_fill: Rgba = if dark { [255, 255, 255, 26] } else { [0, 0, 0, 18] };
        let active_fill: Rgba = if dark { [255, 255, 255, 50] } else { [0, 0, 0, 34] };

        let opt_label = if german { "Optionen" } else { "Options" };
        let cap_label = if let Some(rem) = self.countdown_remaining() {
            format!("{rem}…")
        } else if german {
            "Aufnehmen".to_string()
        } else {
            "Capture".to_string()
        };

        let fpx = 13.0 * s;
        let opt_tw = fonts.measure(opt_label, fpx, Weight::Regular) / s;
        let cap_tw = fonts.measure(&cap_label, fpx, Weight::Bold) / s;

        let close_w = 30.0;
        let opt_w = opt_tw + 26.0; // text + chevron + padding
        let cap_w = (cap_tw + 24.0).max(74.0);
        let pad_x = 10.0;
        let sep_w = 13.0;

        let total_w = pad_x
            + close_w
            + sep_w
            + BTN_SIZE * 3.0
            + BTN_GAP * 2.0
            + sep_w
            + opt_w
            + 8.0
            + cap_w
            + pad_x;

        let tx = ((sw - total_w) * 0.5).round();
        let ty = (sh - TOOLBAR_BOTTOM_MARGIN - TOOLBAR_H).max(32.0).round();
        self.toolbar_rect = (tx, ty, total_w, TOOLBAR_H);

        // Soft drop shadow + frosted pill background + hairline border
        fill_rounded(pm, tx * s, (ty + 4.0) * s, total_w * s, TOOLBAR_H * s, TOOLBAR_RADIUS * s, [0, 0, 0, 56]);
        fill_rounded(pm, tx * s, ty * s, total_w * s, TOOLBAR_H * s, TOOLBAR_RADIUS * s, bg);
        stroke_rounded(pm, tx * s, ty * s, total_w * s, TOOLBAR_H * s, TOOLBAR_RADIUS * s, 1.0 * s, border);

        let mut x = tx + pad_x;
        let cy = ty + TOOLBAR_H * 0.5;

        // 1. Close button (×)
        let close_rect = (x, cy - 14.0, close_w, 28.0);
        let close_hov = self.is_hovered(close_rect);
        if close_hov {
            fill_circle(pm, (x + close_w * 0.5) * s, cy * s, 13.0 * s, hover_fill);
        }
        draw_close_icon(pm, (x + close_w * 0.5) * s, cy * s, 5.0 * s, fg);
        self.targets.push((close_rect, Hit::Close));
        x += close_w;

        // Divider 1
        draw_vline(pm, (x + sep_w * 0.5) * s, (ty + 11.0) * s, (ty + TOOLBAR_H - 11.0) * s, s, sep);
        x += sep_w;

        // 2. Capture mode buttons (Entire Screen, Selected Window, Selected Portion)
        let modes = [Mode::Screen, Mode::Window, Mode::Area];
        for (i, mode) in modes.into_iter().enumerate() {
            let r = (x, cy - BTN_SIZE * 0.5, BTN_SIZE, BTN_SIZE);
            let active = self.mode == mode;
            let hov = self.is_hovered(r);
            if active {
                fill_rounded(pm, r.0 * s, r.1 * s, r.2 * s, r.3 * s, 8.0 * s, active_fill);
            } else if hov {
                fill_rounded(pm, r.0 * s, r.1 * s, r.2 * s, r.3 * s, 8.0 * s, hover_fill);
            }
            let icx = (r.0 + r.2 * 0.5) * s;
            let icy = (r.1 + r.3 * 0.5) * s;
            match mode {
                Mode::Screen => draw_screen_icon(pm, icx, icy, s, fg),
                Mode::Window => draw_window_icon(pm, icx, icy, s, fg),
                Mode::Area => draw_area_icon(pm, icx, icy, s, fg),
            }
            self.targets.push((r, Hit::Mode(mode)));
            x += BTN_SIZE + if i + 1 < modes.len() { BTN_GAP } else { 0.0 };
        }

        // Divider 2
        draw_vline(pm, (x + sep_w * 0.5) * s, (ty + 11.0) * s, (ty + TOOLBAR_H - 11.0) * s, s, sep);
        x += sep_w;

        // 3. Options button (with small chevron)
        let opt_rect = (x, cy - 16.0, opt_w, 32.0);
        if self.options_open {
            fill_rounded(pm, opt_rect.0 * s, opt_rect.1 * s, opt_rect.2 * s, opt_rect.3 * s, 8.0 * s, active_fill);
        } else if self.is_hovered(opt_rect) {
            fill_rounded(pm, opt_rect.0 * s, opt_rect.1 * s, opt_rect.2 * s, opt_rect.3 * s, 8.0 * s, hover_fill);
        }
        let base = fonts.baseline_centered(opt_rect.1 * s, opt_rect.3 * s, fpx, Weight::Regular);
        fonts.draw(pm, opt_label, (opt_rect.0 + 9.0) * s, base, fpx, Weight::Regular, fg);
        draw_chevron_down(pm, (opt_rect.0 + opt_rect.2 - 11.0) * s, cy * s, 3.5 * s, fg);
        self.targets.push((opt_rect, Hit::Options));
        x += opt_w + 8.0;

        // 4. Capture button (or Countdown cancel button)
        let cap_rect = (x, cy - 16.0, cap_w, 32.0);
        let cap_hov = self.is_hovered(cap_rect);
        let (cap_bg, cap_fg) = if self.countdown.is_some() {
            ([225, 58, 52, 240], [255, 255, 255, 255])
        } else if cap_hov {
            let mut a = theme.menu_selection;
            a[3] = 255;
            (a, [255, 255, 255, 255])
        } else {
            let mut a = theme.menu_selection;
            a[3] = 230;
            (a, [255, 255, 255, 255])
        };
        fill_rounded(pm, cap_rect.0 * s, cap_rect.1 * s, cap_rect.2 * s, cap_rect.3 * s, 8.0 * s, cap_bg);
        let cbase = fonts.baseline_centered(cap_rect.1 * s, cap_rect.3 * s, fpx, Weight::Bold);
        let cx_text = cap_rect.0 + (cap_rect.2 - cap_tw) * 0.5;
        fonts.draw(pm, &cap_label, cx_text * s, cbase, fpx, Weight::Bold, cap_fg);
        self.targets.push((cap_rect, Hit::Capture));
    }

    fn paint_options_menu(&mut self, pm: &mut Pixmap, s: f32, sw: f32, fonts: &Fonts, theme: &Theme, german: bool) {
        let dark = theme.bar_text[0] > 128;
        let bg = theme.menu_bg;
        let border = theme.menu_border;
        let fg = theme.menu_text;
        let muted: Rgba = if dark { [165, 165, 172, 255] } else { [110, 110, 118, 255] };
        let sep = theme.menu_separator;
        let sel_bg = theme.menu_selection;
        let sel_fg = theme.menu_selection_text;

        let t = |en: &'static str, de: &'static str| if german { de } else { en };

        #[derive(Clone, Copy)]
        enum Row {
            Header(&'static str),
            Sep,
            Item(&'static str, bool, Hit),
        }

        let rows = [
            Row::Header(t("Save to", "Sichern in")),
            Row::Item(t("Screenshots", "Bildschirmfotos"), self.save_to == SaveTo::Pictures, Hit::OptSave(SaveTo::Pictures)),
            Row::Item(t("Desktop", "Schreibtisch"), self.save_to == SaveTo::Desktop, Hit::OptSave(SaveTo::Desktop)),
            Row::Item(t("Documents", "Dokumente"), self.save_to == SaveTo::Documents, Hit::OptSave(SaveTo::Documents)),
            Row::Item(t("Clipboard", "Zwischenablage"), self.save_to == SaveTo::Clipboard, Hit::OptSave(SaveTo::Clipboard)),
            Row::Sep,
            Row::Header(t("Timer", "Timer")),
            Row::Item(t("None", "Ohne"), self.timer == TimerDelay::None, Hit::OptTimer(TimerDelay::None)),
            Row::Item(t("5 Seconds", "5 Sekunden"), self.timer == TimerDelay::Sec5, Hit::OptTimer(TimerDelay::Sec5)),
            Row::Item(t("10 Seconds", "10 Sekunden"), self.timer == TimerDelay::Sec10, Hit::OptTimer(TimerDelay::Sec10)),
            Row::Sep,
            Row::Header(t("Options", "Optionen")),
            Row::Item(t("Show Mouse Pointer", "Mauszeiger einblenden"), self.show_cursor, Hit::OptCursor),
            Row::Item(t("Remember Last Selection", "Letzte Auswahl merken"), self.remember_selection, Hit::OptRemember),
        ];

        let menu_w = 228.0;
        let pad_y = 6.0;
        let header_h = 22.0;
        let item_h = 26.0;
        let sep_h = 9.0;

        let mut menu_h = pad_y * 2.0;
        for r in &rows {
            menu_h += match r {
                Row::Header(_) => header_h,
                Row::Sep => sep_h,
                Row::Item(..) => item_h,
            };
        }

        // Anchor directly above the Options button
        let opt_x = self
            .targets
            .iter()
            .find(|(_, h)| *h == Hit::Options)
            .map(|(r, _)| r.0 + r.2 * 0.5)
            .unwrap_or(self.toolbar_rect.0 + self.toolbar_rect.2 * 0.7);
        let mx = (opt_x - menu_w * 0.5).clamp(12.0, (sw - menu_w - 12.0).max(12.0)).round();
        let my = (self.toolbar_rect.1 - menu_h - 10.0).max(12.0).round();
        self.menu_rect = Some((mx, my, menu_w, menu_h));

        fill_rounded(pm, mx * s, (my + 4.0) * s, menu_w * s, menu_h * s, 10.0 * s, [0, 0, 0, 64]);
        fill_rounded(pm, mx * s, my * s, menu_w * s, menu_h * s, 10.0 * s, bg);
        stroke_rounded(pm, mx * s, my * s, menu_w * s, menu_h * s, 10.0 * s, 1.0 * s, border);

        let mut y = my + pad_y;
        let f_hdr = 11.0 * s;
        let f_item = 13.0 * s;
        for row in rows {
            match row {
                Row::Header(label) => {
                    let base = fonts.baseline_centered(y * s, header_h * s, f_hdr, Weight::Bold);
                    fonts.draw(pm, label, (mx + 26.0) * s, base, f_hdr, Weight::Bold, muted);
                    y += header_h;
                }
                Row::Sep => {
                    let ly = (y + sep_h * 0.5) * s;
                    if let Some(r) = tiny_skia::Rect::from_xywh((mx + 12.0) * s, ly, (menu_w - 24.0) * s, 1.0 * s) {
                        pm.fill_rect(r, &paint(sep), Transform::identity(), None);
                    }
                    y += sep_h;
                }
                Row::Item(label, checked, hit) => {
                    let r = (mx + 5.0, y, menu_w - 10.0, item_h);
                    let hov = self.is_hovered(r);
                    let text_c = if hov {
                        fill_rounded(pm, r.0 * s, r.1 * s, r.2 * s, r.3 * s, 5.0 * s, sel_bg);
                        sel_fg
                    } else {
                        fg
                    };
                    if checked {
                        draw_checkmark(pm, (mx + 15.0) * s, (y + item_h * 0.5) * s, 4.5 * s, text_c);
                    }
                    let base = fonts.baseline_centered(y * s, item_h * s, f_item, Weight::Regular);
                    fonts.draw(pm, label, (mx + 26.0) * s, base, f_item, Weight::Regular, text_c);
                    self.targets.push((r, hit));
                    y += item_h;
                }
            }
        }
    }

    fn paint_tooltip(&self, pm: &mut Pixmap, s: f32, fonts: &Fonts, theme: &Theme, german: bool) {
        let Some((hx, hy)) = self.hover else { return };
        let hit = self.targets.iter().find(|((rx, ry, rw, rh), _)| {
            hx >= *rx && hx < *rx + *rw && hy >= *ry && hy < *ry + *rh
        });
        let Some((rect, hit)) = hit else { return };
        let label = match hit {
            Hit::Mode(Mode::Screen) => {
                if german { "Gesamten Bildschirm aufnehmen" } else { "Capture Entire Screen" }
            }
            Hit::Mode(Mode::Window) => {
                if german { "Ausgewähltes Fenster aufnehmen" } else { "Capture Selected Window" }
            }
            Hit::Mode(Mode::Area) => {
                if german { "Ausgewählten Bereich aufnehmen" } else { "Capture Selected Portion" }
            }
            _ => return,
        };
        let fpx = 12.0 * s;
        let tw = fonts.measure(label, fpx, Weight::Regular) / s;
        let pw = tw + 18.0;
        let ph = 24.0;
        let px = (rect.0 + rect.2 * 0.5 - pw * 0.5).round();
        let py = (self.toolbar_rect.1 - ph - 8.0).round();
        fill_rounded(pm, px * s, py * s, pw * s, ph * s, 6.0 * s, theme.tooltip_bg);
        stroke_rounded(pm, px * s, py * s, pw * s, ph * s, 6.0 * s, 1.0 * s, theme.menu_border);
        let base = fonts.baseline_centered(py * s, ph * s, fpx, Weight::Regular);
        fonts.draw(pm, label, (px + 9.0) * s, base, fpx, Weight::Regular, theme.tooltip_text);
    }

    fn is_hovered(&self, r: (f32, f32, f32, f32)) -> bool {
        self.hover
            .map(|(hx, hy)| hx >= r.0 && hx < r.0 + r.2 && hy >= r.1 && hy < r.1 + r.3)
            .unwrap_or(false)
    }
}

// ---- vector icons -----------------------------------------------------------

fn rect_path(x: f32, y: f32, w: f32, h: f32) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    pb.move_to(x, y);
    pb.line_to(x + w, y);
    pb.line_to(x + w, y + h);
    pb.line_to(x, y + h);
    pb.close();
    pb.finish()
}

fn draw_vline(pm: &mut Pixmap, x: f32, y0: f32, y1: f32, width: f32, color: Rgba) {
    if let Some(r) = tiny_skia::Rect::from_xywh(x - width * 0.5, y0, width, (y1 - y0).max(1.0)) {
        pm.fill_rect(r, &paint(color), Transform::identity(), None);
    }
}

fn draw_close_icon(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, color: Rgba) {
    let st = Stroke {
        width: (r * 0.34).max(1.4),
        line_cap: LineCap::Round,
        ..Default::default()
    };
    let p = paint(color);
    for (dx0, dy0, dx1, dy1) in [(-r, -r, r, r), (-r, r, r, -r)] {
        let mut pb = PathBuilder::new();
        pb.move_to(cx + dx0, cy + dy0);
        pb.line_to(cx + dx1, cy + dy1);
        if let Some(path) = pb.finish() {
            pm.stroke_path(&path, &p, &st, Transform::identity(), None);
        }
    }
}

fn draw_screen_icon(pm: &mut Pixmap, cx: f32, cy: f32, s: f32, color: Rgba) {
    let w = 22.0 * s;
    let h = 15.0 * s;
    let x = cx - w * 0.5;
    let y = cy - h * 0.5 - 1.0 * s;
    stroke_rounded(pm, x, y, w, h, 2.5 * s, 1.5 * s, color);
    let mut fill = color;
    fill[3] = 48;
    fill_rounded(pm, x + 2.0 * s, y + 2.0 * s, w - 4.0 * s, h - 4.0 * s, 1.2 * s, fill);
    // stand
    if let Some(r) = tiny_skia::Rect::from_xywh(cx - 4.5 * s, y + h + 2.0 * s, 9.0 * s, 1.6 * s) {
        pm.fill_rect(r, &paint(color), Transform::identity(), None);
    }
}

fn draw_window_icon(pm: &mut Pixmap, cx: f32, cy: f32, s: f32, color: Rgba) {
    let w = 22.0 * s;
    let h = 16.0 * s;
    let x = cx - w * 0.5;
    let y = cy - h * 0.5;
    stroke_rounded(pm, x, y, w, h, 3.0 * s, 1.5 * s, color);
    // titlebar divider
    if let Some(r) = tiny_skia::Rect::from_xywh(x + 1.0 * s, y + 5.0 * s, w - 2.0 * s, 1.2 * s) {
        pm.fill_rect(r, &paint(color), Transform::identity(), None);
    }
    // 3 tiny traffic-light dots
    for i in 0..3 {
        fill_circle(pm, x + (4.2 + i as f32 * 2.8) * s, y + 2.7 * s, 0.9 * s, color);
    }
}

fn draw_area_icon(pm: &mut Pixmap, cx: f32, cy: f32, s: f32, color: Rgba) {
    let w = 20.0 * s;
    let h = 15.0 * s;
    let x = cx - w * 0.5;
    let y = cy - h * 0.5;
    if let Some(path) = rect_path(x, y, w, h) {
        let mut st = Stroke {
            width: 1.5 * s,
            ..Default::default()
        };
        st.dash = StrokeDash::new(vec![3.5 * s, 2.5 * s], 0.0);
        pm.stroke_path(&path, &paint(color), &st, Transform::identity(), None);
    }
    // tiny corner dots
    for (dx, dy) in [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)] {
        fill_circle(pm, x + dx, y + dy, 1.8 * s, color);
    }
}

fn draw_chevron_down(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, color: Rgba) {
    let mut pb = PathBuilder::new();
    pb.move_to(cx - r, cy - r * 0.45);
    pb.line_to(cx, cy + r * 0.55);
    pb.line_to(cx + r, cy - r * 0.45);
    if let Some(path) = pb.finish() {
        let st = Stroke {
            width: (r * 0.42).max(1.3),
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Default::default()
        };
        pm.stroke_path(&path, &paint(color), &st, Transform::identity(), None);
    }
}

fn draw_checkmark(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, color: Rgba) {
    let mut pb = PathBuilder::new();
    pb.move_to(cx - r * 0.85, cy);
    pb.line_to(cx - r * 0.15, cy + r * 0.72);
    pb.line_to(cx + r * 0.9, cy - r * 0.72);
    if let Some(path) = pb.finish() {
        let st = Stroke {
            width: (r * 0.36).max(1.4),
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Default::default()
        };
        pm.stroke_path(&path, &paint(color), &st, Transform::identity(), None);
    }
}

fn draw_camera_icon(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, color: Rgba) {
    // Camera body
    let bw = r * 1.9;
    let bh = r * 1.3;
    let bx = cx - bw * 0.5;
    let by = cy - bh * 0.5 + r * 0.12;
    fill_rounded(pm, bx, by, bw, bh, r * 0.28, color);
    // Top viewfinder bump
    let tw = r * 0.76;
    let th = r * 0.32;
    fill_rounded(pm, cx - tw * 0.5, by - th * 0.8, tw, th, r * 0.14, color);
    // Lens cutout
    let mut pb = PathBuilder::new();
    if let Some(p) = PathBuilder::from_circle(cx, by + bh * 0.5, r * 0.42) {
        pm.fill_path(&p, &paint([28, 28, 32, 235]), FillRule::Winding, Transform::identity(), None);
    }
    let _ = &mut pb;
    fill_circle(pm, cx, by + bh * 0.5, r * 0.24, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_selection_centered_and_clamped() {
        let prefs = Prefs::default();
        let mut tool = ScreenshotTool::new("DP-1".into(), vec![], &prefs);
        tool.ensure_selection(1920.0, 1080.0);
        let (x, y, w, h) = tool.selection;
        assert!(w >= 900.0 && h >= 500.0);
        assert!(x > 0.0 && y > 0.0 && x + w <= 1920.0 && y + h <= 1080.0);
    }

    #[test]
    fn drag_resize_respects_min_size() {
        let prefs = Prefs::default();
        let mut tool = ScreenshotTool::new("DP-1".into(), vec![], &prefs);
        tool.ensure_selection(1000.0, 800.0);
        tool.selection = (100.0, 100.0, 200.0, 200.0);
        // Grab right edge at x = 300, y = 200 and drag left past x = 100
        let _ = tool.pointer_press(300.0, 200.0);
        let _ = tool.pointer_motion(50.0, 200.0);
        let _ = tool.pointer_release();
        assert_eq!(tool.selection.0, 100.0);
        assert!(tool.selection.2 >= MIN_SEL);
    }

    #[test]
    fn escape_closes_options_before_tool() {
        use smithay_client_toolkit::seat::keyboard::Keysym;
        let prefs = Prefs::default();
        let mut tool = ScreenshotTool::new("DP-1".into(), vec![], &prefs);
        tool.options_open = true;
        assert!(matches!(tool.key_press(Keysym::Escape, false), Action::Redraw));
        assert!(!tool.options_open);
        assert!(matches!(tool.key_press(Keysym::Escape, false), Action::Close));
    }
}
