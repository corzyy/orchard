//! macOS-like window management policy: placement, zoom, minimize, close,
//! focus cycling.

use std::cell::RefCell;

use smithay::{
    desktop::{layer_map_for_output, space::SpaceElement, WindowSurface},
    input::{pointer::Focus, Seat},
    reexports::wayland_protocols::xdg::shell::server::xdg_toplevel,
    utils::{IsAlive, Logical, Point, Rectangle, Serial, SERIAL_COUNTER},
    wayland::compositor::with_states,
};

use smithay::backend::renderer::element::Id;

use crate::{
    effects::{
        snapshot::snapshot_window,
        spring::{Spring, SpringParams},
        Transient, TransientKind,
    },
    shell::{
        ssd::HEADER_BAR_HEIGHT, PointerResizeSurfaceGrab, ResizeData, ResizeEdge, ResizeState, SurfaceData,
        WindowElement,
    },
    state::{Backend, OrchardState},
};

/// Vertical position of a freshly opened window: macOS places new windows a
/// bit above the optical center.
const CENTER_BIAS_Y: f64 = 0.42;
/// Offset applied when a new window would land exactly on top of another one.
const CASCADE_STEP: i32 = 24;

/// Keyboard layout + repeat settings, from (in order) XKB_DEFAULT_* env,
/// Orchard's settings, and systemd-localed's X11 config.
pub struct KeyboardSettings {
    pub model: String,
    pub layout: String,
    pub variant: String,
    pub options: Option<String>,
    pub repeat_delay: i32,
    pub repeat_rate: i32,
}

fn localed_option(name: &str) -> Option<String> {
    let content = std::fs::read_to_string("/etc/X11/xorg.conf.d/00-keyboard.conf").ok()?;
    content.lines().find_map(|l| {
        let l = l.trim();
        let rest = l.strip_prefix("Option")?.trim();
        let rest = rest.strip_prefix(&format!("\"{name}\""))?.trim();
        Some(rest.trim_matches('"').to_string())
    })
}

pub fn keyboard_settings() -> KeyboardSettings {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let prefs = crate::settings::get().keyboard.clone();
    let pref = |v: &str| Some(v.trim().to_string()).filter(|v| !v.is_empty());
    let layout_set = pref(&prefs.layout).is_some();
    let layout = env("XKB_DEFAULT_LAYOUT")
        .or_else(|| pref(&prefs.layout))
        .or_else(|| localed_option("XkbLayout"))
        .unwrap_or_else(|| "us".into());
    let variant = env("XKB_DEFAULT_VARIANT")
        .or_else(|| if layout_set { Some(prefs.variant.trim().to_string()) } else { None })
        .or_else(|| localed_option("XkbVariant"))
        .unwrap_or_default();
    let model = env("XKB_DEFAULT_MODEL")
        .or_else(|| localed_option("XkbModel"))
        .unwrap_or_else(|| "pc105".into());
    let options = env("XKB_DEFAULT_OPTIONS")
        .or_else(|| pref(&prefs.options))
        .or_else(|| localed_option("XkbOptions"));
    let repeat_delay = Some(prefs.repeat_delay)
        .filter(|v| *v > 0)
        .map(|v| v.clamp(100, 2000))
        .unwrap_or(250);
    let repeat_rate = Some(prefs.repeat_rate)
        .filter(|v| *v > 0)
        .map(|v| v.clamp(1, 100))
        .unwrap_or(30);
    KeyboardSettings {
        model,
        layout,
        variant,
        options,
        repeat_delay,
        repeat_rate,
    }
}

/// Accumulated three/four-finger swipe (libinput units ≈ logical px).
#[derive(Debug, Default, Clone, Copy)]
pub struct Swipe {
    pub dx: f64,
    pub dy: f64,
}

const SWIPE_THRESHOLD: f64 = 90.0;

/// Marker stored in a window's user data until it has been centered once.
pub struct NeedsInitialPlacement;

impl<BackendData: Backend> OrchardState<BackendData> {
    /// macOS trackpad gestures: swipe left/right = Spaces, up = Mission Control,
    /// down = App Exposé (or leave Mission Control).
    pub fn finish_swipe(&mut self, s: Swipe) {
        if s.dx.abs() > s.dy.abs() && s.dx.abs() > SWIPE_THRESHOLD {
            // natural scrolling: fingers to the left reveal the Space on the right
            self.switch_space_relative(if s.dx < 0.0 { 1 } else { -1 });
        } else if s.dy < -SWIPE_THRESHOLD {
            if self.mc.is_none() {
                self.toggle_mission_control(false);
            }
        } else if s.dy > SWIPE_THRESHOLD {
            if self.mc.is_some() {
                self.close_mission_control(None);
            } else {
                self.toggle_mission_control(true);
            }
        }
    }

    /// Usable area of the output a window is (mostly) on: excludes menu bar & dock.
    pub fn usable_area_for(&self, window: &WindowElement) -> Option<Rectangle<i32, Logical>> {
        let output = self.window_output(window)
            .or_else(|| {
                self.space
                    .output_under(self.pointer.current_location())
                    .next()
                    .cloned()
            })
            .or_else(|| self.primary_output())?;
        let geo = self.space.output_geometry(&output)?;
        let zone = layer_map_for_output(&output).non_exclusive_zone();
        Some(Rectangle::new(geo.loc + zone.loc, zone.size))
    }

    /// Center a window the first time it has a size (macOS style), cascading
    /// if another window already sits at the same spot.
    pub fn maybe_place_initially(&mut self, window: &WindowElement) {
        if window.user_data().get::<RefCell<Option<NeedsInitialPlacement>>>().is_none() {
            return;
        }
        let size = window.geometry().size;
        if size.w <= 0 || size.h <= 0 {
            return;
        }
        let pending = window
            .user_data()
            .get::<RefCell<Option<NeedsInitialPlacement>>>()
            .unwrap()
            .borrow_mut()
            .take();
        if pending.is_none() {
            return;
        }
        if self.is_tiling_mode() {
            self.dwindle_place_new(window);
            return;
        }
        let area = self
            .window_output(window)
            .or_else(|| self.active_output())
            .and_then(|o| self.stage_placement_area(&o))
            .or_else(|| self.usable_area_for(window));
        let Some(area) = area else { return };
        let mut x = area.loc.x + (area.size.w - size.w) / 2;
        let mut y = area.loc.y + ((area.size.h - size.h) as f64 * CENTER_BIAS_Y) as i32;
        x = x.max(area.loc.x);
        y = y.max(area.loc.y);
        // cascade
        let occupied: Vec<Point<i32, Logical>> = self
            .space
            .elements()
            .filter(|w| *w != window)
            .filter_map(|w| self.space.element_location(w))
            .collect();
        let mut guard = 0;
        while occupied.iter().any(|p| (p.x - x).abs() < 4 && (p.y - y).abs() < 4) && guard < 20 {
            x += CASCADE_STEP;
            y += CASCADE_STEP;
            guard += 1;
        }
        self.space.map_element(window.clone(), (x, y), false);
        window.start_open_animation(self.clock.now().into());
        // new windows come to the front with keyboard focus, like on macOS
        self.stage_on_new_window(window);
        self.focus_window(window);
    }

    pub fn close_window(&mut self, window: &WindowElement) {
        match window.0.underlying_surface() {
            WindowSurface::Wayland(w) => w.send_close(),
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(w) => {
                let _ = w.close();
            }
        }
    }

    pub fn is_zoomed(&self, window: &WindowElement) -> bool {
        match window.0.underlying_surface() {
            WindowSurface::Wayland(t) => t.with_pending_state(|s| s.states.contains(xdg_toplevel::State::Maximized)),
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(x) => x.is_maximized(),
        }
    }

    /// The green button: toggle between the user size and filling the usable area.
    pub fn toggle_zoom(&mut self, window: &WindowElement) {
        if !window.alive() {
            return;
        }
        #[cfg(feature = "xwayland")]
        if let WindowSurface::X11(x) = window.0.underlying_surface() {
            let x = x.clone();
            if x.is_maximized() {
                use smithay::xwayland::XwmHandler;
                if let Some(xwm) = self.xwm.as_ref().map(|x| x.id()) {
                    XwmHandler::unmaximize_request(self, xwm, x);
                }
            } else {
                self.maximize_request_x11(&x);
            }
            return;
        }
        let WindowSurface::Wayland(toplevel) = window.0.underlying_surface() else {
            return;
        };
        let toplevel = toplevel.clone();
        if self.is_zoomed(window) {
            let mut restore = window.decoration_state().restore_geometry.take();
            if restore.is_none() {
                // zoomed from the start (e.g. restored maximized): two thirds, centered
                if let Some(area) = self.usable_area_for(window) {
                    let header = if window.is_ssd() { HEADER_BAR_HEIGHT } else { 0 };
                    let (w, h) = (area.size.w * 2 / 3, area.size.h * 2 / 3);
                    let loc = (area.loc.x + (area.size.w - w) / 2, area.loc.y + (area.size.h - h) / 2);
                    restore = Some(Rectangle::new(loc.into(), (w, (h - header).max(1)).into()));
                }
            }
            toplevel.with_pending_state(|s| {
                s.states.unset(xdg_toplevel::State::Maximized);
                s.size = restore.map(|r| r.size);
            });
            toplevel.send_pending_configure();
            if let Some(r) = restore {
                let header = if window.is_ssd() { HEADER_BAR_HEIGHT } else { 0 };
                self.animate_frame(window, Rectangle::new(r.loc, (r.size.w, r.size.h + header).into()));
                self.space.map_element(window.clone(), r.loc, true);
            }
        } else {
            let Some(area) = self.usable_area_for(window) else { return };
            let loc = self.space.element_location(window).unwrap_or_default();
            let client = window.client_geometry().size;
            // a tiled window keeps the size it had before tiling
            let was_tiled = window.decoration_state().tile.take().is_some();
            if !(was_tiled && window.decoration_state().restore_geometry.is_some()) {
                window.decoration_state().restore_geometry = Some(Rectangle::new(loc, client));
            }
            let header = if window.is_ssd() { HEADER_BAR_HEIGHT } else { 0 };
            let mut size = area.size;
            size.h -= header;
            toplevel.with_pending_state(|s| {
                s.states.set(xdg_toplevel::State::Maximized);
                s.size = Some(size);
            });
            toplevel.send_pending_configure();
            self.animate_frame(window, area);
            self.space.map_element(window.clone(), area.loc, true);
        }
        if let Some(o) = self.window_output(window) {
            self.stage_update_autohide(&o);
        }
    }

    /// Animate `window` from its current frame to `to` (geometry rect, global
    /// logical). Call before mapping it at the new place.
    pub fn animate_frame(&mut self, window: &WindowElement, to: Rectangle<i32, Logical>) {
        if crate::settings::get().appearance.reduce_motion {
            window.anim().frame = None;
            return;
        }
        let Some(loc) = self.space.element_location(window) else { return };
        let geo = window.geometry();
        if geo.size.w <= 0 || geo.size.h <= 0 {
            return;
        }
        let now: std::time::Duration = self.clock.now().into();
        // mid-animation: continue from where it is on screen
        let from = window
            .anim()
            .frame_at(now)
            .map(|f| f.current(loc, now))
            .unwrap_or_else(|| Rectangle::new(loc, geo.size).to_f64());
        let to_f = to.to_f64();
        let moved = (from.loc.x - to_f.loc.x).abs() + (from.loc.y - to_f.loc.y).abs();
        let resized = (from.size.w - to_f.size.w).abs() + (from.size.h - to_f.size.h).abs();
        if moved < 2.0 && resized < 2.0 {
            return;
        }
        let scale = self.output_scale_for(window);
        let snapshot = if resized >= 2.0 {
            self.backend_data.with_renderer(|r| snapshot_window(r, window, scale)).flatten()
        } else {
            None // a pure move needs no crossfade
        };
        window.anim().frame = Some(crate::effects::FrameAnim {
            from,
            to_size: to.size,
            spring: Spring::new(0.0, 1.0, SpringParams::FRAME, now),
            snapshot,
            snap_geo: geo,
            id: Id::new(),
            commit: Default::default(),
            resized_at: Default::default(),
        });
    }

    fn output_scale_for(&self, window: &WindowElement) -> f64 {
        self.window_output(window).as_ref()
            .or_else(|| self.space.outputs().next())
            .map(|o| o.current_scale().fractional_scale())
            .unwrap_or(1.0)
    }

    /// Where a minimized window goes: its Dock tile if the shell told us, else
    /// the middle of the Dock's screen edge.
    pub fn dock_target(&self, window: &WindowElement) -> Rectangle<f64, Logical> {
        if let Some(r) = self.fx_dock_target(window) {
            return r;
        }
        let out = self.window_output(window)
            .or_else(|| self.primary_output());
        let geo = out
            .and_then(|o| self.space.output_geometry(&o))
            .unwrap_or_else(|| Rectangle::from_size((1280, 800).into()));
        let (x, y) = match crate::settings::get().dock.position {
            orchard_config::DockPosition::Bottom => {
                (geo.loc.x as f64 + geo.size.w as f64 / 2.0 - 32.0, (geo.loc.y + geo.size.h) as f64 - 60.0)
            }
            orchard_config::DockPosition::Left => (geo.loc.x as f64 + 8.0, geo.loc.y as f64 + geo.size.h as f64 / 2.0 - 24.0),
            orchard_config::DockPosition::Right => {
                ((geo.loc.x + geo.size.w) as f64 - 72.0, geo.loc.y as f64 + geo.size.h as f64 / 2.0 - 24.0)
            }
        };
        Rectangle::new((x, y).into(), (64.0, 48.0).into())
    }

    fn fx_dock_target(&self, window: &WindowElement) -> Option<Rectangle<f64, Logical>> {
        self.ipc.dock_tiles.get(&window.app_id()).copied()
    }

    /// Genie target for a window going into (or coming out of) a minimized
    /// Dock tile: the snapshot (with its shadow) mapped so that the window
    /// itself lands exactly where the Dock draws its thumbnail.
    fn thumbnail_target(&self, window: &WindowElement, tile: Rectangle<f64, Logical>) -> Rectangle<f64, Logical> {
        let geo = window.geometry().to_f64();
        let bbox = window.bbox().to_f64();
        let fit = thumb_rect_in_tile(tile, geo.size.w, geo.size.h);
        let k = if geo.size.w > 0.0 { fit.size.w / geo.size.w } else { 1.0 };
        Rectangle::new(
            (
                fit.loc.x - (geo.loc.x - bbox.loc.x) * k,
                fit.loc.y - (geo.loc.y - bbox.loc.y) * k,
            )
                .into(),
            (bbox.size.w * k, bbox.size.h * k).into(),
        )
    }

    /// Picture of a window for its minimized Dock tile, written to
    /// `$XDG_RUNTIME_DIR/orchard-thumbs/` (PNG encoding happens off-thread).
    fn write_thumbnail(&mut self, window: &WindowElement) {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").map(|d| std::path::PathBuf::from(d).join("orchard-thumbs")) else {
            return;
        };
        let Some(Some((rgba, w, h))) = self
            .backend_data
            .with_renderer(|r| crate::effects::snapshot::thumbnail_rgba(r, window, THUMB_PX))
        else {
            return;
        };
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = dir.join(format!("{}-{n}.png", window.ipc_id().0));
        // forget pictures of windows that are no longer minimized (done off the main thread)
        let keep: Vec<String> = self.minimized.iter().filter_map(thumb_path_of).collect();
        window.user_data().insert_if_missing(|| ThumbPath(RefCell::new(None)));
        if let Some(t) = window.user_data().get::<ThumbPath>() {
            *t.0.borrow_mut() = Some(path.to_string_lossy().into_owned());
        }
        std::thread::spawn(move || {
            let _ = std::fs::create_dir_all(&dir);
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for e in entries.flatten() {
                    let p = e.path().to_string_lossy().into_owned();
                    if !keep.contains(&p) {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
            }
            let tmp = path.with_extension("tmp");
            let ok = image::RgbaImage::from_raw(w, h, rgba)
                .map(|img| img.save_with_format(&tmp, image::ImageFormat::Png).is_ok())
                .unwrap_or(false);
            if ok {
                let _ = std::fs::rename(&tmp, &path);
            } else {
                tracing::warn!("could not write {}", path.display());
            }
        });
    }

    /// Snapshot a window for an animation. `location` is its space location.
    pub fn make_transient(
        &mut self,
        window: &WindowElement,
        location: Point<i32, Logical>,
        kind: TransientKind,
        spring: Spring,
    ) -> Option<Transient> {
        let target = self.dock_target(window);
        self.make_transient_to(window, location, kind, spring, target)
    }

    pub fn make_transient_to(
        &mut self,
        window: &WindowElement,
        location: Point<i32, Logical>,
        kind: TransientKind,
        spring: Spring,
        target: Rectangle<f64, Logical>,
    ) -> Option<Transient> {
        let scale = self.output_scale_for(window);
        let snapshot = self
            .backend_data
            .with_renderer(|r| snapshot_window(r, window, scale))??;
        let origin = location - window.geometry().loc + snapshot.bbox.loc;
        let rect = Rectangle::<i32, Logical>::new(origin, snapshot.bbox.size).to_f64();
        Some(Transient {
            id: Id::new(),
            snapshot,
            rect,
            target,
            spring,
            kind,
            commit: Default::default(),
        })
    }

    /// Advance animations; finish restores whose genie has completed.
    pub fn tick_effects(&mut self) {
        self.prune_spaces();
        self.sync_foreign_toplevels();
        self.tick_mission_control();
        self.tick_stage_manager();
        self.update_game_mode();
        self.ipc_broadcast_if_changed();
        let now: std::time::Duration = self.clock.now().into();
        if self
            .fx
            .space_transition
            .as_ref()
            .map(|t| t.spring.is_done_at(now))
            .unwrap_or(false)
        {
            self.fx.space_transition = None;
        }
        if self.fx.transients.is_empty() {
            return;
        }
        let (done, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.fx.transients)
            .into_iter()
            .partition(|t| t.is_done(now));
        self.fx.transients = keep;
        let mut mapped = false;
        for t in done {
            if let TransientKind::Restore { window, location } = t.kind {
                if window.alive() {
                    self.space.map_element(window.clone(), location, true);
                    self.stage_on_window_restored(&window);
                    self.focus_window(&window);
                    mapped = true;
                }
            }
        }
        // this runs right before rendering: without a refresh the window is
        // not yet in elements_for_output, so it would be missing for a frame
        // after its snapshot is gone (flicker at the end of a restore)
        if mapped {
            self.space.refresh();
            self.refit_all_windows();
        }
    }

    /// The yellow button: hide the window until restored (Dock, Cmd+Tab, …).
    pub fn minimize_window(&mut self, window: &WindowElement) {
        if !window.alive() {
            return;
        }
        if window.is_fullscreen() {
            self.exit_fullscreen_space(window);
        }
        let Some(loc) = self.space.element_location(window) else {
            return;
        };
        let now = self.clock.now().into();
        self.write_thumbnail(window);
        // into the slot the Dock opens for this window's thumbnail
        let target = match self.ipc.dock_next_minimized {
            Some(tile) => self.thumbnail_target(window, tile),
            None => self.dock_target(window),
        };
        if let Some(t) = self.make_transient_to(
            window,
            loc,
            TransientKind::Minimize,
            Spring::new(0.0, 1.0, SpringParams::GENIE, now),
            target,
        ) {
            self.fx.transients.push(t);
        }
        window.user_data().insert_if_missing(|| RefCell::new(Point::<i32, Logical>::default()));
        *window
            .user_data()
            .get::<RefCell<Point<i32, Logical>>>()
            .unwrap()
            .borrow_mut() = loc;
        #[cfg(feature = "xwayland")]
        if let Some(x11) = window.0.x11_surface() {
            let _ = x11.set_suspended(true);
        }
        self.space.unmap_elem(window);
        self.minimized.retain(|w| w != window);
        self.minimized.push(window.clone());
        self.stage_on_window_removed(window);
        if self.is_tiling_mode() {
            self.dwindle.remove_everywhere(window.ipc_id().0);
            self.retile_all();
        }
        self.focus_topmost();
    }

    /// Restore a minimized window (most recently minimized if `None`).
    pub fn unminimize(&mut self, window: Option<&WindowElement>) {
        self.minimized.retain(|w| w.alive());
        let idx = match window {
            Some(w) => self.minimized.iter().position(|m| m == w),
            None => self.minimized.len().checked_sub(1),
        };
        let Some(idx) = idx else { return };
        let window = self.minimized.remove(idx);
        let was_hidden = window.decoration_state().hidden;
        window.decoration_state().hidden = false;
        #[cfg(feature = "xwayland")]
        if let Some(x11) = window.0.x11_surface() {
            let _ = x11.set_suspended(false);
        }
        // out of its thumbnail (⌘H-hidden windows have none: from the app icon)
        let target = match self.ipc.dock_window_tiles.get(&window.ipc_id().0).copied() {
            Some(tile) if !was_hidden => self.thumbnail_target(&window, tile),
            _ => self.dock_target(&window),
        };
        if let Some(t) = window.user_data().get::<ThumbPath>() {
            if let Some(p) = t.0.borrow_mut().take() {
                let _ = std::fs::remove_file(p);
            }
        }
        let loc = window
            .user_data()
            .get::<RefCell<Point<i32, Logical>>>()
            .map(|p| *p.borrow())
            .unwrap_or_default();
        let now = self.clock.now().into();
        match self.make_transient_to(
            &window,
            loc,
            TransientKind::Restore {
                window: window.clone(),
                location: loc,
            },
            Spring::new(1.0, 0.0, SpringParams::GENIE, now),
            target,
        ) {
            // the window is mapped when the reverse genie finishes (tick_effects)
            Some(t) => self.fx.transients.push(t),
            None => {
                self.space.map_element(window.clone(), loc, true);
                self.refit_all_windows();
                self.stage_on_window_restored(&window);
                self.focus_window(&window);
            }
        }
    }

    /// Start the close animation for a window that is about to go away.
    pub fn animate_close(&mut self, window: &WindowElement) {
        let Some(loc) = self.space.element_location(window) else { return };
        let now = self.clock.now().into();
        if let Some(t) = self.make_transient(
            window,
            loc,
            TransientKind::Close,
            Spring::new(0.0, 1.0, SpringParams::SMOOTH, now),
        ) {
            self.fx.transients.push(t);
        }
    }

    pub fn focus_window(&mut self, window: &WindowElement) {
        if self.stage_reveal_and_focus(window) {
            return;
        }
        self.focus_window_internal(window);
    }

    /// Focus follows mouse: give keyboard focus to the window under the
    /// pointer without raising it.
    pub fn focus_under_pointer(&mut self, pos: smithay::utils::Point<f64, smithay::utils::Logical>) {
        if self.mc.is_some() || self.switcher.is_some() || self.pointer.is_grabbed() || self.is_locked() {
            return;
        }
        let Some((window, _)) = self.space.element_under(pos).map(|(w, l)| (w.clone(), l)) else { return };
        if self.focused_window().as_ref() == Some(&window) {
            return;
        }
        let serial = SERIAL_COUNTER.next_serial();
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, Some(window.clone().into()), serial);
        }
        for w in self.space.elements() {
            smithay::desktop::space::SpaceElement::set_activate(w, w == &window);
            if let Some(t) = w.0.toplevel() {
                t.send_pending_configure();
            }
        }
    }

    /// Give focus to the topmost remaining window (after close/minimize).
    pub fn focus_topmost(&mut self) {
        if self.is_locked() {
            return;
        }
        let top = self.space.elements().next_back().cloned();
        match top {
            Some(w) => self.focus_window(&w),
            None => {
                let serial = SERIAL_COUNTER.next_serial();
                if let Some(keyboard) = self.seat.get_keyboard() {
                    keyboard.set_focus(self, None, serial);
                }
            }
        }
    }

    /// Currently keyboard-focused toplevel window, if any.
    pub fn focused_window(&self) -> Option<WindowElement> {
        let keyboard = self.seat.get_keyboard()?;
        let focus = keyboard.current_focus()?;
        self.space
            .elements()
            .find(|w| {
                use smithay::wayland::seat::WaylandFocus;
                match (w.wl_surface(), focus.wl_surface()) {
                    (Some(a), Some(b)) => *a == *b,
                    _ => false,
                }
            })
            .cloned()
    }

    /// Cycle focus through windows of the same application (Cmd+`).
    pub fn cycle_app_windows(&mut self) {
        let Some(current) = self.focused_window() else { return };
        let app = current.app_id();
        // bottom-most window of the same app goes to the top
        let next = self
            .space
            .elements()
            .find(|w| **w != current && w.app_id() == app)
            .cloned();
        if let Some(next) = next {
            self.focus_window(&next);
        }
    }

    /// Start an interactive resize from our server-side resize border.
    pub fn start_ssd_resize(
        &mut self,
        window: &WindowElement,
        seat: &Seat<Self>,
        serial: Serial,
        edges: ResizeEdge,
    ) {
        let Some(pointer) = seat.get_pointer() else { return };
        if !pointer.has_grab(serial) {
            return;
        }
        let Some(start_data) = pointer.grab_start_data() else { return };
        self.begin_resize_grab(window, start_data, serial, edges);
    }

    /// Resize `window` from `edges` while the pointer button of `start_data` is held.
    pub fn begin_resize_grab(
        &mut self,
        window: &WindowElement,
        start_data: smithay::input::pointer::GrabStartData<Self>,
        serial: Serial,
        edges: ResizeEdge,
    ) {
        let pointer = self.pointer.clone();
        let Some(surface) = window.wl_surface().map(|s| s.into_owned()) else {
            return;
        };
        let Some(initial_window_location) = self.space.element_location(window) else {
            return;
        };
        let initial_window_size = window.client_geometry().size;
        with_states(&surface, |states| {
            states
                .data_map
                .insert_if_missing(|| RefCell::new(SurfaceData::default()));
            states
                .data_map
                .get::<RefCell<SurfaceData>>()
                .unwrap()
                .borrow_mut()
                .resize_state = ResizeState::Resizing(ResizeData {
                edges,
                initial_window_location,
                initial_window_size,
            });
        });
        // a manual resize ends tiling
        window.decoration_state().tile = None;
        let grab = PointerResizeSurfaceGrab {
            start_data,
            window: window.clone(),
            edges,
            initial_window_location,
            initial_window_size,
            last_window_size: initial_window_size,
        };
        pointer.set_grab(self, grab, serial, Focus::Clear);
    }
}

// ---- ⌘H hide / force quit ---------------------------------------------------

impl<BackendData: Backend> OrchardState<BackendData> {
    /// ⌘H: take every window of `app_id` on the current Space off screen. Unlike
    /// minimizing there is no genie and no Dock thumbnail; activating the app
    /// (Dock, ⌘Tab) brings them all back.
    pub fn hide_app(&mut self, app_id: &str) {
        let windows: Vec<WindowElement> = self.space.elements().filter(|w| w.app_id() == app_id).cloned().collect();
        for w in windows {
            let loc = self.space.element_location(&w).unwrap_or_default();
            w.user_data().insert_if_missing(|| RefCell::new(Point::<i32, Logical>::default()));
            if let Some(p) = w.user_data().get::<RefCell<Point<i32, Logical>>>() {
                *p.borrow_mut() = loc;
            }
            w.decoration_state().hidden = true;
            self.space.unmap_elem(&w);
            self.minimized.retain(|m| m != &w);
            self.minimized.push(w.clone());
            self.stage_on_window_removed(&w);
        }
        if self.is_tiling_mode() {
            self.retile_all();
        }
        self.focus_topmost();
    }

    /// ⌥⌘H: hide every app except the focused one.
    pub fn hide_others(&mut self) {
        let keep = self.focused_window().map(|w| w.app_id());
        let mut apps: Vec<String> = self.space.elements().map(|w| w.app_id()).collect();
        apps.sort();
        apps.dedup();
        for app in apps.into_iter().filter(|a| Some(a) != keep.as_ref()) {
            self.hide_app(&app);
        }
    }

    /// Bring back the hidden windows of `app_id`; true if there were any.
    pub fn unhide_app(&mut self, app_id: &str) -> bool {
        let hidden: Vec<WindowElement> = self
            .minimized
            .iter()
            .filter(|w| w.alive() && w.app_id() == app_id && w.decoration_state().hidden)
            .cloned()
            .collect();
        for w in &hidden {
            self.minimized.retain(|m| m != w);
            w.decoration_state().hidden = false;
            let loc = w
                .user_data()
                .get::<RefCell<Point<i32, Logical>>>()
                .map(|p| *p.borrow())
                .unwrap_or_default();
            self.space.map_element(w.clone(), loc, true);
            self.stage_on_window_restored(w);
        }
        if let Some(top) = hidden.last() {
            if self.is_tiling_mode() {
                self.retile_all();
            }
            self.focus_window(top);
        }
        !hidden.is_empty()
    }

    /// ⌥⌘⇧⎋: kill the frontmost app's process (for apps that stopped responding).
    pub fn force_quit_focused(&mut self) {
        let Some(window) = self.focused_window() else { return };
        let app = window.app_id();
        let mut pid: Option<u32> = None;
        match window.0.underlying_surface() {
            WindowSurface::Wayland(t) => {
                if let Some(client) = smithay::reexports::wayland_server::Resource::client(t.wl_surface()) {
                    pid = client.get_credentials(&self.display_handle).ok().map(|c| c.pid as u32);
                    if pid.is_none() {
                        // no credentials: at least cut its connection
                        self.display_handle
                            .backend_handle()
                            .kill_client(client.id(), smithay::reexports::wayland_server::backend::DisconnectReason::ConnectionClosed);
                    }
                }
            }
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(x) => pid = x.pid(),
        }
        let Some(pid) = pid.filter(|p| *p > 1 && *p != std::process::id()) else {
            tracing::warn!(app, "force quit: no process id");
            return;
        };
        if self.ipc.is_shell_pid(pid) {
            tracing::warn!("force quit: refusing to kill orchard-shell");
            return;
        }
        tracing::warn!(app, pid, "force quit");
        // SAFETY: plain syscall; pid was checked above (> 1, not us, not the shell)
        if unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) } != 0 {
            tracing::warn!(pid, "force quit failed: {}", std::io::Error::last_os_error());
        }
    }
}

// ---- Super + mouse (KWin's Meta + mouse actions) ------------------------------

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

/// Which edges a Super+right-drag resizes, like KWin: the window is split
/// into thirds; corners resize diagonally, the top/bottom middle vertically,
/// and the middle row horizontally from the nearer side.
pub fn modifier_resize_edges(rect: Rectangle<f64, Logical>, pos: Point<f64, Logical>) -> ResizeEdge {
    let x = pos.x - rect.loc.x;
    let y = pos.y - rect.loc.y;
    let (w, h) = (rect.size.w, rect.size.h);
    let left = x < w / 3.0;
    let right = x >= 2.0 * w / 3.0;
    let top = y < h / 3.0;
    let bottom = y >= 2.0 * h / 3.0;
    let horizontal = if left {
        ResizeEdge::LEFT
    } else if right {
        ResizeEdge::RIGHT
    } else {
        ResizeEdge::NONE
    };
    if top {
        ResizeEdge::TOP | horizontal
    } else if bottom {
        ResizeEdge::BOTTOM | horizontal
    } else if x < w / 2.0 {
        ResizeEdge::LEFT
    } else {
        ResizeEdge::RIGHT
    }
}

impl<BackendData: Backend> OrchardState<BackendData> {
    /// A mouse button was pressed while Super is held: left drags the window
    /// under the pointer, right resizes it, middle raises or lowers it.
    /// Returns true when the click was used (and must not reach the client).
    pub fn modifier_click(&mut self, button: u32, serial: Serial) -> bool {
        use smithay::input::pointer::{CursorIcon, CursorImageStatus, GrabStartData};
        if !matches!(button, BTN_LEFT | BTN_RIGHT | BTN_MIDDLE)
            || self.pointer.is_grabbed()
            || self.is_locked()
            || self.mc.is_some()
        {
            return false;
        }
        let pos = self.pointer.current_location();
        let Some((window, loc)) = self.space.element_under(pos).map(|(w, l)| (w.clone(), l)) else {
            return false;
        };
        if window.is_fullscreen() {
            return false;
        }
        let rect = Rectangle::new(loc, window.geometry().size).to_f64();
        tracing::debug!(button, x = rect.loc.x, y = rect.loc.y, w = rect.size.w, h = rect.size.h, "super click");

        if button == BTN_MIDDLE {
            // raise, or lower if it already is the top window
            let top = self.space.elements().next_back().cloned();
            if top.as_ref() == Some(&window) {
                let others: Vec<WindowElement> = self.space.elements().filter(|w| **w != window).cloned().collect();
                for w in &others {
                    self.space.raise_element(w, false);
                }
                self.focus_topmost();
            } else {
                self.focus_window(&window);
            }
            return true;
        }

        self.focus_window(&window);
        let start_data = GrabStartData {
            focus: None,
            button,
            location: pos,
        };
        if button == BTN_LEFT {
            let Some(initial) = self.space.element_location(&window) else { return true };
            self.cursor_status = CursorImageStatus::Named(CursorIcon::Move);
            let grab = crate::shell::PointerMoveSurfaceGrab::new(start_data, window, initial);
            let pointer = self.pointer.clone();
            pointer.set_grab(self, grab, serial, Focus::Clear);
        } else {
            let edges = modifier_resize_edges(rect, pos);
            self.cursor_status = CursorImageStatus::Named(crate::shell::resize_cursor(edges));
            self.begin_resize_grab(&window, start_data, serial, edges);
        }
        true
    }
}

#[cfg(test)]
mod modifier_tests {
    use super::*;

    #[test]
    fn resize_edges_follow_kwin_thirds() {
        let r = Rectangle::<f64, Logical>::new((100.0, 100.0).into(), (300.0, 300.0).into());
        let at = |x: f64, y: f64| modifier_resize_edges(r, (100.0 + x, 100.0 + y).into());
        assert_eq!(at(10.0, 10.0), ResizeEdge::TOP_LEFT);
        assert_eq!(at(150.0, 10.0), ResizeEdge::TOP);
        assert_eq!(at(290.0, 10.0), ResizeEdge::TOP_RIGHT);
        assert_eq!(at(10.0, 150.0), ResizeEdge::LEFT);
        assert_eq!(at(140.0, 150.0), ResizeEdge::LEFT);
        assert_eq!(at(160.0, 150.0), ResizeEdge::RIGHT);
        assert_eq!(at(150.0, 290.0), ResizeEdge::BOTTOM);
        assert_eq!(at(290.0, 290.0), ResizeEdge::BOTTOM_RIGHT);
        assert_eq!(at(10.0, 290.0), ResizeEdge::BOTTOM_LEFT);
    }
}

// ---- minimized-window thumbnails ----------------------------------------------

/// Long side of a Dock thumbnail picture (px).
const THUMB_PX: u32 = 256;

/// Path of a minimized window's thumbnail picture.
struct ThumbPath(RefCell<Option<String>>);

pub fn thumb_path_of(window: &WindowElement) -> Option<String> {
    window.user_data().get::<ThumbPath>().and_then(|t| t.0.borrow().clone())
}

/// Where the Dock draws a minimized window inside its tile (the same rule
/// orchard-shell's dock.rs uses): aspect-fit, centered, a small inset.
pub fn thumb_rect_in_tile(tile: Rectangle<f64, Logical>, w: f64, h: f64) -> Rectangle<f64, Logical> {
    let inset = tile.size.w * 0.06;
    let (aw, ah) = (tile.size.w - 2.0 * inset, tile.size.h - 2.0 * inset);
    let k = (aw / w.max(1.0)).min(ah / h.max(1.0));
    let (fw, fh) = (w * k, h * k);
    Rectangle::new(
        (tile.loc.x + (tile.size.w - fw) / 2.0, tile.loc.y + (tile.size.h - fh) / 2.0).into(),
        (fw, fh).into(),
    )
}
