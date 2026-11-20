//! macOS-style Stage Manager.
//!
//! Keeps the active application (or manually grouped window set) front and
//! centre while recently used apps wait in a vertical strip along the left edge
//! (or right edge when the Dock is on the left) as live, 3D-perspective-tilted
//! miniature thumbnails.
//!
//! Supported interactions (1:1 macOS):
//! - Click a thumbnail in the strip: glide the current stage into the strip and
//!   bring that group onto centre stage.
//! - Shift-click a thumbnail, or drag it out of the strip onto centre stage:
//!   combine its windows into the active stage (multi-app workspace).
//! - Drag a window from a multi-window active stage into the strip edge:
//!   separate that window back into its own stage in the strip.
//! - Hover a thumbnail: smoothly lifts, enlarges, and turns toward the user,
//!   fanning multi-window stacks slightly and showing the app/window title.
//! - Auto-hide: if a window in the active stage overlaps the strip (or "Show
//!   recent applications" is turned off), the strip slides off-screen and
//!   reveals automatically when the pointer touches the screen edge.
//! - Click empty wallpaper: moves the active stage aside to reveal desktop items.
//! - Multi-monitor & Spaces aware: independent strip per display and Space.

use std::{
    cell::Cell,
    collections::HashMap,
    time::Duration,
};

use orchard_config::{DockPosition, StageAppWindows};
use smithay::{
    backend::renderer::{
        element::{AsRenderElements, Element, Id},
        gles::GlesRenderer,
        utils::CommitCounter,
    },
    desktop::{layer_map_for_output, space::SpaceElement},
    output::Output,
    utils::{IsAlive, Logical, Point, Rectangle, Scale, Size},
    wayland::shell::wlr_layer::Layer as WlrLayer,
};

use crate::{
    effects::{
        genie::RectF,
        lerp_rect,
        perspective::PerspectiveElement,
        snapshot::{snapshot_window, Snapshot},
        spring::{Spring, SpringParams},
    },
    shell::{WindowElement, WindowRenderElement},
    state::{Backend, OrchardState},
};

/// Pointer movement before a press on a strip thumbnail becomes a drag.
const DRAG_THRESHOLD: f64 = 6.0;
/// Outer edge thickness (logical px) that reveals an auto-hidden strip.
const EDGE_TRIGGER_PX: f64 = 4.0;
/// Extra hysteresis beyond the strip width before an edge-revealed strip hides again.
const EDGE_LEAVE_HYSTERESIS: f64 = 28.0;
/// Resting 3D tilt angle in radians (~20.6°), facing inward toward centre stage.
pub const REST_YAW_RAD: f32 = 0.36;
/// Hovered 3D tilt angle in radians (~10.3°), turning subtly toward the user.
pub const HOVER_YAW_RAD: f32 = 0.18;
/// Vertical gap between groups in the strip (logical px).
const GROUP_GAP: f64 = 18.0;
/// Margin from the screen's outer edge to the thumbnails (logical px).
const STRIP_MARGIN_X: f64 = 14.0;
/// Minimum interval between live-thumbnail snapshot refreshes per staged window.
const LIVE_SNAP_INTERVAL: Duration = Duration::from_millis(280);
/// Maximum number of stacked cards drawn per group in the strip.
const MAX_STACK_CARDS: usize = 3;

/// One window tracked by Stage Manager.
#[derive(Debug)]
pub struct StagedWindow {
    pub window: WindowElement,
    /// Saved global logical location for restoring to centre stage or turning Stage Manager off.
    pub saved_loc: Point<i32, Logical>,
    /// Cached geometry size (used to compute aspect ratios even if unmapped).
    pub saved_size: Size<i32, Logical>,
    /// Cached offscreen snapshot (includes SSD chrome + drop shadow).
    pub snapshot: Option<Snapshot>,
    /// Last rendered element commit signature for live thumbnail updates.
    pub snapshot_key: Vec<(Id, CommitCounter)>,
    /// Monotonic time of last snapshot capture.
    pub last_snap: Duration,
    /// Stable render element id for strip rendering.
    pub render_id: Id,
    pub commit: Cell<CommitCounter>,
}

impl StagedWindow {
    fn new(window: WindowElement, saved_loc: Point<i32, Logical>, snapshot: Option<Snapshot>, now: Duration) -> Self {
        let saved_size = window.geometry().size;
        Self {
            window,
            saved_loc,
            saved_size,
            snapshot,
            snapshot_key: Vec::new(),
            last_snap: now,
            render_id: Id::new(),
            commit: Cell::new(CommitCounter::default()),
        }
    }

    pub fn next_commit(&self) -> CommitCounter {
        let mut c = self.commit.get();
        c.increment();
        self.commit.set(c);
        c
    }

    pub fn frame_size(&self) -> Size<f64, Logical> {
        let g = self.window.geometry().size;
        if g.w > 0 && g.h > 0 {
            g.to_f64()
        } else if self.saved_size.w > 0 && self.saved_size.h > 0 {
            self.saved_size.to_f64()
        } else {
            (800.0, 600.0).into()
        }
    }
}

/// A stage (group of one or more windows working together).
#[derive(Debug)]
pub struct StageGroup {
    pub id: u64,
    /// Representative app_id (from the frontmost window in the group).
    pub app_id: String,
    /// Windows in stacking order: bottom-most first, frontmost last.
    pub windows: Vec<StagedWindow>,
    /// Animated slot rect in the strip (global logical).
    pub slot_from: Rectangle<f64, Logical>,
    pub slot_target: Rectangle<f64, Logical>,
    pub slot_spring: Option<Spring>,
    /// 0.0 = resting, 1.0 = hovered.
    pub hover_spring: Spring,
    pub hovered: bool,
}

impl StageGroup {
    fn new(id: u64, windows: Vec<StagedWindow>, now: Duration) -> Self {
        let app_id = windows.last().map(|w| w.window.app_id()).unwrap_or_default();
        Self {
            id,
            app_id,
            windows,
            slot_from: Rectangle::default(),
            slot_target: Rectangle::default(),
            slot_spring: None,
            hover_spring: Spring::new(0.0, 0.0, SpringParams::STAGE_HOVER, now),
            hovered: false,
        }
    }

    pub fn slot_at(&self, now: Duration) -> Rectangle<f64, Logical> {
        match &self.slot_spring {
            Some(s) if !s.is_done_at(now) => lerp_rect(self.slot_from, self.slot_target, s.value_at(now)),
            _ => self.slot_target,
        }
    }

    pub fn hover_at(&self, now: Duration) -> f64 {
        self.hover_spring.value_at(now).clamp(0.0, 1.15)
    }

    pub fn primary_title(&self) -> String {
        let front = self.windows.last();
        let title = front.map(|w| w.window.title()).unwrap_or_default();
        if !title.is_empty() {
            return title;
        }
        self.app_id.clone()
    }

    pub fn aspect(&self) -> f64 {
        let sz = self
            .windows
            .last()
            .map(|w| w.frame_size())
            .unwrap_or_else(|| (800.0, 600.0).into());
        (sz.w / sz.h.max(1.0)).clamp(0.6, 2.0)
    }

    fn retarget_slot(&mut self, target: Rectangle<f64, Logical>, now: Duration, animate: bool) {
        if !animate || self.slot_target.size.w < 1.0 || crate::settings::get().appearance.reduce_motion {
            self.slot_from = target;
            self.slot_target = target;
            self.slot_spring = None;
            return;
        }
        let cur = self.slot_at(now);
        let moved = (cur.loc.x - target.loc.x).abs()
            + (cur.loc.y - target.loc.y).abs()
            + (cur.size.w - target.size.w).abs()
            + (cur.size.h - target.size.h).abs();
        if moved < 1.0 {
            self.slot_target = target;
            return;
        }
        self.slot_from = cur;
        self.slot_target = target;
        self.slot_spring = Some(Spring::new(0.0, 1.0, SpringParams::STAGE, now));
    }
}

/// An active 3D perspective glide animation for one window moving between
/// centre stage and the strip (or back to desktop when disabling Stage Manager).
#[derive(Debug)]
pub struct StageGlide {
    pub window: WindowElement,
    pub snapshot: Snapshot,
    /// Window geometry at snapshot time (for exact shadow-inclusive alignment).
    pub snap_geo: Rectangle<i32, Logical>,
    pub from_frame: Rectangle<f64, Logical>,
    pub to_frame: Rectangle<f64, Logical>,
    pub from_yaw: f32,
    pub to_yaw: f32,
    pub from_alpha: f32,
    pub to_alpha: f32,
    pub spring: Spring,
    pub render_id: Id,
    pub commit: Cell<CommitCounter>,
    /// Window is already mapped in `self.space` and arriving onto centre stage:
    /// suppress drawing its live surface until this glide settles.
    pub arriving_on_stage: bool,
    /// Strip group id that this glide is flying into (suppress static thumbnail until settled).
    pub target_group_id: Option<u64>,
}

impl StageGlide {
    pub fn is_done(&self, now: Duration) -> bool {
        self.spring.is_done_at(now)
    }

    pub fn next_commit(&self) -> CommitCounter {
        let mut c = self.commit.get();
        c.increment();
        self.commit.set(c);
        c
    }
}

/// Active pointer press / drag originating from a thumbnail in the Stage Manager strip.
#[derive(Debug)]
pub struct StripDrag {
    pub output_name: String,
    pub group_id: u64,
    pub press: Point<f64, Logical>,
    pub pos: Point<f64, Logical>,
    pub slot_rect: Rectangle<f64, Logical>,
    pub grab: (f64, f64),
    pub shift_held: bool,
    pub active: bool,
    /// 0.0 = tilted in strip, 1.0 = pulled out flat toward centre stage.
    pub pull_spring: Spring,
}

impl StripDrag {
    pub fn rect_at(&self, now: Duration) -> Rectangle<f64, Logical> {
        let p = self.pull_spring.value_at(now).clamp(0.0, 1.0);
        let scale = 1.0 + 0.35 * p;
        let w = self.slot_rect.size.w * scale;
        let h = self.slot_rect.size.h * scale;
        Rectangle::new(
            (self.pos.x - self.grab.0 * w, self.pos.y - self.grab.1 * h).into(),
            (w, h).into(),
        )
    }
}

/// Stage Manager state for a single `(output_name, space_index)`.
#[derive(Debug)]
pub struct SpaceStage {
    pub active: Option<StageGroup>,
    /// Most recently used inactive stages first (index 0 = top of strip).
    pub strip: Vec<StageGroup>,
    pub glides: Vec<StageGlide>,
    /// 1.0 = strip visible, 0.0 = hidden off-screen.
    pub strip_reveal: Spring,
    /// Pointer touched the outer edge to temporarily slide in an auto-hidden strip.
    pub edge_revealed: bool,
    /// User clicked empty wallpaper to reveal desktop items.
    pub desktop_revealed: bool,
}

impl SpaceStage {
    fn new(now: Duration, show_strip: bool) -> Self {
        let v = if show_strip { 1.0 } else { 0.0 };
        Self {
            active: None,
            strip: Vec::new(),
            glides: Vec::new(),
            strip_reveal: Spring::new(v, v, SpringParams::STAGE, now),
            edge_revealed: false,
            desktop_revealed: false,
        }
    }

    pub fn is_animating(&self, now: Duration) -> bool {
        !self.strip_reveal.is_done_at(now)
            || !self.glides.is_empty()
            || self.strip.iter().any(|g| {
                !g.hover_spring.is_done_at(now)
                    || g.slot_spring.as_ref().is_some_and(|s| !s.is_done_at(now))
            })
    }

    pub fn strip_visibility(&self, now: Duration) -> f64 {
        self.strip_reveal.value_at(now).clamp(0.0, 1.0)
    }

    fn set_strip_visible(&mut self, visible: bool, now: Duration) {
        let target = if visible { 1.0 } else { 0.0 };
        if (self.strip_reveal.to - target).abs() > 0.01 {
            if crate::settings::get().appearance.reduce_motion {
                self.strip_reveal = Spring::new(target, target, SpringParams::STAGE, now);
            } else {
                self.strip_reveal.retarget(target, now);
            }
        }
    }
}

/// Compositor-wide Stage Manager state.
#[derive(Debug)]
pub struct StageManager {
    pub enabled: bool,
    next_group_id: u64,
    pub spaces: HashMap<(String, usize), SpaceStage>,
    pub strip_drag: Option<StripDrag>,
    /// Output name where a live window from centre stage is being dragged into the strip.
    pub peel_target: Option<String>,
    /// Glides playing after Stage Manager is turned OFF (`(output_name, glide)`).
    pub exit_glides: Vec<(String, StageGlide)>,
}

impl Default for StageManager {
    fn default() -> Self {
        Self {
            enabled: false,
            next_group_id: 1,
            spaces: HashMap::new(),
            strip_drag: None,
            peel_target: None,
            exit_glides: Vec::new(),
        }
    }
}

impl StageManager {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            ..Self::default()
        }
    }

    fn alloc_group_id(&mut self) -> u64 {
        let id = self.next_group_id;
        self.next_group_id += 1;
        id
    }

    /// True if any Stage Manager spring or glide needs continuous redrawing.
    pub fn is_animating(&self, now: Duration) -> bool {
        if !self.exit_glides.is_empty() {
            return true;
        }
        if let Some(drag) = &self.strip_drag {
            if !drag.pull_spring.is_done_at(now) {
                return true;
            }
        }
        if !self.enabled {
            return false;
        }
        self.spaces.values().any(|s| s.is_animating(now))
    }

    /// True if `window` is currently gliding onto centre stage (so `render.rs`
    /// hides the static live element until the glide settles).
    pub fn is_gliding_onto_stage(&self, window: &WindowElement, now: Duration) -> bool {
        if self.exit_glides.iter().any(|(_, g)| g.arriving_on_stage && g.window == *window && !g.is_done(now)) {
            return true;
        }
        if !self.enabled {
            return false;
        }
        self.spaces.values().any(|s| {
            s.glides
                .iter()
                .any(|g| g.arriving_on_stage && g.window == *window && !g.is_done(now))
        })
    }

    /// All alive windows currently parked in any Stage Manager strip across all outputs/Spaces.
    pub fn all_staged_windows(&self) -> Vec<(&str, usize, &StagedWindow)> {
        if !self.enabled {
            return Vec::new();
        }
        let mut out = Vec::new();
        for ((output, space_idx), st) in &self.spaces {
            for g in &st.strip {
                for sw in &g.windows {
                    if sw.window.alive() {
                        out.push((output.as_str(), *space_idx, sw));
                    }
                }
            }
        }
        out
    }

    /// Alive staged windows on one specific `(output, space_idx)`.
    pub fn strip_windows_for(&self, output: &str, space_idx: usize) -> Vec<(WindowElement, Point<i32, Logical>)> {
        if !self.enabled {
            return Vec::new();
        }
        let Some(st) = self.spaces.get(&(output.to_string(), space_idx)) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        // Oldest group first so bottom-to-top stacking order is preserved.
        for g in st.strip.iter().rev() {
            for sw in &g.windows {
                if sw.window.alive() {
                    out.push((sw.window.clone(), sw.saved_loc));
                }
            }
        }
        out
    }

    /// Find which `(output_name, space_idx, group_id)` holds `window` in its strip.
    pub fn find_in_strip(&self, window: &WindowElement) -> Option<(String, usize, u64)> {
        if !self.enabled {
            return None;
        }
        for ((output, space_idx), st) in &self.spaces {
            for g in &st.strip {
                if g.windows.iter().any(|sw| sw.window == *window) {
                    return Some((output.clone(), *space_idx, g.id));
                }
            }
        }
        None
    }

    /// Find a strip group containing `app_id` on any output's active Space (or any Space).
    pub fn find_app_in_strip(&self, app_id: &str, active_spaces: &HashMap<String, usize>) -> Option<(String, usize, u64)> {
        if !self.enabled {
            return None;
        }
        // Prefer an output's currently active Space
        for ((output, space_idx), st) in &self.spaces {
            let active = active_spaces.get(output).copied().unwrap_or(0);
            if *space_idx != active {
                continue;
            }
            if let Some(g) = st
                .strip
                .iter()
                .find(|g| g.windows.iter().any(|sw| sw.window.alive() && sw.window.app_id() == app_id))
            {
                return Some((output.clone(), *space_idx, g.id));
            }
        }
        // Fallback: any inactive Space
        for ((output, space_idx), st) in &self.spaces {
            if let Some(g) = st
                .strip
                .iter()
                .find(|g| g.windows.iter().any(|sw| sw.window.alive() && sw.window.app_id() == app_id))
            {
                return Some((output.clone(), *space_idx, g.id));
            }
        }
        None
    }
}

// ---- Geometry helpers --------------------------------------------------------

/// Stage Manager sits on the left edge unless the Dock is on the left, matching macOS.
pub fn strip_on_right() -> bool {
    crate::settings::get().dock.position == DockPosition::Left
}

/// Yaw angle (radians) for resting thumbnails on the current strip side.
pub fn strip_rest_yaw() -> f32 {
    if strip_on_right() { -REST_YAW_RAD } else { REST_YAW_RAD }
}

/// Yaw angle (radians) for hovered thumbnails on the current strip side.
pub fn strip_hover_yaw() -> f32 {
    if strip_on_right() { -HOVER_YAW_RAD } else { HOVER_YAW_RAD }
}

/// Width of the Stage Manager strip on an output (logical px).
pub fn strip_width(output_geo: Rectangle<i32, Logical>) -> f64 {
    (output_geo.size.w as f64 * 0.115).clamp(138.0, 188.0)
}

/// Global logical rect of the Stage Manager strip zone on an output (inside usable area).
pub fn strip_zone(output_geo: Rectangle<i32, Logical>, usable: Rectangle<i32, Logical>) -> Rectangle<f64, Logical> {
    let w = strip_width(output_geo).min(usable.size.w as f64 * 0.35);
    let x = if strip_on_right() {
        (usable.loc.x + usable.size.w) as f64 - w
    } else {
        usable.loc.x as f64
    };
    Rectangle::new((x, usable.loc.y as f64).into(), (w, usable.size.h.max(1) as f64).into())
}

/// Maximum number of recent groups shown simultaneously in the strip for a given strip height.
pub fn max_visible_groups(strip_h: f64) -> usize {
    ((strip_h - 32.0) / 136.0).floor().clamp(4.0, 6.0) as usize
}

/// Compute global logical slot rectangles for the strip groups (index 0 = top).
pub fn compute_strip_slots(
    zone: Rectangle<f64, Logical>,
    aspects: &[f64],
    on_right: bool,
) -> Vec<Rectangle<f64, Logical>> {
    let n = aspects.len();
    if n == 0 {
        return Vec::new();
    }
    let visible = max_visible_groups(zone.size.h).min(n);
    let pad_y = 20.0;
    let avail_h = (zone.size.h - 2.0 * pad_y).max(60.0);
    let max_w = (zone.size.w - 2.0 * STRIP_MARGIN_X).max(64.0);
    let max_h = ((avail_h - (visible.saturating_sub(1) as f64) * GROUP_GAP) / visible.max(1) as f64)
        .clamp(56.0, 124.0);

    let mut sizes: Vec<(f64, f64)> = Vec::with_capacity(visible);
    for &asp in aspects.iter().take(visible) {
        let a = asp.clamp(0.6, 2.0);
        let (mut w, mut h) = (max_w, max_w / a);
        if h > max_h {
            h = max_h;
            w = h * a;
        }
        sizes.push((w, h));
    }

    let total_h: f64 = sizes.iter().map(|(_, h)| *h).sum::<f64>()
        + (visible.saturating_sub(1) as f64) * GROUP_GAP;
    let mut y = zone.loc.y + ((zone.size.h - total_h) * 0.5).max(pad_y);
    let mut out = Vec::with_capacity(n);
    for (w, h) in sizes {
        let x = if on_right {
            zone.loc.x + zone.size.w - STRIP_MARGIN_X - w
        } else {
            zone.loc.x + STRIP_MARGIN_X
        };
        out.push(Rectangle::new((x, y).into(), (w, h).into()));
        y += h + GROUP_GAP;
    }
    // Overflow groups sit just below the bottom of the strip (not rendered while hidden)
    let off_y = zone.loc.y + zone.size.h + 24.0;
    for _ in visible..n {
        let x = if on_right {
            zone.loc.x + zone.size.w - STRIP_MARGIN_X - max_w
        } else {
            zone.loc.x + STRIP_MARGIN_X
        };
        out.push(Rectangle::new((x, off_y).into(), (max_w, max_h * 0.8).into()));
    }
    out
}

/// Map a window's frame rect (without shadow) to the full snapshot quad rect
/// (including its proportional drop shadow from `bbox`).
pub fn snapshot_quad_for_frame(
    frame: Rectangle<f64, Logical>,
    geo: Rectangle<i32, Logical>,
    bbox: Rectangle<i32, Logical>,
) -> Rectangle<f64, Logical> {
    let sx = frame.size.w / geo.size.w.max(1) as f64;
    let sy = frame.size.h / geo.size.h.max(1) as f64;
    let off_x = (bbox.loc.x - geo.loc.x) as f64;
    let off_y = (bbox.loc.y - geo.loc.y) as f64;
    Rectangle::new(
        (frame.loc.x + off_x * sx, frame.loc.y + off_y * sy).into(),
        (bbox.size.w as f64 * sx, bbox.size.h as f64 * sy).into(),
    )
}

fn rect_overlaps(a: Rectangle<f64, Logical>, b: Rectangle<f64, Logical>) -> bool {
    a.loc.x < b.loc.x + b.size.w
        && a.loc.x + a.size.w > b.loc.x
        && a.loc.y < b.loc.y + b.size.h
        && a.loc.y + a.size.h > b.loc.y
}

fn window_has_parent(window: &WindowElement) -> bool {
    if let Some(t) = window.0.toplevel() {
        return t.parent().is_some();
    }
    #[cfg(feature = "xwayland")]
    if let Some(x) = window.0.x11_surface() {
        return x.is_transient_for().is_some();
    }
    false
}

/// Frame rect of a window in global logical coordinates, given its space location.
fn frame_rect(window: &WindowElement, loc: Point<i32, Logical>) -> Rectangle<f64, Logical> {
    Rectangle::new(loc, window.geometry().size).to_f64()
}

// ---- Compositor integration on OrchardState ---------------------------------

impl<BackendData: Backend + 'static> OrchardState<BackendData> {
    /// Usable area of `output` (excludes menu bar and Dock exclusive zones).
    pub(crate) fn output_usable_area(&self, output: &Output) -> Option<Rectangle<i32, Logical>> {
        let geo = self.space.output_geometry(output)?;
        let zone = layer_map_for_output(output).non_exclusive_zone();
        Some(Rectangle::new(geo.loc + zone.loc, zone.size))
    }

    /// Usable area for placing new windows: when Stage Manager is enabled and
    /// `show_recent_apps` is on, reserve the strip width so freshly opened
    /// windows naturally frame in centre stage beside the strip.
    pub fn stage_placement_area(&self, output: &Output) -> Option<Rectangle<i32, Logical>> {
        let mut area = self.output_usable_area(output)?;
        let cfg = crate::settings::get();
        if self.stage.enabled && cfg.stage_manager.show_recent_apps {
            let geo = self.space.output_geometry(output)?;
            let sw = strip_width(geo).round() as i32;
            if area.size.w > sw + 320 {
                if strip_on_right() {
                    area.size.w -= sw;
                } else {
                    area.loc.x += sw;
                    area.size.w -= sw;
                }
            }
        }
        Some(area)
    }

    fn output_by_name(&self, name: &str) -> Option<Output> {
        self.space.outputs().find(|o| o.name() == name).cloned()
    }

    fn active_space_map(&self) -> HashMap<String, usize> {
        self.space
            .outputs()
            .map(|o| {
                let n = o.name();
                let (a, _) = self.spaces.info(&n);
                (n, a)
            })
            .collect()
    }

    fn capture_stage_snapshot(&mut self, window: &WindowElement) -> Option<Snapshot> {
        let scale = self
            .window_output(window)
            .or_else(|| self.primary_output())
            .map(|o| o.current_scale().fractional_scale())
            .unwrap_or(1.0);
        // Slightly cap snapshot scale for very large windows? Keep output scale so
        // glides at full size stay razor sharp and match the live window 1:1.
        self.backend_data
            .with_renderer(|r| snapshot_window(r, window, scale))
            .flatten()
    }

    /// Enable Stage Manager: keep the focused/topmost group on centre stage on
    /// each display and glide all other windows into the recent strip.
    pub fn stage_manager_enable(&mut self) {
        if self.stage.enabled {
            return;
        }
        self.stage.enabled = true;
        self.stage.exit_glides.clear();
        self.stage.strip_drag = None;
        self.stage.peel_target = None;
        let outputs: Vec<Output> = self.space.outputs().cloned().collect();
        for output in outputs {
            self.stage_organize_output(&output, true);
        }
        tracing::info!("Stage Manager enabled");
    }

    /// Disable Stage Manager: glide all staged windows back to their desktop
    /// positions and map them back into `self.space` (or their inactive Spaces).
    pub fn stage_manager_disable(&mut self) {
        if !self.stage.enabled {
            return;
        }
        let now: Duration = self.clock.now().into();
        let reduce_motion = crate::settings::get().appearance.reduce_motion;
        let yaw = strip_rest_yaw();
        self.stage.enabled = false;
        self.stage.strip_drag = None;
        self.stage.peel_target = None;

        let all_spaces = std::mem::take(&mut self.stage.spaces);
        for ((output_name, space_idx), mut st) in all_spaces {
            let (active_idx, _) = self.spaces.info(&output_name);
            let is_active_space = space_idx == active_idx;
            if is_active_space {
                // Remember active windows to re-raise them on top of restored strip windows
                let active_wins: Vec<WindowElement> = st
                    .active
                    .take()
                    .map(|g| g.windows.into_iter().map(|sw| sw.window).filter(|w| w.alive()).collect())
                    .unwrap_or_default();

                let visible_count = self
                    .output_by_name(&output_name)
                    .and_then(|o| self.output_usable_area(&o))
                    .map(|u| max_visible_groups(u.size.h as f64))
                    .unwrap_or(5);

                // Restore strip groups oldest-first so MRU stacking is preserved
                for (rev_i, g) in st.strip.into_iter().enumerate().rev() {
                    let slot = g.slot_at(now);
                    let in_visible = rev_i < visible_count;
                    for sw in g.windows {
                        if !sw.window.alive() {
                            continue;
                        }
                        self.space.map_element(sw.window.clone(), sw.saved_loc, false);
                        if !reduce_motion && in_visible && slot.size.w > 1.0 {
                            if let Some(snap) = sw.snapshot {
                                let to_frame = frame_rect(&sw.window, sw.saved_loc);
                                self.stage.exit_glides.push((
                                    output_name.clone(),
                                    StageGlide {
                                        window: sw.window.clone(),
                                        snap_geo: sw.window.geometry(),
                                        snapshot: snap,
                                        from_frame: slot,
                                        to_frame,
                                        from_yaw: yaw,
                                        to_yaw: 0.0,
                                        from_alpha: 1.0,
                                        to_alpha: 1.0,
                                        spring: Spring::new(0.0, 1.0, SpringParams::STAGE, now),
                                        render_id: Id::new(),
                                        commit: Cell::new(CommitCounter::default()),
                                        arriving_on_stage: true,
                                        target_group_id: None,
                                    },
                                ));
                            }
                        }
                    }
                }
                for w in &active_wins {
                    self.space.raise_element(w, false);
                }
                if let Some(top) = active_wins.last() {
                    self.focus_window_internal(top);
                } else {
                    self.focus_topmost();
                }
            } else {
                // Inactive Space: put staged windows back into that SpaceSlot
                let spaces = self.spaces.for_output(&output_name);
                if let Some(slot) = spaces.slots.get_mut(space_idx) {
                    for g in st.strip.into_iter().rev() {
                        for sw in g.windows {
                            if sw.window.alive() && !slot.windows.iter().any(|(w, _)| *w == sw.window) {
                                slot.windows.push((sw.window, sw.saved_loc));
                            }
                        }
                    }
                }
            }
        }
        self.space.refresh();
        for o in self.space.outputs().cloned().collect::<Vec<_>>() {
            self.backend_data.reset_buffers(&o);
        }
        tracing::info!("Stage Manager disabled");
    }

    /// Apply Stage Manager settings changes (called from `apply_settings`).
    pub fn stage_apply_settings(&mut self, cfg: &orchard_config::StageManagerSettings) {
        if cfg.enabled && !self.stage.enabled {
            self.stage_manager_enable();
        } else if !cfg.enabled && self.stage.enabled {
            self.stage_manager_disable();
        } else if self.stage.enabled {
            // Re-layout strips (Dock side or show_recent_apps may have changed)
            let outputs: Vec<Output> = self.space.outputs().cloned().collect();
            for output in outputs {
                self.stage_relayout_output(&output, true);
                self.stage_update_autohide(&output);
            }
        }
    }

    /// Organize currently mapped windows on `output`'s active Space into Stage Manager groups.
    pub fn stage_organize_output(&mut self, output: &Output, animate: bool) {
        if !self.stage.enabled {
            return;
        }
        let now: Duration = self.clock.now().into();
        let name = output.name();
        let (space_idx, _) = self.spaces.info(&name);
        let cfg = crate::settings::get().stage_manager.clone();
        let reduce_motion = crate::settings::get().appearance.reduce_motion;

        let mapped = self.windows_on_output(output);
        let non_fs: Vec<(WindowElement, Point<i32, Logical>)> = mapped
            .into_iter()
            .filter(|(w, _)| w.alive() && !w.is_fullscreen())
            .collect();

        if non_fs.is_empty() {
            let key = (name.clone(), space_idx);
            self.stage
                .spaces
                .entry(key)
                .or_insert_with(|| SpaceStage::new(now, cfg.show_recent_apps));
            self.stage_relayout_output(output, animate);
            self.stage_update_autohide(output);
            return;
        }

        // Focused window on this output (or topmost window) defines the active stage
        let focused = self.focused_window();
        let front = focused
            .filter(|f| non_fs.iter().any(|(w, _)| w == f))
            .unwrap_or_else(|| non_fs.last().unwrap().0.clone());

        // Group windows in MRU order (topmost first)
        let mut grouped: Vec<Vec<(WindowElement, Point<i32, Logical>)>> = Vec::new();
        match cfg.app_windows {
            StageAppWindows::AllAtOnce => {
                // Active app first
                let active_app = front.app_id();
                let active_wins: Vec<_> = non_fs
                    .iter()
                    .filter(|(w, _)| w.app_id() == active_app)
                    .cloned()
                    .collect();
                grouped.push(active_wins);
                // Other apps in MRU order (walk from top of stack down)
                let mut seen_apps = vec![active_app];
                for (w, _) in non_fs.iter().rev() {
                    let app = w.app_id();
                    if seen_apps.contains(&app) {
                        continue;
                    }
                    seen_apps.push(app.clone());
                    let wins: Vec<_> = non_fs
                        .iter()
                        .filter(|(x, _)| x.app_id() == app)
                        .cloned()
                        .collect();
                    grouped.push(wins);
                }
            }
            StageAppWindows::OneAtATime => {
                let mut rest = Vec::new();
                for item in non_fs.iter().rev() {
                    if item.0 == front {
                        grouped.insert(0, vec![item.clone()]);
                    } else {
                        rest.push(vec![item.clone()]);
                    }
                }
                grouped.extend(rest);
            }
        }

        let mut iter = grouped.into_iter();
        let active_raw = iter.next().unwrap_or_default();
        let active_id = self.stage.alloc_group_id();
        let active_group = StageGroup::new(
            active_id,
            active_raw
                .into_iter()
                .map(|(w, loc)| StagedWindow::new(w, loc, None, now))
                .collect(),
            now,
        );

        let mut strip_groups = Vec::new();
        let mut pending_glides: Vec<(WindowElement, Snapshot, Rectangle<i32, Logical>, Rectangle<f64, Logical>, u64)> =
            Vec::new();
        for group_wins in iter {
            let gid = self.stage.alloc_group_id();
            let mut staged = Vec::new();
            for (w, loc) in group_wins {
                let snap = self.capture_stage_snapshot(&w);
                let geo = w.geometry();
                let real = frame_rect(&w, loc);
                self.space.unmap_elem(&w);
                if animate && !reduce_motion {
                    if let Some(s) = snap.clone() {
                        pending_glides.push((w.clone(), s, geo, real, gid));
                    }
                }
                staged.push(StagedWindow::new(w, loc, snap, now));
            }
            strip_groups.push(StageGroup::new(gid, staged, now));
        }

        let key = (name.clone(), space_idx);
        let st = self
            .stage
            .spaces
            .entry(key)
            .or_insert_with(|| SpaceStage::new(now, cfg.show_recent_apps));
        st.active = Some(active_group);
        // Merge any pre-existing strip groups (e.g. when returning to a Space)
        for old in std::mem::take(&mut st.strip) {
            let alive: Vec<_> = old.windows.into_iter().filter(|sw| sw.window.alive()).collect();
            if !alive.is_empty() {
                let gid = old.id;
                let mut g = StageGroup::new(gid, alive, now);
                g.slot_from = old.slot_from;
                g.slot_target = old.slot_target;
                strip_groups.push(g);
            }
        }
        st.strip = strip_groups;
        st.desktop_revealed = false;

        self.stage_relayout_output(output, false);

        // Now attach glides to their computed strip slot targets
        if !pending_glides.is_empty() {
            let yaw = strip_rest_yaw();
            if let Some(st) = self.stage.spaces.get_mut(&(name, space_idx)) {
                for (w, snap, snap_geo, from_frame, gid) in pending_glides {
                    if let Some(g) = st.strip.iter().find(|g| g.id == gid) {
                        st.glides.push(StageGlide {
                            window: w,
                            snapshot: snap,
                            snap_geo,
                            from_frame,
                            to_frame: g.slot_target,
                            from_yaw: 0.0,
                            to_yaw: yaw,
                            from_alpha: 1.0,
                            to_alpha: 1.0,
                            spring: Spring::new(0.0, 1.0, SpringParams::STAGE, now),
                            render_id: Id::new(),
                            commit: Cell::new(CommitCounter::default()),
                            arriving_on_stage: false,
                            target_group_id: Some(gid),
                        });
                    }
                }
            }
        }

        self.stage_update_autohide(output);
        self.focus_window_internal(&front);
    }

    /// Recompute strip thumbnail slot rects on `output`'s active Space.
    pub fn stage_relayout_output(&mut self, output: &Output, animate: bool) {
        if !self.stage.enabled {
            return;
        }
        let Some(geo) = self.space.output_geometry(output) else { return };
        let Some(usable) = self.output_usable_area(output) else { return };
        let zone = strip_zone(geo, usable);
        let on_right = strip_on_right();
        let now: Duration = self.clock.now().into();
        let name = output.name();
        let (space_idx, _) = self.spaces.info(&name);
        let Some(st) = self.stage.spaces.get_mut(&(name, space_idx)) else {
            return;
        };
        let aspects: Vec<f64> = st.strip.iter().map(|g| g.aspect()).collect();
        let slots = compute_strip_slots(zone, &aspects, on_right);
        for (g, slot) in st.strip.iter_mut().zip(slots) {
            g.retarget_slot(slot, now, animate);
        }
    }

    /// Re-evaluate strip auto-hide for `output`'s active Space.
    pub fn stage_update_autohide(&mut self, output: &Output) {
        if !self.stage.enabled {
            return;
        }
        let Some(geo) = self.space.output_geometry(output) else { return };
        let Some(usable) = self.output_usable_area(output) else { return };
        let zone = strip_zone(geo, usable);
        let cfg = crate::settings::get().stage_manager.clone();
        let now: Duration = self.clock.now().into();
        let name = output.name();
        let (space_idx, _) = self.spaces.info(&name);

        // Slightly inset the inner edge of the strip zone by 8px so a window parked
        // neatly right next to the strip doesn't accidentally trigger auto-hide.
        let inner_inset = 8.0;
        let overlap_zone = if strip_on_right() {
            Rectangle::new(
                (zone.loc.x + inner_inset, zone.loc.y).into(),
                ((zone.size.w - inner_inset).max(1.0), zone.size.h).into(),
            )
        } else {
            Rectangle::new(
                zone.loc,
                ((zone.size.w - inner_inset).max(1.0), zone.size.h).into(),
            )
        };

        let active_overlaps = self.windows_on_output(output).into_iter().any(|(w, loc)| {
            w.alive() && !w.is_fullscreen() && rect_overlaps(frame_rect(&w, loc), overlap_zone)
        });

        let peeling_here = self.stage.peel_target.as_deref() == Some(name.as_str());
        let dragging_here = self
            .stage
            .strip_drag
            .as_ref()
            .is_some_and(|d| d.output_name == name);

        let Some(st) = self.stage.spaces.get_mut(&(name, space_idx)) else {
            return;
        };
        let wants_visible = if st.strip.is_empty() && !peeling_here {
            false
        } else if st.edge_revealed || peeling_here || dragging_here || st.desktop_revealed {
            true
        } else if !cfg.show_recent_apps {
            false
        } else {
            !active_overlaps
        };
        st.set_strip_visible(wants_visible, now);
    }

    /// Called when a newly created window has been placed on screen (`maybe_place_initially`).
    pub fn stage_on_new_window(&mut self, window: &WindowElement) {
        if !self.stage.enabled || window.is_fullscreen() {
            return;
        }
        let Some(output) = self.window_output(window).or_else(|| self.active_output()) else {
            return;
        };
        let Some(loc) = self.space.element_location(window) else { return };
        let name = output.name();
        let (space_idx, _) = self.spaces.info(&name);
        let now: Duration = self.clock.now().into();
        let cfg = crate::settings::get().stage_manager.clone();

        // Check if this window belongs with the current active stage
        let joins_active = {
            let st = self
                .stage
                .spaces
                .entry((name.clone(), space_idx))
                .or_insert_with(|| SpaceStage::new(now, cfg.show_recent_apps));
            st.desktop_revealed = false;
            match &st.active {
                None => true,
                Some(active) => {
                    window_has_parent(window)
                        || (cfg.app_windows == StageAppWindows::AllAtOnce
                            && active.windows.iter().any(|sw| sw.window.alive() && sw.window.app_id() == window.app_id()))
                }
            }
        };

        if joins_active {
            let gid = self.stage.alloc_group_id();
            let st = self
                .stage
                .spaces
                .entry((name.clone(), space_idx))
                .or_insert_with(|| SpaceStage::new(now, cfg.show_recent_apps));
            match st.active.as_mut() {
                Some(active) => {
                    if !active.windows.iter().any(|sw| sw.window == *window) {
                        active.windows.push(StagedWindow::new(window.clone(), loc, None, now));
                        active.app_id = window.app_id();
                    }
                }
                None => {
                    st.active = Some(StageGroup::new(
                        gid,
                        vec![StagedWindow::new(window.clone(), loc, None, now)],
                        now,
                    ));
                }
            }
            self.stage_update_autohide(&output);
            return;
        }

        // Different app (or OneAtATime): move current active stage to strip[0],
        // and if AllAtOnce + an existing strip group has this app_id, bring it onto stage too!
        let existing_gid = if cfg.app_windows == StageAppWindows::AllAtOnce {
            self.stage
                .spaces
                .get(&(name.clone(), space_idx))
                .and_then(|st| {
                    st.strip
                        .iter()
                        .find(|g| g.windows.iter().any(|sw| sw.window.alive() && sw.window.app_id() == window.app_id()))
                        .map(|g| g.id)
                })
        } else {
            None
        };

        self.stage_park_active(&output, true);

        if let Some(gid) = existing_gid {
            self.stage_switch_to_group(&output, gid, false);
            if let Some(st) = self.stage.spaces.get_mut(&(name.clone(), space_idx)) {
                if let Some(active) = st.active.as_mut() {
                    if !active.windows.iter().any(|sw| sw.window == *window) {
                        active.windows.push(StagedWindow::new(window.clone(), loc, None, now));
                    }
                }
            }
            self.space.raise_element(window, true);
        } else {
            let gid = self.stage.alloc_group_id();
            if let Some(st) = self.stage.spaces.get_mut(&(name, space_idx)) {
                st.active = Some(StageGroup::new(
                    gid,
                    vec![StagedWindow::new(window.clone(), loc, None, now)],
                    now,
                ));
                st.desktop_revealed = false;
            }
        }
        self.stage_relayout_output(&output, true);
        self.stage_update_autohide(&output);
        self.focus_window_internal(window);
    }

    /// Move the current active stage on `output`'s active Space into `strip[0]`.
    fn stage_park_active(&mut self, output: &Output, animate: bool) {
        let name = output.name();
        let (space_idx, _) = self.spaces.info(&name);
        let now: Duration = self.clock.now().into();
        let reduce_motion = crate::settings::get().appearance.reduce_motion;

        let active_group = self
            .stage
            .spaces
            .get_mut(&(name.clone(), space_idx))
            .and_then(|st| st.active.take());
        let Some(mut group) = active_group else { return };

        // Preserve live stacking order (bottom-to-top) so the frontmost window on stage
        // becomes the frontmost card in the strip and restores on top when switched back.
        let order: Vec<WindowElement> = self.space.elements().cloned().collect();
        group.windows.sort_by_key(|sw| {
            order
                .iter()
                .position(|w| w == &sw.window)
                .unwrap_or(usize::MAX)
        });

        // Retain alive windows, refresh their saved locations + capture snapshots
        let mut parked_wins = Vec::new();
        let mut pending_glides = Vec::new();
        for mut sw in group.windows {
            if !sw.window.alive() {
                continue;
            }
            if sw.window.is_fullscreen() {
                continue;
            }
            if let Some(loc) = self.space.element_location(&sw.window) {
                sw.saved_loc = loc;
            }
            sw.saved_size = sw.window.geometry().size;
            let snap = self.capture_stage_snapshot(&sw.window).or(sw.snapshot.take());
            let geo = sw.window.geometry();
            let from_frame = frame_rect(&sw.window, sw.saved_loc);
            self.space.unmap_elem(&sw.window);
            if animate && !reduce_motion {
                if let Some(s) = snap.clone() {
                    pending_glides.push((sw.window.clone(), s, geo, from_frame));
                }
            }
            sw.snapshot = snap;
            sw.last_snap = now;
            parked_wins.push(sw);
        }

        if parked_wins.is_empty() {
            return;
        }
        group.app_id = parked_wins.last().map(|w| w.window.app_id()).unwrap_or_default();
        group.windows = parked_wins;
        group.hovered = false;
        group.hover_spring = Spring::new(0.0, 0.0, SpringParams::STAGE_HOVER, now);
        let gid = group.id;

        if let Some(st) = self.stage.spaces.get_mut(&(name.clone(), space_idx)) {
            st.strip.insert(0, group);
        }
        self.stage_relayout_output(output, animate);

        if !pending_glides.is_empty() {
            let yaw = strip_rest_yaw();
            if let Some(st) = self.stage.spaces.get_mut(&(name, space_idx)) {
                let target_slot = st
                    .strip
                    .iter()
                    .find(|g| g.id == gid)
                    .map(|g| g.slot_target)
                    .unwrap_or_default();
                for (w, snap, snap_geo, from_frame) in pending_glides {
                    st.glides.push(StageGlide {
                        window: w,
                        snapshot: snap,
                        snap_geo,
                        from_frame,
                        to_frame: target_slot,
                        from_yaw: 0.0,
                        to_yaw: yaw,
                        from_alpha: 1.0,
                        to_alpha: 1.0,
                        spring: Spring::new(0.0, 1.0, SpringParams::STAGE, now),
                        render_id: Id::new(),
                        commit: Cell::new(CommitCounter::default()),
                        arriving_on_stage: false,
                        target_group_id: Some(gid),
                    });
                }
            }
        }
    }

    /// Switch centre stage on `output` to strip group `group_id`.
    pub fn stage_switch_to_group(&mut self, output: &Output, group_id: u64, park_current: bool) {
        if !self.stage.enabled {
            return;
        }
        let name = output.name();
        let (space_idx, _) = self.spaces.info(&name);
        let now: Duration = self.clock.now().into();
        let reduce_motion = crate::settings::get().appearance.reduce_motion;

        // Grab the target group's current visual slot BEFORE parking modifies the strip
        let target_slot = self
            .stage
            .spaces
            .get(&(name.clone(), space_idx))
            .and_then(|st| st.strip.iter().find(|g| g.id == group_id))
            .map(|g| g.slot_at(now))
            .unwrap_or_default();
        let from_yaw = self
            .stage
            .spaces
            .get(&(name.clone(), space_idx))
            .and_then(|st| st.strip.iter().find(|g| g.id == group_id))
            .map(|g| {
                let h = g.hover_at(now) as f32;
                strip_rest_yaw() + (strip_hover_yaw() - strip_rest_yaw()) * h
            })
            .unwrap_or_else(strip_rest_yaw);

        // Extract the target group out of the strip first so it doesn't shift while parking
        let Some(mut incoming) = self
            .stage
            .spaces
            .get_mut(&(name.clone(), space_idx))
            .and_then(|st| {
                let pos = st.strip.iter().position(|g| g.id == group_id)?;
                Some(st.strip.remove(pos))
            })
        else {
            return;
        };

        if park_current {
            self.stage_park_active(output, true);
        }

        // Map incoming windows back into `self.space` and launch arriving glides
        let mut alive_wins = Vec::new();
        let mut arriving_glides = Vec::new();
        for sw in incoming.windows {
            if !sw.window.alive() {
                continue;
            }
            self.space.map_element(sw.window.clone(), sw.saved_loc, false);
            if !reduce_motion && target_slot.size.w > 1.0 {
                if let Some(snap) = sw.snapshot.clone() {
                    let to_frame = frame_rect(&sw.window, sw.saved_loc);
                    arriving_glides.push(StageGlide {
                        window: sw.window.clone(),
                        snap_geo: sw.window.geometry(),
                        snapshot: snap,
                        from_frame: target_slot,
                        to_frame,
                        from_yaw,
                        to_yaw: 0.0,
                        from_alpha: 1.0,
                        to_alpha: 1.0,
                        spring: Spring::new(0.0, 1.0, SpringParams::STAGE, now),
                        render_id: Id::new(),
                        commit: Cell::new(CommitCounter::default()),
                        arriving_on_stage: true,
                        target_group_id: None,
                    });
                }
            }
            alive_wins.push(sw);
        }

        let top_win = alive_wins.last().map(|sw| sw.window.clone());
        incoming.windows = alive_wins;
        incoming.hovered = false;
        incoming.hover_spring = Spring::new(0.0, 0.0, SpringParams::STAGE_HOVER, now);

        if let Some(st) = self.stage.spaces.get_mut(&(name, space_idx)) {
            st.glides.retain(|g| g.target_group_id != Some(group_id));
            st.glides.extend(arriving_glides);
            st.active = if incoming.windows.is_empty() { None } else { Some(incoming) };
            st.desktop_revealed = false;
            st.edge_revealed = false;
        }

        self.space.refresh();
        self.stage_relayout_output(output, true);
        self.stage_update_autohide(output);
        if let Some(top) = top_win {
            self.focus_window_internal(&top);
        }
    }

    /// Combine strip group `group_id` into the current active stage (Shift+Click
    /// or dragging a thumbnail out of the strip onto centre stage).
    pub fn stage_merge_group_into_active(
        &mut self,
        output: &Output,
        group_id: u64,
        from_rect_override: Option<Rectangle<f64, Logical>>,
        drop_center: Option<Point<f64, Logical>>,
    ) {
        if !self.stage.enabled {
            return;
        }
        let name = output.name();
        let (space_idx, _) = self.spaces.info(&name);
        let now: Duration = self.clock.now().into();
        let reduce_motion = crate::settings::get().appearance.reduce_motion;

        let has_active = self
            .stage
            .spaces
            .get(&(name.clone(), space_idx))
            .and_then(|st| st.active.as_ref())
            .is_some_and(|g| !g.windows.is_empty());
        if !has_active {
            self.stage_switch_to_group(output, group_id, false);
            return;
        }

        let Some((incoming, start_slot)) = self
            .stage
            .spaces
            .get_mut(&(name.clone(), space_idx))
            .and_then(|st| {
                let pos = st.strip.iter().position(|g| g.id == group_id)?;
                let g = st.strip.remove(pos);
                let slot = from_rect_override.unwrap_or_else(|| g.slot_at(now));
                Some((g, slot))
            })
        else {
            return;
        };

        let usable = self.output_usable_area(output);
        let multi = incoming.windows.len() > 1;
        let from_yaw = if from_rect_override.is_some() { 0.0 } else { strip_rest_yaw() };

        let mut added_wins = Vec::new();
        let mut glides = Vec::new();
        for mut sw in incoming.windows {
            if !sw.window.alive() {
                continue;
            }
            // If a single window was dragged onto centre stage, place it around the drop spot
            if !multi {
                if let (Some(dc), Some(u)) = (drop_center, usable) {
                    let sz = sw.window.geometry().size;
                    let x = (dc.x - sz.w as f64 * 0.5)
                        .clamp(u.loc.x as f64, (u.loc.x + u.size.w - sz.w).max(u.loc.x) as f64)
                        .round() as i32;
                    let y = (dc.y - sz.h as f64 * 0.5)
                        .clamp(u.loc.y as f64, (u.loc.y + u.size.h - sz.h).max(u.loc.y) as f64)
                        .round() as i32;
                    sw.saved_loc = (x, y).into();
                }
            }
            self.space.map_element(sw.window.clone(), sw.saved_loc, false);
            if !reduce_motion && start_slot.size.w > 1.0 {
                if let Some(snap) = sw.snapshot.clone() {
                    let to_frame = frame_rect(&sw.window, sw.saved_loc);
                    glides.push(StageGlide {
                        window: sw.window.clone(),
                        snap_geo: sw.window.geometry(),
                        snapshot: snap,
                        from_frame: start_slot,
                        to_frame,
                        from_yaw,
                        to_yaw: 0.0,
                        from_alpha: 1.0,
                        to_alpha: 1.0,
                        spring: Spring::new(0.0, 1.0, SpringParams::STAGE, now),
                        render_id: Id::new(),
                        commit: Cell::new(CommitCounter::default()),
                        arriving_on_stage: true,
                        target_group_id: None,
                    });
                }
            }
            added_wins.push(sw);
        }

        let top_win = added_wins.last().map(|sw| sw.window.clone());
        if let Some(st) = self.stage.spaces.get_mut(&(name, space_idx)) {
            st.glides.retain(|g| g.target_group_id != Some(group_id));
            st.glides.extend(glides);
            if let Some(active) = st.active.as_mut() {
                for sw in added_wins {
                    if !active.windows.iter().any(|x| x.window == sw.window) {
                        active.windows.push(sw);
                    }
                }
                if let Some(last) = active.windows.last() {
                    active.app_id = last.window.app_id();
                }
            }
            st.desktop_revealed = false;
            st.edge_revealed = false;
        }

        self.space.refresh();
        self.stage_relayout_output(output, true);
        self.stage_update_autohide(output);
        if let Some(top) = top_win {
            self.focus_window_internal(&top);
        }
    }

    /// Separate one window out of a multi-window active stage and glide it into the strip.
    pub fn stage_peel_window_to_strip(&mut self, window: &WindowElement, output: &Output) -> bool {
        if !self.stage.enabled || !window.alive() {
            return false;
        }
        let name = output.name();
        let (space_idx, _) = self.spaces.info(&name);
        let now: Duration = self.clock.now().into();
        let reduce_motion = crate::settings::get().appearance.reduce_motion;

        let can_peel = self
            .stage
            .spaces
            .get(&(name.clone(), space_idx))
            .and_then(|st| st.active.as_ref())
            .is_some_and(|g| g.windows.len() >= 2 && g.windows.iter().any(|sw| sw.window == *window));
        if !can_peel {
            return false;
        }

        let loc = self.space.element_location(window).unwrap_or_default();
        let snap = self.capture_stage_snapshot(window);
        let geo = window.geometry();
        let from_frame = frame_rect(window, loc);
        self.space.unmap_elem(window);

        let gid = self.stage.alloc_group_id();
        let sw = StagedWindow::new(window.clone(), loc, snap.clone(), now);
        let new_group = StageGroup::new(gid, vec![sw], now);

        let next_focus = if let Some(st) = self.stage.spaces.get_mut(&(name.clone(), space_idx)) {
            if let Some(active) = st.active.as_mut() {
                active.windows.retain(|x| x.window != *window);
                if let Some(last) = active.windows.last() {
                    active.app_id = last.window.app_id();
                }
            }
            st.strip.insert(0, new_group);
            st.active
                .as_ref()
                .and_then(|a| a.windows.last())
                .map(|sw| sw.window.clone())
        } else {
            None
        };

        self.stage_relayout_output(output, true);

        if !reduce_motion {
            if let Some(snap) = snap {
                let yaw = strip_rest_yaw();
                if let Some(st) = self.stage.spaces.get_mut(&(name, space_idx)) {
                    let target_slot = st
                        .strip
                        .iter()
                        .find(|g| g.id == gid)
                        .map(|g| g.slot_target)
                        .unwrap_or_default();
                    st.glides.push(StageGlide {
                        window: window.clone(),
                        snapshot: snap,
                        snap_geo: geo,
                        from_frame,
                        to_frame: target_slot,
                        from_yaw: 0.0,
                        to_yaw: yaw,
                        from_alpha: 1.0,
                        to_alpha: 1.0,
                        spring: Spring::new(0.0, 1.0, SpringParams::STAGE, now),
                        render_id: Id::new(),
                        commit: Cell::new(CommitCounter::default()),
                        arriving_on_stage: false,
                        target_group_id: Some(gid),
                    });
                }
            }
        }

        self.stage_update_autohide(output);
        if let Some(w) = next_focus {
            self.focus_window_internal(&w);
        }
        true
    }

    /// Called while dragging a live window from centre stage (`PointerMoveSurfaceGrab`):
    /// updates strip peel target and auto-hide responsiveness.
    pub fn stage_on_live_drag_motion(&mut self, window: &WindowElement, pointer: Point<f64, Logical>) {
        if !self.stage.enabled {
            return;
        }
        let Some(output) = self.space.output_under(pointer).next().cloned() else {
            self.stage.peel_target = None;
            return;
        };
        let Some(geo) = self.space.output_geometry(&output) else { return };
        let Some(usable) = self.output_usable_area(&output) else { return };
        let name = output.name();
        let (space_idx, _) = self.spaces.info(&name);

        let multi_active = self
            .stage
            .spaces
            .get(&(name.clone(), space_idx))
            .and_then(|st| st.active.as_ref())
            .is_some_and(|g| g.windows.len() >= 2 && g.windows.iter().any(|sw| sw.window == *window));

        let zone = strip_zone(geo, usable);
        let in_peel_zone = multi_active && zone.contains(pointer);
        self.stage.peel_target = in_peel_zone.then_some(name);
        self.stage_update_autohide(&output);
    }

    /// Called when a live window drag finishes (`PointerMoveSurfaceGrab::button`).
    /// Returns `true` if the window was peeled into the strip.
    pub fn stage_on_live_drag_end(&mut self, window: &WindowElement, pointer: Point<f64, Logical>) -> bool {
        if !self.stage.enabled {
            return false;
        }
        let peel = self.stage.peel_target.take();
        if let Some(name) = peel {
            if let Some(output) = self.output_by_name(&name) {
                if let (Some(geo), Some(usable)) = (self.space.output_geometry(&output), self.output_usable_area(&output)) {
                    if strip_zone(geo, usable).contains(pointer) {
                        return self.stage_peel_window_to_strip(window, &output);
                    }
                }
            }
        }
        // Update saved location and strip auto-hide for the output it landed on
        if let Some(output) = self.window_output(window) {
            let name = output.name();
            let (space_idx, _) = self.spaces.info(&name);
            if let Some(loc) = self.space.element_location(window) {
                if let Some(st) = self.stage.spaces.get_mut(&(name, space_idx)) {
                    if let Some(active) = st.active.as_mut() {
                        if let Some(sw) = active.windows.iter_mut().find(|x| x.window == *window) {
                            sw.saved_loc = loc;
                        }
                    }
                }
            }
            self.stage_update_autohide(&output);
        }
        false
    }

    /// Called when a window is closed, minimized, or hidden (`⌘H`).
    /// Removes it from Stage Manager and auto-promotes `strip[0]` if active stage became empty.
    pub fn stage_on_window_removed(&mut self, window: &WindowElement) {
        if !self.stage.enabled {
            return;
        }
        let mut promote_outputs: Vec<String> = Vec::new();
        let mut changed_outputs: Vec<String> = Vec::new();
        let active_spaces = self.active_space_map();

        for ((output_name, space_idx), st) in self.stage.spaces.iter_mut() {
            let mut touched = false;
            st.glides.retain(|g| g.window != *window);
            if let Some(active) = st.active.as_mut() {
                let before = active.windows.len();
                active.windows.retain(|sw| sw.window != *window && sw.window.alive());
                if active.windows.len() != before {
                    touched = true;
                    if active.windows.is_empty() {
                        st.active = None;
                        let is_active_space = active_spaces.get(output_name).copied().unwrap_or(0) == *space_idx;
                        if is_active_space && !st.desktop_revealed && !st.strip.is_empty() {
                            promote_outputs.push(output_name.clone());
                        }
                    } else if let Some(last) = active.windows.last() {
                        active.app_id = last.window.app_id();
                    }
                }
            }
            let before_strip = st.strip.len();
            for g in &mut st.strip {
                let b = g.windows.len();
                g.windows.retain(|sw| sw.window != *window && sw.window.alive());
                if g.windows.len() != b {
                    touched = true;
                    if let Some(last) = g.windows.last() {
                        g.app_id = last.window.app_id();
                    }
                }
            }
            st.strip.retain(|g| !g.windows.is_empty());
            if st.strip.len() != before_strip {
                touched = true;
            }
            if touched && active_spaces.get(output_name).copied().unwrap_or(0) == *space_idx {
                changed_outputs.push(output_name.clone());
            }
        }

        for out_name in promote_outputs {
            if let Some(output) = self.output_by_name(&out_name) {
                let (space_idx, _) = self.spaces.info(&out_name);
                let top_gid = self
                    .stage
                    .spaces
                    .get(&(out_name.clone(), space_idx))
                    .and_then(|st| st.strip.first())
                    .map(|g| g.id);
                if let Some(gid) = top_gid {
                    self.stage_switch_to_group(&output, gid, false);
                }
            }
        }
        for out_name in changed_outputs {
            if let Some(output) = self.output_by_name(&out_name) {
                self.stage_relayout_output(&output, true);
                self.stage_update_autohide(&output);
            }
        }
    }

    /// Called when a window is unminimized or unhidden back onto `self.space`.
    pub fn stage_on_window_restored(&mut self, window: &WindowElement) {
        if !self.stage.enabled {
            return;
        }
        self.stage_on_new_window(window);
    }

    /// Activate an app when Stage Manager is enabled. Returns `true` if Stage Manager handled it.
    pub fn stage_activate_app(&mut self, app_id: &str, switch_space: bool) -> bool {
        if !self.stage.enabled {
            return false;
        }
        // 1. Already in the active stage on any output's current Space?
        let active_spaces = self.active_space_map();
        let mut active_match: Option<(String, Vec<WindowElement>)> = None;
        for ((out_name, space_idx), st) in &self.stage.spaces {
            if active_spaces.get(out_name).copied().unwrap_or(0) != *space_idx {
                continue;
            }
            if let Some(active) = &st.active {
                let wins: Vec<_> = active
                    .windows
                    .iter()
                    .filter(|sw| sw.window.alive() && sw.window.app_id() == app_id)
                    .map(|sw| sw.window.clone())
                    .collect();
                if !wins.is_empty() {
                    active_match = Some((out_name.clone(), wins));
                    break;
                }
            }
        }

        // In OneAtATime mode, clicking an already-active app cycles to its next window in the strip (macOS!)
        if let Some((out_name, wins)) = active_match {
            if crate::settings::get().stage_manager.app_windows == StageAppWindows::OneAtATime {
                let (space_idx, _) = self.spaces.info(&out_name);
                let next_gid = self
                    .stage
                    .spaces
                    .get(&(out_name.clone(), space_idx))
                    .and_then(|st| {
                        st.strip
                            .iter()
                            .find(|g| g.windows.iter().any(|sw| sw.window.alive() && sw.window.app_id() == app_id))
                            .map(|g| g.id)
                    });
                if let (Some(output), Some(gid)) = (self.output_by_name(&out_name), next_gid) {
                    self.stage_switch_to_group(&output, gid, true);
                    return true;
                }
            }
            for w in &wins {
                self.space.raise_element(w, false);
            }
            if let Some(top) = wins.last() {
                self.focus_window_internal(top);
            }
            return true;
        }

        // 2. Waiting in the strip?
        if let Some((out_name, space_idx, gid)) = self.stage.find_app_in_strip(app_id, &active_spaces) {
            let cur_space = active_spaces.get(&out_name).copied().unwrap_or(0);
            if space_idx != cur_space {
                if !switch_space {
                    return false;
                }
                if let Some(output) = self.output_by_name(&out_name) {
                    self.switch_space(&output, space_idx);
                }
            }
            if let Some(output) = self.output_by_name(&out_name) {
                self.stage_switch_to_group(&output, gid, true);
                return true;
            }
        }

        false
    }

    /// Bring a specific window (e.g. from IPC `Activate { id }` or Mission Control pick)
    /// onto centre stage if it currently lives in a Stage Manager strip.
    pub fn stage_reveal_and_focus(&mut self, window: &WindowElement) -> bool {
        if !self.stage.enabled {
            return false;
        }
        let Some((out_name, space_idx, gid)) = self.stage.find_in_strip(window) else {
            return false;
        };
        let (active_idx, _) = self.spaces.info(&out_name);
        let Some(output) = self.output_by_name(&out_name) else {
            return false;
        };
        if space_idx != active_idx {
            self.switch_space(&output, space_idx);
        }
        self.stage_switch_to_group(&output, gid, true);
        self.focus_window_internal(window);
        true
    }

    /// Called by `spaces.rs` right after switching Spaces on `output`.
    pub fn stage_after_space_switch(&mut self, output: &Output) {
        if !self.stage.enabled {
            return;
        }
        let name = output.name();
        let (space_idx, _) = self.spaces.info(&name);
        let has_state = self.stage.spaces.contains_key(&(name.clone(), space_idx));
        if has_state {
            // Sync active group with whatever windows are mapped on this Space now
            self.stage_reconcile_space(output, space_idx);
        } else {
            self.stage_organize_output(output, false);
        }
        self.stage_relayout_output(output, false);
        self.stage_update_autohide(output);
    }

    /// Reconcile an existing `SpaceStage` with `self.space` (drops dead windows,
    /// adds any unexpected mapped windows into active).
    fn stage_reconcile_space(&mut self, output: &Output, space_idx: usize) {
        let now: Duration = self.clock.now().into();
        let name = output.name();
        let mapped = self.windows_on_output(output);
        let gid = self.stage.alloc_group_id();
        let Some(st) = self.stage.spaces.get_mut(&(name, space_idx)) else {
            return;
        };
        for g in &mut st.strip {
            g.windows.retain(|sw| sw.window.alive());
        }
        st.strip.retain(|g| !g.windows.is_empty());
        if let Some(active) = st.active.as_mut() {
            active.windows.retain(|sw| sw.window.alive());
            if active.windows.is_empty() {
                st.active = None;
            }
        }
        // Any mapped non-fullscreen window on this output not in `active` belongs in `active`
        for (w, loc) in mapped {
            if !w.alive() || w.is_fullscreen() {
                continue;
            }
            let in_active = st
                .active
                .as_ref()
                .is_some_and(|a| a.windows.iter().any(|sw| sw.window == w));
            if !in_active {
                match st.active.as_mut() {
                    Some(a) => a.windows.push(StagedWindow::new(w, loc, None, now)),
                    None => st.active = Some(StageGroup::new(gid, vec![StagedWindow::new(w, loc, None, now)], now)),
                }
            }
        }
    }

    /// Called by `spaces.rs` when Space `removed_idx` is deleted on `output` and
    /// its windows move to `target_idx` (pre-removal indices).
    pub fn stage_on_space_removed(&mut self, output: &Output, removed_idx: usize, target_idx: usize) {
        if !self.stage.enabled {
            return;
        }
        let name = output.name();
        let removed_stage = self.stage.spaces.remove(&(name.clone(), removed_idx));
        // Re-index keys > removed_idx down by 1
        let keys: Vec<(String, usize)> = self
            .stage
            .spaces
            .keys()
            .filter(|(n, i)| n == &name && *i > removed_idx)
            .cloned()
            .collect();
        let mut shifted = Vec::new();
        for k in keys {
            if let Some(v) = self.stage.spaces.remove(&k) {
                shifted.push(((k.0, k.1 - 1), v));
            }
        }
        for (k, v) in shifted {
            self.stage.spaces.insert(k, v);
        }
        let new_target = if target_idx > removed_idx { target_idx - 1 } else { target_idx };
        if let Some(rem) = removed_stage {
            let now: Duration = self.clock.now().into();
            let cfg = crate::settings::get().stage_manager.clone();
            let dst = self
                .stage
                .spaces
                .entry((name.clone(), new_target))
                .or_insert_with(|| SpaceStage::new(now, cfg.show_recent_apps));
            // Merge removed strip groups into destination strip
            for g in rem.strip {
                if !g.windows.is_empty() {
                    dst.strip.push(g);
                }
            }
        }
        self.stage_relayout_output(output, true);
        self.stage_update_autohide(output);
    }

    /// Called every frame from `tick_effects()`: prune finished glides, prune dead
    /// windows, and lazily refresh live snapshots of visible strip thumbnails.
    pub fn tick_stage_manager(&mut self) {
        let now: Duration = self.clock.now().into();
        self.stage.exit_glides.retain(|(_, g)| !g.is_done(now));
        if !self.stage.enabled {
            return;
        }

        // Prune completed glides & dead windows
        let mut dead_found = false;
        let active_spaces = self.active_space_map();
        for ((out_name, space_idx), st) in self.stage.spaces.iter_mut() {
            st.glides.retain(|g| !g.is_done(now) && g.window.alive());
            for g in &mut st.strip {
                let before = g.windows.len();
                g.windows.retain(|sw| sw.window.alive());
                if g.windows.len() != before {
                    dead_found = true;
                }
            }
            st.strip.retain(|g| !g.windows.is_empty());
            if let Some(active) = st.active.as_mut() {
                let before = active.windows.len();
                active.windows.retain(|sw| sw.window.alive());
                if active.windows.len() != before {
                    dead_found = true;
                }
                if active.windows.is_empty() {
                    st.active = None;
                }
            }
            if active_spaces.get(out_name).copied().unwrap_or(0) == *space_idx {
                // Keep saved_loc fresh for live windows in active stage
                if let Some(active) = st.active.as_mut() {
                    for sw in &mut active.windows {
                        if let Some(loc) = self.space.element_location(&sw.window) {
                            sw.saved_loc = loc;
                        }
                    }
                }
            }
        }
        if dead_found {
            let outputs: Vec<Output> = self.space.outputs().cloned().collect();
            for o in outputs {
                self.stage_relayout_output(&o, true);
                self.stage_update_autohide(&o);
            }
        } else {
            // Keep auto-hide in sync when clients commit a new size (zoom, tile, resize)
            let outputs: Vec<Output> = self.space.outputs().cloned().collect();
            for o in outputs {
                self.stage_update_autohide(&o);
            }
        }

        // Refresh at most 1 visible strip thumbnail per frame if its surface committed
        // (keeps videos/terminals/chats live in the strip without hurting frame pacing).
        if self.mc.is_some() || self.protocols.locked {
            return;
        }
        let mut refresh_target: Option<(String, usize, usize, usize, WindowElement, f64)> = None;
        for output in self.space.outputs() {
            let name = output.name();
            let (space_idx, _) = self.spaces.info(&name);
            let Some(usable) = self.output_usable_area(output) else { continue };
            let visible_max = max_visible_groups(usable.size.h as f64);
            let Some(st) = self.stage.spaces.get(&(name.clone(), space_idx)) else { continue };
            if st.strip_visibility(now) < 0.05 {
                continue;
            }
            let scale = output.current_scale().fractional_scale();
            'search: for (gi, g) in st.strip.iter().take(visible_max).enumerate() {
                let len = g.windows.len();
                let start = len.saturating_sub(MAX_STACK_CARDS);
                for (wi, sw) in g.windows.iter().enumerate().skip(start) {
                    if sw.window.alive()
                        && (sw.snapshot.is_none() || now.saturating_sub(sw.last_snap) >= LIVE_SNAP_INTERVAL)
                    {
                        refresh_target = Some((name.clone(), space_idx, gi, wi, sw.window.clone(), scale));
                        break 'search;
                    }
                }
            }
            if refresh_target.is_some() {
                break;
            }
        }

        if let Some((out_name, space_idx, gi, wi, window, scale)) = refresh_target {
            let prev_key = self
                .stage
                .spaces
                .get(&(out_name.clone(), space_idx))
                .and_then(|st| st.strip.get(gi))
                .and_then(|g| g.windows.get(wi))
                .map(|sw| (sw.snapshot.is_some(), sw.snapshot_key.clone()));
            let Some((has_snap, old_key)) = prev_key else { return };

            let updated = self.backend_data.with_renderer(|renderer: &mut GlesRenderer| {
                let bbox = SpaceElement::bbox(&window);
                if bbox.is_empty() {
                    return None;
                }
                let origin: Point<i32, smithay::utils::Physical> =
                    (Point::<i32, Logical>::from((0, 0)) - bbox.loc).to_physical_precise_round(scale);
                let elems: Vec<WindowRenderElement> =
                    AsRenderElements::<GlesRenderer>::render_elements(&window, renderer, origin, Scale::from(scale), 1.0);
                let key: Vec<(Id, CommitCounter)> =
                    elems.iter().map(|e| (e.id().clone(), e.current_commit())).collect();
                if has_snap && key == old_key {
                    return Some((None, key));
                }
                let snap = crate::effects::snapshot::snapshot_elements_into(renderer, &elems, bbox, scale, None);
                Some((snap, key))
            });

            if let Some(st) = self.stage.spaces.get_mut(&(out_name, space_idx)) {
                if let Some(sw) = st.strip.get_mut(gi).and_then(|g| g.windows.get_mut(wi)) {
                    sw.last_snap = now;
                    if let Some(Some((maybe_snap, key))) = updated {
                        sw.snapshot_key = key;
                        if let Some(snap) = maybe_snap {
                            sw.snapshot = Some(snap);
                            sw.next_commit();
                        }
                    }
                }
            }
        }
    }

    // ---- Pointer interaction ------------------------------------------------

    /// Hit-test the Stage Manager strip at `pos` (global logical).
    /// Returns `(output, group_id, slot_rect)` if `pos` is over a visible thumbnail.
    pub fn stage_strip_hit(&self, pos: Point<f64, Logical>) -> Option<(Output, u64, Rectangle<f64, Logical>)> {
        if !self.stage.enabled || self.mc.is_some() || self.shot_select.is_some() || self.protocols.locked {
            return None;
        }
        let output = self.space.output_under(pos).next()?.clone();
        // Fullscreen hides Stage Manager
        if output
            .user_data()
            .get::<crate::shell::FullscreenSurface>()
            .and_then(|f| f.get())
            .is_some()
        {
            return None;
        }
        let geo = self.space.output_geometry(&output)?;
        let local = pos - geo.loc.to_f64();
        let layers = layer_map_for_output(&output);
        // Overlay and Top layers (Control Center, menus, menu bar, Dock, Launchpad) always win
        if crate::render::layer_input_under(&layers, WlrLayer::Overlay, local).is_some()
            || crate::render::layer_input_under(&layers, WlrLayer::Top, local).is_some()
        {
            return None;
        }
        drop(layers);

        let usable = self.output_usable_area(&output)?;
        let name = output.name();
        let (space_idx, _) = self.spaces.info(&name);
        let st = self.stage.spaces.get(&(name, space_idx))?;
        let now: Duration = self.clock.now().into();
        let vis = st.strip_visibility(now);
        if vis < 0.35 {
            return None;
        }
        let visible_max = max_visible_groups(usable.size.h as f64);
        let on_right = strip_on_right();
        let slide_off = (1.0 - vis) * (strip_width(geo) + 24.0) * if on_right { 1.0 } else { -1.0 };

        for g in st.strip.iter().take(visible_max) {
            let mut r = g.slot_at(now);
            r.loc.x += slide_off;
            // Generous 8px hit padding around the tilted thumbnail so clicking feels effortless
            let hit = Rectangle::new(
                (r.loc.x - 8.0, r.loc.y - 6.0).into(),
                (r.size.w + 16.0, r.size.h + 12.0).into(),
            );
            if hit.contains(pos) {
                return Some((output, g.id, r));
            }
        }
        None
    }

    /// True if Stage Manager currently owns the pointer (hovering a strip item or dragging one).
    pub fn stage_owns_pointer(&self, pos: Point<f64, Logical>) -> bool {
        self.stage.strip_drag.is_some() || self.stage_strip_hit(pos).is_some()
    }

    /// Handle pointer motion for Stage Manager (hover animations, edge reveal, strip drag).
    pub fn stage_pointer_motion(&mut self, pos: Point<f64, Logical>) {
        if !self.stage.enabled || self.mc.is_some() || self.protocols.locked {
            return;
        }
        let now: Duration = self.clock.now().into();

        // Update active strip drag
        if let Some(drag) = self.stage.strip_drag.as_mut() {
            drag.pos = pos;
            if !drag.active {
                let d = (pos.x - drag.press.x).hypot(pos.y - drag.press.y);
                if d > DRAG_THRESHOLD {
                    drag.active = true;
                    drag.pull_spring.retarget(1.0, now);
                }
            }
            return;
        }

        // Edge-reveal check on each output
        let outputs: Vec<Output> = self.space.outputs().cloned().collect();
        let on_right = strip_on_right();
        for output in outputs {
            let Some(geo) = self.space.output_geometry(&output) else { continue };
            let Some(usable) = self.output_usable_area(&output) else { continue };
            let name = output.name();
            let (space_idx, _) = self.spaces.info(&name);
            let sw = strip_width(geo);

            let on_this_output = geo.to_f64().contains(pos);
            let in_vert = pos.y >= usable.loc.y as f64 && pos.y <= (usable.loc.y + usable.size.h) as f64;
            let dist_from_edge = if on_right {
                (geo.loc.x + geo.size.w) as f64 - pos.x
            } else {
                pos.x - geo.loc.x as f64
            };

            let mut needs_autohide_update = false;
            if let Some(st) = self.stage.spaces.get_mut(&(name.clone(), space_idx)) {
                if !st.strip.is_empty() && on_this_output && in_vert && dist_from_edge <= EDGE_TRIGGER_PX {
                    if !st.edge_revealed {
                        st.edge_revealed = true;
                        needs_autohide_update = true;
                    }
                } else if st.edge_revealed && (!on_this_output || dist_from_edge > sw + EDGE_LEAVE_HYSTERESIS) {
                    st.edge_revealed = false;
                    needs_autohide_update = true;
                }
            }
            if needs_autohide_update {
                self.stage_update_autohide(&output);
            }
        }

        // Hover springs for strip thumbnails
        let hit = self.stage_strip_hit(pos).map(|(o, gid, _)| (o.name(), gid));
        let active_spaces = self.active_space_map();
        for ((out_name, space_idx), st) in self.stage.spaces.iter_mut() {
            if active_spaces.get(out_name).copied().unwrap_or(0) != *space_idx {
                continue;
            }
            for g in &mut st.strip {
                let want = hit.as_ref().is_some_and(|(o, gid)| o == out_name && *gid == g.id);
                if g.hovered != want {
                    g.hovered = want;
                    let target = if want { 1.0 } else { 0.0 };
                    if crate::settings::get().appearance.reduce_motion {
                        g.hover_spring = Spring::new(target, target, SpringParams::STAGE_HOVER, now);
                    } else {
                        g.hover_spring.retarget(target, now);
                    }
                }
            }
        }
    }

    /// Handle left mouse button press/release for Stage Manager.
    /// Returns `true` if Stage Manager consumed the event (must not reach clients).
    pub fn stage_pointer_button(&mut self, button: u32, pressed: bool) -> bool {
        if !self.stage.enabled || self.mc.is_some() || self.shot_select.is_some() || self.protocols.locked {
            return false;
        }
        if button != 0x110 {
            return false;
        }
        let pos = self.pointer.current_location();
        let now: Duration = self.clock.now().into();

        if !pressed {
            let Some(drag) = self.stage.strip_drag.take() else {
                return false;
            };
            let Some(output) = self.output_by_name(&drag.output_name) else {
                return true;
            };
            if !drag.active {
                // Click on strip thumbnail!
                if drag.shift_held {
                    self.stage_merge_group_into_active(&output, drag.group_id, None, None);
                } else {
                    self.stage_switch_to_group(&output, drag.group_id, true);
                }
            } else {
                // Dropped a dragged thumbnail: if outside the strip zone -> combine into active stage!
                let outside_strip = self
                    .space
                    .output_geometry(&output)
                    .zip(self.output_usable_area(&output))
                    .is_some_and(|(geo, usable)| !strip_zone(geo, usable).contains(pos));
                if outside_strip {
                    let drop_rect = drag.rect_at(now);
                    self.stage_merge_group_into_active(&output, drag.group_id, Some(drop_rect), Some(pos));
                } else {
                    // Dropped back in strip: glide smoothly back into place
                    self.stage_relayout_output(&output, true);
                }
            }
            return true;
        }

        // Button pressed: check if clicking a strip thumbnail
        if let Some((output, gid, slot_rect)) = self.stage_strip_hit(pos) {
            let shift_held = self
                .seat
                .get_keyboard()
                .map(|k| k.modifier_state().shift)
                .unwrap_or(false);
            let grab = (
                ((pos.x - slot_rect.loc.x) / slot_rect.size.w.max(1.0)).clamp(0.0, 1.0),
                ((pos.y - slot_rect.loc.y) / slot_rect.size.h.max(1.0)).clamp(0.0, 1.0),
            );
            self.stage.strip_drag = Some(StripDrag {
                output_name: output.name(),
                group_id: gid,
                press: pos,
                pos,
                slot_rect,
                grab,
                shift_held,
                active: false,
                pull_spring: Spring::new(0.0, 0.0, SpringParams::STAGE_HOVER, now),
            });
            return true;
        }

        // Click on empty wallpaper: move active stage aside to reveal desktop items (macOS Sonoma/Sequoia)
        let cfg = crate::settings::get();
        if cfg.stage_manager.click_wallpaper_to_reveal && !self.pointer.is_grabbed() {
            let output = self.space.output_under(pos).next().cloned();
            if let Some(output) = output {
                let has_fs = output
                    .user_data()
                    .get::<crate::shell::FullscreenSurface>()
                    .and_then(|f| f.get())
                    .is_some();
                if !has_fs {
                    let geo = self.space.output_geometry(&output).unwrap_or_default();
                    let local = pos - geo.loc.to_f64();
                    let layers = layer_map_for_output(&output);
                    let on_top_layer = crate::render::layer_input_under(&layers, WlrLayer::Overlay, local).is_some()
                        || crate::render::layer_input_under(&layers, WlrLayer::Top, local).is_some();
                    drop(layers);
                    let on_window = self.space.element_under(pos).is_some();
                    if !on_top_layer && !on_window {
                        let name = output.name();
                        let (space_idx, _) = self.spaces.info(&name);
                        let has_active = self
                            .stage
                            .spaces
                            .get(&(name.clone(), space_idx))
                            .and_then(|st| st.active.as_ref())
                            .is_some_and(|g| !g.windows.is_empty());
                        if has_active {
                            self.stage_park_active(&output, true);
                            if let Some(st) = self.stage.spaces.get_mut(&(name, space_idx)) {
                                st.desktop_revealed = true;
                            }
                            self.stage_update_autohide(&output);
                            // Clear keyboard focus from hidden windows
                            let serial = smithay::utils::SERIAL_COUNTER.next_serial();
                            if let Some(kb) = self.seat.get_keyboard() {
                                kb.set_focus(self, None, serial);
                            }
                        } else if !cfg.windows.show_desktop_icons {
                            // No desktop icons to click: clicking wallpaper again restores top stage
                            let top_gid = self
                                .stage
                                .spaces
                                .get(&(name.clone(), space_idx))
                                .filter(|st| st.desktop_revealed)
                                .and_then(|st| st.strip.first())
                                .map(|g| g.id);
                            if let Some(gid) = top_gid {
                                self.stage_switch_to_group(&output, gid, false);
                            }
                        }
                    }
                }
            }
        }
        false
    }

    /// Raise and keyboard-focus `window` without re-triggering Stage Manager hooks.
    pub(crate) fn focus_window_internal(&mut self, window: &WindowElement) {
        self.space.raise_element(window, true);
        if self.is_locked() {
            return;
        }
        let serial = smithay::utils::SERIAL_COUNTER.next_serial();
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, Some(window.clone().into()), serial);
        }
        self.space.elements().for_each(|w| {
            if let Some(t) = w.0.toplevel() {
                t.send_pending_configure();
            }
        });
    }
}

// ---- Rendering ---------------------------------------------------------------

/// Render Stage Manager elements (strip thumbnails, hover labels, active glides,
/// and dragged thumbnail) for `output` into `out` (front-to-back order).
#[allow(clippy::too_many_arguments)]
pub fn stage_manager_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geo: Rectangle<i32, Logical>,
    stage: &StageManager,
    space_idx: usize,
    fx: &mut crate::effects::Effects,
    scale: f64,
    now: Duration,
    out: &mut Vec<crate::render::OutputRenderElements>,
) {
    let origin = output_geo.loc.to_f64();
    let focal = (920.0 * scale) as f32;
    let out_name = output.name();

    let to_phys = |r: Rectangle<f64, Logical>| RectF {
        x: ((r.loc.x - origin.x) * scale) as f32,
        y: ((r.loc.y - origin.y) * scale) as f32,
        w: (r.size.w * scale) as f32,
        h: (r.size.h * scale) as f32,
    };

    // 1. Exit glides (playing right after turning Stage Manager OFF)
    for (o_name, g) in &stage.exit_glides {
        if o_name != &out_name || g.is_done(now) {
            continue;
        }
        push_glide(g, &to_phys, focal, now, out);
    }

    if !stage.enabled {
        return;
    }

    let Some(st) = stage.spaces.get(&(out_name.clone(), space_idx)) else {
        return;
    };

    // 2. Dragged thumbnail from the strip floats above everything
    if let Some(drag) = stage.strip_drag.as_ref().filter(|d| d.active && d.output_name == out_name) {
        if let Some(group) = st.strip.iter().find(|g| g.id == drag.group_id) {
            let drag_rect = drag.rect_at(now);
            let p = drag.pull_spring.value_at(now).clamp(0.0, 1.0) as f32;
            let yaw = strip_rest_yaw() * (1.0 - p);
            for sw in group.windows.iter().rev().take(MAX_STACK_CARDS) {
                if let Some(snap) = sw.snapshot.as_ref() {
                    let quad = snapshot_quad_for_frame(drag_rect, sw.window.geometry(), snap.bbox);
                    out.push(crate::render::OutputRenderElements::Perspective(
                        PerspectiveElement::new(
                            sw.render_id.clone(),
                            sw.next_commit(),
                            snap.texture.clone(),
                            (snap.size.w, snap.size.h),
                            to_phys(quad),
                            yaw,
                            focal,
                            0.95,
                            1.0,
                        ),
                    ));
                }
            }
        }
    }

    // 3. Active transition glides (arriving onto stage or flying into strip)
    for g in st.glides.iter().rev() {
        if g.is_done(now) {
            continue;
        }
        push_glide(g, &to_phys, focal, now, out);
    }

    // 4. Recent-apps strip along the side
    let vis = st.strip_visibility(now);
    if vis <= 0.01 && stage.peel_target.as_deref() != Some(out_name.as_str()) {
        return;
    }
    let zone_h = layer_map_for_output(output).non_exclusive_zone().size.h as f64;
    let visible_max = max_visible_groups(zone_h);
    let on_right = strip_on_right();
    let slide_off = (1.0 - vis) * (strip_width(output_geo) + 24.0) * if on_right { 1.0 } else { -1.0 };
    let strip_alpha = (vis as f32).clamp(0.0, 1.0);
    let dragged_gid = stage
        .strip_drag
        .as_ref()
        .filter(|d| d.active && d.output_name == out_name)
        .map(|d| d.group_id);

    // Draw hovered group title pill on top of thumbnails
    for g in st.strip.iter().take(visible_max) {
        if Some(g.id) == dragged_gid {
            continue;
        }
        let hover = g.hover_at(now);
        if hover > 0.25 {
            let mut slot = g.slot_at(now);
            slot.loc.x += slide_off;
            let title = g.primary_title();
            if !title.is_empty() {
                if let Some(el) = crate::render::stage_label_element(
                    renderer,
                    fx,
                    &title,
                    slot.loc.x - origin.x + slot.size.w * 0.5,
                    slot.loc.y - origin.y + slot.size.h + 6.0,
                    scale,
                    (hover as f32).clamp(0.0, 1.0) * strip_alpha,
                ) {
                    out.push(el);
                }
            }
        }
    }

    // Draw strip groups (front-to-back within each stack: frontmost card first!)
    let rest_yaw = strip_rest_yaw();
    let hov_yaw = strip_hover_yaw();
    for g in st.strip.iter().take(visible_max) {
        if Some(g.id) == dragged_gid {
            continue;
        }
        // Hide static thumbnail while its glide into the strip is still in flight
        if st.glides.iter().any(|gl| gl.target_group_id == Some(g.id) && !gl.is_done(now)) {
            continue;
        }
        let mut slot = g.slot_at(now);
        if slot.size.w < 1.0 || slot.size.h < 1.0 {
            continue;
        }
        let hover = g.hover_at(now);
        // Hover lifts slightly inward and scales up ~7%
        let lift = 1.0 + 0.07 * hover;
        let inward = (if on_right { -6.0 } else { 6.0 }) * hover;
        let cx = slot.loc.x + slot.size.w * 0.5 + slide_off + inward;
        let cy = slot.loc.y + slot.size.h * 0.5;
        let sw_w = slot.size.w * lift;
        let sw_h = slot.size.h * lift;
        slot = Rectangle::new((cx - sw_w * 0.5, cy - sw_h * 0.5).into(), (sw_w, sw_h).into());

        let yaw = rest_yaw + (hov_yaw - rest_yaw) * hover as f32;
        let count = g.windows.len().min(MAX_STACK_CARDS);
        // Front-to-back order for `out` (frontmost = depth 0, behind = depth 1, 2)
        for (depth, sw) in g.windows.iter().rev().take(count).enumerate() {
            let Some(snap) = sw.snapshot.as_ref() else { continue };
            let d = depth as f64;
            let card_scale = 1.0 - 0.07 * d;
            // Back cards peek subtly inward & slightly up; hover fans them slightly wider
            let fan_x = (if on_right { -1.0 } else { 1.0 }) * (10.0 + 4.0 * hover) * d;
            let fan_y = -(6.0 + 2.5 * hover) * d;
            let cw = slot.size.w * card_scale;
            let ch = slot.size.h * card_scale;
            let card_frame = Rectangle::new(
                (slot.loc.x + (slot.size.w - cw) * 0.5 + fan_x, slot.loc.y + (slot.size.h - ch) * 0.5 + fan_y).into(),
                (cw, ch).into(),
            );
            let quad = snapshot_quad_for_frame(card_frame, sw.window.geometry(), snap.bbox);
            let brightness = (1.0 - 0.11 * depth as f32).max(0.7);
            out.push(crate::render::OutputRenderElements::Perspective(
                PerspectiveElement::new(
                    sw.render_id.clone(),
                    sw.next_commit(),
                    snap.texture.clone(),
                    (snap.size.w, snap.size.h),
                    to_phys(quad),
                    yaw,
                    focal,
                    strip_alpha,
                    brightness,
                ),
            ));
        }
    }

    // Subtle translucent drop zone behind thumbnails when peeling a window from centre stage into the strip
    if stage.peel_target.as_deref() == Some(out_name.as_str()) {
        let non_excl = layer_map_for_output(output).non_exclusive_zone();
        let usable = Rectangle::new(output_geo.loc + non_excl.loc, non_excl.size);
        let z = strip_zone(output_geo, usable);
        let local = Rectangle::new(
            (z.loc.x - origin.x + 8.0, z.loc.y - origin.y + 12.0).into(),
            ((z.size.w - 16.0).max(1.0), (z.size.h - 24.0).max(1.0)).into(),
        );
        if let Some(el) = crate::render::stage_peel_highlight(renderer, fx, local, scale) {
            out.push(el);
        }
    }
}

fn push_glide(
    g: &StageGlide,
    to_phys: &impl Fn(Rectangle<f64, Logical>) -> RectF,
    focal: f32,
    now: Duration,
    out: &mut Vec<crate::render::OutputRenderElements>,
) {
    let p = g.spring.value_at(now);
    let pf = p.clamp(0.0, 1.0) as f32;
    let cur_frame = lerp_rect(g.from_frame, g.to_frame, p);
    let quad = snapshot_quad_for_frame(cur_frame, g.snap_geo, g.snapshot.bbox);
    let yaw = g.from_yaw + (g.to_yaw - g.from_yaw) * pf;
    let alpha = (g.from_alpha + (g.to_alpha - g.from_alpha) * pf).clamp(0.0, 1.0);
    out.push(crate::render::OutputRenderElements::Perspective(
        PerspectiveElement::new(
            g.render_id.clone(),
            g.next_commit(),
            g.snapshot.texture.clone(),
            (g.snapshot.size.w, g.snapshot.size.h),
            to_phys(quad),
            yaw,
            focal,
            alpha,
            1.0,
        ),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_slots_never_overlap_and_fit_inside_zone() {
        let zone = Rectangle::<f64, Logical>::new((0.0, 28.0).into(), (168.0, 960.0).into());
        let aspects = [1.6, 1.33, 0.8, 1.5, 1.77, 1.2];
        let slots = compute_strip_slots(zone, &aspects, false);
        assert_eq!(slots.len(), aspects.len());
        let visible = max_visible_groups(zone.size.h).min(aspects.len());
        for i in 0..visible {
            let a = slots[i];
            assert!(a.loc.x >= zone.loc.x && a.loc.x + a.size.w <= zone.loc.x + zone.size.w + 1e-6);
            assert!(a.loc.y >= zone.loc.y && a.loc.y + a.size.h <= zone.loc.y + zone.size.h + 1e-6);
            for b in &slots[i + 1..visible] {
                assert!(!rect_overlaps(a, *b), "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn snapshot_quad_aligns_shadow_bbox_proportionally() {
        // Window geometry: (0, 0, 800, 600), shadow bbox extends 20px on all sides
        let geo = Rectangle::<i32, Logical>::new((0, 0).into(), (800, 600).into());
        let bbox = Rectangle::<i32, Logical>::new((-20, -20).into(), (840, 640).into());
        // Full size at (100, 50)
        let full = snapshot_quad_for_frame(Rectangle::new((100.0, 50.0).into(), (800.0, 600.0).into()), geo, bbox);
        assert!((full.loc.x - 80.0).abs() < 1e-6);
        assert!((full.loc.y - 30.0).abs() < 1e-6);
        assert!((full.size.w - 840.0).abs() < 1e-6);
        assert!((full.size.h - 640.0).abs() < 1e-6);
        // Quarter size thumbnail (200x150) at (16, 100)
        let thumb = snapshot_quad_for_frame(Rectangle::new((16.0, 100.0).into(), (200.0, 150.0).into()), geo, bbox);
        assert!((thumb.loc.x - 11.0).abs() < 1e-6);
        assert!((thumb.loc.y - 95.0).abs() < 1e-6);
        assert!((thumb.size.w - 210.0).abs() < 1e-6);
        assert!((thumb.size.h - 160.0).abs() < 1e-6);
    }
}
