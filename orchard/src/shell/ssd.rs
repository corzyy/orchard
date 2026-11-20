//! macOS-style server-side decorations: title bar with traffic lights, window
//! shadow, and invisible resize borders.

use std::cell::{RefCell, RefMut};

use smithay::{
    desktop::WindowSurface,
    input::{pointer::ButtonEvent, Seat},
    utils::{Logical, Point, Rectangle, Serial},
    wayland::{compositor::with_states, shell::xdg::XdgToplevelSurfaceData},
};

use crate::{
    chrome::{theme, titlebar::TrafficLight, WindowChrome},
    state::Backend,
    OrchardState,
};

use super::{grabs::ResizeEdge, WindowElement};

pub const HEADER_BAR_HEIGHT: i32 = theme::TITLEBAR_HEIGHT;
/// Width of the invisible resize border around SSD windows (logical px).
pub const RESIZE_BORDER: i32 = 6;
/// Maximum time between two clicks to count as a double click.
const DOUBLE_CLICK_MS: u32 = 400;

const BTN_LEFT: u32 = 0x110;

pub struct WindowState {
    pub is_ssd: bool,
    /// what the client negotiated; `is_ssd` is forced off while full screen
    pub wants_ssd: bool,
    pub fullscreen: bool,
    pub activated: bool,
    pub header_bar: HeaderBar,
    /// Geometry to restore when leaving the zoomed (maximized) state.
    pub restore_geometry: Option<Rectangle<i32, Logical>>,
    /// tiled to a screen half / quarter (see tiling.rs)
    pub tile: Option<crate::tiling::Tile>,
    /// hidden with ⌘H (kept in the minimized list, but not shown in the Dock)
    pub hidden: bool,
    pub chrome: WindowChrome,
}

#[derive(Debug, Clone, Default)]
pub struct HeaderBar {
    /// pointer location relative to the window's top-left (including title bar)
    pub pointer_loc: Option<Point<f64, Logical>>,
    pub pressed: Option<TrafficLight>,
    last_click: Option<(u32, Point<f64, Logical>)>,
}

/// What a pointer press on the decoration should do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DecorationHit {
    Button(TrafficLight),
    Titlebar,
    Resize(ResizeEdge),
}

/// Classify a point (window-relative, logical) on an SSD window of the given
/// client size.
pub fn decoration_hit(loc: Point<f64, Logical>, client_w: i32, client_h: i32) -> Option<DecorationHit> {
    let total_h = (client_h + HEADER_BAR_HEIGHT) as f64;
    let w = client_w as f64;
    let b = RESIZE_BORDER as f64;
    let mut edges = ResizeEdge::empty();
    if loc.x < 0.0 {
        edges |= ResizeEdge::LEFT;
    } else if loc.x >= w {
        edges |= ResizeEdge::RIGHT;
    }
    if loc.y < 0.0 {
        edges |= ResizeEdge::TOP;
    } else if loc.y >= total_h {
        edges |= ResizeEdge::BOTTOM;
    }
    // generous corners: within the corner radius along the edge counts as corner
    let corner = theme::corner_radius() as f64 + b;
    if !edges.is_empty() {
        if edges == ResizeEdge::LEFT || edges == ResizeEdge::RIGHT {
            if loc.y < corner - b {
                edges |= ResizeEdge::TOP;
            } else if loc.y > total_h - corner + b {
                edges |= ResizeEdge::BOTTOM;
            }
        } else if edges == ResizeEdge::TOP || edges == ResizeEdge::BOTTOM {
            if loc.x < corner - b {
                edges |= ResizeEdge::LEFT;
            } else if loc.x > w - corner + b {
                edges |= ResizeEdge::RIGHT;
            }
        }
        if loc.x < -b || loc.x >= w + b || loc.y < -b || loc.y >= total_h + b {
            return None;
        }
        return Some(DecorationHit::Resize(edges));
    }
    if loc.y < HEADER_BAR_HEIGHT as f64 {
        if let Some(b) = TrafficLight::hit(loc.x, loc.y) {
            return Some(DecorationHit::Button(b));
        }
        return Some(DecorationHit::Titlebar);
    }
    None
}

impl HeaderBar {
    pub fn pointer_enter(&mut self, loc: Point<f64, Logical>) {
        self.pointer_loc = Some(loc);
    }

    pub fn pointer_leave(&mut self) {
        self.pointer_loc = None;
        self.pressed = None;
    }

    pub fn group_hovered(&self) -> bool {
        self.pointer_loc
            .map(|l| TrafficLight::in_group(l.x, l.y))
            .unwrap_or(false)
    }

    /// Handle a pointer button on the decoration. Actions that mutate the
    /// compositor are deferred to an idle callback so we never re-enter the
    /// window's `RefCell` while it is borrowed.
    pub fn button<BackendData: Backend>(
        &mut self,
        seat: &Seat<OrchardState<BackendData>>,
        state: &mut OrchardState<BackendData>,
        window: &WindowElement,
        client_size: (i32, i32),
        event: &ButtonEvent,
    ) {
        use smithay::backend::input::ButtonState;
        let Some(loc) = self.pointer_loc else { return };
        let hit = decoration_hit(loc, client_size.0, client_size.1);
        if event.button != BTN_LEFT {
            return;
        }
        match event.state {
            ButtonState::Pressed => match hit {
                Some(DecorationHit::Button(b)) => self.pressed = Some(b),
                Some(DecorationHit::Titlebar) => {
                    let double = self
                        .last_click
                        .map(|(t, p)| {
                            event.time.wrapping_sub(t) <= DOUBLE_CLICK_MS
                                && (p.x - loc.x).abs() < 4.0
                                && (p.y - loc.y).abs() < 4.0
                        })
                        .unwrap_or(false);
                    if double {
                        self.last_click = None;
                        let window = window.clone();
                        use orchard_config::TitlebarDoubleClick as D;
                        match crate::settings::get().windows.double_click_titlebar {
                            D::Zoom => {
                                state.handle.insert_idle(move |data| data.toggle_zoom(&window));
                            }
                            D::Minimize => {
                                state.handle.insert_idle(move |data| data.minimize_window(&window));
                            }
                            D::Nothing => {}
                        }
                    } else {
                        self.last_click = Some((event.time, loc));
                        start_move(state, seat, window, event.serial);
                    }
                }
                Some(DecorationHit::Resize(edges)) => {
                    let window = window.clone();
                    let seat = seat.clone();
                    let serial = event.serial;
                    state
                        .handle
                        .insert_idle(move |data| data.start_ssd_resize(&window, &seat, serial, edges));
                }
                None => {}
            },
            ButtonState::Released => {
                let pressed = self.pressed.take();
                if let (Some(p), Some(DecorationHit::Button(b))) = (pressed, hit) {
                    if p == b {
                        let window = window.clone();
                        // green maximizes (zoom); Option-click goes full screen
                        let alt = seat.get_keyboard().map(|k| k.modifier_state().alt).unwrap_or(false);
                        state.handle.insert_idle(move |data| match b {
                            TrafficLight::Close => data.close_window(&window),
                            TrafficLight::Minimize => data.minimize_window(&window),
                            TrafficLight::Zoom if alt => data.toggle_fullscreen(&window),
                            TrafficLight::Zoom => data.toggle_zoom(&window),
                        });
                    }
                }
            }
        }
    }

    pub fn touch_down<BackendData: Backend>(
        &mut self,
        seat: &Seat<OrchardState<BackendData>>,
        state: &mut OrchardState<BackendData>,
        window: &WindowElement,
        serial: Serial,
    ) {
        match self.pointer_loc.and_then(|l| TrafficLight::hit(l.x, l.y)) {
            Some(b) => self.pressed = Some(b),
            None => start_move(state, seat, window, serial),
        }
    }

    pub fn touch_up<BackendData: Backend>(
        &mut self,
        _seat: &Seat<OrchardState<BackendData>>,
        state: &mut OrchardState<BackendData>,
        window: &WindowElement,
        _serial: Serial,
    ) {
        let pressed = self.pressed.take();
        let hit = self.pointer_loc.and_then(|l| TrafficLight::hit(l.x, l.y));
        if let (Some(p), Some(b)) = (pressed, hit) {
            if p == b {
                let window = window.clone();
                state.handle.insert_idle(move |data| match b {
                    TrafficLight::Close => data.close_window(&window),
                    TrafficLight::Minimize => data.minimize_window(&window),
                    TrafficLight::Zoom => data.toggle_zoom(&window),
                });
            }
        }
    }
}

fn start_move<BackendData: Backend>(
    state: &mut OrchardState<BackendData>,
    seat: &Seat<OrchardState<BackendData>>,
    window: &WindowElement,
    serial: Serial,
) {
    match window.0.underlying_surface() {
        WindowSurface::Wayland(w) => {
            let seat = seat.clone();
            let toplevel = w.clone();
            state
                .handle
                .insert_idle(move |data| data.move_request_xdg(&toplevel, &seat, serial));
        }
        #[cfg(feature = "xwayland")]
        WindowSurface::X11(w) => {
            let window = w.clone();
            state
                .handle
                .insert_idle(move |data| data.move_request_x11(&window));
        }
    }
}

impl WindowElement {
    pub fn decoration_state(&self) -> RefMut<'_, WindowState> {
        self.user_data().insert_if_missing(|| {
            RefCell::new(WindowState {
                is_ssd: false,
                wants_ssd: false,
                fullscreen: false,
                activated: false,
                header_bar: HeaderBar::default(),
                restore_geometry: None,
                tile: None,
                hidden: false,
                chrome: WindowChrome::default(),
            })
        });

        self.user_data()
            .get::<RefCell<WindowState>>()
            .unwrap()
            .borrow_mut()
    }

    pub fn set_ssd(&self, ssd: bool) {
        let mut st = self.decoration_state();
        st.wants_ssd = ssd;
        st.is_ssd = ssd && !st.fullscreen;
    }

    pub fn set_fullscreen_flag(&self, fullscreen: bool) {
        let mut st = self.decoration_state();
        st.fullscreen = fullscreen;
        st.is_ssd = st.wants_ssd && !fullscreen;
    }

    pub fn is_fullscreen(&self) -> bool {
        self.decoration_state().fullscreen
    }

    pub fn is_ssd(&self) -> bool {
        self.decoration_state().is_ssd
    }

    /// Geometry of the client surface only (without our title bar).
    pub fn client_geometry(&self) -> Rectangle<i32, Logical> {
        smithay::desktop::space::SpaceElement::geometry(&self.0)
    }

    /// Window title as reported by the client.
    pub fn title(&self) -> String {
        match self.0.underlying_surface() {
            WindowSurface::Wayland(t) => with_states(t.wl_surface(), |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|d| d.lock().unwrap().title.clone())
                    .unwrap_or_default()
            }),
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(x) => x.title(),
        }
    }

    /// Application id (xdg app_id or X11 WM_CLASS).
    pub fn app_id(&self) -> String {
        match self.0.underlying_surface() {
            WindowSurface::Wayland(t) => with_states(t.wl_surface(), |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|d| d.lock().unwrap().app_id.clone())
                    .unwrap_or_default()
            }),
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(x) => x.class(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_points() {
        let hit = |x, y| decoration_hit(Point::from((x, y)), 400, 300);
        assert_eq!(hit(14.0, 14.0), Some(DecorationHit::Button(TrafficLight::Close)));
        assert_eq!(hit(200.0, 10.0), Some(DecorationHit::Titlebar));
        assert_eq!(hit(200.0, 100.0), None);
        assert_eq!(hit(-3.0, 150.0), Some(DecorationHit::Resize(ResizeEdge::LEFT)));
        assert_eq!(
            hit(402.0, 327.0),
            Some(DecorationHit::Resize(ResizeEdge::BOTTOM_RIGHT))
        );
        assert_eq!(hit(-20.0, 150.0), None);
    }
}
