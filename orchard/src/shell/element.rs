use std::{borrow::Cow, time::Duration};

use smithay::{
    backend::renderer::{
        element::{
            memory::MemoryRenderBufferRenderElement, solid::SolidColorRenderElement,
            surface::WaylandSurfaceRenderElement, AsRenderElements, Kind,
        },
    },
    desktop::{
        space::SpaceElement, utils::OutputPresentationFeedback, Window, WindowSurface, WindowSurfaceType,
    },
    input::{
        pointer::{
            AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
            GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
            GestureSwipeUpdateEvent, MotionEvent, PointerTarget, RelativeMotionEvent,
        },
        touch::TouchTarget,
        Seat,
    },
    output::Output,
    reexports::{
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::protocol::wl_surface::WlSurface,
    },
    render_elements,
    utils::{user_data::UserDataMap, IsAlive, Logical, Physical, Point, Rectangle, Scale, Serial},
    wayland::{compositor::SurfaceData as WlSurfaceData, dmabuf::DmabufFeedback, seat::WaylandFocus},
};

use super::ssd::{decoration_hit, HEADER_BAR_HEIGHT, RESIZE_BORDER};
use crate::chrome::{self, theme, titlebar::TitlebarParams};
use crate::effects::rounded::{Corners, RoundedElement};
use smithay::backend::renderer::{element::{Element, Id}, gles::GlesRenderer};
use crate::{focus::PointerFocusTarget, state::Backend, OrchardState};

#[derive(Debug, Clone, PartialEq)]
pub struct WindowElement(pub Window);

impl WindowElement {
    pub fn surface_under(
        &self,
        location: Point<f64, Logical>,
        window_type: WindowSurfaceType,
    ) -> Option<(PointerFocusTarget, Point<i32, Logical>)> {
        let is_ssd = self.decoration_state().is_ssd;
        if is_ssd {
            let client = self.client_geometry();
            if decoration_hit(location, client.size.w, client.size.h).is_some() {
                return Some((PointerFocusTarget::SSD(SSD(self.clone())), Point::default()));
            }
        }
        let offset = if is_ssd {
            Point::from((0, HEADER_BAR_HEIGHT))
        } else {
            Point::default()
        };

        let surface_under = self.0.surface_under(location - offset.to_f64(), window_type);
        let (under, loc) = match self.0.underlying_surface() {
            WindowSurface::Wayland(_) => {
                surface_under.map(|(surface, loc)| (PointerFocusTarget::WlSurface(surface), loc))
            }
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(s) => {
                surface_under.map(|(_, loc)| (PointerFocusTarget::X11Surface(s.clone()), loc))
            }
        }?;
        Some((under, loc + offset))
    }

    pub fn with_surfaces<F>(&self, processor: F)
    where
        F: FnMut(&WlSurface, &WlSurfaceData),
    {
        self.0.with_surfaces(processor);
    }

    pub fn send_frame<T, F>(
        &self,
        output: &Output,
        time: T,
        throttle: Option<Duration>,
        primary_scan_out_output: F,
    ) where
        T: Into<Duration>,
        F: FnMut(&WlSurface, &WlSurfaceData) -> Option<Output> + Copy,
    {
        self.0.send_frame(output, time, throttle, primary_scan_out_output)
    }

    pub fn send_dmabuf_feedback<'a, P, F>(
        &self,
        output: &Output,
        primary_scan_out_output: P,
        select_dmabuf_feedback: F,
    ) where
        P: FnMut(&WlSurface, &WlSurfaceData) -> Option<Output> + Copy,
        F: Fn(&WlSurface, &WlSurfaceData) -> &'a DmabufFeedback + Copy,
    {
        self.0
            .send_dmabuf_feedback(output, primary_scan_out_output, select_dmabuf_feedback)
    }

    pub fn take_presentation_feedback<F1, F2>(
        &self,
        output_feedback: &mut OutputPresentationFeedback,
        primary_scan_out_output: F1,
        presentation_feedback_flags: F2,
    ) where
        F1: FnMut(&WlSurface, &WlSurfaceData) -> Option<Output> + Copy,
        F2: FnMut(&WlSurface, &WlSurfaceData) -> wp_presentation_feedback::Kind + Copy,
    {
        self.0.take_presentation_feedback(
            output_feedback,
            primary_scan_out_output,
            presentation_feedback_flags,
        )
    }

    #[cfg(feature = "xwayland")]
    #[inline]
    pub fn is_x11(&self) -> bool {
        self.0.is_x11()
    }

    #[inline]
    pub fn is_wayland(&self) -> bool {
        self.0.is_wayland()
    }

    #[inline]
    pub fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        self.0.wl_surface()
    }

    #[inline]
    pub fn user_data(&self) -> &UserDataMap {
        self.0.user_data()
    }
}

impl IsAlive for WindowElement {
    #[inline]
    fn alive(&self) -> bool {
        self.0.alive()
    }
}

/// Cursor shown over our decorations: resize arrows on the border.
fn ssd_cursor(loc: Point<f64, Logical>, w: i32, h: i32) -> smithay::input::pointer::CursorImageStatus {
    use super::ssd::DecorationHit;
    use smithay::input::pointer::{CursorIcon, CursorImageStatus};
    let icon = match decoration_hit(loc, w, h) {
        Some(DecorationHit::Resize(e)) => resize_cursor(e),
        _ => CursorIcon::Default,
    };
    CursorImageStatus::Named(icon)
}

/// Resize arrow for a set of edges.
pub fn resize_cursor(edges: super::grabs::ResizeEdge) -> smithay::input::pointer::CursorIcon {
    use super::grabs::ResizeEdge;
    use smithay::input::pointer::CursorIcon;
    match edges {
        ResizeEdge::TOP => CursorIcon::NResize,
        ResizeEdge::BOTTOM => CursorIcon::SResize,
        ResizeEdge::LEFT => CursorIcon::WResize,
        ResizeEdge::RIGHT => CursorIcon::EResize,
        ResizeEdge::TOP_LEFT => CursorIcon::NwResize,
        ResizeEdge::TOP_RIGHT => CursorIcon::NeResize,
        ResizeEdge::BOTTOM_LEFT => CursorIcon::SwResize,
        ResizeEdge::BOTTOM_RIGHT => CursorIcon::SeResize,
        _ => CursorIcon::Default,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SSD(WindowElement);

impl IsAlive for SSD {
    #[inline]
    fn alive(&self) -> bool {
        self.0.alive()
    }
}

impl WaylandFocus for SSD {
    #[inline]
    fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        self.0.wl_surface()
    }
}

impl<BackendData: Backend> PointerTarget<OrchardState<BackendData>> for SSD {
    fn enter(
        &self,
        _seat: &Seat<OrchardState<BackendData>>,
        data: &mut OrchardState<BackendData>,
        event: &MotionEvent,
    ) {
        let client = self.0.client_geometry().size;
        let on_zoom = {
            let mut state = self.0.decoration_state();
            if state.is_ssd {
                state.header_bar.pointer_enter(event.location);
                data.cursor_status = ssd_cursor(event.location, client.w, client.h);
                crate::chrome::titlebar::TrafficLight::hit(event.location.x, event.location.y)
                    == Some(crate::chrome::titlebar::TrafficLight::Zoom)
            } else {
                false
            }
        };
        data.tile_menu_hover(&self.0, on_zoom);
    }
    fn motion(
        &self,
        _seat: &Seat<OrchardState<BackendData>>,
        data: &mut OrchardState<BackendData>,
        event: &MotionEvent,
    ) {
        let client = self.0.client_geometry().size;
        let on_zoom = {
            let mut state = self.0.decoration_state();
            if state.is_ssd {
                state.header_bar.pointer_enter(event.location);
                data.cursor_status = ssd_cursor(event.location, client.w, client.h);
                crate::chrome::titlebar::TrafficLight::hit(event.location.x, event.location.y)
                    == Some(crate::chrome::titlebar::TrafficLight::Zoom)
            } else {
                false
            }
        };
        data.tile_menu_hover(&self.0, on_zoom);
    }
    fn relative_motion(
        &self,
        _seat: &Seat<OrchardState<BackendData>>,
        _data: &mut OrchardState<BackendData>,
        _event: &RelativeMotionEvent,
    ) {
    }
    fn button(
        &self,
        seat: &Seat<OrchardState<BackendData>>,
        data: &mut OrchardState<BackendData>,
        event: &ButtonEvent,
    ) {
        let client = self.0.client_geometry().size;
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state
                .header_bar
                .button(seat, data, &self.0, (client.w, client.h), event);
        }
    }
    fn axis(
        &self,
        _seat: &Seat<OrchardState<BackendData>>,
        _data: &mut OrchardState<BackendData>,
        _frame: AxisFrame,
    ) {
    }
    fn frame(&self, _seat: &Seat<OrchardState<BackendData>>, _data: &mut OrchardState<BackendData>) {}
    fn leave(
        &self,
        _seat: &Seat<OrchardState<BackendData>>,
        data: &mut OrchardState<BackendData>,
        _serial: Serial,
        _time: u32,
    ) {
        {
            let mut state = self.0.decoration_state();
            if state.is_ssd {
                state.header_bar.pointer_leave();
            }
        }
        data.tile_menu_hover(&self.0, false);
    }
    fn gesture_swipe_begin(
        &self,
        _seat: &Seat<OrchardState<BackendData>>,
        _data: &mut OrchardState<BackendData>,
        _event: &GestureSwipeBeginEvent,
    ) {
    }
    fn gesture_swipe_update(
        &self,
        _seat: &Seat<OrchardState<BackendData>>,
        _data: &mut OrchardState<BackendData>,
        _event: &GestureSwipeUpdateEvent,
    ) {
    }
    fn gesture_swipe_end(
        &self,
        _seat: &Seat<OrchardState<BackendData>>,
        _data: &mut OrchardState<BackendData>,
        _event: &GestureSwipeEndEvent,
    ) {
    }
    fn gesture_pinch_begin(
        &self,
        _seat: &Seat<OrchardState<BackendData>>,
        _data: &mut OrchardState<BackendData>,
        _event: &GesturePinchBeginEvent,
    ) {
    }
    fn gesture_pinch_update(
        &self,
        _seat: &Seat<OrchardState<BackendData>>,
        _data: &mut OrchardState<BackendData>,
        _event: &GesturePinchUpdateEvent,
    ) {
    }
    fn gesture_pinch_end(
        &self,
        _seat: &Seat<OrchardState<BackendData>>,
        _data: &mut OrchardState<BackendData>,
        _event: &GesturePinchEndEvent,
    ) {
    }
    fn gesture_hold_begin(
        &self,
        _seat: &Seat<OrchardState<BackendData>>,
        _data: &mut OrchardState<BackendData>,
        _event: &GestureHoldBeginEvent,
    ) {
    }
    fn gesture_hold_end(
        &self,
        _seat: &Seat<OrchardState<BackendData>>,
        _data: &mut OrchardState<BackendData>,
        _event: &GestureHoldEndEvent,
    ) {
    }
}

impl<BackendData: Backend> TouchTarget<OrchardState<BackendData>> for SSD {
    fn down(
        &self,
        seat: &Seat<OrchardState<BackendData>>,
        data: &mut OrchardState<BackendData>,
        event: &smithay::input::touch::DownEvent,
        _seq: Serial,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.pointer_enter(event.location);
            state.header_bar.touch_down(seat, data, &self.0, event.serial);
        }
    }

    fn up(
        &self,
        seat: &Seat<OrchardState<BackendData>>,
        data: &mut OrchardState<BackendData>,
        event: &smithay::input::touch::UpEvent,
        _seq: Serial,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.touch_up(seat, data, &self.0, event.serial);
        }
    }

    fn motion(
        &self,
        _seat: &Seat<OrchardState<BackendData>>,
        _data: &mut OrchardState<BackendData>,
        event: &smithay::input::touch::MotionEvent,
        _seq: Serial,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.pointer_enter(event.location);
        }
    }

    fn frame(
        &self,
        _seat: &Seat<OrchardState<BackendData>>,
        _data: &mut OrchardState<BackendData>,
        _seq: Serial,
    ) {
    }

    fn cancel(
        &self,
        _seat: &Seat<OrchardState<BackendData>>,
        _data: &mut OrchardState<BackendData>,
        _seq: Serial,
    ) {
    }

    fn shape(
        &self,
        _seat: &Seat<OrchardState<BackendData>>,
        _data: &mut OrchardState<BackendData>,
        _event: &smithay::input::touch::ShapeEvent,
        _seq: Serial,
    ) {
    }

    fn orientation(
        &self,
        _seat: &Seat<OrchardState<BackendData>>,
        _data: &mut OrchardState<BackendData>,
        _event: &smithay::input::touch::OrientationEvent,
        _seq: Serial,
    ) {
    }
}

impl SpaceElement for WindowElement {
    fn geometry(&self) -> Rectangle<i32, Logical> {
        let mut geo = SpaceElement::geometry(&self.0);
        if self.decoration_state().is_ssd {
            geo.size.h += HEADER_BAR_HEIGHT;
        }
        geo
    }
    fn bbox(&self) -> Rectangle<i32, Logical> {
        let mut bbox = SpaceElement::bbox(&self.0);
        if self.decoration_state().is_ssd {
            bbox.size.h += HEADER_BAR_HEIGHT;
            // room for the drop shadow and the invisible resize border
            let e = chrome::shadow_extent_logical().max(RESIZE_BORDER);
            bbox.loc -= Point::from((e, e));
            bbox.size += smithay::utils::Size::from((2 * e, 2 * e));
        }
        bbox
    }
    fn is_in_input_region(&self, point: &Point<f64, Logical>) -> bool {
        if self.decoration_state().is_ssd {
            let client = self.client_geometry();
            decoration_hit(*point, client.size.w, client.size.h).is_some()
                || SpaceElement::is_in_input_region(
                    &self.0,
                    &(*point - Point::from((0.0, HEADER_BAR_HEIGHT as f64))),
                )
        } else {
            SpaceElement::is_in_input_region(&self.0, point)
        }
    }
    fn z_index(&self) -> u8 {
        SpaceElement::z_index(&self.0)
    }

    fn set_activate(&self, activated: bool) {
        self.decoration_state().activated = activated;
        SpaceElement::set_activate(&self.0, activated);
    }
    fn output_enter(&self, output: &Output, overlap: Rectangle<i32, Logical>) {
        SpaceElement::output_enter(&self.0, output, overlap);
    }
    fn output_leave(&self, output: &Output) {
        SpaceElement::output_leave(&self.0, output);
    }
    #[profiling::function]
    fn refresh(&self) {
        SpaceElement::refresh(&self.0);
    }
}

render_elements!(
    pub WindowRenderElement<=GlesRenderer>;
    Window=WaylandSurfaceRenderElement<GlesRenderer>,
    Rounded=RoundedElement,
    Decoration=SolidColorRenderElement,
    Chrome=MemoryRenderBufferRenderElement<GlesRenderer>,
);

impl std::fmt::Debug for WindowRenderElement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Window(arg0) => f.debug_tuple("Window").field(arg0).finish(),
            Self::Rounded(arg0) => f.debug_tuple("Rounded").field(arg0).finish(),
            Self::Decoration(arg0) => f.debug_tuple("Decoration").field(arg0).finish(),
            Self::Chrome(arg0) => f.debug_tuple("Chrome").field(arg0).finish(),
            Self::_GenericCatcher(_) => f.write_str("_GenericCatcher"),
        }
    }
}

impl AsRenderElements<GlesRenderer> for WindowElement {
    type RenderElement = WindowRenderElement;

    fn render_elements<C: From<Self::RenderElement>>(
        &self,
        renderer: &mut GlesRenderer,
        location: Point<i32, Physical>,
        scale: Scale<f64>,
        alpha: f32,
    ) -> Vec<C> {
        let window_bbox = SpaceElement::bbox(&self.0);
        let is_ssd = self.decoration_state().is_ssd;

        if is_ssd && !window_bbox.is_empty() {
            let client_geo = SpaceElement::geometry(&self.0);
            let title = self.title();
            let mut state = self.decoration_state();
            let active = state.activated;
            let width = client_geo.size.w;
            let params = TitlebarParams {
                width,
                scale: scale.x,
                appearance: theme::appearance(),
                radius: theme::corner_radius(),
                active,
                group_hovered: state.header_bar.group_hovered(),
                pressed: state.header_bar.pressed,
                title,
                fullscreen: false,
                alt_held: chrome::alt_held(),
            };

            let mut vec: Vec<WindowRenderElement> = Vec::new();
            if let Some(buf) = state.chrome.titlebar(params) {
                match MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    location.to_f64(),
                    buf,
                    Some(alpha),
                    None,
                    Some((width, HEADER_BAR_HEIGHT).into()),
                    Kind::Unspecified,
                ) {
                    Ok(elem) => vec.push(WindowRenderElement::Chrome(elem)),
                    Err(err) => tracing::warn!("failed to upload title bar: {err:?}"),
                }
            }

            let header_phys = (scale.y * HEADER_BAR_HEIGHT as f64).round() as i32;
            let mut client_location = location;
            client_location.y += header_phys;
            // client surfaces; the main surface gets rounded bottom corners
            let main_id = self.wl_surface().map(|s| Id::from_wayland_resource(&*s));
            let program = crate::effects::rounded::program(renderer);
            let radius = (theme::corner_radius() as f64 * scale.x) as f32;
            let rim_rgba = match (theme::appearance(), active) {
                (theme::Appearance::Dark, true) => [1.0, 1.0, 1.0, 0.14],
                (theme::Appearance::Dark, false) => [1.0, 1.0, 1.0, 0.07],
                (theme::Appearance::Light, true) => [0.0, 0.0, 0.0, 0.10],
                (theme::Appearance::Light, false) => [0.0, 0.0, 0.0, 0.06],
            };
            let rim_w = (scale.x as f32).max(1.0);
            let client_elems: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                AsRenderElements::render_elements(&self.0, renderer, client_location, scale, alpha);
            for e in client_elems {
                match (&program, &main_id) {
                    (Some(p), Some(id)) if e.id() == id => vec.push(WindowRenderElement::Rounded(
                        RoundedElement::new(e, p.clone(), radius, Corners::BOTTOM).with_rim(rim_rgba, rim_w),
                    )),
                    _ => vec.push(WindowRenderElement::Window(e)),
                }
            }

            // drop shadow behind everything
            let win_w = (width as f64 * scale.x).round() as i32;
            let win_h = header_phys + (client_geo.size.h as f64 * scale.y).round() as i32;
            let shadow = state.chrome.shadow(scale.x, active);
            let rects = chrome::shadow::layout(&shadow.nine, win_w, win_h);
            for (buf, (dx, dy, dw, dh)) in shadow.buffers.iter().zip(rects) {
                if dw <= 0 || dh <= 0 {
                    continue;
                }
                let size = (
                    ((dw as f64) / scale.x).round().max(1.0) as i32,
                    ((dh as f64) / scale.y).round().max(1.0) as i32,
                );
                let loc = Point::<i32, Physical>::from((location.x + dx, location.y + dy));
                if let Ok(elem) = MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    loc.to_f64(),
                    buf,
                    Some(alpha),
                    None,
                    Some(size.into()),
                    Kind::Unspecified,
                ) {
                    vec.push(WindowRenderElement::Chrome(elem));
                }
            }
            vec.into_iter().map(C::from).collect()
        } else {
            AsRenderElements::render_elements(&self.0, renderer, location, scale, alpha)
                .into_iter()
                .map(C::from)
                .collect()
        }
    }
}
