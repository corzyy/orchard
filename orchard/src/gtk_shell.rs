//! `gtk_shell1`: GTK 3 and GTK 4 tell the compositor where a window's
//! `GMenuModel` menu bar lives on D-Bus (`org.gtk.Menus` / `org.gtk.Actions`),
//! so the menu bar can show it. Announcing the `global_menu_bar` capability
//! makes GtkApplication windows drop their in-window menu bar.
//!
//! Only version 4 is offered: from version 5 on GTK hands title-bar double /
//! right / middle clicks to the compositor (`titlebar_gesture`), and GTK's own
//! handling of those is what libadwaita header bars expect here.

use std::sync::Mutex;

use smithay::{
    reexports::wayland_server::{
        protocol::wl_surface::WlSurface, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
    },
    wayland::compositor::with_states,
};

use crate::state::{Backend, OrchardState};

#[allow(dead_code, non_upper_case_globals, non_camel_case_types, unused_imports, clippy::all)]
pub mod protocol {
    use smithay::reexports::wayland_server;
    use smithay::reexports::wayland_server::protocol::*;
    pub mod __interfaces {
        use smithay::reexports::wayland_server::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("../protocols/gtk-shell.xml");
    }
    use self::__interfaces::*;
    wayland_scanner::generate_server_code!("../protocols/gtk-shell.xml");
}

use protocol::{gtk_shell1, gtk_surface1};

const VERSION: u32 = 4;
/// `gtk_shell1.capability.global_menu_bar` (GTK tests it as a bit)
const GLOBAL_MENU_BAR: u32 = 2;

/// Where a GTK window's menus live on the session bus.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GtkMenu {
    /// unique bus name of the app
    pub bus: String,
    /// `org.gtk.Menus` of the menu bar
    pub menubar: String,
    /// `org.gtk.Actions` for `app.*` actions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// `org.gtk.Actions` for `win.*` actions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub win: Option<String>,
}

#[derive(Default)]
struct GtkMenuSlot(Mutex<Option<GtkMenu>>);

pub fn init<B: Backend + 'static>(dh: &DisplayHandle) {
    dh.create_global::<OrchardState<B>, gtk_shell1::GtkShell1, _>(VERSION, ());
}

/// The menu a GTK client announced for `surface`.
pub fn menu(surface: &WlSurface) -> Option<GtkMenu> {
    with_states(surface, |states| {
        states
            .data_map
            .get::<GtkMenuSlot>()
            .and_then(|a| a.0.lock().ok().and_then(|g| g.clone()))
    })
}

impl<B: Backend + 'static> GlobalDispatch<gtk_shell1::GtkShell1, ()> for OrchardState<B> {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<gtk_shell1::GtkShell1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let shell = data_init.init(resource, ());
        // the shell shows menu bars unless global menus are switched off
        let caps = if std::env::var_os("ORCHARD_NO_GLOBAL_MENU").is_some() { 0 } else { GLOBAL_MENU_BAR };
        shell.capabilities(caps);
    }
}

impl<B: Backend + 'static> Dispatch<gtk_shell1::GtkShell1, ()> for OrchardState<B> {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _shell: &gtk_shell1::GtkShell1,
        request: gtk_shell1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            gtk_shell1::Request::GetGtkSurface { gtk_surface, surface } => {
                data_init.init(gtk_surface, surface);
            }
            // startup notification goes through xdg-activation; no bell
            gtk_shell1::Request::SetStartupId { .. }
            | gtk_shell1::Request::SystemBell { .. }
            | gtk_shell1::Request::NotifyLaunch { .. } => {}
        }
    }
}

impl<B: Backend + 'static> Dispatch<gtk_surface1::GtkSurface1, WlSurface> for OrchardState<B> {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &gtk_surface1::GtkSurface1,
        request: gtk_surface1::Request,
        surface: &WlSurface,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let gtk_surface1::Request::SetDbusProperties {
            menubar_path,
            window_object_path,
            application_object_path,
            unique_bus_name,
            ..
        } = request
        else {
            // set_modal / present / request_focus: focus follows xdg-activation
            return;
        };
        if !surface.is_alive() {
            return;
        }
        let menu = match (unique_bus_name, menubar_path) {
            (Some(bus), Some(menubar)) if !bus.is_empty() && !menubar.is_empty() => Some(GtkMenu {
                bus,
                menubar,
                app: application_object_path.filter(|p| !p.is_empty()),
                win: window_object_path.filter(|p| !p.is_empty()),
            }),
            _ => None,
        };
        tracing::debug!(?menu, "gtk menu");
        with_states(surface, |states| {
            states.data_map.insert_if_missing_threadsafe(GtkMenuSlot::default);
            if let Some(slot) = states.data_map.get::<GtkMenuSlot>() {
                if let Ok(mut g) = slot.0.lock() {
                    *g = menu;
                }
            }
        });
    }
}
