//! orchard-settings: fast, native Wayland System Settings for Orchard.
//!
//! Pure Wayland SHM + tiny-skia + fontdue (no OpenGL/EGL), event-driven via
//! calloop with zero idle CPU usage and instant startup.

mod about;
mod icons;
mod pages;
mod sys;
mod theme;
mod ui;

use std::time::{Duration, Instant};

use orchard_config::Settings;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm, delegate_xdg_shell, delegate_xdg_window,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            channel,
            timer::{TimeoutAction, Timer},
            EventLoop, LoopHandle, RegistrationToken,
        },
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
        pointer::{AxisScroll, PointerEvent, PointerEventKind, PointerHandler, BTN_LEFT},
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
    icons::Icons,
    pages::{Page, PageState, ALL_PAGES, SIDEBAR},
    sys::Wake,
    theme::{resolve_dark, skia_color, Fonts, Palette, Weight},
    ui::{hash_id, Rect, TextEvent, Ui, UiState},
};

const INIT_W: u32 = 860;
const INIT_H: u32 = 640;
const MIN_W: u32 = 700;
const MIN_H: u32 = 460;
const SIDEBAR_W: f32 = 216.0;
const TOOLBAR_H: f32 = 46.0;

struct App {
    loop_handle: LoopHandle<'static, App>,
    registry: RegistryState,
    output: OutputState,
    shm: Shm,
    seat: SeatState,
    window: Window,
    pool: SlotPool,

    fonts: Fonts,
    icons: Icons,
    pal: Palette,
    styled_key: (bool, [u8; 3]),

    settings: Settings,
    saved: Settings,
    save_timer: Option<RegistrationToken>,
    compositor_live: Option<bool>,

    page: Page,
    search: String,
    page_state: PageState,
    ui_state: UiState,
    wake: Wake,

    width: u32,
    height: u32,
    scale: i32,
    configured: bool,
    exit: bool,
    mods: Modifiers,
    last_frame: Instant,
}

impl App {
    fn update_theme(&mut self) {
        let dark = resolve_dark(self.settings.appearance.mode);
        let accent = self.settings.appearance.accent.rgb();
        if self.styled_key != (dark, accent) {
            self.pal = Palette::new(dark, accent);
            self.styled_key = (dark, accent);
        }
    }

    fn schedule_save(&mut self) {
        if self.settings == self.saved {
            return;
        }
        if let Some(tok) = self.save_timer.take() {
            self.loop_handle.remove(tok);
        }
        let timer = Timer::from_duration(Duration::from_millis(180));
        if let Ok(tok) = self.loop_handle.insert_source(timer, |_, _, app: &mut App| {
            app.save_timer = None;
            app.save_now();
            TimeoutAction::Drop
        }) {
            self.save_timer = Some(tok);
        }
    }

    fn save_now(&mut self) {
        if self.settings == self.saved {
            return;
        }
        if let Err(e) = self.settings.save() {
            eprintln!("orchard-settings: save failed: {e}");
            return;
        }
        self.saved = self.settings.clone();
        let reply = orchard_config::ipc_request(&serde_json::json!({"request": "reload_settings"}));
        self.compositor_live = Some(matches!(reply, Ok(ref v) if v["event"] == "ok"));
    }

    fn render(&mut self) {
        if !self.configured || self.width == 0 || self.height == 0 {
            return;
        }
        self.update_theme();
        let s = self.scale.max(1) as f32;
        let pw = (self.width as i32 * self.scale.max(1)) as u32;
        let ph = (self.height as i32 * self.scale.max(1)) as u32;
        let Some(mut pm) = Pixmap::new(pw, ph) else {
            return;
        };

        let mut pass = 0;
        loop {
            let dry_run = false;
            pm.fill(skia_color(self.pal.content));
            self.ui_state.hovered_pointer = false;

            let win_w = self.width as f32;
            let win_h = self.height as f32;
            let prev_page = self.page;

            let needs_relayout = {
                let mut ui = Ui::new(
                    &mut pm,
                    &self.fonts,
                    &mut self.icons,
                    self.pal,
                    &mut self.ui_state,
                    s,
                    win_w,
                    win_h,
                    dry_run,
                );

                // 1. Main scrollable content area (drawn first so toolbar/sidebar cleanly mask edges)
                let content_x = SIDEBAR_W;
                let content_w = (win_w - SIDEBAR_W).max(200.0);
                let inner_w = (content_w - 48.0).min(640.0);
                let pad_x = content_x + ((content_w - inner_w) * 0.5).max(16.0);

                ui.clip = Some(Rect::new(content_x, TOOLBAR_H, content_w, (win_h - TOOLBAR_H).max(0.0)));
                ui.x = pad_x;
                ui.w = inner_w;
                let start_y = TOOLBAR_H + 12.0 - ui.st.scroll_y;
                ui.y = start_y;

                if self.compositor_live == Some(false) {
                    ui.error_line(Some("Saved. Restart Orchard for changes to take effect."));
                }

                pages::show(&mut ui, self.page, &mut self.settings, &mut self.page_state, &self.wake);

                let total_h = (ui.y - start_y) + 28.0;
                let view_h = (win_h - TOOLBAR_H).max(1.0);
                let max_scroll = (total_h - view_h).max(0.0);
                ui.st.max_scroll = max_scroll;
                if ui.st.scroll_y > max_scroll {
                    ui.st.scroll_y = max_scroll;
                }

                // Subtle scrollbar
                if max_scroll > 1.0 {
                    let frac = (view_h / total_h).clamp(0.08, 1.0);
                    let bar_h = (view_h * frac).max(28.0);
                    let bar_y = TOOLBAR_H + (ui.st.scroll_y / max_scroll) * (view_h - bar_h);
                    let bar_r = Rect::new(win_w - 8.0, bar_y + 2.0, 4.0, (bar_h - 4.0).max(12.0));
                    ui.fill_rect(bar_r, 2.0, ui.pal.text3);
                }

                // 2. Top toolbar over content area
                ui.clip = None;
                let tb = Rect::new(SIDEBAR_W, 0.0, content_w, TOOLBAR_H);
                ui.fill_rect(tb, 0.0, self.pal.content);
                ui.line(SIDEBAR_W, TOOLBAR_H, win_w, TOOLBAR_H, 1.0, self.pal.group_border);
                ui.text(self.page.title(), SIDEBAR_W + 20.0, TOOLBAR_H * 0.5, 15.0, Weight::Bold, self.pal.text);

                // 3. Left sidebar
                let sb = Rect::new(0.0, 0.0, SIDEBAR_W, win_h);
                ui.fill_rect(sb, 0.0, self.pal.sidebar);
                ui.line(SIDEBAR_W, 0.0, SIDEBAR_W, win_h, 1.0, self.pal.group_border);

                // Search input
                let search_r = Rect::new(12.0, 12.0, SIDEBAR_W - 24.0, 28.0);
                let _ = ui.text_input_at(hash_id("sidebar-search"), search_r, &mut self.search, "Search", false);

                // Sidebar items
                let mut sy = 52.0;
                let q = self.search.trim().to_lowercase();
                if !q.is_empty() {
                    let mut any = false;
                    for &pg in ALL_PAGES {
                        if pg.title().to_lowercase().contains(&q) || pg.keywords().contains(&q) {
                            any = true;
                            if draw_sidebar_item(&mut ui, pg, self.page == pg, sy) {
                                self.page = pg;
                            }
                            sy += 30.0;
                        }
                    }
                    if !any {
                        ui.text_center("No Results", SIDEBAR_W * 0.5, sy + 24.0, 12.5, Weight::Regular, self.pal.text2);
                    }
                } else {
                    for section in SIDEBAR {
                        for &pg in *section {
                            if draw_sidebar_item(&mut ui, pg, self.page == pg, sy) {
                                self.page = pg;
                            }
                            sy += 30.0;
                        }
                        sy += 12.0;
                    }
                }

                // 4. Open popup menu overlay (always on top!)
                ui.render_popup_overlay();

                ui.needs_relayout
            };

            // Clear one-shot click/press/text events after first real pass
            self.ui_state.clicked = false;
            self.ui_state.just_pressed = false;
            self.ui_state.text_events.clear();

            if self.page != prev_page {
                self.ui_state.reset_page();
                self.ui_state.group_heights.clear();
                pass += 1;
                if pass < 3 {
                    continue;
                }
            }
            if self.ui_state.popup_result.is_some() && pass < 3 {
                pass += 1;
                continue;
            }
            if needs_relayout && pass < 2 {
                pass += 1;
                continue;
            }
            if self.settings != self.saved {
                self.update_theme();
                self.schedule_save();
                if pass < 2 {
                    pass += 1;
                    continue;
                }
            }
            break;
        }

        // Blit Pixmap (RGBA premultiplied) -> Wayland SHM (ARGB8888 / BGRA premultiplied)
        let stride = pw as i32 * 4;
        let Ok((buf, canvas)) = self
            .pool
            .create_buffer(pw as i32, ph as i32, stride, wl_shm::Format::Argb8888)
        else {
            return;
        };
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
        self.last_frame = Instant::now();
    }
}

fn draw_sidebar_item(ui: &mut Ui, pg: Page, selected: bool, y: f32) -> bool {
    let r = Rect::new(10.0, y, SIDEBAR_W - 20.0, 28.0);
    if selected {
        ui.fill_rect(r, 6.0, ui.pal.accent);
    } else if ui.hovered(r) {
        ui.fill_rect(r, 6.0, ui.pal.hover);
    }
    let (icon, col) = pg.icon();
    let tile = Rect::new(r.x + 6.0, r.cy() - 10.0, 20.0, 20.0);
    ui.icon_tile(tile, col, icon);
    let fg = if selected { [255, 255, 255, 255] } else { ui.pal.text };
    ui.text_elided(pg.title(), r.x + 34.0, r.cy(), r.w - 40.0, 13.0, Weight::Regular, fg);
    ui.clicked(r)
}

fn main() {
    let arg1 = std::env::args().nth(1);
    if matches!(arg1.as_deref(), Some("--about-device" | "--about" | "about-device")) {
        about::run();
        return;
    }
    let start_page = arg1
        .as_deref()
        .and_then(Page::from_arg)
        .unwrap_or(Page::Appearance);

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
    window.set_title("System Settings");
    window.set_app_id("org.orchard.settings");
    window.set_min_size(Some((MIN_W, MIN_H)));
    window.commit();

    let settings = Settings::load();
    let dark = resolve_dark(settings.appearance.mode);
    let accent = settings.appearance.accent.rgb();
    let pal = Palette::new(dark, accent);

    let pool = SlotPool::new((INIT_W * INIT_H * 4 * 4) as usize, &shm).expect("shm pool");
    let mut event_loop: EventLoop<App> = EventLoop::try_new().expect("event loop");
    let loop_handle = event_loop.handle();

    let (wake_tx, wake_rx) = channel::channel::<()>();
    loop_handle
        .insert_source(wake_rx, |_, _, app: &mut App| {
            app.render();
        })
        .expect("wake channel");

    WaylandSource::new(conn, event_queue)
        .insert(loop_handle.clone())
        .expect("wayland source");

    let mut app = App {
        loop_handle,
        registry,
        output,
        shm,
        seat,
        window,
        pool,
        fonts: Fonts::load(),
        icons: Icons::default(),
        pal,
        styled_key: (dark, accent),
        saved: settings.clone(),
        settings,
        save_timer: None,
        compositor_live: None,
        page: start_page,
        search: String::new(),
        page_state: PageState::default(),
        ui_state: UiState::default(),
        wake: Wake(wake_tx),
        width: INIT_W,
        height: INIT_H,
        scale: 1,
        configured: false,
        exit: false,
        mods: Modifiers::default(),
        last_frame: Instant::now(),
    };

    while !app.exit {
        if event_loop.dispatch(None, &mut app).is_err() {
            break;
        }
    }
    app.save_now();
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, f: i32) {
        let f = f.max(1);
        if self.scale != f {
            self.scale = f;
            self.render();
        }
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState { &mut self.output }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl WindowHandler for App {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = true;
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window, cfg: WindowConfigure, _: u32) {
        let nw = cfg.new_size.0.map(|n| n.get()).unwrap_or(self.width).max(MIN_W);
        let nh = cfg.new_size.1.map(|n| n.get()).unwrap_or(self.height).max(MIN_H);
        let resized = !self.configured || nw != self.width || nh != self.height;
        self.width = nw;
        self.height = nh;
        self.configured = true;
        if resized {
            self.ui_state.group_heights.clear();
            self.render();
        }
    }
}

impl SeatHandler for App {
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

impl KeyboardHandler for App {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {}

    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, ev: KeyEvent) {
        let cmd = self.mods.logo || self.mods.ctrl;
        if cmd && (ev.keysym == Keysym::w || ev.keysym == Keysym::W) {
            self.exit = true;
            return;
        }
        if cmd && (ev.keysym == Keysym::f || ev.keysym == Keysym::F) {
            self.ui_state.focused_input = Some(hash_id("sidebar-search"));
            self.render();
            return;
        }
        if ev.keysym == Keysym::Escape {
            if self.ui_state.open_popup.is_some() {
                self.ui_state.open_popup = None;
                self.render();
                return;
            }
            if self.ui_state.focused_input.is_some() {
                self.ui_state.text_events.push(TextEvent::Escape);
                self.render();
                return;
            }
        }
        if self.ui_state.focused_input.is_some() {
            match ev.keysym {
                Keysym::BackSpace => self.ui_state.text_events.push(TextEvent::Backspace),
                Keysym::Return | Keysym::KP_Enter => self.ui_state.text_events.push(TextEvent::Enter),
                _ => {
                    if let Some(txt) = ev.utf8 {
                        for c in txt.chars() {
                            if !c.is_control() {
                                self.ui_state.text_events.push(TextEvent::Char(c));
                            }
                        }
                    }
                }
            }
            self.render();
        }
    }

    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}

    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, mods: Modifiers, _: u32) {
        self.mods = mods;
    }
}

impl PointerHandler for App {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        let mut redraw = false;
        for ev in events {
            match ev.kind {
                PointerEventKind::Enter { .. } => {
                    self.ui_state.pointer_in = true;
                    self.ui_state.pointer = (ev.position.0 as f32, ev.position.1 as f32);
                    redraw = true;
                }
                PointerEventKind::Leave { .. } => {
                    self.ui_state.pointer_in = false;
                    self.ui_state.pointer_down = false;
                    self.ui_state.active_slider = None;
                    redraw = true;
                }
                PointerEventKind::Motion { .. } => {
                    self.ui_state.pointer = (ev.position.0 as f32, ev.position.1 as f32);
                    redraw = true;
                }
                PointerEventKind::Press { button: BTN_LEFT, .. } => {
                    self.ui_state.pointer = (ev.position.0 as f32, ev.position.1 as f32);
                    self.ui_state.pointer_down = true;
                    self.ui_state.just_pressed = true;
                    self.ui_state.pressed_at = Some(self.ui_state.pointer);
                    // Unfocus text input if clicking elsewhere (input widget re-focuses itself if clicked)
                    self.ui_state.focused_input = None;
                    redraw = true;
                }
                PointerEventKind::Release { button: BTN_LEFT, .. } => {
                    self.ui_state.pointer = (ev.position.0 as f32, ev.position.1 as f32);
                    self.ui_state.pointer_down = false;
                    self.ui_state.clicked = self.ui_state.active_slider.is_none();
                    self.ui_state.active_slider = None;
                    redraw = true;
                }
                PointerEventKind::Axis { vertical, .. } => {
                    let dy = scroll_delta(vertical);
                    if dy.abs() > 0.01 {
                        if let Some(p) = self.ui_state.open_popup.as_mut() {
                            p.scroll = (p.scroll + dy).max(0.0);
                        } else if self.ui_state.pointer.0 >= SIDEBAR_W {
                            self.ui_state.scroll_y = (self.ui_state.scroll_y + dy)
                                .clamp(0.0, self.ui_state.max_scroll);
                        }
                        redraw = true;
                    }
                }
                _ => {}
            }
        }
        if redraw {
            self.render();
        }
    }
}

fn scroll_delta(axis: AxisScroll) -> f32 {
    if axis.absolute.abs() > 0.01 {
        axis.absolute as f32 * 1.4
    } else if axis.discrete != 0 {
        axis.discrete as f32 * 36.0
    } else {
        0.0
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm { &mut self.shm }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState { &mut self.registry }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(App);
delegate_output!(App);
delegate_shm!(App);
delegate_seat!(App);
delegate_keyboard!(App);
delegate_pointer!(App);
delegate_xdg_shell!(App);
delegate_xdg_window!(App);
delegate_registry!(App);
