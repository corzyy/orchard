//! `org_kde_kwin_blur`: lets Wayland clients (Konsole, Dolphin, Kitty, etc.)
//! request Orchard's live Kawase frosted-glass blur behind translucent windows.

use std::sync::Mutex;

use smithay::{
    backend::renderer::element::Id,
    reexports::wayland_server::{
        protocol::wl_surface::WlSurface, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
    },
    wayland::compositor::with_states,
};
use wayland_protocols_plasma::blur::server::{
    org_kde_kwin_blur::{self, OrgKdeKwinBlur},
    org_kde_kwin_blur_manager::{self, OrgKdeKwinBlurManager},
};

use crate::{
    shell::WindowElement,
    state::{Backend, OrchardState},
};

struct BlurState {
    enabled: bool,
    backdrop_id: Id,
}

/// Per-surface KDE blur state stored in `WlSurface`'s `data_map`.
pub struct SurfaceBlur(Mutex<BlurState>);

impl Default for SurfaceBlur {
    fn default() -> Self {
        Self(Mutex::new(BlurState {
            enabled: false,
            backdrop_id: Id::new(),
        }))
    }
}

pub fn init<B: Backend + 'static>(dh: &DisplayHandle) {
    dh.create_global::<OrchardState<B>, OrgKdeKwinBlurManager, _>(1, ());
}

/// If `window`'s main surface has requested KDE blur, return its stable backdrop `Id`.
pub fn window_blur_id(window: &WindowElement) -> Option<Id> {
    let surface = window.wl_surface()?;
    with_states(&surface, |states| {
        let sb = states.data_map.get::<SurfaceBlur>()?;
        let g = sb.0.lock().ok()?;
        g.enabled.then(|| g.backdrop_id.clone())
    })
}

fn set_enabled(surface: &WlSurface, enabled: bool) {
    if !surface.is_alive() {
        return;
    }
    with_states(surface, |states| {
        states.data_map.insert_if_missing_threadsafe(SurfaceBlur::default);
        if let Some(sb) = states.data_map.get::<SurfaceBlur>() {
            if let Ok(mut g) = sb.0.lock() {
                g.enabled = enabled;
            }
        }
    });
}

impl<B: Backend + 'static> GlobalDispatch<OrgKdeKwinBlurManager, ()> for OrchardState<B> {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<OrgKdeKwinBlurManager>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl<B: Backend + 'static> Dispatch<OrgKdeKwinBlurManager, ()> for OrchardState<B> {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _manager: &OrgKdeKwinBlurManager,
        request: org_kde_kwin_blur_manager::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            org_kde_kwin_blur_manager::Request::Create { id, surface } => {
                data_init.init(id, surface);
            }
            org_kde_kwin_blur_manager::Request::Unset { surface } => {
                set_enabled(&surface, false);
            }
            _ => {}
        }
    }
}

impl<B: Backend + 'static> Dispatch<OrgKdeKwinBlur, WlSurface> for OrchardState<B> {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _blur: &OrgKdeKwinBlur,
        request: org_kde_kwin_blur::Request,
        surface: &WlSurface,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            org_kde_kwin_blur::Request::Commit => {
                set_enabled(surface, true);
            }
            org_kde_kwin_blur::Request::SetRegion { .. } => {
                // Full-window blur region; applied on Commit.
            }
            org_kde_kwin_blur::Request::Release => {
                set_enabled(surface, false);
            }
            _ => {}
        }
    }
}
