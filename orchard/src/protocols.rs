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
        tracing::info!("session locked");
        self.protocols.locked = true;
        // close transient UI
        self.mc = None;
        self.switcher_cancel();
        // the next repaint shows only lock surfaces / black; confirm right away
        confirmation.lock();
        for output in self.space.outputs().cloned().collect::<Vec<_>>() {
            self.backend_data.reset_buffers(&output);
        }
        let serial = smithay::utils::SERIAL_COUNTER.next_serial();
        if let Some(k) = self.seat.get_keyboard() {
            k.set_focus(self, None, serial);
        }
    }

    fn unlock(&mut self) {
        tracing::info!("session unlocked");
        self.protocols.locked = false;
        for output in self.space.outputs() {
            if let Some(slot) = output.user_data().get::<OutputLockSurface>() {
                slot.0.borrow_mut().take();
            }
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
        // keyboard focus goes to the lock surface
        let serial = smithay::utils::SERIAL_COUNTER.next_serial();
        if let Some(k) = self.seat.get_keyboard() {
            k.set_focus(
                self,
                Some(crate::focus::KeyboardFocusTarget::Surface(surface.wl_surface().clone())),
                serial,
            );
        }
        *output.user_data().get::<OutputLockSurface>().unwrap().0.borrow_mut() = Some(surface);
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
