//! Additional protocols for daily use: cursor-shape, idle notify / inhibit,
//! ext-foreign-toplevel-list and ext-session-lock.

use std::{cell::RefCell, collections::HashSet};

use smithay::{
    delegate_cursor_shape, delegate_foreign_toplevel_list, delegate_idle_inhibit, delegate_idle_notify,
    delegate_session_lock,
    output::Output,
    reexports::wayland_server::protocol::{wl_output::WlOutput, wl_surface::WlSurface},
    utils::IsAlive,
    wayland::{
        foreign_toplevel_list::{ForeignToplevelHandle, ForeignToplevelListHandler, ForeignToplevelListState},
        idle_inhibit::IdleInhibitHandler,
        idle_notify::{IdleNotifierHandler, IdleNotifierState},
        session_lock::{LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker},
    },
};

use crate::state::{Backend, OrchardState};

/// Per-output lock surface while the session is locked.
#[derive(Default)]
pub struct OutputLockSurface(pub RefCell<Option<LockSurface>>);

#[derive(Default)]
pub struct ProtocolState {
    pub idle_inhibitors: HashSet<WlSurface>,
    /// ext-foreign-toplevel handles: window, handle, last title, last app_id
    pub toplevels: Vec<(crate::shell::WindowElement, ForeignToplevelHandle, String, String)>,
    pub locked: bool,
    /// the ext-session-lock object holding the lock (one at a time)
    pub active_lock: Option<smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_v1::ExtSessionLockV1>,
    /// the locker Orchard started itself, and when (see `supervise_locker`)
    pub locker_child: Option<std::process::Child>,
    pub locker_started: Option<std::time::Instant>,
}

impl std::fmt::Debug for ProtocolState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProtocolState")
            .field("locked", &self.locked)
            .finish_non_exhaustive()
    }
}

// ---- cursor shape: everything happens in SeatHandler::cursor_image ----------
delegate_cursor_shape!(@<BackendData: Backend + 'static> OrchardState<BackendData>);

// ---- idle notify / inhibit -------------------------------------------------
impl<BackendData: Backend + 'static> IdleNotifierHandler for OrchardState<BackendData> {
    fn idle_notifier_state(&mut self) -> &mut IdleNotifierState<Self> {
        &mut self.idle_notifier_state
    }
}
delegate_idle_notify!(@<BackendData: Backend + 'static> OrchardState<BackendData>);

impl<BackendData: Backend + 'static> IdleInhibitHandler for OrchardState<BackendData> {
    fn inhibit(&mut self, surface: WlSurface) {
        self.protocols.idle_inhibitors.insert(surface);
        self.idle_notifier_state.set_is_inhibited(true);
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        self.protocols.idle_inhibitors.remove(&surface);
        self.protocols.idle_inhibitors.retain(|s| s.alive());
        let inhibited = !self.protocols.idle_inhibitors.is_empty();
        self.idle_notifier_state.set_is_inhibited(inhibited);
    }
}
delegate_idle_inhibit!(@<BackendData: Backend + 'static> OrchardState<BackendData>);

// ---- ext-foreign-toplevel-list ---------------------------------------------
impl<BackendData: Backend> ForeignToplevelListHandler for OrchardState<BackendData> {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelListState {
        &mut self.foreign_toplevel_list_state
    }
}
delegate_foreign_toplevel_list!(@<BackendData: Backend + 'static> OrchardState<BackendData>);

// ---- ext-session-lock ------------------------------------------------------
impl<BackendData: Backend> SessionLockHandler for OrchardState<BackendData> {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        use smithay::reexports::wayland_server::Resource;
        let lock = confirmation.ext_session_lock().clone();
        if self.protocols.active_lock.as_ref().is_some_and(|l| l.is_alive()) {
            // Only one lock at a time. Smithay runs `unlock` for *any* lock
            // object's unlock_and_destroy (even one that was refused), so the
            // second locker gets a protocol error: libwayland then stops
            // reading its requests and disconnects it. (Disconnecting it
            // directly here, inside its own dispatch, crashes libwayland.)
            tracing::warn!("rejecting a second session lock");
            drop(confirmation); // sends `finished`
            lock.post_error(
                smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_v1::Error::InvalidUnlock,
                "the session is already locked",
            );
            return;
        }
        tracing::info!("session locked");
        self.protocols.active_lock = Some(lock);
        self.protocols.locked = true;
        self.fx.unlock_fade = None;
        // close transient UI
        self.mc = None;
        self.switcher_cancel();
        // the next repaint shows only lock surfaces over the wallpaper; confirm right away
        confirmation.lock();
        for output in self.space.outputs().cloned().collect::<Vec<_>>() {
            self.backend_data.reset_buffers(&output);
        }
        self.enforce_lock_focus();
    }

    fn unlock(&mut self) {
        tracing::info!("session unlocked");
        self.protocols.locked = false;
        self.protocols.active_lock = None;
        self.prefs_rt.idle_locked = false;
        for output in self.space.outputs() {
            if let Some(slot) = output.user_data().get::<OutputLockSurface>() {
                slot.0.borrow_mut().take();
            }
        }
        // the lock backdrop dissolves into the desktop (render.rs)
        self.fx.unlock_fade = Some(self.clock.now().into());
        for output in self.space.outputs().cloned().collect::<Vec<_>>() {
            self.backend_data.reset_buffers(&output);
        }
        self.focus_topmost();
    }

    fn new_surface(&mut self, surface: LockSurface, wl_output: WlOutput) {
        let Some(output) = Output::from_resource(&wl_output) else { return };
        let Some(geo) = self.space.output_geometry(&output) else { return };
        surface.with_pending_state(|s| {
            s.size = Some((geo.size.w as u32, geo.size.h as u32).into());
        });
        surface.send_configure();
        output.user_data().insert_if_missing(OutputLockSurface::default);
        *output.user_data().get::<OutputLockSurface>().unwrap().0.borrow_mut() = Some(surface);
        // keyboard focus goes to the lock surface
        self.enforce_lock_focus();
    }
}
delegate_session_lock!(@<BackendData: Backend + 'static> OrchardState<BackendData>);

/// The lock surface shown on an output (no state borrow needed).
pub fn lock_surface_of(output: &Output) -> Option<WlSurface> {
    output
        .user_data()
        .get::<OutputLockSurface>()
        .and_then(|s| s.0.borrow().as_ref().map(|l| l.wl_surface().clone()))
}

impl<BackendData: Backend> OrchardState<BackendData> {
    pub fn is_locked(&self) -> bool {
        self.protocols.locked
    }

    /// While locked, keyboard focus is a live lock surface (the one under the
    /// pointer, else the first), or nothing until the locker has one.
    pub fn enforce_lock_focus(&mut self) {
        use smithay::wayland::seat::WaylandFocus;
        if !self.protocols.locked {
            return;
        }
        let Some(keyboard) = self.seat.get_keyboard() else { return };
        let surfaces: Vec<WlSurface> = self
            .space
            .outputs()
            .filter_map(lock_surface_of)
            .filter(|s| s.alive())
            .collect();
        let current = keyboard.current_focus().and_then(|f| f.wl_surface().map(|s| s.into_owned()));
        if current.as_ref().is_some_and(|c| surfaces.contains(c)) {
            return;
        }
        let pointer = self.pointer.current_location();
        let preferred = self
            .space
            .output_under(pointer)
            .next()
            .and_then(lock_surface_of)
            .filter(|s| s.alive())
            .or_else(|| surfaces.first().cloned());
        if current.is_none() && preferred.is_none() {
            return;
        }
        let serial = smithay::utils::SERIAL_COUNTER.next_serial();
        keyboard.set_focus(self, preferred.map(crate::focus::KeyboardFocusTarget::Surface), serial);
    }

    /// Start the configured locker (see `settings::locker`), tracked so the
    /// watchdog can tell whether it is still coming up.
    pub fn spawn_locker(&mut self) {
        let command = crate::settings::locker();
        tracing::info!(command, "starting locker");
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg(&command);
        self.child_env(&mut cmd);
        match cmd.spawn() {
            Ok(child) => {
                if let Some(mut old) = self.protocols.locker_child.replace(child) {
                    let _ = old.try_wait();
                }
            }
            Err(e) => tracing::warn!("starting locker {command:?} failed: {e}"),
        }
        self.protocols.locker_started = Some(std::time::Instant::now());
    }

    /// Lock the session from the very first frame (login without a greeter:
    /// `orchard-session --locked`). The desktop keeps loading behind the lock.
    pub fn lock_at_startup(&mut self) {
        tracing::info!("starting locked");
        self.protocols.locked = true;
        self.spawn_locker();
    }

    /// Called every second. A locked session must never be left without a
    /// locker: if the lock client died (crash, OOM, killed) the session stays
    /// locked, and a new locker is started.
    pub fn supervise_locker(&mut self) {
        // reap our locker when it exits
        let running = match self.protocols.locker_child.as_mut().map(|c| c.try_wait()) {
            Some(Ok(None)) => true,
            Some(_) => {
                self.protocols.locker_child = None;
                false
            }
            None => false,
        };
        use smithay::reexports::wayland_server::Resource;
        if !self.protocols.locked || self.protocols.active_lock.as_ref().is_some_and(|l| l.is_alive()) {
            return;
        }
        let since = self.protocols.locker_started.map(|t| t.elapsed()).unwrap_or(std::time::Duration::MAX);
        if running && since < std::time::Duration::from_secs(5) {
            return; // still starting up
        }
        if since < std::time::Duration::from_secs(2) {
            return; // don't spin on a locker that exits right away
        }
        if running {
            if let Some(mut c) = self.protocols.locker_child.take() {
                tracing::warn!("locker did not lock within 5 s; restarting it");
                let _ = c.kill();
                let _ = c.wait();
            }
        } else {
            tracing::warn!("session is locked but no locker is running; restarting it");
        }
        self.spawn_locker();
    }

    pub fn lock_surface_for(&self, output: &Output) -> Option<LockSurface> {
        output
            .user_data()
            .get::<OutputLockSurface>()
            .and_then(|s| s.0.borrow().clone())
    }

    /// Keep ext-foreign-toplevel handles in sync with our windows.
    pub fn sync_foreign_toplevels(&mut self) {
        let windows: Vec<_> = self
            .space
            .elements()
            .chain(self.minimized.iter())
            .cloned()
            .chain(
                self.spaces
                    .outputs
                    .values()
                    .flat_map(|o| o.slots.iter())
                    .flat_map(|s| s.windows.iter().map(|(w, _)| w.clone()).chain(s.fullscreen.clone())),
            )
            .filter(|w| w.alive())
            .collect();
        // closed windows
        let mut toplevels = std::mem::take(&mut self.protocols.toplevels);
        toplevels.retain(|(w, handle, _, _)| {
            let keep = w.alive() && windows.contains(w);
            if !keep {
                self.foreign_toplevel_list_state.remove_toplevel(handle);
            }
            keep
        });
        for w in windows {
            let title = w.title();
            let app_id = w.app_id();
            match toplevels.iter_mut().find(|(x, _, _, _)| *x == w) {
                Some((_, handle, t, a)) => {
                    if *t != title || *a != app_id {
                        handle.send_title(&title);
                        handle.send_app_id(&app_id);
                        handle.send_done();
                        *t = title;
                        *a = app_id;
                    }
                }
                None => {
                    let handle = self
                        .foreign_toplevel_list_state
                        .new_toplevel::<Self>(&title, &app_id);
                    toplevels.push((w, handle, title, app_id));
                }
            }
        }
        self.protocols.toplevels = toplevels;
    }
}
