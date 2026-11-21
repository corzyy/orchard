//! Full-screen title bar reveal, like macOS: pushing the pointer against the
//! top edge of a full-screen output slides in the menu bar and a title bar
//! with traffic lights above the window. Green (or red / yellow) there leaves
//! full screen; moving the pointer back down hides it again.

use std::cell::RefCell;

use smithay::{
    wayland::seat::WaylandFocus,
    desktop::layer_map_for_output,
    input::pointer::CursorImageStatus,
    output::Output,
    utils::{Logical, Point, Rectangle},
    wayland::pointer_constraints::with_pointer_constraint,
};

use crate::{
    chrome::{titlebar::TrafficLight, WindowChrome},
    shell::{ssd::HEADER_BAR_HEIGHT, FullscreenSurface, WindowElement},
    state::Backend,
    OrchardState,
};

/// How close to the top edge (logical px) the pointer has to get.
const TRIGGER: f64 = 2.0;
/// Extra room below the title bar before the reveal hides again.
const SLACK: f64 = 12.0;

pub struct Reveal {
    pub window: WindowElement,
    /// height of the menu bar strip (the exclusive zone at the top)
    pub menu_h: i32,
    /// pointer in title-bar coordinates, while over the title bar
    pub hover: Option<Point<f64, Logical>>,
    pub pressed: Option<TrafficLight>,
    pub chrome: WindowChrome,
}

impl Reveal {
    /// The title bar strip, output-local.
    pub fn bar(&self, out_w: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((0, self.menu_h).into(), (out_w, HEADER_BAR_HEIGHT).into())
    }
}

#[derive(Default)]
pub struct RevealSlot(pub RefCell<Option<Reveal>>);

/// The reveal shown on this output, if it still belongs to its full-screen window.
pub fn with_reveal<R>(output: &Output, f: impl FnOnce(&mut Reveal) -> R) -> Option<R> {
    let slot = output.user_data().get::<RevealSlot>()?;
    let fs = output.user_data().get::<FullscreenSurface>().and_then(|f| f.get());
    let mut slot = slot.0.borrow_mut();
    if slot.as_ref().map(|r| Some(&r.window) != fs.as_ref()).unwrap_or(false) {
        *slot = None;
    }
    slot.as_mut().map(f)
}

fn set_reveal(output: &Output, r: Option<Reveal>) {
    output.user_data().insert_if_missing(RevealSlot::default);
    *output.user_data().get::<RevealSlot>().unwrap().0.borrow_mut() = r;
}

/// Hide a reveal that is no longer wanted (left full screen, switched Space).
pub fn clear_reveal(output: &Output) {
    if output.user_data().get::<RevealSlot>().is_some() {
        set_reveal(output, None);
    }
}

/// What lies under an output-local point while a reveal is shown.
pub enum RevealHit {
    MenuBar,
    TitleBar,
}

pub fn reveal_hit(output: &Output, local: Point<f64, Logical>, out_w: i32) -> Option<RevealHit> {
    with_reveal(output, |r| {
        if local.y < r.menu_h as f64 {
            Some(RevealHit::MenuBar)
        } else if r.bar(out_w).to_f64().contains(local) {
            Some(RevealHit::TitleBar)
        } else {
            None
        }
    })
    .flatten()
}

impl<BackendData: Backend> OrchardState<BackendData> {
    fn pointer_constrained(&self) -> bool {
        let pointer = self.pointer.clone();
        let Some(surface) = pointer.current_focus().and_then(|f| f.wl_surface().map(|s| s.into_owned())) else {
            return false;
        };
        let mut active = false;
        with_pointer_constraint(&surface, &pointer, |c| active = c.map(|c| c.is_active()).unwrap_or(false));
        active
    }

    /// Show / hide / track the reveal for a new pointer position.
    pub fn update_fs_reveal(&mut self, pos: Point<f64, Logical>) {
        // Fast path: skip allocating `Vec<Output>` on every raw pointer motion
        // when no output has a fullscreen window or active reveal.
        let any_relevant = self.space.outputs().any(|o| {
            o.user_data().get::<FullscreenSurface>().and_then(|f| f.get()).is_some()
                || with_reveal(o, |_| ()).is_some()
        });
        if !any_relevant {
            return;
        }
        let outputs: Vec<Output> = self.space.outputs().cloned().collect();
        for output in outputs {
            let Some(geo) = self.space.output_geometry(&output) else { continue };
            let here = geo.to_f64().contains(pos);
            let local = pos - geo.loc.to_f64();
            let shown = with_reveal(&output, |r| {
                let limit = (r.menu_h + HEADER_BAR_HEIGHT) as f64 + SLACK;
                // keep it while a traffic light is held
                let keep = here && (local.y <= limit || r.pressed.is_some());
                if keep {
                    let bar = r.bar(geo.size.w).to_f64();
                    r.hover = bar.contains(local).then(|| local - bar.loc);
                }
                keep
            });
            match shown {
                Some(true) => continue,
                Some(false) => {
                    set_reveal(&output, None);
                    self.backend_data.reset_buffers(&output);
                    continue;
                }
                None => {}
            }
            if !here || local.y > TRIGGER || self.mc.is_some() || self.pointer.is_grabbed() {
                continue;
            }
            let Some(window) = output.user_data().get::<FullscreenSurface>().and_then(|f| f.get()) else {
                continue;
            };
            // games holding the pointer keep the whole screen
            if self.pointer_constrained() {
                continue;
            }
            let menu_h = layer_map_for_output(&output).non_exclusive_zone().loc.y.max(0);
            tracing::debug!(output = %output.name(), menu_h, "full-screen reveal");
            set_reveal(
                &output,
                Some(Reveal { window, menu_h, hover: None, pressed: None, chrome: WindowChrome::default() }),
            );
            self.backend_data.reset_buffers(&output);
        }
    }

    /// A pointer button while a reveal is shown. Returns true when the title
    /// bar took it (it is not passed on to the client).
    pub fn fs_reveal_button(&mut self, button: u32, pressed: bool) -> bool {
        let pos = self.pointer.current_location();
        let Some(output) = self.space.output_under(pos).next().cloned() else { return false };
        let Some(geo) = self.space.output_geometry(&output) else { return false };
        let local = pos - geo.loc.to_f64();
        let was_pressed = with_reveal(&output, |r| r.pressed).flatten();
        let on_bar = matches!(reveal_hit(&output, local, geo.size.w), Some(RevealHit::TitleBar));
        if !on_bar && was_pressed.is_none() {
            return false;
        }
        if button != 0x110 {
            return on_bar;
        }
        let hit = with_reveal(&output, |r| r.hover.and_then(|h| TrafficLight::hit(h.x, h.y))).flatten();
        if pressed {
            with_reveal(&output, |r| r.pressed = hit);
            return true;
        }
        let window = with_reveal(&output, |r| {
            r.pressed = None;
            r.window.clone()
        });
        if let (Some(window), Some(b), true) = (window, hit, was_pressed == hit) {
            set_reveal(&output, None);
            match b {
                TrafficLight::Close => self.close_window(&window),
                TrafficLight::Minimize => {
                    self.exit_fullscreen_space(&window);
                    self.minimize_window(&window);
                }
                TrafficLight::Zoom => self.exit_fullscreen_space(&window),
            }
            self.backend_data.reset_buffers(&output);
            // the pointer is over something else now
            let pos = self.pointer.current_location();
            self.inject_pointer_motion(pos, self.clock.now().as_millis());
        }
        true
    }

    /// The title bar shows the normal arrow (the client may have hidden it).
    pub fn fs_reveal_cursor(&mut self, pos: Point<f64, Logical>) {
        let Some(output) = self.space.output_under(pos).next().cloned() else { return };
        let Some(geo) = self.space.output_geometry(&output) else { return };
        if matches!(reveal_hit(&output, pos - geo.loc.to_f64(), geo.size.w), Some(RevealHit::TitleBar)) {
            self.cursor_status = CursorImageStatus::default_named();
        }
    }
}
