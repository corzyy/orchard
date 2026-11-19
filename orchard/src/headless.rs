//! Headless backend: renders offscreen on the GPU (EGL device, no window).
//!
//! Used for automated testing while no display is available. Control it by
//! appending lines to `$XDG_RUNTIME_DIR/orchard-ctl`:
//!
//! ```text
//! motion <x> <y>      move the pointer (logical coordinates)
//! down | up           press / release the left mouse button
//! rdown | rup | mdown | mup   right / middle button
//! super down|up       treat mouse buttons as if Super were held
//! action <name>       run a window action (close, minimize, unminimize, zoom, cycle, cycle-app, quit)
//! shot <path.png>     save the next frame as PNG
//! spawn <cmd>         run a command with WAYLAND_DISPLAY set
//! ```

use std::{
    path::PathBuf,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        allocator::{dmabuf::Dmabuf, Fourcc},
        egl::{EGLContext, EGLDevice, EGLDisplay},
        renderer::{
            damage::OutputDamageTracker,
            gles::{GlesRenderer, GlesTexture},
            Bind, ExportMem, ImportDma, ImportEgl, ImportMemWl, Offscreen,
        },
    },
    delegate_dmabuf,
    input::keyboard::LedState,
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::EventLoop,
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::{protocol::wl_surface, Display},
    },
    utils::{Rectangle, Size, Transform},
    wayland::{
        dmabuf::{DmabufFeedbackBuilder, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        presentation::Refresh,
    },
};
use tracing::{error, info, warn};

use crate::{
    render::{render_output, CustomRenderElements, RenderCtx},
    state::{take_presentation_feedback, update_primary_scanout_output, Backend, OrchardState},
};

pub const OUTPUT_NAME: &str = "headless";

pub struct HeadlessData {
    renderer: GlesRenderer,
    texture: GlesTexture,
    damage_tracker: OutputDamageTracker,
    dmabuf_state: (DmabufState, DmabufGlobal),
    full_redraw: u8,
}

impl DmabufHandler for OrchardState<HeadlessData> {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.backend_data.dmabuf_state.0
    }

    fn dmabuf_imported(&mut self, _global: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        if self.backend_data.renderer.import_dmabuf(&dmabuf, None).is_ok() {
            let _ = notifier.successful::<OrchardState<HeadlessData>>();
        } else {
            notifier.failed();
        }
    }
}
delegate_dmabuf!(OrchardState<HeadlessData>);

impl Backend for HeadlessData {
    // driven by the `rel DX DY` control command
    const HAS_RELATIVE_MOTION: bool = true;
    fn seat_name(&self) -> String {
        String::from("headless")
    }
    fn reset_buffers(&mut self, _output: &Output) {
        self.full_redraw = 4;
    }
    fn early_import(&mut self, _surface: &wl_surface::WlSurface) {}
    fn update_led_state(&mut self, _led_state: LedState) {}
    fn with_renderer<T>(&mut self, f: impl FnOnce(&mut GlesRenderer) -> T) -> Option<T> {
        Some(f(&mut self.renderer))
    }
    // virtual outputs: offer 60 and 120 Hz so the settings path can be tested
    fn output_modes(&self, output: &Output) -> Vec<Mode> {
        let Some(size) = output.preferred_mode().map(|m| m.size) else { return Vec::new() };
        [60_000, 120_000].into_iter().map(|refresh| Mode { size, refresh }).collect()
    }
    fn set_output_mode(&mut self, output: &Output, mode: Mode) -> bool {
        let ok = self.output_modes(output).contains(&mode);
        info!(?mode, ok, "{}: headless mode change", output.name());
        ok
    }
}

fn ctl_path() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("orchard-ctl"))
}

fn take_ctl_lines() -> Vec<String> {
    let Some(path) = ctl_path() else { return Vec::new() };
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    if content.is_empty() {
        return Vec::new();
    }
    let _ = std::fs::write(&path, "");
    content.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()
}

fn parse_size() -> (i32, i32) {
    std::env::var("ORCHARD_HEADLESS_SIZE")
        .ok()
        .and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((1440, 900))
}

pub fn run_headless() {
    let mut event_loop = EventLoop::try_new().unwrap();
    let display = Display::new().unwrap();
    let mut display_handle = display.handle();

    let device = match EGLDevice::enumerate().map(|mut it| it.find(|d| d.try_get_render_node().ok().flatten().is_some())) {
        Ok(Some(d)) => d,
        other => {
            error!("no usable EGL device for headless rendering: {:?}", other.err());
            return;
        }
    };
    let render_node = device.try_get_render_node().ok().flatten();
    let egl_display = match unsafe { EGLDisplay::new(device) } {
        Ok(d) => d,
        Err(e) => {
            error!("failed to create EGL display: {e}");
            return;
        }
    };
    let context = EGLContext::new(&egl_display).expect("EGL context");
    let mut renderer = unsafe { GlesRenderer::new(context) }.expect("GLES renderer");

    let (w, h) = parse_size();
    let size: Size<i32, smithay::utils::Buffer> = (w, h).into();
    let texture: GlesTexture = renderer
        .create_buffer(Fourcc::Abgr8888, size)
        .expect("offscreen buffer");

    let mode = Mode {
        size: (w, h).into(),
        refresh: 60_000,
    };
    let output = Output::new(
        OUTPUT_NAME.to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Orchard".into(),
            model: "Headless".into(),
        },
    );
    let _global = output.create_global::<OrchardState<HeadlessData>>(&display.handle());
    let scale = std::env::var("ORCHARD_HEADLESS_SCALE")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(1.0);
    output.change_current_state(
        Some(mode),
        Some(Transform::Normal),
        Some(smithay::output::Scale::Fractional(scale)),
        Some((0, 0).into()),
    );
    output.set_preferred(mode);

    let formats = renderer.dmabuf_formats();
    let mut dmabuf_state = DmabufState::new();
    let dmabuf_global = match render_node
        .and_then(|node| DmabufFeedbackBuilder::new(node.dev_id(), formats.clone()).build().ok())
    {
        Some(feedback) => dmabuf_state
            .create_global_with_default_feedback::<OrchardState<HeadlessData>>(&display.handle(), &feedback),
        None => dmabuf_state.create_global::<OrchardState<HeadlessData>>(&display.handle(), formats),
    };

    if renderer.bind_wl_display(&display.handle()).is_ok() {
        info!("EGL hardware-acceleration enabled");
    }

    let damage_tracker = OutputDamageTracker::from_output(&output);
    let data = HeadlessData {
        renderer,
        texture,
        damage_tracker,
        dmabuf_state: (dmabuf_state, dmabuf_global),
        full_redraw: 0,
    };
    let mut state = OrchardState::init(display, event_loop.handle(), data, true);
    state
        .shm_state
        .update_formats(state.backend_data.renderer.shm_formats());
    state.space.map_output(&output, (0, 0));

    // ORCHARD_HEADLESS_OUTPUTS=2: a second, never-rendered output to the
    // right (tests multi-display logic such as the primary display)
    let extra = std::env::var("ORCHARD_HEADLESS_OUTPUTS")
        .ok()
        .and_then(|n| n.parse::<i32>().ok())
        .unwrap_or(1);
    let mut _extra_globals = Vec::new();
    let mut extra_outputs: Vec<Output> = Vec::new();
    for i in 1..extra.max(1) {
        let o = Output::new(
            format!("{OUTPUT_NAME}-{}", i + 1),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Orchard".into(),
                model: "Headless".into(),
            },
        );
        _extra_globals.push(o.create_global::<OrchardState<HeadlessData>>(&state.display_handle));
        let pos = (w * i, 0);
        o.change_current_state(Some(mode), Some(Transform::Normal), Some(smithay::output::Scale::Integer(1)), Some(pos.into()));
        o.set_preferred(mode);
        state.space.map_output(&o, pos);
        extra_outputs.push(o);
    }

    #[cfg(feature = "xwayland")]
    state.start_xwayland();

    if let Some(p) = ctl_path() {
        let _ = std::fs::write(&p, "");
        info!("headless control file: {}", p.display());
    }
    info!("Initialization completed, starting the main loop.");

    let start = Instant::now();
    let frame = Duration::from_micros(16_667);
    let mut next_frame = Instant::now();
    let mut pending_shot: Option<PathBuf> = None;

    while state.running.load(Ordering::SeqCst) {
        let now_ms = start.elapsed().as_millis() as u32;
        for line in take_ctl_lines() {
            let mut parts = line.splitn(2, ' ');
            let cmd = parts.next().unwrap_or_default();
            let rest = parts.next().unwrap_or_default().trim();
            match cmd {
                "motion" => {
                    let mut it = rest.split_whitespace().filter_map(|v| v.parse::<f64>().ok());
                    if let (Some(x), Some(y)) = (it.next(), it.next()) {
                        state.inject_pointer_motion((x, y).into(), now_ms);
                    }
                }
                "rel" => {
                    let mut it = rest.split_whitespace().filter_map(|v| v.parse::<f64>().ok());
                    if let (Some(dx), Some(dy)) = (it.next(), it.next()) {
                        let d = (dx, dy).into();
                        state.pointer_move_relative(d, d, now_ms as u64 * 1000, now_ms);
                    }
                }
                "down" => state.inject_pointer_button(0x110, true, now_ms),
                "up" => state.inject_pointer_button(0x110, false, now_ms),
                "rdown" => state.inject_pointer_button(0x111, true, now_ms),
                "rup" => state.inject_pointer_button(0x111, false, now_ms),
                "mdown" => state.inject_pointer_button(0x112, true, now_ms),
                "mup" => state.inject_pointer_button(0x112, false, now_ms),
                "super" => state.fake_logo = rest == "down",
                "action" => state.run_named_action(rest),
                "shot" => pending_shot = Some(PathBuf::from(rest)),
                "spawn" => {
                    let mut cmd = std::process::Command::new("sh");
                    cmd.arg("-c").arg(rest);
                    if let Some(s) = &state.socket_name {
                        cmd.env("WAYLAND_DISPLAY", s);
                    }
                    #[cfg(feature = "xwayland")]
                    if let Some(d) = state.xdisplay {
                        cmd.env("DISPLAY", format!(":{d}"));
                    }
                    if let Err(e) = cmd.spawn() {
                        warn!("spawn {rest:?} failed: {e}");
                    }
                }
                other => warn!("unknown control command {other:?}"),
            }
        }

        if Instant::now() >= next_frame {
            next_frame += frame;
            if next_frame < Instant::now() {
                next_frame = Instant::now() + frame;
            }
            let frame_target = state.clock.now() + frame;
            state.pre_repaint(&output, frame_target);

            let full = state.backend_data.full_redraw > 0 || pending_shot.is_some();
            state.backend_data.full_redraw = state.backend_data.full_redraw.saturating_sub(1);
            let data = &mut state.backend_data;
            let renderer = &mut data.renderer;
            let result = renderer.bind(&mut data.texture).map_err(|e| e.to_string()).and_then(|mut target| {
                let mut ctx = RenderCtx {
                    fx: &mut state.fx,
                    backdrops: &state.ipc.backdrops,
                    mc: state.mc.as_ref(),
                    space_info: state.spaces.info(&output.name()),
                    locked: state.protocols.locked,
                    lock_surface: crate::protocols::lock_surface_of(&output),
                };
                let res = render_output(
                    &output,
                    &state.space,
                    &mut ctx,
                    Vec::<CustomRenderElements<GlesRenderer>>::new(),
                    renderer,
                    &mut target,
                    &mut data.damage_tracker,
                    if full { 0 } else { 1 },
                    state.clock.now().into(),
                )
                .map(|r| (r.damage.is_some(), r.states))
                .map_err(|e| format!("{e:?}"))?;
                if let Some(path) = pending_shot.as_ref() {
                    let region = Rectangle::from_size(size);
                    let shot = renderer
                        .copy_framebuffer(&target, region, Fourcc::Abgr8888)
                        .and_then(|m| renderer.map_texture(&m).map(|d| d.to_vec()));
                    match shot {
                        Ok(bytes) => match image::RgbaImage::from_raw(w as u32, h as u32, bytes) {
                            Some(img) => match img.save(path) {
                                Ok(()) => info!("screenshot saved to {}", path.display()),
                                Err(e) => warn!("screenshot save failed: {e}"),
                            },
                            None => warn!("unexpected screenshot size"),
                        },
                        Err(e) => warn!("screenshot readback failed: {e:?}"),
                    }
                }
                Ok(res)
            });
            match result {
                Ok((_rendered, states)) => {
                    // Must run before presentation feedback is collected: feedback
                    // is only sent for surfaces whose primary output is known.
                    // Without it, EGL clients using presentation-time / fifo
                    // (NVIDIA) block forever in eglSwapBuffers.
                    update_primary_scanout_output(&state.space, &output, &state.dnd_icon, &state.cursor_status, &states);
                    {
                        // no vsync here: every frame counts as presented
                        let mut feedback = take_presentation_feedback(&output, &state.space, &states);
                        feedback.presented(
                            frame_target,
                            Refresh::fixed(frame),
                            0,
                            wp_presentation_feedback::Kind::empty(),
                        );
                    }
                    state.post_repaint(&output, frame_target, None, &states);
                }
                Err(e) => warn!("headless render failed: {e}"),
            }
            // the other (normally never rendered) outputs: <shot>-2.png, ...
            if let Some(path) = pending_shot.take() {
                for (i, o) in extra_outputs.iter().enumerate() {
                    state.pre_repaint(o, frame_target);
                    let p = path.with_file_name(format!(
                        "{}-{}.png",
                        path.file_stem().and_then(|s| s.to_str()).unwrap_or("shot"),
                        i + 2
                    ));
                    if let Err(e) = shot_output(&mut state, o, size, &p) {
                        warn!("screenshot of {} failed: {e}", o.name());
                    }
                }
            }
        }

        let timeout = next_frame.saturating_duration_since(Instant::now()).min(Duration::from_millis(4));
        if event_loop.dispatch(Some(timeout), &mut state).is_err() {
            state.running.store(false, Ordering::SeqCst);
        } else {
            state.space.refresh();
            state.popups.cleanup();
            let _ = display_handle.flush_clients();
        }
    }
}

/// Render `output` into a fresh buffer and save it as a PNG (tests only).
fn shot_output(
    state: &mut OrchardState<HeadlessData>,
    output: &Output,
    size: Size<i32, smithay::utils::Buffer>,
    path: &std::path::Path,
) -> Result<(), String> {
    let data = &mut state.backend_data;
    let renderer = &mut data.renderer;
    let mut tex: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, size).map_err(|e| format!("{e:?}"))?;
    let mut tracker = OutputDamageTracker::from_output(output);
    let mut target = renderer.bind(&mut tex).map_err(|e| format!("{e:?}"))?;
    let mut ctx = RenderCtx {
        fx: &mut state.fx,
        backdrops: &state.ipc.backdrops,
        mc: state.mc.as_ref(),
        space_info: state.spaces.info(&output.name()),
        locked: state.protocols.locked,
        lock_surface: crate::protocols::lock_surface_of(output),
    };
    render_output(
        output,
        &state.space,
        &mut ctx,
        Vec::<CustomRenderElements<GlesRenderer>>::new(),
        renderer,
        &mut target,
        &mut tracker,
        0,
        state.clock.now().into(),
    )
    .map_err(|e| format!("{e:?}"))?;
    let bytes = renderer
        .copy_framebuffer(&target, Rectangle::from_size(size), Fourcc::Abgr8888)
        .and_then(|m| renderer.map_texture(&m).map(|d| d.to_vec()))
        .map_err(|e| format!("{e:?}"))?;
    let img = image::RgbaImage::from_raw(size.w as u32, size.h as u32, bytes).ok_or("unexpected size")?;
    img.save(path).map_err(|e| e.to_string())?;
    info!("screenshot saved to {}", path.display());
    Ok(())
}
