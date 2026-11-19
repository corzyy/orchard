//! `org_kde_kwin_appmenu`: Qt (and Firefox) tell the compositor where a window's
//! menu lives on D-Bus (`com.canonical.dbusmenu`), so the menu bar can show it.

use std::sync::Mutex;

use smithay::{
    reexports::wayland_server::{
        protocol::wl_surface::WlSurface, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
    },
    wayland::compositor::with_states,
};
use wayland_protocols_plasma::appmenu::server::{
    org_kde_kwin_appmenu::{self, OrgKdeKwinAppmenu},
    org_kde_kwin_appmenu_manager::{self, OrgKdeKwinAppmenuManager},
};

use crate::state::{Backend, OrchardState};

/// D-Bus address of a surface's menu: (service name, object path).
#[derive(Default)]
pub struct AppMenuAddress(pub Mutex<Option<(String, String)>>);

pub fn init<B: Backend + 'static>(dh: &DisplayHandle) {
    dh.create_global::<OrchardState<B>, OrgKdeKwinAppmenuManager, _>(2, ());
}

/// The menu address a client registered for `surface`.
pub fn address(surface: &WlSurface) -> Option<(String, String)> {
    with_states(surface, |states| {
        states
            .data_map
            .get::<AppMenuAddress>()
            .and_then(|a| a.0.lock().ok().and_then(|g| g.clone()))
    })
}

impl<B: Backend + 'static> GlobalDispatch<OrgKdeKwinAppmenuManager, ()> for OrchardState<B> {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<OrgKdeKwinAppmenuManager>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl<B: Backend + 'static> Dispatch<OrgKdeKwinAppmenuManager, ()> for OrchardState<B> {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _manager: &OrgKdeKwinAppmenuManager,
        request: org_kde_kwin_appmenu_manager::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let org_kde_kwin_appmenu_manager::Request::Create { id, surface } = request {
            data_init.init(id, surface);
        }
    }
}

impl<B: Backend + 'static> Dispatch<OrgKdeKwinAppmenu, WlSurface> for OrchardState<B> {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _menu: &OrgKdeKwinAppmenu,
        request: org_kde_kwin_appmenu::Request,
        surface: &WlSurface,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let org_kde_kwin_appmenu::Request::SetAddress { service_name, object_path } = request {
            if !surface.is_alive() {
                return;
            }
            tracing::debug!(service_name, object_path, "app menu");
            with_states(surface, |states| {
                states.data_map.insert_if_missing_threadsafe(AppMenuAddress::default);
                if let Some(a) = states.data_map.get::<AppMenuAddress>() {
                    if let Ok(mut g) = a.0.lock() {
                        *g = Some((service_name, object_path));
                    }
                }
            });
        }
    }
}
