use std::{convert::TryInto, sync::atomic::Ordering};

use crate::{focus::PointerFocusTarget, shell::FullscreenSurface, OrchardState};

#[cfg(feature = "udev")]
use crate::udev::UdevData;
#[cfg(feature = "udev")]
use smithay::backend::renderer::DebugFlags;

use smithay::{
    backend::input::{
        self, Axis, AxisSource, Event, InputBackend, InputEvent, KeyState, KeyboardKeyEvent,
        PointerAxisEvent, PointerButtonEvent,
    },
    desktop::{layer_map_for_output, WindowSurfaceType},
    input::{
        keyboard::{keysyms as xkb, FilterResult, Keysym, ModifiersState},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    output::Scale,
    reexports::{
        wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1,
        wayland_server::protocol::wl_pointer,
    },
    utils::{Logical, Point, Serial, Transform, SERIAL_COUNTER as SCOUNTER},
    wayland::{
        compositor::with_states,
        input_method::InputMethodSeat,
        keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitorSeat,
        shell::wlr_layer::{KeyboardInteractivity, Layer as WlrLayer, LayerSurfaceCachedState},
    },
};

#[cfg(any(feature = "winit", feature = "x11", feature = "udev"))]
use smithay::backend::input::AbsolutePositionEvent;

#[cfg(any(feature = "winit", feature = "x11"))]
use smithay::output::Output;
use tracing::{debug, error, info, warn};

use crate::state::Backend;
#[cfg(feature = "udev")]
use smithay::{
    backend::{
        input::{
            Device, DeviceCapability, GestureBeginEvent, GestureEndEvent, GesturePinchUpdateEvent as _,
            GestureSwipeUpdateEvent as _, PointerMotionEvent, ProximityState, TabletToolButtonEvent,
            TabletToolEvent, TabletToolProximityEvent, TabletToolTipEvent, TabletToolTipState, TouchEvent,
        },
        session::Session,
    },
    input::{
        pointer::{
            GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent,
            GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent,
            RelativeMotionEvent,
        },
        touch::{DownEvent, UpEvent},
    },
    reexports::wayland_server::DisplayHandle,
    wayland::{
        pointer_constraints::{with_pointer_constraint, PointerConstraint},
        seat::WaylandFocus,
        tablet_manager::{TabletDescriptor, TabletSeatTrait},
    },
};

impl<BackendData: Backend> OrchardState<BackendData> {
    fn process_common_key_action(&mut self, action: KeyAction) {
        match action {
            KeyAction::None => (),

            KeyAction::Quit => {
                info!("Quitting.");
                self.running.store(false, Ordering::SeqCst);
            }

            KeyAction::Run(cmd) => {
                info!(cmd, "Starting program");
                self.spawn(&cmd);
            }

            KeyAction::TogglePreview => {
                self.show_window_preview = !self.show_window_preview;
            }

            KeyAction::ToggleDecorations => {
                for element in self.space.elements() {
                    #[allow(irrefutable_let_patterns)]
                    if let Some(toplevel) = element.0.toplevel() {
                        let mode_changed = toplevel.with_pending_state(|state| {
                            if let Some(current_mode) = state.decoration_mode {
                                let new_mode =
                                    if current_mode == zxdg_toplevel_decoration_v1::Mode::ClientSide {
                                        zxdg_toplevel_decoration_v1::Mode::ServerSide
                                    } else {
                                        zxdg_toplevel_decoration_v1::Mode::ClientSide
                                    };
                                state.decoration_mode = Some(new_mode);
                                true
                            } else {
                                false
                            }
                        });

                        if mode_changed && toplevel.is_initial_configure_sent() {
                            toplevel.send_pending_configure();
                        }
                    }
                }
            }

            KeyAction::CloseWindow => {
                if let Some(w) = self.focused_window() {
                    self.close_window(&w);
                }
            }
            KeyAction::QuitApp => {
                if let Some(w) = self.focused_window() {
                    let app = w.app_id();
                    let windows: Vec<_> = self
                        .space
                        .elements()
                        .filter(|e| e.app_id() == app)
                        .cloned()
                        .collect();
                    for w in windows {
                        self.close_window(&w);
                    }
                }
            }
            KeyAction::Minimize => {
                if let Some(w) = self.focused_window() {
                    self.minimize_window(&w);
                }
            }
            KeyAction::Unminimize => self.unminimize(None),
            KeyAction::Zoom => {
                if let Some(w) = self.focused_window() {
                    self.toggle_zoom(&w);
                }
            }
            KeyAction::Screenshot => self.take_screenshot(),
            KeyAction::ScreenshotToClipboard => self.screenshot_screen(true),
            KeyAction::ScreenshotArea(clip) => self.start_area_screenshot(clip),
            KeyAction::ScreenshotCancel => self.cancel_area_screenshot(),
            KeyAction::ScreenshotWindowMode => self.toggle_screenshot_window_mode(),
            KeyAction::Media(key, fine) => self.ipc_broadcast(&crate::ipc::Event::MediaKey { key, fine }),
            KeyAction::Tile(t) => {
                if let Some(w) = self.focused_window() {
                    self.tile_window(&w, t);
                }
            }
            KeyAction::HideApp => {
                if let Some(w) = self.focused_window() {
                    self.hide_app(&w.app_id());
                }
            }
            KeyAction::HideOthers => self.hide_others(),
            KeyAction::ForceQuit => self.force_quit_focused(),
            KeyAction::Untile => {
                if let Some(w) = self.focused_window() {
                    self.untile_window(&w);
                }
            }
            KeyAction::Center => {
                if let Some(w) = self.focused_window() {
                    self.center_window(&w);
                }
            }
            KeyAction::Fullscreen => {
                if let Some(w) = self.focused_window() {
                    self.toggle_fullscreen(&w);
                }
            }
            KeyAction::CycleAppWindows => self.cycle_app_windows(),
            KeyAction::SpaceRelative(d) => self.switch_space_relative(d),
            KeyAction::SpaceIndex(i) => {
                if let Some(output) = self.active_output() {
                    self.goto_space(&output, i);
                }
            }
            KeyAction::MoveToSpace(d) => {
                if self.mc.is_none() {
                    self.move_focused_to_space_relative(d)
                }
            }
            KeyAction::MoveToSpaceIndex(i) => self.move_focused_to_space(i, false),
            KeyAction::McNav(dx, dy) => self.mc_nav(dx, dy),
            KeyAction::McPick => self.mc_pick(),
            KeyAction::MissionControl => self.toggle_mission_control(false),
            KeyAction::Switcher(forward) => self.switcher_step(forward),
            KeyAction::SwitcherCommit => self.switcher_commit(),
            KeyAction::SwitcherCancel => self.switcher_cancel(),
            KeyAction::Launcher => self.ipc_broadcast(&crate::ipc::Event::LauncherToggle),
            KeyAction::Launchpad => {
                if self.mc.is_some() {
                    self.close_mission_control(None);
                }
                self.ipc_broadcast(&crate::ipc::Event::LaunchpadToggle)
            }
            KeyAction::AppExpose => self.toggle_mission_control(true),
            KeyAction::CycleWindows => {
                let bottom = self.space.elements().next().cloned();
                if let Some(bottom) = bottom {
                    self.focus_window(&bottom);
                }
            }

            // backend specific (VT switch, output scale …): ignored where unsupported
            other => debug!(?other, "key action not supported by this backend"),
        }
    }

    fn keyboard_key_to_action<B: InputBackend>(&mut self, evt: B::KeyboardKeyEvent) -> KeyAction {
        let keycode = evt.key_code();
        let state = evt.state();
        debug!(?keycode, ?state, "key");
        let serial = SCOUNTER.next_serial();
        let time = Event::time_msec(&evt);
        let mut suppressed_keys = self.suppressed_keys.clone();
        let keyboard = self.seat.get_keyboard().unwrap();

        for layer in self.layer_shell_state.layer_surfaces().rev() {
            let data = with_states(layer.wl_surface(), |states| {
                *states.cached_state.get::<LayerSurfaceCachedState>().current()
            });
            if data.keyboard_interactivity == KeyboardInteractivity::Exclusive
                && (data.layer == WlrLayer::Top || data.layer == WlrLayer::Overlay)
            {
                let surface = self.space.outputs().find_map(|o| {
                    let map = layer_map_for_output(o);
                    let cloned = map.layers().find(|l| l.layer_surface() == &layer).cloned();
                    cloned
                });
                if let Some(surface) = surface {
                    keyboard.set_focus(self, Some(surface.into()), serial);
                    // a key whose press triggered a shortcut (e.g. the Space of
                    // Super+Space that opened this layer) is released here: drop
                    // it from the suppressed set, or its next ordinary release
                    // would be swallowed and the client would see it stuck down
                    let was_suppressed = suppressed_keys.contains(&keycode);
                    suppressed_keys.retain(|k| *k != keycode);
                    self.suppressed_keys = suppressed_keys;
                    if state == KeyState::Released && was_suppressed {
                        keyboard.input::<(), _>(self, keycode, state, serial, time, |_, _, _| {
                            FilterResult::Intercept(())
                        });
                    } else {
                        keyboard.input::<(), _>(self, keycode, state, serial, time, |_, _, _| {
                            FilterResult::Forward
                        });
                    }
                    return KeyAction::None;
                };
            }
        }

        let inhibited = self
            .space
            .element_under(self.pointer.current_location())
            .and_then(|(window, _)| {
                let surface = window.wl_surface()?;
                self.seat.keyboard_shortcuts_inhibitor_for_surface(&surface)
            })
            .map(|inhibitor| inhibitor.is_active())
            .unwrap_or(false);

        let action = keyboard
            .input(self, keycode, state, serial, time, |st, modifiers, handle| {
                let keysym = handle.modified_sym();

                debug!(
                    ?state,
                    mods = ?modifiers,
                    keysym = ::xkbcommon::xkb::keysym_get_name(keysym),
                    "keysym"
                );

                // If the key is pressed and triggered a action
                // we will not forward the key to the client.
                // Additionally add the key to the suppressed keys
                // so that we can decide on a release if the key
                // should be forwarded to the client or not.
                if let KeyState::Pressed = state {
                    // a fresh press means any earlier suppressed press of this
                    // key was released somewhere we did not see
                    suppressed_keys.retain(|k| *k != keycode);
                    if !inhibited {
                        let action = if st.protocols.locked {
                            // while locked only VT switching works
                            match process_keyboard_shortcut(*modifiers, keysym) {
                                Some(KeyAction::VtSwitch(vt)) => Some(KeyAction::VtSwitch(vt)),
                                Some(KeyAction::Media(k, f)) => Some(KeyAction::Media(k, f)),
                                _ => None,
                            }
                        } else if st.shot_select.is_some() && keysym == Keysym::Escape {
                            Some(KeyAction::ScreenshotCancel)
                        } else if st.shot_select.is_some() && keysym == Keysym::space {
                            Some(KeyAction::ScreenshotWindowMode)
                        } else if st.switcher.is_some() && keysym == Keysym::Escape {
                            Some(KeyAction::SwitcherCancel)
                        } else if !BackendData::IS_SESSION && keysym == Keysym::F11 {
                            // nested (dist/nested.sh): F11 would put an app inside
                            // full screen, which breaks the nested session
                            Some(KeyAction::None)
                        } else if st.mc.is_some() && keysym == Keysym::Escape {
                            // Escape leaves Mission Control
                            Some(KeyAction::MissionControl)
                        } else if let Some(a) = st.mc.as_ref().and_then(|_| mc_key(modifiers, keysym)) {
                            Some(a)
                        } else if modifiers.logo
                            && modifiers.ctrl
                            && modifiers.shift
                            && (10..=18).contains(&handle.raw_code().raw())
                        {
                            // ⌃⌘⇧1…9 by key position (shifted digits differ per layout)
                            Some(KeyAction::MoveToSpaceIndex((handle.raw_code().raw() - 10) as usize))
                        } else {
                            process_keyboard_shortcut(*modifiers, keysym)
                        };

                        if action.is_some() {
                            suppressed_keys.push(keycode);
                        }

                        action
                            .map(FilterResult::Intercept)
                            .unwrap_or(FilterResult::Forward)
                    } else {
                        FilterResult::Forward
                    }
                } else if st.switcher.is_some() && (keysym == Keysym::Super_L || keysym == Keysym::Super_R) {
                    FilterResult::Intercept(KeyAction::SwitcherCommit)
                } else {
                    let suppressed = suppressed_keys.contains(&keycode);
                    if suppressed {
                        suppressed_keys.retain(|k| *k != keycode);
                        FilterResult::Intercept(KeyAction::None)
                    } else {
                        FilterResult::Forward
                    }
                }
            })
            .unwrap_or(KeyAction::None);

        self.suppressed_keys = suppressed_keys;
        action
    }

    fn on_pointer_button<B: InputBackend>(&mut self, evt: B::PointerButtonEvent) {
        let serial = SCOUNTER.next_serial();
        let button = evt.button_code();

        let state = wl_pointer::ButtonState::from(evt.state());
        if self.shot_select.is_some() && button == 0x110 {
            self.screenshot_button(state == wl_pointer::ButtonState::Pressed);
            return;
        }
        if self.mc.is_some() {
            if button == 0x110 {
                let pos = self.pointer.current_location();
                if state == wl_pointer::ButtonState::Pressed {
                    self.mc_press(pos);
                } else {
                    self.mc_release(pos);
                }
            }
            return;
        }

        if self.fs_reveal_button(button, state == wl_pointer::ButtonState::Pressed) {
            return;
        }
        if wl_pointer::ButtonState::Pressed == state {
            // Super + left/right/middle: move / resize / raise-lower (KWin style)
            let logo = self.seat.get_keyboard().map(|k| k.modifier_state().logo).unwrap_or(false);
            if logo && self.modifier_click(button, serial) {
                return;
            }
            self.update_keyboard_focus(self.pointer.current_location(), serial);
        };
        let pointer = self.pointer.clone();
        pointer.button(
            self,
            &ButtonEvent {
                button,
                state: state.try_into().unwrap(),
                serial,
                time: evt.time_msec(),
            },
        );
        pointer.frame(self);
    }

    fn update_keyboard_focus(&mut self, location: Point<f64, Logical>, serial: Serial) {
        if self.protocols.locked {
            return;
        }
        let keyboard = self.seat.get_keyboard().unwrap();
        let touch = self.seat.get_touch();
        let input_method = self.seat.input_method();
        // change the keyboard focus unless the pointer or keyboard is grabbed
        // We test for any matching surface type here but always use the root
        // (in case of a window the toplevel) surface for the focus.
        // So for example if a user clicks on a subsurface or popup the toplevel
        // will receive the keyboard focus. Directly assigning the focus to the
        // matching surface leads to issues with clients dismissing popups and
        // subsurface menus (for example firefox-wayland).
        // see here for a discussion about that issue:
        // https://gitlab.freedesktop.org/wayland/wayland/-/issues/294
        if !self.pointer.is_grabbed()
            && (!keyboard.is_grabbed() || input_method.keyboard_grabbed())
            && !touch.map(|touch| touch.is_grabbed()).unwrap_or(false)
        {
            let output = self.space.output_under(location).next().cloned();
            if let Some(output) = output.as_ref() {
                let output_geo = self.space.output_geometry(output).unwrap();
                let revealed = crate::fs_reveal::reveal_hit(output, location - output_geo.loc.to_f64(), output_geo.size.w)
                    .is_some();
                if let Some(window) = output
                    .user_data()
                    .get::<FullscreenSurface>()
                    .and_then(|f| f.get())
                    .filter(|_| !revealed)
                {
                    if let Some((_, _)) =
                        window.surface_under(location - output_geo.loc.to_f64(), WindowSurfaceType::ALL)
                    {
                        #[cfg(feature = "xwayland")]
                        if let Some(surface) = window.0.x11_surface() {
                            self.xwm.as_mut().unwrap().raise_window(surface).unwrap();
                        }
                        keyboard.set_focus(self, Some(window.into()), serial);
                        return;
                    }
                }

                let layers = layer_map_for_output(output);
                let local = location - output_geo.loc.to_f64();
                if let Some((layer, _, _)) = crate::render::layer_input_under(&layers, WlrLayer::Overlay, local)
                    .or_else(|| crate::render::layer_input_under(&layers, WlrLayer::Top, local))
                {
                    if layer.can_receive_keyboard_focus() {
                        drop(layers);
                        keyboard.set_focus(self, Some(layer.into()), serial);
                        return;
                    }
                }
            }

            if let Some((window, _)) = self.space.element_under(location).map(|(w, p)| (w.clone(), p)) {
                self.space.raise_element(&window, true);
                #[cfg(feature = "xwayland")]
                if let Some(surface) = window.0.x11_surface() {
                    self.xwm.as_mut().unwrap().raise_window(surface).unwrap();
                }
                keyboard.set_focus(self, Some(window.into()), serial);
                return;
            }

            if let Some(output) = output.as_ref() {
                let output_geo = self.space.output_geometry(output).unwrap();
                let layers = layer_map_for_output(output);
                if let Some(layer) = layers
                    .layer_under(WlrLayer::Bottom, location - output_geo.loc.to_f64())
                    .or_else(|| layers.layer_under(WlrLayer::Background, location - output_geo.loc.to_f64()))
                {
                    if layer.can_receive_keyboard_focus() {
                        if let Some((_, _)) = layer.surface_under(
                            location
                                - output_geo.loc.to_f64()
                                - layers.layer_geometry(layer).unwrap().loc.to_f64(),
                            WindowSurfaceType::ALL,
                        ) {
                            keyboard.set_focus(self, Some(layer.clone().into()), serial);
                        }
                    }
                }
            };
        }
    }

    pub fn surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(PointerFocusTarget, Point<f64, Logical>)> {
        if self.mc.is_some() || self.shot_select.is_some() {
            // Mission Control / screenshot picking owns the pointer
            return None;
        }
        if self.protocols.locked {
            let output = self.space.output_under(pos).next()?;
            let geo = self.space.output_geometry(output)?;
            let lock = self.lock_surface_for(output)?;
            return Some((PointerFocusTarget::from(lock.wl_surface()), geo.loc.to_f64()));
        }
        let output = self.space.outputs().find(|o| {
            let geometry = self.space.output_geometry(o).unwrap();
            geometry.contains(pos.to_i32_round())
        })?;
        let output_geo = self.space.output_geometry(output).unwrap();
        let layers = layer_map_for_output(output);

        let mut under = None;
        // full-screen reveal: the menu bar is a layer, the title bar is ours
        let reveal = crate::fs_reveal::reveal_hit(output, pos - output_geo.loc.to_f64(), output_geo.size.w);
        if matches!(reveal, Some(crate::fs_reveal::RevealHit::TitleBar)) {
            return None;
        }
        if reveal.is_some() {
            // (menu bar: falls through to the Top layer below)
        } else if let Some((surface, loc)) = output
            .user_data()
            .get::<FullscreenSurface>()
            .and_then(|f| f.get())
            .and_then(|w| w.surface_under(pos - output_geo.loc.to_f64(), WindowSurfaceType::ALL))
        {
            under = Some((surface, loc + output_geo.loc));
        } else if let Some(focus) = crate::render::layer_input_under(&layers, WlrLayer::Overlay, pos - output_geo.loc.to_f64())
            .or_else(|| crate::render::layer_input_under(&layers, WlrLayer::Top, pos - output_geo.loc.to_f64()))
            .map(|(_, surface, loc)| (PointerFocusTarget::from(surface), loc + output_geo.loc))
        {
            under = Some(focus)
        } else if let Some(focus) = self.space.element_under(pos).and_then(|(window, loc)| {
            window
                .surface_under(pos - loc.to_f64(), WindowSurfaceType::ALL)
                .map(|(surface, surf_loc)| (surface, surf_loc + loc))
        }) {
            under = Some(focus);
        } else if let Some(focus) = layers
            .layer_under(WlrLayer::Bottom, pos - output_geo.loc.to_f64())
            .or_else(|| layers.layer_under(WlrLayer::Background, pos - output_geo.loc.to_f64()))
            .and_then(|layer| {
                let layer_loc = layers.layer_geometry(layer).unwrap().loc;
                layer
                    .surface_under(
                        pos - output_geo.loc.to_f64() - layer_loc.to_f64(),
                        WindowSurfaceType::ALL,
                    )
                    .map(|(surface, loc)| {
                        (
                            PointerFocusTarget::from(surface),
                            loc + layer_loc + output_geo.loc,
                        )
                    })
            })
        {
            under = Some(focus)
        };
        under.map(|(s, l)| (s, l.to_f64()))
    }

    fn on_pointer_axis<B: InputBackend>(&mut self, evt: B::PointerAxisEvent) {
        let horizontal_amount = evt
            .amount(input::Axis::Horizontal)
            .unwrap_or_else(|| evt.amount_v120(input::Axis::Horizontal).unwrap_or(0.0) * 15.0 / 120.);
        let vertical_amount = evt
            .amount(input::Axis::Vertical)
            .unwrap_or_else(|| evt.amount_v120(input::Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.);
        let horizontal_amount_discrete = evt.amount_v120(input::Axis::Horizontal);
        let vertical_amount_discrete = evt.amount_v120(input::Axis::Vertical);
        // Settings ▸ Mouse ▸ scroll speed
        let factor = crate::settings::get().pointer.scroll_factor.clamp(0.1, 5.0);
        let horizontal_amount = horizontal_amount * factor;
        let vertical_amount = vertical_amount * factor;
        let horizontal_amount_discrete = horizontal_amount_discrete.map(|v| v * factor);
        let vertical_amount_discrete = vertical_amount_discrete.map(|v| v * factor);

        {
            let mut frame = AxisFrame::new(evt.time_msec()).source(evt.source());
            if horizontal_amount != 0.0 {
                frame = frame.relative_direction(Axis::Horizontal, evt.relative_direction(Axis::Horizontal));
                frame = frame.value(Axis::Horizontal, horizontal_amount);
                if let Some(discrete) = horizontal_amount_discrete {
                    frame = frame.v120(Axis::Horizontal, discrete as i32);
                }
            }
            if vertical_amount != 0.0 {
                frame = frame.relative_direction(Axis::Vertical, evt.relative_direction(Axis::Vertical));
                frame = frame.value(Axis::Vertical, vertical_amount);
                if let Some(discrete) = vertical_amount_discrete {
                    frame = frame.v120(Axis::Vertical, discrete as i32);
                }
            }
            if evt.source() == AxisSource::Finger {
                if evt.amount(Axis::Horizontal) == Some(0.0) {
                    frame = frame.stop(Axis::Horizontal);
                }
                if evt.amount(Axis::Vertical) == Some(0.0) {
                    frame = frame.stop(Axis::Vertical);
                }
            }
            let pointer = self.pointer.clone();
            pointer.axis(self, frame);
            pointer.frame(self);
        }
    }
}

#[cfg(any(feature = "winit", feature = "x11"))]
impl<BackendData: Backend> OrchardState<BackendData> {
    pub fn process_input_event_windowed<B: InputBackend>(&mut self, event: InputEvent<B>, output_name: &str) {
        self.idle_notifier_state.notify_activity(&self.seat);
        self.note_activity();
        match event {
            InputEvent::Keyboard { event } => match self.keyboard_key_to_action::<B>(event) {
                KeyAction::ScaleUp => {
                    let output = self
                        .space
                        .outputs()
                        .find(|o| o.name() == output_name)
                        .unwrap()
                        .clone();

                    let current_scale = output.current_scale().fractional_scale();
                    let new_scale = current_scale + 0.25;
                    output.change_current_state(None, None, Some(Scale::Fractional(new_scale)), None);

                    crate::shell::fixup_positions(&mut self.space, self.pointer.current_location());
                    self.backend_data.reset_buffers(&output);
                }

                KeyAction::ScaleDown => {
                    let output = self
                        .space
                        .outputs()
                        .find(|o| o.name() == output_name)
                        .unwrap()
                        .clone();

                    let current_scale = output.current_scale().fractional_scale();
                    let new_scale = f64::max(1.0, current_scale - 0.25);
                    output.change_current_state(None, None, Some(Scale::Fractional(new_scale)), None);

                    crate::shell::fixup_positions(&mut self.space, self.pointer.current_location());
                    self.backend_data.reset_buffers(&output);
                }

                KeyAction::RotateOutput => {
                    let output = self
                        .space
                        .outputs()
                        .find(|o| o.name() == output_name)
                        .unwrap()
                        .clone();

                    let current_transform = output.current_transform();
                    let new_transform = match current_transform {
                        Transform::Normal => Transform::_90,
                        Transform::_90 => Transform::_180,
                        Transform::_180 => Transform::_270,
                        Transform::_270 => Transform::Flipped,
                        Transform::Flipped => Transform::Flipped90,
                        Transform::Flipped90 => Transform::Flipped180,
                        Transform::Flipped180 => Transform::Flipped270,
                        Transform::Flipped270 => Transform::Normal,
                    };
                    tracing::info!(?current_transform, ?new_transform, output = ?output.name(), "changing output transform");
                    output.change_current_state(None, Some(new_transform), None, None);
                    crate::shell::fixup_positions(&mut self.space, self.pointer.current_location());
                    self.backend_data.reset_buffers(&output);
                }

                action => self.process_common_key_action(action),
            },

            InputEvent::PointerMotionAbsolute { event } => {
                let output = self
                    .space
                    .outputs()
                    .find(|o| o.name() == output_name)
                    .unwrap()
                    .clone();
                self.on_pointer_move_absolute_windowed::<B>(event, &output)
            }
            InputEvent::PointerButton { event } => self.on_pointer_button::<B>(event),
            InputEvent::PointerAxis { event } => self.on_pointer_axis::<B>(event),
            _ => (), // other events are not handled in anvil (yet)
        }
    }

    fn on_pointer_move_absolute_windowed<B: InputBackend>(
        &mut self,
        evt: B::PointerMotionAbsoluteEvent,
        output: &Output,
    ) {
        let output_geo = self.space.output_geometry(output).unwrap();

        let pos = evt.position_transformed(output_geo.size) + output_geo.loc.to_f64();
        let serial = SCOUNTER.next_serial();
        if self.mc.is_some() {
            self.mc_motion(pos);
        }

        let pointer = self.pointer.clone();
        let under = self.surface_under(pos);
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial,
                time: evt.time_msec(),
            },
        );
        pointer.frame(self);
        self.after_pointer_motion(pos);
    }

    /// Synthetic absolute pointer motion (headless testing / automation).
    pub fn inject_pointer_motion(&mut self, pos: Point<f64, Logical>, time: u32) {
        if self.mc.is_some() {
            self.mc_motion(pos);
        }
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        let under = self.surface_under(pos);
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial,
                time,
            },
        );
        pointer.frame(self);
        self.after_pointer_motion(pos);
    }

    /// Synthetic pointer button (headless testing / automation).
    pub fn inject_pointer_button(&mut self, button: u32, pressed: bool, time: u32) {
        if self.shot_select.is_some() && button == 0x110 {
            self.screenshot_button(pressed);
            return;
        }
        if self.mc.is_some() {
            if button == 0x110 {
                let pos = self.pointer.current_location();
                if pressed {
                    self.mc_press(pos);
                } else {
                    self.mc_release(pos);
                }
            }
            return;
        }
        if self.fs_reveal_button(button, pressed) {
            return;
        }
        let serial = SCOUNTER.next_serial();
        if pressed {
            let logo = self.seat.get_keyboard().map(|k| k.modifier_state().logo).unwrap_or(false)
                || self.fake_logo;
            if logo && self.modifier_click(button, serial) {
                return;
            }
            self.update_keyboard_focus(self.pointer.current_location(), serial);
        }
        let pointer = self.pointer.clone();
        pointer.button(
            self,
            &ButtonEvent {
                button,
                state: if pressed {
                    smithay::backend::input::ButtonState::Pressed
                } else {
                    smithay::backend::input::ButtonState::Released
                },
                serial,
                time,
            },
        );
        pointer.frame(self);
    }

    /// Run a named window-management action (headless testing / automation).
    pub fn run_named_action(&mut self, name: &str) {
        let action = match name {
            "close" => KeyAction::CloseWindow,
            "quit-app" => KeyAction::QuitApp,
            "minimize" => KeyAction::Minimize,
            "unminimize" => KeyAction::Unminimize,
            "zoom" => KeyAction::Zoom,
            "cycle-app" => KeyAction::CycleAppWindows,
            "cycle" => KeyAction::CycleWindows,
            "quit" => KeyAction::Quit,
            "space-left" => KeyAction::SpaceRelative(-1),
            "space-right" => KeyAction::SpaceRelative(1),
            "move-space-left" => KeyAction::MoveToSpace(-1),
            "move-space-right" => KeyAction::MoveToSpace(1),
            "mission-control" => KeyAction::MissionControl,
            "app-expose" => KeyAction::AppExpose,
            "switcher-next" => KeyAction::Switcher(true),
            "switcher-commit" => KeyAction::SwitcherCommit,
            "launcher" => KeyAction::Launcher,
            "launchpad" => KeyAction::Launchpad,
            "fullscreen" => KeyAction::Fullscreen,
            "screenshot" => KeyAction::Screenshot,
            "screenshot-area" => KeyAction::ScreenshotArea(false),
            "screenshot-cancel" => KeyAction::ScreenshotCancel,
            "screenshot-window" => KeyAction::ScreenshotWindowMode,
            "lock" => KeyAction::Run(crate::settings::locker()),
            "terminal" => KeyAction::Run(crate::settings::terminal()),
            "volume-up" => KeyAction::Media("volume_up", false),
            "volume-down" => KeyAction::Media("volume_down", false),
            "volume-mute" => KeyAction::Media("volume_mute", false),
            "brightness-up" => KeyAction::Media("brightness_up", false),
            "brightness-down" => KeyAction::Media("brightness_down", false),
            "play-pause" => KeyAction::Media("play_pause", false),
            "tile-left" => KeyAction::Tile(crate::tiling::Tile::Left),
            "tile-right" => KeyAction::Tile(crate::tiling::Tile::Right),
            "tile-top" => KeyAction::Tile(crate::tiling::Tile::Top),
            "tile-bottom" => KeyAction::Tile(crate::tiling::Tile::Bottom),
            "tile-fill" => KeyAction::Tile(crate::tiling::Tile::Fill),
            "untile" => KeyAction::Untile,
            "center" => KeyAction::Center,
            "hide" => KeyAction::HideApp,
            "hide-others" => KeyAction::HideOthers,
            "force-quit" => KeyAction::ForceQuit,
            "logout" => KeyAction::Quit,
            "mc-left" => KeyAction::McNav(-1, 0),
            "mc-right" => KeyAction::McNav(1, 0),
            "mc-up" => KeyAction::McNav(0, -1),
            "mc-down" => KeyAction::McNav(0, 1),
            "mc-pick" => KeyAction::McPick,
            other if other.starts_with("space-") || other.starts_with("move-to-space-") => {
                let n = other.rsplit('-').next().and_then(|n| n.parse::<usize>().ok());
                match n {
                    Some(n) if n >= 1 && other.starts_with("space-") => KeyAction::SpaceIndex(n - 1),
                    Some(n) if n >= 1 => KeyAction::MoveToSpaceIndex(n - 1),
                    _ => {
                        warn!("unknown action {other:?}");
                        return;
                    }
                }
            }
            other => {
                warn!("unknown action {other:?}");
                return;
            }
        };
        self.process_common_key_action(action);
    }

    pub fn release_all_keys(&mut self) {
        let keyboard = self.seat.get_keyboard().unwrap();
        for keycode in keyboard.pressed_keys() {
            keyboard.input(
                self,
                keycode,
                KeyState::Released,
                SCOUNTER.next_serial(),
                0,
                |_, _, _| FilterResult::Forward::<bool>,
            );
        }
    }

    /// Relative pointer motion (mice), honouring pointer locks and
    /// confinement. Also driven by the headless `rel DX DY` command.
    pub fn pointer_move_relative(
        &mut self,
        delta: Point<f64, Logical>,
        delta_unaccel: Point<f64, Logical>,
        utime: u64,
        time_msec: u32,
    ) {
        let mut pointer_location = self.pointer.current_location();
        let serial = SCOUNTER.next_serial();

        let pointer = self.pointer.clone();
        let under = self.surface_under(pointer_location);

        // The constraint belongs to the surface with pointer focus. (Looking
        // it up under the cursor let anything drawn above the game, a panel,
        // a popup, a notification, silently drop the lock.)
        let mut pointer_locked = false;
        let mut confine: Option<(Point<f64, Logical>, smithay::utils::Rectangle<i32, Logical>, Option<smithay::wayland::compositor::RegionAttributes>)> = None;
        let mut constrained_target = None;
        if let Some(focus) = pointer.current_focus() {
            if let Some(surface) = focus.wl_surface().map(|s| s.into_owned()) {
                let win = self.surface_window(&surface);
                with_pointer_constraint(&surface, &pointer, |constraint| match constraint {
                    Some(constraint) if constraint.is_active() => {
                        let origin = win.as_ref().map(|w| w.1);
                        match &*constraint {
                            PointerConstraint::Locked(_) => pointer_locked = true,
                            PointerConstraint::Confined(c) => {
                                if let Some((_, origin, frame)) = &win {
                                    confine = Some((*origin, *frame, c.region().cloned()));
                                }
                            }
                        }
                        constrained_target = Some((focus.clone(), origin.unwrap_or_default()));
                    }
                    _ => {}
                });
            }
        }
        let constrained = constrained_target.is_some();

        pointer.relative_motion(
            self,
            if constrained { constrained_target.clone() } else { under.clone() },
            &RelativeMotionEvent {
                delta: delta,
                delta_unaccel: delta_unaccel,
                utime: utime,
            },
        );

        // If pointer is locked, only emit relative motion
        if pointer_locked {
            pointer.frame(self);
            return;
        }

        pointer_location += delta;

        // clamp to screen limits
        // this event is never generated by winit
        pointer_location = self.clamp_coords(pointer_location);

        // Confined: slide along the window's edges, never leave it
        if let Some((origin, frame, region)) = &confine {
            pointer_location = clamp_into(pointer_location, *frame);
            if let Some(region) = region {
                if !region.contains((pointer_location - *origin).to_i32_round()) {
                    pointer.frame(self);
                    return;
                }
            }
        }
        if self.mc.is_some() {
            self.mc_motion(pointer_location);
        }

        let new_under = if constrained { constrained_target.clone() } else { self.surface_under(pointer_location) };

        // focus the surface the pointer moved onto (not the one it left)
        pointer.motion(
            self,
            new_under.clone(),
            &MotionEvent {
                location: pointer_location,
                serial,
                time: time_msec,
            },
        );
        pointer.frame(self);
        self.after_pointer_motion(pointer_location);

        // If pointer is now in a constraint region, activate it
        // TODO Anywhere else pointer is moved needs to do this
        if let Some((under, surface_location)) =
            new_under.and_then(|(target, loc)| Some((target.wl_surface()?.into_owned(), loc)))
        {
            with_pointer_constraint(&under, &pointer, |constraint| match constraint {
                Some(constraint) if !constraint.is_active() => {
                    let point = (pointer_location - surface_location).to_i32_round();
                    if constraint.region().map_or(true, |region| region.contains(point)) {
                        constraint.activate();
                    }
                }
                _ => {}
            });
        }
    }

    pub fn clamp_coords(&self, pos: Point<f64, Logical>) -> Point<f64, Logical> {
        if self.space.outputs().next().is_none() {
            return pos;
        }

        let (pos_x, pos_y) = pos.into();
        let max_x = self
            .space
            .outputs()
            .fold(0, |acc, o| acc + self.space.output_geometry(o).unwrap().size.w);
        let clamped_x = pos_x.clamp(0.0, max_x as f64);
        let max_y = self
            .space
            .outputs()
            .find(|o| {
                let geo = self.space.output_geometry(o).unwrap();
                geo.contains((clamped_x as i32, 0))
            })
            .map(|o| self.space.output_geometry(o).unwrap().size.h);

        if let Some(max_y) = max_y {
            let clamped_y = pos_y.clamp(0.0, max_y as f64);
            (clamped_x, clamped_y).into()
        } else {
            (clamped_x, pos_y).into()
        }
    }

}

#[cfg(feature = "udev")]
impl OrchardState<UdevData> {
    pub fn process_input_event<B: InputBackend>(&mut self, dh: &DisplayHandle, event: InputEvent<B>) {
        self.idle_notifier_state.notify_activity(&self.seat);
        self.note_activity();
        match event {
            InputEvent::Keyboard { event, .. } => match self.keyboard_key_to_action::<B>(event) {
                #[cfg(feature = "udev")]
                KeyAction::VtSwitch(vt) => {
                    info!(to = vt, "Trying to switch vt");
                    if let Err(err) = self.backend_data.session.change_vt(vt) {
                        error!(vt, "Error switching vt: {}", err);
                    }
                }
                KeyAction::Screen(num) => {
                    let geometry = self
                        .space
                        .outputs()
                        .nth(num)
                        .map(|o| self.space.output_geometry(o).unwrap());

                    if let Some(geometry) = geometry {
                        let x = geometry.loc.x as f64 + geometry.size.w as f64 / 2.0;
                        let y = geometry.size.h as f64 / 2.0;
                        let location = (x, y).into();
                        let pointer = self.pointer.clone();
                        let under = self.surface_under(location);
                        pointer.motion(
                            self,
                            under,
                            &MotionEvent {
                                location,
                                serial: SCOUNTER.next_serial(),
                                time: self.clock.now().as_millis(),
                            },
                        );
                        pointer.frame(self);
                    }
                }
                KeyAction::ScaleUp => {
                    let pos = self.pointer.current_location().to_i32_round();
                    let output = self
                        .space
                        .outputs()
                        .find(|o| self.space.output_geometry(o).unwrap().contains(pos))
                        .cloned();

                    if let Some(output) = output {
                        let (output_location, scale) = (
                            self.space.output_geometry(&output).unwrap().loc,
                            output.current_scale().fractional_scale(),
                        );
                        let new_scale = scale + 0.25;
                        output.change_current_state(None, None, Some(Scale::Fractional(new_scale)), None);

                        let rescale = scale / new_scale;
                        let output_location = output_location.to_f64();
                        let mut pointer_output_location = self.pointer.current_location() - output_location;
                        pointer_output_location.x *= rescale;
                        pointer_output_location.y *= rescale;
                        let pointer_location = output_location + pointer_output_location;

                        crate::shell::fixup_positions(&mut self.space, pointer_location);
                        let pointer = self.pointer.clone();
                        let under = self.surface_under(pointer_location);
                        pointer.motion(
                            self,
                            under,
                            &MotionEvent {
                                location: pointer_location,
                                serial: SCOUNTER.next_serial(),
                                time: self.clock.now().as_millis(),
                            },
                        );
                        pointer.frame(self);
                        self.backend_data.reset_buffers(&output);
                    }
                }
                KeyAction::ScaleDown => {
                    let pos = self.pointer.current_location().to_i32_round();
                    let output = self
                        .space
                        .outputs()
                        .find(|o| self.space.output_geometry(o).unwrap().contains(pos))
                        .cloned();

                    if let Some(output) = output {
                        let (output_location, scale) = (
                            self.space.output_geometry(&output).unwrap().loc,
                            output.current_scale().fractional_scale(),
                        );
                        let new_scale = f64::max(1.0, scale - 0.25);
                        output.change_current_state(None, None, Some(Scale::Fractional(new_scale)), None);

                        let rescale = scale / new_scale;
                        let output_location = output_location.to_f64();
                        let mut pointer_output_location = self.pointer.current_location() - output_location;
                        pointer_output_location.x *= rescale;
                        pointer_output_location.y *= rescale;
                        let pointer_location = output_location + pointer_output_location;

                        crate::shell::fixup_positions(&mut self.space, pointer_location);
                        let pointer = self.pointer.clone();
                        let under = self.surface_under(pointer_location);
                        pointer.motion(
                            self,
                            under,
                            &MotionEvent {
                                location: pointer_location,
                                serial: SCOUNTER.next_serial(),
                                time: self.clock.now().as_millis(),
                            },
                        );
                        pointer.frame(self);
                        self.backend_data.reset_buffers(&output);
                    }
                }
                KeyAction::RotateOutput => {
                    let pos = self.pointer.current_location().to_i32_round();
                    let output = self
                        .space
                        .outputs()
                        .find(|o| self.space.output_geometry(o).unwrap().contains(pos))
                        .cloned();

                    if let Some(output) = output {
                        let current_transform = output.current_transform();
                        let new_transform = match current_transform {
                            Transform::Normal => Transform::_90,
                            Transform::_90 => Transform::_180,
                            Transform::_180 => Transform::_270,
                            Transform::_270 => Transform::Flipped,
                            Transform::Flipped => Transform::Flipped90,
                            Transform::Flipped90 => Transform::Flipped180,
                            Transform::Flipped180 => Transform::Flipped270,
                            Transform::Flipped270 => Transform::Normal,
                        };
                        output.change_current_state(None, Some(new_transform), None, None);
                        crate::shell::fixup_positions(&mut self.space, self.pointer.current_location());
                        self.backend_data.reset_buffers(&output);
                    }
                }
                KeyAction::ToggleTint => {
                    let mut debug_flags = self.backend_data.debug_flags();
                    debug_flags.toggle(DebugFlags::TINT);
                    self.backend_data.set_debug_flags(debug_flags);
                }

                action => self.process_common_key_action(action),
            },
            InputEvent::PointerMotion { event, .. } => self.on_pointer_move::<B>(dh, event),
            InputEvent::PointerMotionAbsolute { event, .. } => self.on_pointer_move_absolute::<B>(dh, event),
            InputEvent::PointerButton { event, .. } => self.on_pointer_button::<B>(event),
            InputEvent::PointerAxis { event, .. } => self.on_pointer_axis::<B>(event),
            InputEvent::TabletToolAxis { event, .. } => self.on_tablet_tool_axis::<B>(event),
            InputEvent::TabletToolProximity { event, .. } => self.on_tablet_tool_proximity::<B>(dh, event),
            InputEvent::TabletToolTip { event, .. } => self.on_tablet_tool_tip::<B>(event),
            InputEvent::TabletToolButton { event, .. } => self.on_tablet_button::<B>(event),
            InputEvent::GestureSwipeBegin { event, .. } => self.on_gesture_swipe_begin::<B>(event),
            InputEvent::GestureSwipeUpdate { event, .. } => self.on_gesture_swipe_update::<B>(event),
            InputEvent::GestureSwipeEnd { event, .. } => self.on_gesture_swipe_end::<B>(event),
            InputEvent::GesturePinchBegin { event, .. } => self.on_gesture_pinch_begin::<B>(event),
            InputEvent::GesturePinchUpdate { event, .. } => self.on_gesture_pinch_update::<B>(event),
            InputEvent::GesturePinchEnd { event, .. } => self.on_gesture_pinch_end::<B>(event),
            InputEvent::GestureHoldBegin { event, .. } => self.on_gesture_hold_begin::<B>(event),
            InputEvent::GestureHoldEnd { event, .. } => self.on_gesture_hold_end::<B>(event),

            InputEvent::TouchDown { event } => self.on_touch_down::<B>(event),
            InputEvent::TouchUp { event } => self.on_touch_up::<B>(event),
            InputEvent::TouchMotion { event } => self.on_touch_motion::<B>(event),
            InputEvent::TouchFrame { event } => self.on_touch_frame::<B>(event),
            InputEvent::TouchCancel { event } => self.on_touch_cancel::<B>(event),

            InputEvent::DeviceAdded { device } => {
                if device.has_capability(DeviceCapability::TabletTool) {
                    self.seat
                        .tablet_seat()
                        .add_tablet::<Self>(dh, &TabletDescriptor::from(&device));
                }
                if device.has_capability(DeviceCapability::Touch) && self.seat.get_touch().is_none() {
                    self.seat.add_touch();
                }
            }
            InputEvent::DeviceRemoved { device } => {
                if device.has_capability(DeviceCapability::TabletTool) {
                    let tablet_seat = self.seat.tablet_seat();

                    tablet_seat.remove_tablet(&TabletDescriptor::from(&device));

                    // If there are no tablets in seat we can remove all tools
                    if tablet_seat.count_tablets() == 0 {
                        tablet_seat.clear_tools();
                    }
                }
            }
            _ => {
                // other events are not handled in anvil (yet)
            }
        }
    }

    fn on_pointer_move<B: InputBackend>(&mut self, _dh: &DisplayHandle, evt: B::PointerMotionEvent) {
        self.pointer_move_relative(evt.delta(), evt.delta_unaccel(), evt.time(), evt.time_msec());
    }

    fn on_pointer_move_absolute<B: InputBackend>(
        &mut self,
        _dh: &DisplayHandle,
        evt: B::PointerMotionAbsoluteEvent,
    ) {
        let serial = SCOUNTER.next_serial();

        let max_x = self
            .space
            .outputs()
            .fold(0, |acc, o| acc + self.space.output_geometry(o).unwrap().size.w);

        // no outputs (e.g. every monitor unplugged): nothing to map the event onto
        let Some(max_y) = self
            .space
            .outputs()
            .filter_map(|o| self.space.output_geometry(o))
            .map(|geo| geo.size.h)
            .max()
        else {
            return;
        };

        let mut pointer_location = (evt.x_transformed(max_x), evt.y_transformed(max_y)).into();

        // clamp to screen limits
        pointer_location = self.clamp_coords(pointer_location);

        let pointer = self.pointer.clone();
        let under = self.surface_under(pointer_location);

        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pointer_location,
                serial,
                time: evt.time_msec(),
            },
        );
        pointer.frame(self);
    }

    fn on_tablet_tool_axis<B: InputBackend>(&mut self, evt: B::TabletToolAxisEvent) {
        let tablet_seat = self.seat.tablet_seat();

        if let Some(pointer_location) = self.touch_location_transformed(&evt) {
            let pointer = self.pointer.clone();
            let under = self.surface_under(pointer_location);
            let tablet = tablet_seat.get_tablet(&TabletDescriptor::from(&evt.device()));
            let tool = tablet_seat.get_tool(&evt.tool());

            pointer.motion(
                self,
                under.clone(),
                &MotionEvent {
                    location: pointer_location,
                    serial: SCOUNTER.next_serial(),
                    time: self.clock.now().as_millis(),
                },
            );

            if let (Some(tablet), Some(tool)) = (tablet, tool) {
                if evt.pressure_has_changed() {
                    tool.pressure(evt.pressure());
                }
                if evt.distance_has_changed() {
                    tool.distance(evt.distance());
                }
                if evt.tilt_has_changed() {
                    tool.tilt(evt.tilt());
                }
                if evt.slider_has_changed() {
                    tool.slider_position(evt.slider_position());
                }
                if evt.rotation_has_changed() {
                    tool.rotation(evt.rotation());
                }
                if evt.wheel_has_changed() {
                    tool.wheel(evt.wheel_delta(), evt.wheel_delta_discrete());
                }

                tool.motion(
                    pointer_location,
                    under.and_then(|(f, loc)| f.wl_surface().map(|s| (s.into_owned(), loc))),
                    &tablet,
                    SCOUNTER.next_serial(),
                    evt.time_msec(),
                );
            }

            pointer.frame(self);
        }
    }

    fn on_tablet_tool_proximity<B: InputBackend>(
        &mut self,
        dh: &DisplayHandle,
        evt: B::TabletToolProximityEvent,
    ) {
        let tablet_seat = self.seat.tablet_seat();

        if let Some(pointer_location) = self.touch_location_transformed(&evt) {
            let tool = evt.tool();
            tablet_seat.add_tool::<Self>(self, dh, &tool);

            let pointer = self.pointer.clone();
            let under = self.surface_under(pointer_location);
            let tablet = tablet_seat.get_tablet(&TabletDescriptor::from(&evt.device()));
            let tool = tablet_seat.get_tool(&tool);

            pointer.motion(
                self,
                under.clone(),
                &MotionEvent {
                    location: pointer_location,
                    serial: SCOUNTER.next_serial(),
                    time: evt.time_msec(),
                },
            );
            pointer.frame(self);

            if let (Some(under), Some(tablet), Some(tool)) = (
                under.and_then(|(f, loc)| f.wl_surface().map(|s| (s.into_owned(), loc))),
                tablet,
                tool,
            ) {
                match evt.state() {
                    ProximityState::In => tool.proximity_in(
                        pointer_location,
                        under,
                        &tablet,
                        SCOUNTER.next_serial(),
                        evt.time_msec(),
                    ),
                    ProximityState::Out => tool.proximity_out(evt.time_msec()),
                }
            }
        }
    }

    fn on_tablet_tool_tip<B: InputBackend>(&mut self, evt: B::TabletToolTipEvent) {
        let tool = self.seat.tablet_seat().get_tool(&evt.tool());

        if let Some(tool) = tool {
            match evt.tip_state() {
                TabletToolTipState::Down => {
                    let serial = SCOUNTER.next_serial();
                    tool.tip_down(serial, evt.time_msec());

                    // change the keyboard focus
                    self.update_keyboard_focus(self.pointer.current_location(), serial);
                }
                TabletToolTipState::Up => {
                    tool.tip_up(evt.time_msec());
                }
            }
        }
    }

    fn on_tablet_button<B: InputBackend>(&mut self, evt: B::TabletToolButtonEvent) {
        let tool = self.seat.tablet_seat().get_tool(&evt.tool());

        if let Some(tool) = tool {
            tool.button(
                evt.button(),
                evt.button_state(),
                SCOUNTER.next_serial(),
                evt.time_msec(),
            );
        }
    }

    fn on_gesture_swipe_begin<B: InputBackend>(&mut self, evt: B::GestureSwipeBeginEvent) {
        // three/four-finger swipes belong to the compositor (Spaces, Mission Control)
        if evt.fingers() >= 3 && crate::settings::get().desktop.gestures {
            self.swipe = Some(crate::wm::Swipe::default());
            return;
        }
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_swipe_begin(
            self,
            &GestureSwipeBeginEvent {
                serial,
                time: evt.time_msec(),
                fingers: evt.fingers(),
            },
        );
    }

    fn on_gesture_swipe_update<B: InputBackend>(&mut self, evt: B::GestureSwipeUpdateEvent) {
        if let Some(swipe) = self.swipe.as_mut() {
            swipe.dx += evt.delta().x;
            swipe.dy += evt.delta().y;
            return;
        }
        let pointer = self.pointer.clone();
        pointer.gesture_swipe_update(
            self,
            &GestureSwipeUpdateEvent {
                time: evt.time_msec(),
                delta: evt.delta(),
            },
        );
    }

    fn on_gesture_swipe_end<B: InputBackend>(&mut self, evt: B::GestureSwipeEndEvent) {
        if let Some(swipe) = self.swipe.take() {
            if !evt.cancelled() {
                self.finish_swipe(swipe);
            }
            return;
        }
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_swipe_end(
            self,
            &GestureSwipeEndEvent {
                serial,
                time: evt.time_msec(),
                cancelled: evt.cancelled(),
            },
        );
    }

    fn on_gesture_pinch_begin<B: InputBackend>(&mut self, evt: B::GesturePinchBeginEvent) {
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_pinch_begin(
            self,
            &GesturePinchBeginEvent {
                serial,
                time: evt.time_msec(),
                fingers: evt.fingers(),
            },
        );
    }

    fn on_gesture_pinch_update<B: InputBackend>(&mut self, evt: B::GesturePinchUpdateEvent) {
        let pointer = self.pointer.clone();
        pointer.gesture_pinch_update(
            self,
            &GesturePinchUpdateEvent {
                time: evt.time_msec(),
                delta: evt.delta(),
                scale: evt.scale(),
                rotation: evt.rotation(),
            },
        );
    }

    fn on_gesture_pinch_end<B: InputBackend>(&mut self, evt: B::GesturePinchEndEvent) {
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_pinch_end(
            self,
            &GesturePinchEndEvent {
                serial,
                time: evt.time_msec(),
                cancelled: evt.cancelled(),
            },
        );
    }

    fn on_gesture_hold_begin<B: InputBackend>(&mut self, evt: B::GestureHoldBeginEvent) {
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_hold_begin(
            self,
            &GestureHoldBeginEvent {
                serial,
                time: evt.time_msec(),
                fingers: evt.fingers(),
            },
        );
    }

    fn on_gesture_hold_end<B: InputBackend>(&mut self, evt: B::GestureHoldEndEvent) {
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_hold_end(
            self,
            &GestureHoldEndEvent {
                serial,
                time: evt.time_msec(),
                cancelled: evt.cancelled(),
            },
        );
    }

    fn touch_location_transformed<B: InputBackend, E: AbsolutePositionEvent<B>>(
        &self,
        evt: &E,
    ) -> Option<Point<f64, Logical>> {
        let output = self
            .space
            .outputs()
            .find(|output| output.name().starts_with("eDP"))
            .or_else(|| self.space.outputs().next());

        let output = output?;
        let output_geometry = self.space.output_geometry(output)?;

        let transform = output.current_transform();
        let size = transform.invert().transform_size(output_geometry.size);
        Some(
            transform.transform_point_in(evt.position_transformed(size), &size.to_f64())
                + output_geometry.loc.to_f64(),
        )
    }

    fn on_touch_down<B: InputBackend>(&mut self, evt: B::TouchDownEvent) {
        let Some(handle) = self.seat.get_touch() else {
            return;
        };

        let Some(touch_location) = self.touch_location_transformed(&evt) else {
            return;
        };

        let serial = SCOUNTER.next_serial();
        self.update_keyboard_focus(touch_location, serial);

        let under = self.surface_under(touch_location);
        handle.down(
            self,
            under,
            &DownEvent {
                slot: evt.slot(),
                location: touch_location,
                serial,
                time: evt.time_msec(),
            },
        );
    }
    fn on_touch_up<B: InputBackend>(&mut self, evt: B::TouchUpEvent) {
        let Some(handle) = self.seat.get_touch() else {
            return;
        };
        let serial = SCOUNTER.next_serial();
        handle.up(
            self,
            &UpEvent {
                slot: evt.slot(),
                serial,
                time: evt.time_msec(),
            },
        )
    }
    fn on_touch_motion<B: InputBackend>(&mut self, evt: B::TouchMotionEvent) {
        let Some(handle) = self.seat.get_touch() else {
            return;
        };
        let Some(touch_location) = self.touch_location_transformed(&evt) else {
            return;
        };

        let under = self.surface_under(touch_location);
        handle.motion(
            self,
            under,
            &smithay::input::touch::MotionEvent {
                slot: evt.slot(),
                location: touch_location,
                time: evt.time_msec(),
            },
        );
    }
    fn on_touch_frame<B: InputBackend>(&mut self, _evt: B::TouchFrameEvent) {
        let Some(handle) = self.seat.get_touch() else {
            return;
        };
        handle.frame(self);
    }
    fn on_touch_cancel<B: InputBackend>(&mut self, _evt: B::TouchCancelEvent) {
        let Some(handle) = self.seat.get_touch() else {
            return;
        };
        handle.cancel(self);
    }

}

/// Possible results of a keyboard action
#[allow(dead_code)] // some of these are only read if udev is enabled
#[derive(Debug)]
enum KeyAction {
    /// Quit the compositor
    Quit,
    /// Trigger a vt-switch
    VtSwitch(i32),
    /// run a command
    Run(String),
    /// Switch the current screen
    Screen(usize),
    ScaleUp,
    ScaleDown,
    TogglePreview,
    RotateOutput,
    ToggleTint,
    ToggleDecorations,
    /// Cmd+W
    CloseWindow,
    /// Cmd+Q: close every window of the focused app
    QuitApp,
    /// Cmd+M
    Minimize,
    /// restore the most recently minimized window
    Unminimize,
    /// Ctrl+Cmd+F (until real full-screen Spaces exist)
    Zoom,
    /// Cmd+`
    CycleAppWindows,
    /// Cmd+Tab (simple version until the app switcher exists)
    CycleWindows,
    /// Ctrl+Super+Left/Right
    SpaceRelative(i32),
    /// Ctrl+Super+1..9
    SpaceIndex(usize),
    /// Ctrl+Super+Shift+Left/Right: take the focused window along
    MoveToSpace(i32),
    /// Ctrl+Super+Shift+1..9: move the focused window to that Space
    MoveToSpaceIndex(usize),
    /// arrow keys in Mission Control: select a window
    McNav(i32, i32),
    /// Return / Space in Mission Control: open the selected window
    McPick,
    /// Super+Up / F3
    MissionControl,
    /// Super+Down: windows of the focused app
    AppExpose,
    /// Super+Tab (true) / Super+Shift+Tab (false)
    Switcher(bool),
    /// Super released while the switcher is open
    SwitcherCommit,
    /// Escape while the switcher is open
    SwitcherCancel,
    /// Super+Space
    Launcher,
    /// F4 / Launchpad key
    Launchpad,
    /// Ctrl+Super+F: full screen in its own Space
    Fullscreen,
    /// Super+Shift+3
    Screenshot,
    /// Ctrl+Super+Shift+3: screen to the clipboard
    ScreenshotToClipboard,
    /// Super+Shift+4 (with Ctrl: to the clipboard)
    ScreenshotArea(bool),
    /// Escape / Space while picking an area
    ScreenshotCancel,
    ScreenshotWindowMode,
    /// volume / brightness / media keys, handled by orchard-shell (key, fine step)
    Media(&'static str, bool),
    /// Option+Cmd+arrows: tile the focused window
    Tile(crate::tiling::Tile),
    /// Option+Cmd+R: back to the size before tiling
    Untile,
    /// Cmd+H / Option+Cmd+H
    HideApp,
    HideOthers,
    /// Option+Cmd+Shift+Escape
    ForceQuit,
    /// Option+Cmd+C
    Center,
    /// Do nothing more
    None,
}

/// Orchard's keyboard shortcuts. Super acts as macOS' Command key.
fn process_keyboard_shortcut(modifiers: ModifiersState, keysym: Keysym) -> Option<KeyAction> {
    let cmd = modifiers.logo;
    if let Some(key) = media_key(keysym) {
        // macOS: Shift+Option+volume/brightness changes in quarter steps
        return Some(KeyAction::Media(key, modifiers.shift && modifiers.alt));
    }
    if modifiers.ctrl && modifiers.alt && keysym == Keysym::BackSpace {
        // ctrl+alt+backspace = quit the compositor
        Some(KeyAction::Quit)
    } else if (xkb::KEY_XF86Switch_VT_1..=xkb::KEY_XF86Switch_VT_12).contains(&keysym.raw()) {
        Some(KeyAction::VtSwitch(
            (keysym.raw() - xkb::KEY_XF86Switch_VT_1 + 1) as i32,
        ))
    } else if cmd && modifiers.alt && !modifiers.ctrl && !modifiers.shift && tile_key(keysym).is_some() {
        tile_key(keysym)
    } else if cmd && keysym == Keysym::Return {
        Some(KeyAction::Run(crate::settings::terminal()))
    } else if cmd && modifiers.shift && (keysym == Keysym::_3 || keysym == Keysym::section || keysym == Keysym::numbersign) {
        // Shift+3 is "§" on German layouts, "#" on US
        Some(if modifiers.ctrl { KeyAction::ScreenshotToClipboard } else { KeyAction::Screenshot })
    } else if cmd && modifiers.shift && (keysym == Keysym::_4 || keysym == Keysym::dollar) {
        Some(KeyAction::ScreenshotArea(modifiers.ctrl))
    } else if cmd && modifiers.alt && modifiers.shift && keysym == Keysym::Escape {
        Some(KeyAction::ForceQuit)
    } else if cmd && modifiers.alt && !modifiers.shift && keysym == Keysym::Escape {
        // macOS shows its Force Quit window here; the closest thing is the system monitor
        Some(KeyAction::Run("plasma-systemmonitor || ksysguard".into()))
    } else if cmd && modifiers.alt && modifiers.shift && (keysym == Keysym::q || keysym == Keysym::Q) {
        // log out without asking (⌥⇧⌘Q on macOS)
        Some(KeyAction::Quit)
    } else if cmd && !modifiers.ctrl && !modifiers.shift && (keysym == Keysym::h || keysym == Keysym::H) {
        Some(if modifiers.alt { KeyAction::HideOthers } else { KeyAction::HideApp })
    } else if cmd && modifiers.ctrl && keysym == Keysym::q {
        // macOS: Ctrl+Cmd+Q locks the screen
        Some(KeyAction::Run(crate::settings::locker()))
    } else if cmd && !modifiers.shift && keysym == Keysym::q {
        Some(KeyAction::QuitApp)
    } else if cmd && !modifiers.shift && keysym == Keysym::w {
        Some(KeyAction::CloseWindow)
    } else if cmd && !modifiers.shift && keysym == Keysym::m {
        Some(KeyAction::Minimize)
    } else if cmd && modifiers.alt && keysym == Keysym::m {
        Some(KeyAction::Unminimize)
    } else if cmd && modifiers.ctrl && keysym == Keysym::f {
        Some(KeyAction::Fullscreen)
    } else if cmd && modifiers.alt && keysym == Keysym::f {
        Some(KeyAction::Zoom)
    } else if cmd && (keysym == Keysym::grave || keysym == Keysym::dead_grave || keysym == Keysym::asciicircum || keysym == Keysym::dead_circumflex) {
        // ` on US layouts, ^ (left of 1) on German layouts
        Some(KeyAction::CycleAppWindows)
    } else if cmd && (keysym == Keysym::Tab || keysym == Keysym::ISO_Left_Tab) {
        Some(KeyAction::Switcher(!modifiers.shift))
    } else if cmd && keysym == Keysym::space {
        Some(KeyAction::Launcher)
    } else if cmd && modifiers.ctrl && (xkb::KEY_1..=xkb::KEY_9).contains(&keysym.raw()) {
        Some(KeyAction::SpaceIndex((keysym.raw() - xkb::KEY_1) as usize))
    } else if cmd && modifiers.ctrl && modifiers.shift && keysym == Keysym::Left {
        Some(KeyAction::MoveToSpace(-1))
    } else if cmd && modifiers.ctrl && modifiers.shift && keysym == Keysym::Right {
        Some(KeyAction::MoveToSpace(1))
    } else if cmd && modifiers.ctrl && keysym == Keysym::Left {
        Some(KeyAction::SpaceRelative(-1))
    } else if cmd && modifiers.ctrl && keysym == Keysym::Right {
        Some(KeyAction::SpaceRelative(1))
    } else if (cmd && keysym == Keysym::Up) || keysym == Keysym::F3 || keysym == Keysym::XF86_LaunchA {
        Some(KeyAction::MissionControl)
    } else if cmd && keysym == Keysym::Down {
        Some(KeyAction::AppExpose)
    } else if !cmd && !modifiers.ctrl && !modifiers.alt && (keysym == Keysym::F4 || keysym == Keysym::XF86_LaunchB) {
        // the Launchpad key of Mac keyboards
        Some(KeyAction::Launchpad)
    } else {
        None
    }
}

/// Keys of their own while Mission Control is open (no ⌘/⌃/⌥ held).
fn mc_key(modifiers: &ModifiersState, keysym: Keysym) -> Option<KeyAction> {
    if modifiers.logo || modifiers.ctrl || modifiers.alt {
        return None;
    }
    Some(match keysym {
        Keysym::Left => KeyAction::McNav(-1, 0),
        Keysym::Right => KeyAction::McNav(1, 0),
        Keysym::Up => KeyAction::McNav(0, -1),
        Keysym::Down => KeyAction::McNav(0, 1),
        Keysym::Return | Keysym::KP_Enter | Keysym::space => KeyAction::McPick,
        _ => return None,
    })
}

/// Name of a media / hardware key, as sent to orchard-shell.
fn media_key(keysym: Keysym) -> Option<&'static str> {
    Some(match keysym {
        Keysym::XF86_AudioRaiseVolume => "volume_up",
        Keysym::XF86_AudioLowerVolume => "volume_down",
        Keysym::XF86_AudioMute => "volume_mute",
        Keysym::XF86_AudioMicMute => "mic_mute",
        Keysym::XF86_MonBrightnessUp => "brightness_up",
        Keysym::XF86_MonBrightnessDown => "brightness_down",
        Keysym::XF86_AudioPlay | Keysym::XF86_AudioPause => "play_pause",
        Keysym::XF86_AudioNext => "next",
        Keysym::XF86_AudioPrev => "previous",
        Keysym::XF86_AudioStop => "stop",
        _ => return None,
    })
}

/// Option+Cmd+…: macOS Sequoia's window tiling (there it is Fn+Ctrl+…).
fn tile_key(keysym: Keysym) -> Option<KeyAction> {
    use crate::tiling::Tile;
    Some(match keysym {
        Keysym::Left => KeyAction::Tile(Tile::Left),
        Keysym::Right => KeyAction::Tile(Tile::Right),
        Keysym::Up => KeyAction::Tile(Tile::Top),
        Keysym::Down => KeyAction::Tile(Tile::Bottom),
        Keysym::c | Keysym::C => KeyAction::Center,
        Keysym::r | Keysym::R => KeyAction::Untile,
        _ => return None,
    })
}

/// `p` moved inside `r` (both global logical), staying half a pixel off the
/// far edges so it still hits the window.
pub fn clamp_into(p: Point<f64, Logical>, r: smithay::utils::Rectangle<i32, Logical>) -> Point<f64, Logical> {
    let max_x = (r.loc.x + r.size.w) as f64 - 0.5;
    let max_y = (r.loc.y + r.size.h) as f64 - 0.5;
    (
        p.x.clamp(r.loc.x as f64, max_x.max(r.loc.x as f64)),
        p.y.clamp(r.loc.y as f64, max_y.max(r.loc.y as f64)),
    )
        .into()
}
