//! Spaces (virtual desktops), one strip per output, macOS style.
//!
//! Windows of the active Space live in the smithay `Space`; windows of other
//! Spaces are unmapped and remembered here with their positions. Switching
//! slides the old and new Space horizontally.

use std::collections::HashMap;

use smithay::{
    output::Output,
    utils::{IsAlive, Logical, Point},
};

use crate::{
    effects::spring::{Spring, SpringParams},
    shell::WindowElement,
    state::{Backend, OrchardState},
};

#[derive(Debug, Default, Clone)]
pub struct SpaceSlot {
    /// windows + their locations while this Space is not active
    pub windows: Vec<(WindowElement, Point<i32, Logical>)>,
    /// a full-screen app's own Space (removed when it leaves full screen)
    pub fullscreen: Option<WindowElement>,
}

#[derive(Debug, Clone)]
pub struct OutputSpaces {
    pub active: usize,
    pub slots: Vec<SpaceSlot>,
}

impl Default for OutputSpaces {
    fn default() -> Self {
        OutputSpaces {
            active: 0,
            slots: vec![SpaceSlot::default()],
        }
    }
}

/// An in-progress slide between two Spaces on one output.
#[derive(Debug)]
pub struct SpaceTransition {
    pub output: String,
    /// windows of the Space we are leaving, drawn from their old positions
    pub from: Vec<(WindowElement, Point<i32, Logical>)>,
    /// +1.0 when moving to a Space on the right
    pub dir: f64,
    pub spring: Spring,
    /// no Space in that direction: the current one nudges and springs back
    pub bounce: bool,
}

#[derive(Debug)]
pub struct Spaces {
    pub outputs: HashMap<String, OutputSpaces>,
    /// number of Spaces per output from the last session
    saved: HashMap<String, usize>,
}

impl Default for Spaces {
    fn default() -> Self {
        Spaces {
            outputs: HashMap::new(),
            saved: load_counts(),
        }
    }
}

/// `$XDG_STATE_HOME/orchard/spaces.json`: `{"DP-1": 3, ...}`
fn counts_path() -> Option<std::path::PathBuf> {
    crate::crash::state_dir().map(|d| d.join("spaces.json"))
}

fn load_counts() -> HashMap<String, usize> {
    counts_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

impl Spaces {
    pub fn for_output(&mut self, name: &str) -> &mut OutputSpaces {
        let saved = self.saved.get(name).copied().unwrap_or(1).clamp(1, 16);
        self.outputs.entry(name.to_string()).or_insert_with(|| OutputSpaces {
            active: 0,
            slots: vec![SpaceSlot::default(); saved],
        })
    }

    /// Remember how many Spaces each output has, for the next session.
    pub fn save(&mut self) {
        for (name, o) in &self.outputs {
            self.saved.insert(name.clone(), o.slots.len());
        }
        let Some(path) = counts_path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match serde_json::to_string(&self.saved) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    tracing::warn!("saving {}: {e}", path.display());
                }
            }
            Err(e) => tracing::warn!("saving Space counts: {e}"),
        }
    }

    pub fn info(&self, name: &str) -> (usize, usize) {
        self.outputs
            .get(name)
            .map(|o| (o.active, o.slots.len()))
            .unwrap_or_else(|| (0, self.saved.get(name).copied().unwrap_or(1).clamp(1, 16)))
    }
}

impl<BackendData: Backend> OrchardState<BackendData> {
    /// The output that shows the menu bar and Dock.
    pub fn primary_output(&self) -> Option<smithay::output::Output> {
        let s = crate::settings::get();
        s.displays
            .primary
            .as_ref()
            .and_then(|name| self.space.outputs().find(|o| o.name() == *name))
            .or_else(|| self.space.outputs().next())
            .cloned()
    }

    /// The output the user is working on: under the pointer.
    pub fn active_output(&self) -> Option<Output> {
        self.space
            .output_under(self.pointer.current_location())
            .next()
            .cloned()
            .or_else(|| self.primary_output())
    }

    /// The display a window belongs to: the one holding the centre of its
    /// frame (else the one it overlaps most). Not `outputs_for_element()
    /// .first()`: that includes the shadow, so a window tiled against the
    /// seam of the right display was counted on the left one.
    pub fn window_output(&self, window: &WindowElement) -> Option<Output> {
        let loc = self.space.element_location(window)?;
        let frame = smithay::utils::Rectangle::new(loc, smithay::desktop::space::SpaceElement::geometry(window).size);
        let center = frame.to_f64().loc + frame.to_f64().size.downscale(2.0).to_point();
        let mut best: Option<(i64, Output)> = None;
        for o in self.space.outputs() {
            let Some(geo) = self.space.output_geometry(o) else { continue };
            if geo.to_f64().contains(center) {
                return Some(o.clone());
            }
            let area = geo
                .intersection(frame)
                .map(|r| r.size.w as i64 * r.size.h as i64)
                .unwrap_or(0);
            if area > 0 && best.as_ref().map(|(a, _)| area > *a).unwrap_or(true) {
                best = Some((area, o.clone()));
            }
        }
        best.map(|(_, o)| o)
            .or_else(|| self.primary_output())
    }

    pub(crate) fn windows_on_output(&self, output: &Output) -> Vec<(WindowElement, Point<i32, Logical>)> {
        // bottom-to-top, so remapping in this order restores stacking
        self.space
            .elements()
            .filter(|w| {
                self.window_output(w)
                    .map(|o| o == *output)
                    .unwrap_or_else(|| self.space.outputs().next() == Some(output))
            })
            .filter_map(|w| Some((w.clone(), self.space.element_location(w)?)))
            .collect()
    }

    /// Switch the given output to Space `idx` with a slide animation.
    pub fn switch_space(&mut self, output: &Output, idx: usize) {
        let name = output.name();
        let (active, count) = self.spaces.info(&name);
        if idx >= count || idx == active {
            return;
        }
        let fullscreen_leaving = output
            .user_data()
            .get::<crate::shell::FullscreenSurface>()
            .and_then(|f| f.clear());
        let all = self.windows_on_output(output);
        for (w, _) in &all {
            self.space.unmap_elem(w);
        }
        // the full-screen window belongs to its slot, not the window list
        let leaving: Vec<_> = all
            .into_iter()
            .filter(|(w, _)| Some(w) != fullscreen_leaving.as_ref())
            .collect();

        let spaces = self.spaces.for_output(&name);
        spaces.slots[active].windows = leaving.clone();
        if let Some(fs) = fullscreen_leaving {
            spaces.slots[active].fullscreen = Some(fs);
        }
        spaces.active = idx;
        let entering = std::mem::take(&mut spaces.slots[idx].windows);
        let entering_fullscreen = spaces.slots[idx].fullscreen.clone();

        for (w, loc) in entering.iter().filter(|(w, _)| w.alive()) {
            self.space.map_element(w.clone(), *loc, false);
        }
        if let Some(fs) = entering_fullscreen.filter(|w| w.alive()) {
            if let Some(geo) = self.space.output_geometry(output) {
                self.space.map_element(fs.clone(), geo.loc, true);
            }
            output
                .user_data()
                .insert_if_missing(crate::shell::FullscreenSurface::default);
            output
                .user_data()
                .get::<crate::shell::FullscreenSurface>()
                .unwrap()
                .set(fs);
        }

        let now = self.clock.now().into();
        self.fx.space_transition = Some(SpaceTransition {
            output: name,
            from: leaving,
            dir: if idx > active { 1.0 } else { -1.0 },
            spring: Spring::new(0.0, 1.0, SpringParams::SMOOTH, now),
            bounce: false,
        });
        self.backend_data.reset_buffers(output);
        self.refit_windows_on_output(output);
        self.focus_topmost();
    }

    pub fn switch_space_relative(&mut self, delta: i32) {
        let Some(output) = self.active_output() else { return };
        let (active, count) = self.spaces.info(&output.name());
        let target = active as i32 + delta;
        if target < 0 || target >= count as i32 {
            if self.mc.is_none() {
                self.bounce_space(&output, delta.signum() as f64);
            }
            return;
        }
        self.goto_space(&output, target as usize);
    }

    /// ⌃⌘1…9 / ⌃⌘←→: switch Spaces, inside Mission Control when it is open.
    pub fn goto_space(&mut self, output: &Output, idx: usize) {
        if self.mc.as_ref().map(|m| !m.closing).unwrap_or(false) {
            self.mc_switch_space(output, idx);
        } else {
            self.switch_space(output, idx);
        }
    }

    /// No Space in that direction: nudge the current one and spring back.
    fn bounce_space(&mut self, output: &Output, dir: f64) {
        let now: std::time::Duration = self.clock.now().into();
        if self.fx.space_transition.as_ref().map(|t| !t.spring.is_done_at(now)).unwrap_or(false) {
            return;
        }
        self.fx.space_transition = Some(SpaceTransition {
            output: output.name(),
            from: Vec::new(),
            dir,
            spring: Spring::new(0.0, 1.0, SpringParams::SMOOTH, now),
            bounce: true,
        });
    }

    pub fn add_space(&mut self, output: &Output) -> usize {
        let spaces = self.spaces.for_output(&output.name());
        spaces.slots.push(SpaceSlot::default());
        let idx = spaces.slots.len() - 1;
        self.spaces.save();
        idx
    }

    /// Remove a Space. Its windows move to the Space on its left (the right
    /// one for the first Space); removing the current Space switches there.
    pub fn remove_space(&mut self, output: &Output, idx: usize) {
        let name = output.name();
        let (active, count) = self.spaces.info(&name);
        if count <= 1 || idx >= count {
            return;
        }
        let target = if idx > 0 { idx - 1 } else { 1 };
        if idx == active {
            self.switch_space(output, target);
        }
        // a full-screen window of that Space leaves full screen first
        let fullscreen = self.spaces.for_output(&name).slots[idx].fullscreen.clone();
        if let Some(fs) = fullscreen {
            self.exit_fullscreen_space(&fs);
        }
        let spaces = self.spaces.for_output(&name);
        let slot = spaces.slots.remove(idx);
        if idx < spaces.active {
            spaces.active -= 1;
        }
        let target = if target > idx { target - 1 } else { target };
        if target == spaces.active {
            for (w, loc) in slot.windows.into_iter().filter(|(w, _)| w.alive()) {
                self.space.map_element(w, loc, false);
            }
            self.backend_data.reset_buffers(output);
        } else {
            spaces.slots[target].windows.extend(slot.windows);
        }
        tracing::info!(output = %name, space = idx + 1, "removed Space");
        self.spaces.save();
    }

    /// Move a window that is on screen to Space `idx` of `output`. Moving it
    /// to another output keeps its relative position there. Returns false if
    /// nothing changed.
    pub fn move_window_to_space(&mut self, window: &WindowElement, output: &Output, idx: usize) -> bool {
        let name = output.name();
        let (active, count) = self.spaces.info(&name);
        if idx >= count {
            return false;
        }
        if window.is_fullscreen() {
            self.exit_fullscreen_space(window);
        }
        let Some(loc) = self.space.element_location(window) else {
            return false;
        };
        let src = self.window_output(window);
        let same_output = src.as_ref() == Some(output);
        if same_output && idx == active {
            return false;
        }
        let loc = match (src.and_then(|o| self.space.output_geometry(&o)), self.space.output_geometry(output)) {
            (Some(from), Some(to)) if !same_output => {
                let size = smithay::desktop::space::SpaceElement::geometry(window).size;
                let x = to.loc.x + (loc.x - from.loc.x).clamp(0, (to.size.w - size.w).max(0));
                let y = to.loc.y + (loc.y - from.loc.y).clamp(28, (to.size.h - size.h).max(28));
                Point::from((x, y))
            }
            _ => loc,
        };
        self.space.unmap_elem(window);
        if idx == active {
            self.space.map_element(window.clone(), loc, true);
        } else {
            self.spaces.for_output(&name).slots[idx].windows.push((window.clone(), loc));
        }
        // outputs_for_element is only updated by a refresh
        self.space.refresh();
        self.backend_data.reset_buffers(output);
        true
    }

    /// Move the focused window one Space left/right (creating one on the right if needed).
    pub fn move_focused_to_space_relative(&mut self, delta: i32) {
        let Some(output) = self
            .focused_window()
            .and_then(|w| self.window_output(&w))
            .or_else(|| self.active_output())
        else {
            return;
        };
        let (active, _) = self.spaces.info(&output.name());
        let target = active as i32 + delta;
        if target < 0 {
            self.bounce_space(&output, -1.0);
            return;
        }
        self.move_focused_to_space(target as usize, true);
    }

    /// ⌃⌘⇧1…9: move the focused window to that Space and follow it.
    /// `create`: add a Space on the right if `target` is one past the last.
    pub fn move_focused_to_space(&mut self, target: usize, create: bool) {
        if self.mc.is_some() {
            return;
        }
        let Some(window) = self.focused_window() else { return };
        if self.space.element_location(&window).is_none() {
            return;
        }
        let Some(output) = self.window_output(&window)
            .or_else(|| self.active_output())
        else {
            return;
        };
        let (active, count) = self.spaces.info(&output.name());
        if target == active {
            return;
        }
        if target >= count {
            if !create || target > count {
                return;
            }
            self.add_space(&output);
        }
        if self.move_window_to_space(&window, &output, target) {
            self.switch_space(&output, target);
            self.focus_window(&window);
        }
    }

    /// Ensure windows that were closed while on an inactive Space are dropped.
    pub fn prune_spaces(&mut self) {
        for spaces in self.spaces.outputs.values_mut() {
            for slot in &mut spaces.slots {
                slot.windows.retain(|(w, _)| w.alive());
                if slot.fullscreen.as_ref().map(|w| !w.alive()).unwrap_or(false) {
                    slot.fullscreen = None;
                }
            }
        }
    }

    /// All windows of an output's Spaces (for Mission Control thumbnails).
    pub fn space_windows(&self, output: &Output, idx: usize) -> Vec<(WindowElement, Point<i32, Logical>)> {
        let name = output.name();
        let (active, _) = self.spaces.info(&name);
        let mut out = Vec::new();
        if idx == active {
            out.extend(self.windows_on_output(output));
            return out;
        }
        if let Some(slot) = self.spaces.outputs.get(&name).and_then(|o| o.slots.get(idx)) {
            out.extend(slot.windows.clone());
        }
        out
    }

    /// Which Space (output name, index) currently holds a window, if inactive.
    pub fn find_window_space(&self, window: &WindowElement) -> Option<(String, usize)> {
        self.spaces.outputs.iter().find_map(|(name, o)| {
            o.slots.iter().enumerate().find_map(|(i, s)| {
                (s.windows.iter().any(|(w, _)| w == window) || s.fullscreen.as_ref() == Some(window))
                    .then(|| (name.clone(), i))
            })
        })
    }

    /// Bring a window's Space to the front (e.g. when activated from the Dock).
    pub fn reveal_window(&mut self, window: &WindowElement) {
        if let Some((name, idx)) = self.find_window_space(window) {
            let output = self.space.outputs().find(|o| o.name() == name).cloned();
            if let Some(output) = output {
                self.switch_space(&output, idx);
            }
        }
    }
}

/// Where a full-screen window came from, to put it back afterwards.
#[derive(Debug, Clone, Copy)]
pub struct FullscreenOrigin {
    pub space: usize,
    pub loc: Point<i32, Logical>,
    pub size: smithay::utils::Size<i32, Logical>,
}

impl<BackendData: Backend> OrchardState<BackendData> {
    pub fn toggle_fullscreen(&mut self, window: &WindowElement) {
        if window.is_fullscreen() {
            self.exit_fullscreen_space(window);
        } else {
            self.enter_fullscreen_space(window);
        }
    }

    /// Full screen in place: the window covers its output on the current
    /// Space (no separate full-screen Space).
    pub fn enter_fullscreen_space(&mut self, window: &WindowElement) {
        if window.is_fullscreen() {
            return;
        }
        // a nested Orchard (dist/nested.sh) breaks when its window goes full screen
        if window.app_id() == crate::NESTED_APP_ID {
            return;
        }
        let Some(output) = self.window_output(window)
            .or_else(|| self.active_output())
        else {
            return;
        };
        let Some(out_geo) = self.space.output_geometry(&output) else { return };
        let name = output.name();
        let (active, _) = self.spaces.info(&name);
        let loc = self.space.element_location(window).unwrap_or(out_geo.loc);
        let size = window.client_geometry().size;
        window.user_data().insert_if_missing(|| std::cell::Cell::new(None::<FullscreenOrigin>));
        window
            .user_data()
            .get::<std::cell::Cell<Option<FullscreenOrigin>>>()
            .unwrap()
            .set(Some(FullscreenOrigin { space: active, loc, size }));

        window.set_fullscreen_flag(true);
        match window.0.underlying_surface() {
            smithay::desktop::WindowSurface::Wayland(t) => {
                use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
                t.with_pending_state(|s| {
                    s.states.set(xdg_toplevel::State::Fullscreen);
                    s.size = Some(out_geo.size);
                });
                if t.is_initial_configure_sent() {
                    t.send_pending_configure();
                }
            }
            #[cfg(feature = "xwayland")]
            smithay::desktop::WindowSurface::X11(x) => {
                let _ = x.set_fullscreen(true);
                let _ = x.configure(out_geo);
            }
        }
        // the output shows only this window (render.rs), raised and focused
        self.space.map_element(window.clone(), out_geo.loc, true);
        output.user_data().insert_if_missing(crate::shell::FullscreenSurface::default);
        if let Some(f) = output.user_data().get::<crate::shell::FullscreenSurface>() {
            f.set(window.clone());
        }
        self.backend_data.reset_buffers(&output);
        self.focus_window(window);
    }

    /// Leave full screen: back to the previous size and position, on the Space
    /// the window is on.
    pub fn exit_fullscreen_space(&mut self, window: &WindowElement) {
        if !window.is_fullscreen() {
            return;
        }
        let origin = window
            .user_data()
            .get::<std::cell::Cell<Option<FullscreenOrigin>>>()
            .and_then(|c| c.take());
        window.set_fullscreen_flag(false);
        match window.0.underlying_surface() {
            smithay::desktop::WindowSurface::Wayland(t) => {
                use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
                t.with_pending_state(|s| {
                    s.states.unset(xdg_toplevel::State::Fullscreen);
                    s.size = origin.map(|o| o.size);
                    s.fullscreen_output = None;
                });
                t.send_pending_configure();
            }
            #[cfg(feature = "xwayland")]
            smithay::desktop::WindowSurface::X11(x) => {
                let _ = x.set_fullscreen(false);
                if let Some(o) = origin {
                    let _ = x.configure(smithay::utils::Rectangle::new(o.loc, o.size));
                }
            }
        }
        let Some((name, idx)) = self.find_window_space(window).or_else(|| {
            // on the shown Space: the output it covers
            let o = self.window_output(window).or_else(|| self.active_output())?;
            let (a, _) = self.spaces.info(&o.name());
            Some((o.name(), a))
        }) else {
            return;
        };
        let Some(output) = self.space.outputs().find(|o| o.name() == name).cloned() else { return };
        if let Some(f) = output.user_data().get::<crate::shell::FullscreenSurface>() {
            if f.get().as_ref() == Some(window) {
                f.clear();
            }
        }
        let loc = origin.map(|o| o.loc).unwrap_or_default();
        let spaces = self.spaces.for_output(&name);
        let active = spaces.active;
        if let Some(slot) = spaces.slots.get_mut(idx) {
            if slot.fullscreen.as_ref() == Some(window) {
                slot.fullscreen = None;
            }
        }
        if idx == active {
            self.space.map_element(window.clone(), loc, true);
            self.backend_data.reset_buffers(&output);
            self.focus_window(window);
        } else if let Some(slot) = self.spaces.for_output(&name).slots.get_mut(idx) {
            // left full screen while its Space is not shown: back into that Space
            slot.windows.retain(|(w, _)| w != window);
            slot.windows.push((window.clone(), loc));
        }
    }
}

/// How far the current Space nudges when there is no Space to switch to.
const BOUNCE_PX: f64 = 48.0;

/// Offset (logical px) applied to windows on `output` during a Space slide:
/// (offset for entering windows, offset for leaving windows).
pub fn transition_offsets(t: &SpaceTransition, output_width: f64, now: std::time::Duration) -> (f64, f64) {
    let p = t.spring.value_at(now).clamp(0.0, 1.0);
    if t.bounce {
        return (-t.dir * BOUNCE_PX * (std::f64::consts::PI * p).sin(), 0.0);
    }
    let gap = 40.0;
    let w = output_width + gap;
    (t.dir * w * (1.0 - p), -t.dir * w * p)
}
