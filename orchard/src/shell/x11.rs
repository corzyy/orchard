use std::{cell::RefCell, os::unix::io::OwnedFd};

use smithay::{
    desktop::{space::SpaceElement, Window},
    input::pointer::Focus,
    utils::{Logical, Rectangle, SERIAL_COUNTER},
    wayland::{
        compositor::with_states,
        selection::{
            data_device::{
                clear_data_device_selection, current_data_device_selection_userdata,
                request_data_device_client_selection, set_data_device_selection,
            },
            primary_selection::{
                clear_primary_selection, current_primary_selection_userdata,
                request_primary_client_selection, set_primary_selection,
            },
            SelectionTarget,
        },
        xwayland_shell::{XWaylandShellHandler, XWaylandShellState},
    },
    xwayland::{
        xwm::{Reorder, ResizeEdge as X11ResizeEdge, XwmId},
        X11Surface, X11Wm, XwmHandler,
    },
};
use tracing::{error, trace};

use crate::{focus::KeyboardFocusTarget, state::Backend, OrchardState};

use super::{
    place_new_window, FullscreenSurface, PointerMoveSurfaceGrab, PointerResizeSurfaceGrab, ResizeData,
    ResizeState, SurfaceData, TouchMoveSurfaceGrab, WindowElement,
};

#[derive(Debug, Default)]
struct OldGeometry(RefCell<Option<Rectangle<i32, Logical>>>);
impl OldGeometry {
    pub fn save(&self, geo: Rectangle<i32, Logical>) {
        *self.0.borrow_mut() = Some(geo);
    }

    pub fn restore(&self) -> Option<Rectangle<i32, Logical>> {
        self.0.borrow_mut().take()
    }
}

impl<BackendData: Backend> XWaylandShellHandler for OrchardState<BackendData> {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }
}

impl<BackendData: Backend> XwmHandler for OrchardState<BackendData> {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        self.xwm.as_mut().unwrap()
    }

    fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {}
    fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let _ = window.set_mapped(true);
        let window = WindowElement(Window::new_x11_window(window));
        place_new_window(&mut self.space, self.pointer.current_location(), &window, true);
        let Some(bbox) = self.space.element_bbox(&window) else { return };
        let Some(xsurface) = window.0.x11_surface() else {
            return;
        };
        let _ = xsurface.configure(Some(bbox));
        window.set_ssd(!xsurface.is_decorated());
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        let location = window.geometry().loc;
        let window = WindowElement(Window::new_x11_window(window));
        self.space.map_element(window, location, true);
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        let maybe = self
            .space
            .elements()
            .find(|e| matches!(e.0.x11_surface(), Some(w) if w == &window))
            .cloned();
        if let Some(elem) = maybe {
            if !window.is_override_redirect() {
                self.animate_close(&elem);
            }
            self.space.unmap_elem(&elem);
            if !window.is_override_redirect() && self.is_tiling_mode() {
                self.dwindle.remove_everywhere(elem.ipc_id().0);
                self.retile_all();
            }
        }
        self.minimized
            .retain(|e| !matches!(e.0.x11_surface(), Some(w) if w == &window));
        if !window.is_override_redirect() {
            let _ = window.set_mapped(false);
            self.focus_topmost();
        }
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.minimized
            .retain(|e| !matches!(e.0.x11_surface(), Some(w) if w == &window));
    }

    fn configure_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        _x: Option<i32>,
        _y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        // we just set the new size, but don't let windows move themselves around freely
        let mut geo = window.geometry();
        if let Some(w) = w {
            geo.size.w = w as i32;
        }
        if let Some(h) = h {
            geo.size.h = h as i32;
        }
        let _ = window.configure(geo);
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        geometry: Rectangle<i32, Logical>,
        _above: Option<u32>,
    ) {
        let Some(elem) = self
            .space
            .elements()
            .find(|e| matches!(e.0.x11_surface(), Some(w) if w == &window))
            .cloned()
        else {
            return;
        };
        self.space.map_element(elem, geometry.loc, false);
        // TODO: We don't properly handle the order of override-redirect windows here,
        //       they are always mapped top and then never reordered.
    }

    fn maximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        self.maximize_request_x11(&window);
    }

    fn unmaximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let Some(elem) = self
            .space
            .elements()
            .find(|e| matches!(e.0.x11_surface(), Some(w) if w == &window))
            .cloned()
        else {
            return;
        };

        let _ = window.set_maximized(false);
        if let Some(old_geo) = window
            .user_data()
            .get::<OldGeometry>()
            .and_then(|data| data.restore())
        {
            let _ = window.configure(old_geo);
            self.animate_frame(&elem, old_geo);
            self.space.map_element(elem, old_geo.loc, false);
        }
    }

    fn fullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(elem) = self
            .space
            .elements()
            .find(|e| matches!(e.0.x11_surface(), Some(w) if w == &window))
        {
            let outputs_for_window = self.space.outputs_for_element(elem);
            // the window may not be mapped yet: use the primary output instead
            let Some(output) = outputs_for_window.first().or_else(|| self.space.outputs().next()) else {
                return;
            };
            let Some(geometry) = self.space.output_geometry(output) else { return };

            let _ = window.set_fullscreen(true);
            elem.set_ssd(false);
            let _ = window.configure(geometry);
            output.user_data().insert_if_missing(FullscreenSurface::default);
            if let Some(f) = output.user_data().get::<FullscreenSurface>() {
                f.set(elem.clone());
            }
            trace!("Fullscreening: {:?}", elem);
        }
    }

    fn unfullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(elem) = self
            .space
            .elements()
            .find(|e| matches!(e.0.x11_surface(), Some(w) if w == &window))
        {
            let _ = window.set_fullscreen(false);
            elem.set_ssd(!window.is_decorated());
            if let Some(output) = self.space.outputs().find(|o| {
                o.user_data()
                    .get::<FullscreenSurface>()
                    .and_then(|f| f.get())
                    .map(|w| &w == elem)
                    .unwrap_or(false)
            }) {
                trace!("Unfullscreening: {:?}", elem);
                if let Some(f) = output.user_data().get::<FullscreenSurface>() {
                    f.clear();
                }
                let _ = window.configure(self.space.element_bbox(elem));
                self.backend_data.reset_buffers(output);
            }
        }
    }

    fn minimize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let elem = self
            .space
            .elements()
            .find(|e| matches!(e.0.x11_surface(), Some(w) if w == &window))
            .cloned();
        if let Some(elem) = elem {
            self.minimize_window(&elem);
        }
    }

    fn unminimize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let elem = self
            .minimized
            .iter()
            .find(|e| matches!(e.0.x11_surface(), Some(w) if w == &window))
            .cloned();
        if let Some(elem) = elem {
            self.unminimize(Some(&elem));
        }
    }

    fn resize_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32, edges: X11ResizeEdge) {
        // luckily anvil only supports one seat anyway...
        // a client may ask without a button held: then there is nothing to grab
        let Some(start_data) = self.pointer.grab_start_data() else { return };

        let Some(element) = self
            .space
            .elements()
            .find(|e| matches!(e.0.x11_surface(), Some(w) if w == &window))
        else {
            return;
        };

        let geometry = element.geometry();
        let Some(loc) = self.space.element_location(element) else { return };
        let (initial_window_location, initial_window_size) = (loc, geometry.size);

        let Some(wl_surface) = element.wl_surface() else { return };
        with_states(&wl_surface, move |states| {
            if let Some(data) = states.data_map.get::<RefCell<SurfaceData>>() {
                data.borrow_mut().resize_state = ResizeState::Resizing(ResizeData {
                    edges: edges.into(),
                    initial_window_location,
                    initial_window_size,
                });
            }
        });

        // a manual resize ends tiling
        element.decoration_state().tile = None;
        let grab = PointerResizeSurfaceGrab {
            start_data,
            window: element.clone(),
            edges: edges.into(),
            initial_window_location,
            initial_window_size,
            last_window_size: initial_window_size,
        };

        let pointer = self.pointer.clone();
        pointer.set_grab(self, grab, SERIAL_COUNTER.next_serial(), Focus::Clear);
    }

    fn move_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32) {
        self.move_request_x11(&window)
    }

    fn allow_selection_access(&mut self, xwm: XwmId, _selection: SelectionTarget) -> bool {
        if let Some(keyboard) = self.seat.get_keyboard() {
            // check that an X11 window is focused
            if let Some(KeyboardFocusTarget::Window(w)) = keyboard.current_focus() {
                if let Some(surface) = w.x11_surface() {
                    if surface.xwm_id() == Some(xwm) {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn send_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_type: String, fd: OwnedFd) {
        match selection {
            SelectionTarget::Clipboard => {
                if let Err(err) = request_data_device_client_selection(&self.seat, mime_type, fd) {
                    error!(?err, "Failed to request current wayland clipboard for Xwayland",);
                }
            }
            SelectionTarget::Primary => {
                if let Err(err) = request_primary_client_selection(&self.seat, mime_type, fd) {
                    error!(
                        ?err,
                        "Failed to request current wayland primary selection for Xwayland",
                    );
                }
            }
        }
    }

    fn new_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        trace!(?selection, ?mime_types, "Got Selection from X11",);
        // TODO check, that focused windows is X11 window before doing this
        match selection {
            SelectionTarget::Clipboard => {
                set_data_device_selection(&self.display_handle, &self.seat, mime_types, ())
            }
            SelectionTarget::Primary => {
                set_primary_selection(&self.display_handle, &self.seat, mime_types, ())
            }
        }
    }

    fn cleared_selection(&mut self, _xwm: XwmId, selection: SelectionTarget) {
        match selection {
            SelectionTarget::Clipboard => {
                if current_data_device_selection_userdata(&self.seat).is_some() {
                    clear_data_device_selection(&self.display_handle, &self.seat)
                }
            }
            SelectionTarget::Primary => {
                if current_primary_selection_userdata(&self.seat).is_some() {
                    clear_primary_selection(&self.display_handle, &self.seat)
                }
            }
        }
    }
}

impl<BackendData: Backend> OrchardState<BackendData> {
    pub fn maximize_request_x11(&mut self, window: &X11Surface) {
        let Some(elem) = self
            .space
            .elements()
            .find(|e| matches!(e.0.x11_surface(), Some(w) if w == window))
            .cloned()
        else {
            return;
        };

        let Some(old_geo) = self.space.element_bbox(&elem) else { return };
        // fill the usable area (menu bar and Dock stay visible), like zoom does
        let Some(area) = self.usable_area_for(&elem) else { return };
        let header = if elem.is_ssd() { crate::shell::ssd::HEADER_BAR_HEIGHT } else { 0 };
        let client = Rectangle::new(
            area.loc + smithay::utils::Point::from((0, header)),
            (area.size.w, (area.size.h - header).max(1)).into(),
        );

        if elem.decoration_state().tile.take().is_none() {
            let loc = self.space.element_location(&elem).unwrap_or_default();
            elem.decoration_state().restore_geometry = Some(Rectangle::new(loc, elem.client_geometry().size));
        }
        let _ = window.set_maximized(true);
        let _ = window.configure(client);
        self.animate_frame(&elem, area);
        window.user_data().insert_if_missing(OldGeometry::default);
        if let Some(g) = window.user_data().get::<OldGeometry>() {
            g.save(old_geo);
        }
        self.space.map_element(elem, area.loc, false);
    }

    pub fn move_request_x11(&mut self, window: &X11Surface) {
        if let Some(touch) = self.seat.get_touch() {
            if let Some(start_data) = touch.grab_start_data() {
                let element = self
                    .space
                    .elements()
                    .find(|e| matches!(e.0.x11_surface(), Some(w) if w == window));

                if let Some(element) = element {
                    let Some(mut initial_window_location) = self.space.element_location(element) else { return };

                    // If surface is maximized then unmaximize it
                    if window.is_maximized() {
                        let _ = window.set_maximized(false);
                        let pos = start_data.location;
                        initial_window_location = (pos.x as i32, pos.y as i32).into();
                        if let Some(old_geo) = window
                            .user_data()
                            .get::<OldGeometry>()
                            .and_then(|data| data.restore())
                        {
                            let _ = window.configure(Rectangle::new(initial_window_location, old_geo.size));
                        }
                    }

                    let grab = TouchMoveSurfaceGrab {
                        start_data,
                        window: element.clone(),
                        initial_window_location,
                    };

                    touch.set_grab(self, grab, SERIAL_COUNTER.next_serial());
                    return;
                }
            }
        }

        // luckily anvil only supports one seat anyway...
        let Some(start_data) = self.pointer.grab_start_data() else {
            return;
        };

        let Some(element) = self
            .space
            .elements()
            .find(|e| matches!(e.0.x11_surface(), Some(w) if w == window))
        else {
            return;
        };

        let Some(initial_window_location) = self.space.element_location(element) else { return };
        // a zoomed / tiled window gets its old size back once it is actually dragged
        let grab = PointerMoveSurfaceGrab::new(start_data, element.clone(), initial_window_location);

        let pointer = self.pointer.clone();
        pointer.set_grab(self, grab, SERIAL_COUNTER.next_serial(), Focus::Clear);
    }
}
