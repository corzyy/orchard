//! Standalone compact "About Device" window (`orchard-settings --about-device`).
//! Pure Wayland SHM + tiny-skia, zero OpenGL.

use std::sync::mpsc;

use orchard_config::Settings;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm, delegate_xdg_shell, delegate_xdg_window,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{channel, EventLoop},
        calloop_wayland_source::WaylandSource,
        client::{
            globals::registry_queue_init,
            protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
            Connection, QueueHandle,
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler, BTN_LEFT},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        xdg::{
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
            XdgShell,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use tiny_skia::Pixmap;

use crate::{
    pages::system::{load_about, AboutInfo},
    sys,
    theme::{fill_circle, fill_rrect, mix, resolve_dark, stroke_rrect, Fonts, Palette, Weight},
    ui::Rect,
};

const W: u32 = 320;
const H: u32 = 410;

struct AboutApp {
    registry: RegistryState,
    output: OutputState,
    shm: Shm,
    seat: SeatState,
    window: Window,
    pool: SlotPool,
    fonts: Fonts,
    pal: Palette,
    scale: i32,
    width: u32,
    height: u32,
    configured: bool,
    exit: bool,
    pointer: (f32, f32),
    btn_rect: Rect,
    info: Option<AboutInfo>,
    info_rx: mpsc::Receiver<AboutInfo>,
}

impl AboutApp {
    fn draw(&mut self) {
        if !self.configured || self.width == 0 || self.height == 0 {
            return;
        }
        let s = self.scale.max(1) as f32;
        let pw = (self.width as i32 * self.scale.max(1)) as u32;
        let ph = (self.height as i32 * self.scale.max(1)) as u32;
        let mut pm = match Pixmap::new(pw, ph) {
            Some(p) => p,
            None => return,
        };
        let p = self.pal;
        pm.fill(crate::theme::skia_color(p.content));

        let w = self.width as f32;
        let h = self.height as f32;

        if let Some(info) = &self.info {
            let is_laptop = matches!(
                info.chassis.as_str(),
                "laptop" | "notebook" | "convertible" | "portable" | "sub-notebook"
            );
            draw_device(&mut pm, w * 0.5, 58.0, s, is_laptop, p);

            let title = if info.hostname.is_empty() {
                "This Device"
            } else {
                info.hostname.as_str()
            };
            let tw = self.fonts.measure(title, 20.0, Weight::Bold);
            self.fonts.draw(
                &mut pm,
                title,
                (w * 0.5 - tw * 0.5) * s,
                122.0 * s,
                20.0 * s,
                Weight::Bold,
                p.text,
                None,
            );
            if !info.model.is_empty() {
                let m = self.fonts.elide(&info.model, 12.0, Weight::Regular, w - 40.0);
                let mw = self.fonts.measure(&m, 12.0, Weight::Regular);
                self.fonts.draw(
                    &mut pm,
                    &m,
                    (w * 0.5 - mw * 0.5) * s,
                    142.0 * s,
                    12.0 * s,
                    Weight::Regular,
                    p.text2,
                    None,
                );
            }

            let mut rows: Vec<(&str, String)> = Vec::new();
            if !info.cpu.is_empty() {
                rows.push(("Processor", clean_cpu(&info.cpu)));
            }
            if let Some(gpu) = info.gpus.first() {
                rows.push(("Graphics", gpu.clone()));
            }
            if !info.memory.is_empty() {
                rows.push(("Memory", info.memory.clone()));
            }
            if !info.os.is_empty() {
                rows.push(("OS", info.os.clone()));
            }
            if !info.kernel.is_empty() {
                rows.push(("Kernel", info.kernel.clone()));
            }
            rows.push(("Desktop", format!("Orchard {}", env!("CARGO_PKG_VERSION"))));
            if !info.uptime.is_empty() {
                rows.push(("Uptime", info.uptime.clone()));
            }

            let mut y = 176.0;
            let label_r = 104.0;
            let val_x = 116.0;
            let val_max = (w - val_x - 20.0).max(80.0);
            for (k, v) in &rows {
                let kw = self.fonts.measure(k, 12.0, Weight::Bold);
                self.fonts.draw(
                    &mut pm,
                    k,
                    (label_r - kw) * s,
                    y * s,
                    12.0 * s,
                    Weight::Bold,
                    p.text,
                    None,
                );
                let vs = self.fonts.elide(v, 12.0, Weight::Regular, val_max);
                self.fonts.draw(
                    &mut pm,
                    &vs,
                    val_x * s,
                    y * s,
                    12.0 * s,
                    Weight::Regular,
                    p.text2,
                    None,
                );
                y += 22.0;
            }
        } else {
            let msg = "Loading…";
            let mw = self.fonts.measure(msg, 13.0, Weight::Regular);
            self.fonts.draw(
                &mut pm,
                msg,
                (w * 0.5 - mw * 0.5) * s,
                (h * 0.5) * s,
                13.0 * s,
                Weight::Regular,
                p.text2,
                None,
            );
        }

        // "More Info…" button
        let bw = 106.0;
        let bh = 26.0;
        let br = Rect::new((w - bw) * 0.5, h - 62.0, bw, bh);
        self.btn_rect = br;
        let hov = br.contains(self.pointer.0, self.pointer.1);
        let fill = if hov { mix(p.control, p.text, 0.06) } else { p.control };
        fill_rrect(&mut pm, br.x * s, br.y * s, br.w * s, br.h * s, 6.0 * s, fill);
        stroke_rrect(
            &mut pm,
            br.x * s,
            br.y * s,
            br.w * s,
            br.h * s,
            6.0 * s,
            (1.0 * s).max(1.0),
            p.control_border,
        );
        let bl = "More Info…";
        let blw = self.fonts.measure(bl, 12.5, Weight::Regular);
        self.fonts.draw(
            &mut pm,
            bl,
            (br.cx() - blw * 0.5) * s,
            (br.cy() + 4.2) * s,
            12.5 * s,
            Weight::Regular,
            p.text,
            None,
        );

        let foot = "Orchard Desktop · Wayland";
        let fw = self.fonts.measure(foot, 10.5, Weight::Regular);
        self.fonts.draw(
            &mut pm,
            foot,
            (w * 0.5 - fw * 0.5) * s,
            (h - 16.0) * s,
            10.5 * s,
            Weight::Regular,
            p.text3,
            None,
        );

        let stride = pw as i32 * 4;
        let Ok((buf, canvas)) = self.pool.create_buffer(pw as i32, ph as i32, stride, wl_shm::Format::Argb8888)
        else {
            return;
        };
        // Premultiplied RGBA -> premultiplied BGRA (wl_shm Argb8888 little-endian)
        let src = pm.data();
        for (d, c) in canvas.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
            d[0] = c[2];
            d[1] = c[1];
            d[2] = c[0];
            d[3] = c[3];
        }
        let surf = self.window.wl_surface();
        surf.set_buffer_scale(self.scale.max(1));
        let _ = buf.attach_to(surf);
        surf.damage_buffer(0, 0, pw as i32, ph as i32);
        surf.commit();
    }
}

fn draw_device(pm: &mut Pixmap, cx: f32, cy: f32, s: f32, is_laptop: bool, p: Palette) {
    let body = if p.dark { [0x6E, 0x6E, 0x73, 255] } else { [0xC5, 0xC5, 0xCA, 255] };
    let hi = if p.dark { [0x8E, 0x8E, 0x93, 255] } else { [0xE4, 0xE4, 0xE9, 255] };
    let bezel = [0x1C, 0x1C, 0x1E, 255];
    let screen_top = mix(p.accent, [0x1D, 0x26, 0x3B, 255], 0.35);
    let screen_bot = mix(p.accent, [0x0E, 0x11, 0x1A, 255], 0.68);

    let (dw, dh) = if is_laptop { (96.0, 60.0) } else { (104.0, 64.0) };
    let x = cx - dw * 0.5;
    let y = cy - dh * 0.5;
    fill_rrect(pm, x * s, (y + 2.0) * s, dw * s, dh * s, 7.0 * s, [0, 0, 0, 35]);
    fill_rrect(pm, x * s, y * s, dw * s, dh * s, 7.0 * s, hi);
    stroke_rrect(pm, x * s, y * s, dw * s, dh * s, 7.0 * s, s, body);
    fill_rrect(pm, (x + 2.0) * s, (y + 2.0) * s, (dw - 4.0) * s, (dh - 4.0) * s, 5.0 * s, bezel);
    fill_rrect(pm, (x + 5.0) * s, (y + 5.0) * s, (dw - 10.0) * s, (dh - 10.0) * s, 3.0 * s, screen_bot);
    fill_rrect(pm, (x + 5.0) * s, (y + 5.0) * s, (dw - 10.0) * s, (dh - 10.0) * 0.55 * s, 3.0 * s, screen_top);
    fill_circle(pm, (cx + 10.0) * s, (cy + 2.0) * s, 18.0 * s, [p.accent[0], p.accent[1], p.accent[2], 90]);

    if is_laptop {
        fill_rrect(pm, (cx - 58.0) * s, (y + dh + 1.0) * s, 116.0 * s, 7.0 * s, 3.0 * s, hi);
        stroke_rrect(pm, (cx - 58.0) * s, (y + dh + 1.0) * s, 116.0 * s, 7.0 * s, 3.0 * s, s, body);
    } else {
        fill_rrect(pm, (cx - 10.0) * s, (y + dh) * s, 20.0 * s, 12.0 * s, 1.0 * s, body);
        fill_rrect(pm, (cx - 22.0) * s, (y + dh + 11.0) * s, 44.0 * s, 4.0 * s, 2.0 * s, hi);
    }
}

pub fn clean_cpu(raw: &str) -> String {
    raw.replace("(R)", "")
        .replace("(TM)", "")
        .replace(" CPU", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn run() {
    let conn = Connection::connect_to_env().expect("connect to Wayland");
    let (globals, event_queue) = registry_queue_init(&conn).expect("registry");
    let qh = event_queue.handle();

    let registry = RegistryState::new(&globals);
    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor");
    let xdg_shell = XdgShell::bind(&globals, &qh).expect("xdg_wm_base");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm");
    let seat = SeatState::new(&globals, &qh);
    let output = OutputState::new(&globals, &qh);

    let surface = compositor.create_surface(&qh);
    let window = xdg_shell.create_window(surface, WindowDecorations::RequestServer, &qh);
    window.set_title("About Device");
    window.set_app_id("org.orchard.about");
    window.set_min_size(Some((W, H)));
    window.set_max_size(Some((W, H)));
    window.commit();

    let settings = Settings::load();
    let pal = Palette::new(resolve_dark(settings.appearance.mode), settings.appearance.accent.rgb());
    let pool = SlotPool::new((W * H * 4 * 4) as usize, &shm).expect("shm pool");

    let mut event_loop: EventLoop<AboutApp> = EventLoop::try_new().expect("event loop");
    let (wake_tx, wake_rx) = channel::channel::<()>();
    let (info_tx, info_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = info_tx.send(load_about());
        let _ = wake_tx.send(());
    });

    event_loop
        .handle()
        .insert_source(wake_rx, |_, _, app: &mut AboutApp| {
            if let Ok(info) = app.info_rx.try_recv() {
                app.info = Some(info);
                app.draw();
            }
        })
        .expect("wake source");

    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .expect("wayland source");

    let mut app = AboutApp {
        registry,
        output,
        shm,
        seat,
        window,
        pool,
        fonts: Fonts::load(),
        pal,
        scale: 1,
        width: W,
        height: H,
        configured: false,
        exit: false,
        pointer: (0.0, 0.0),
        btn_rect: Rect::default(),
        info: None,
        info_rx,
    };

    while !app.exit {
        if event_loop.dispatch(None, &mut app).is_err() {
            break;
        }
    }
}

impl CompositorHandler for AboutApp {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, f: i32) {
        self.scale = f.max(1);
        self.draw();
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for AboutApp {
    fn output_state(&mut self) -> &mut OutputState { &mut self.output }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl WindowHandler for AboutApp {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = true;
    }
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window, cfg: WindowConfigure, _: u32) {
        self.width = cfg.new_size.0.map(|n| n.get()).unwrap_or(W);
        self.height = cfg.new_size.1.map(|n| n.get()).unwrap_or(H);
        self.configured = true;
        self.draw();
    }
}

impl SeatHandler for AboutApp {
    fn seat_state(&mut self) -> &mut SeatState { &mut self.seat }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard {
            let _ = self.seat.get_keyboard(qh, &seat, None);
        } else if cap == Capability::Pointer {
            let _ = self.seat.get_pointer(qh, &seat);
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, _: Capability) {}
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for AboutApp {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {}
    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, ev: KeyEvent) {
        if ev.keysym == Keysym::Escape {
            self.exit = true;
        }
    }
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: Modifiers, _: u32) {}
}

impl PointerHandler for AboutApp {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        let mut redraw = false;
        for ev in events {
            match ev.kind {
                PointerEventKind::Motion { .. } | PointerEventKind::Enter { .. } => {
                    let was = self.btn_rect.contains(self.pointer.0, self.pointer.1);
                    self.pointer = (ev.position.0 as f32, ev.position.1 as f32);
                    let now = self.btn_rect.contains(self.pointer.0, self.pointer.1);
                    if was != now {
                        redraw = true;
                    }
                }
                PointerEventKind::Release { button: BTN_LEFT, .. } => {
                    if self.btn_rect.contains(self.pointer.0, self.pointer.1) {
                        let exe = std::env::current_exe()
                            .ok()
                            .and_then(|p| p.to_str().map(String::from))
                            .unwrap_or_else(|| "orchard-settings".into());
                        sys::spawn_detached(&exe, &["system"]);
                    }
                }
                _ => {}
            }
        }
        if redraw {
            self.draw();
        }
    }
}

impl ShmHandler for AboutApp {
    fn shm_state(&mut self) -> &mut Shm { &mut self.shm }
}

impl ProvidesRegistryState for AboutApp {
    fn registry(&mut self) -> &mut RegistryState { &mut self.registry }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(AboutApp);
delegate_output!(AboutApp);
delegate_shm!(AboutApp);
delegate_seat!(AboutApp);
delegate_keyboard!(AboutApp);
delegate_pointer!(AboutApp);
delegate_xdg_shell!(AboutApp);
delegate_xdg_window!(AboutApp);
delegate_registry!(AboutApp);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_cpu_strips_trademarks() {
        assert_eq!(
            clean_cpu("Intel(R) Core(TM) i7-12700K CPU (20 threads)"),
            "Intel Core i7-12700K (20 threads)"
        );
    }
}
