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

use orchard_config::{Settings, ThemeMode};
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
    theme::{mix, resolve_dark, skia_color, with_alpha, Fonts, Palette, Weight},
    ui::{hash_id, Rect, TextEvent, Ui, UiState},
};

const INIT_W: u32 = 880;
const INIT_H: u32 = 660;
const MIN_W: u32 = 720;
const MIN_H: u32 = 500;
const SIDEBAR_W: f32 = 220.0;
const TOOLBAR_H: f32 = 52.0;
const ROW_H: f32 = 28.0;
/// Upper bound between frames while waiting for a frame callback, so a
/// compositor that withholds callbacks can't stall the UI.
const FRAME_FALLBACK: Duration = Duration::from_millis(50);

struct App {
    loop_handle: LoopHandle<'static, App>,
    qh: QueueHandle<App>,
    registry: RegistryState,
    output: OutputState,
    shm: Shm,
    seat: SeatState,
    window: Window,
    pool: SlotPool,
    pixmap: Option<Pixmap>,

    fonts: Fonts,
    icons: Icons,
    pal: Palette,
    styled_key: (ThemeMode, [u8; 3]),

    settings: Settings,
    saved: Settings,
    save_timer: Option<RegistrationToken>,
    compositor_live: Option<bool>,

    page: Page,
    back: Vec<Page>,
    forward: Vec<Page>,
    search: String,
    user: (String, String),
    page_state: PageState,
    ui_state: UiState,
    wake: Wake,

    width: u32,
    height: u32,
    scale: i32,
    configured: bool,
    exit: bool,
    mods: Modifiers,

    frame_pending: bool,
    dirty: bool,
    redraw_timer: Option<RegistrationToken>,
    last_frame: Instant,
}

/// Navigation requested while drawing a frame.
#[derive(Clone, Copy)]
enum Nav {
    Go(Page),
    Back,
    Forward,
}

impl App {
    fn update_theme(&mut self) {
        // Keyed on the *mode*, not the resolved value: resolving `Auto` spawns
        // gsettings, which must not happen on every frame.
        let key = (self.settings.appearance.mode, self.settings.appearance.accent.rgb());
        if self.styled_key != key {
            self.pal = Palette::new(resolve_dark(key.0), key.1);
            self.styled_key = key;
        }
    }

    fn navigate(&mut self, nav: Nav) {
        let target = match nav {
            Nav::Go(p) if p != self.page => {
                self.back.push(self.page);
                self.forward.clear();
                p
            }
            Nav::Back => match self.back.pop() {
                Some(p) => {
                    self.forward.push(self.page);
                    p
                }
                None => return,
            },
            Nav::Forward => match self.forward.pop() {
                Some(p) => {
                    self.back.push(self.page);
                    p
                }
                None => return,
            },
            _ => return,
        };
        self.page = target;
        self.ui_state.reset_page();
        self.ui_state.group_heights.clear();
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

    /// Redraw at most once per compositor frame: pointer motion arrives far
    /// faster than the display refreshes.
    fn request_redraw(&mut self) {
        if self.frame_pending && self.last_frame.elapsed() < FRAME_FALLBACK {
            self.dirty = true;
            if self.redraw_timer.is_none() {
                let timer = Timer::from_duration(FRAME_FALLBACK);
                self.redraw_timer = self
                    .loop_handle
                    .insert_source(timer, |_, _, app: &mut App| {
                        app.redraw_timer = None;
                        if app.dirty {
                            app.render();
                        }
                        TimeoutAction::Drop
                    })
                    .ok();
            }
            return;
        }
        self.render();
    }

    fn render(&mut self) {
        if !self.configured || self.width == 0 || self.height == 0 {
            return;
        }
        self.dirty = false;
        if let Some(tok) = self.redraw_timer.take() {
            self.loop_handle.remove(tok);
        }
        self.update_theme();
        let s = self.scale.max(1) as f32;
        let pw = (self.width as i32 * self.scale.max(1)) as u32;
        let ph = (self.height as i32 * self.scale.max(1)) as u32;
        let mut pm = match self.pixmap.take() {
            Some(pm) if pm.width() == pw && pm.height() == ph => pm,
            _ => match Pixmap::new(pw, ph) {
                Some(pm) => pm,
                None => return,
            },
        };

        let mut pass = 0;
        loop {
            pm.fill(skia_color(self.pal.content));
            self.ui_state.hovered_pointer = false;
            let before = self.settings.clone();

            let win_w = self.width as f32;
            let win_h = self.height as f32;
            let nav: Option<Nav>;
            let can_back = !self.back.is_empty();
            let can_fwd = !self.forward.is_empty();

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
                    false,
                );
                let mut view = View {
                    page: self.page,
                    settings: &mut self.settings,
                    page_state: &mut self.page_state,
                    wake: &self.wake,
                    search: &mut self.search,
                    user: &self.user,
                    can_back,
                    can_fwd,
                    compositor_live: self.compositor_live,
                };
                nav = draw_window(&mut ui, &mut view);
                ui.needs_relayout
            };

            self.ui_state.clicked = false;
            self.ui_state.just_pressed = false;
            self.ui_state.text_events.clear();

            let settings_changed = self.settings != before;
            if settings_changed {
                self.update_theme();
                self.schedule_save();
            }

            let prev = self.page;
            if let Some(n) = nav {
                self.navigate(n);
            }
            let again = self.page != prev
                || self.ui_state.popup_result.is_some()
                || needs_relayout
                || settings_changed;
            if again && pass < 3 {
                pass += 1;
                continue;
            }
            break;
        }

        // Pixmap (premultiplied RGBA) -> wl_shm ARGB8888 (BGRA bytes on little endian).
        let stride = pw as i32 * 4;
        let Ok((buf, canvas)) = self
            .pool
            .create_buffer(pw as i32, ph as i32, stride, wl_shm::Format::Argb8888)
        else {
            self.pixmap = Some(pm);
            return;
        };
        for (d, c) in canvas.chunks_exact_mut(4).zip(pm.data().chunks_exact(4)) {
            d[0] = c[2];
            d[1] = c[1];
            d[2] = c[0];
            d[3] = c[3];
        }
        self.pixmap = Some(pm);
        let surf = self.window.wl_surface().clone();
        surf.set_buffer_scale(self.scale.max(1));
        let _ = buf.attach_to(&surf);
        surf.damage_buffer(0, 0, pw as i32, ph as i32);
        surf.frame(&self.qh, surf.clone());
        surf.commit();
        self.frame_pending = true;
        self.last_frame = Instant::now();
    }
}

/// Everything a frame needs besides the painter.
struct View<'a> {
    page: Page,
    settings: &'a mut Settings,
    page_state: &'a mut PageState,
    wake: &'a Wake,
    search: &'a mut String,
    user: &'a (String, String),
    can_back: bool,
    can_fwd: bool,
    compositor_live: Option<bool>,
}

/// Paint the whole window: content pane, toolbar, sidebar, pop-up overlay.
fn draw_window(ui: &mut Ui, v: &mut View) -> Option<Nav> {
    let mut nav = None;
    let (win_w, win_h) = (ui.win_w, ui.win_h);
    let (can_back, can_fwd) = (v.can_back, v.can_fwd);
    // 1. Scrollable content (drawn first; toolbar and sidebar mask its edges).
    let content_x = SIDEBAR_W;
    let content_w = (win_w - SIDEBAR_W).max(200.0);
    let inner_w = (content_w - 40.0).min(680.0);
    let pad_x = content_x + ((content_w - inner_w) * 0.5).max(20.0);

    ui.clip = Some(Rect::new(content_x, TOOLBAR_H, content_w, (win_h - TOOLBAR_H).max(0.0)));
    ui.x = pad_x;
    ui.w = inner_w;
    let start_y = TOOLBAR_H + 4.0 - ui.st.scroll_y;
    ui.y = start_y;

    if v.compositor_live == Some(false) {
        ui.error_line(Some("Saved. Restart Orchard for changes to take effect."));
    }

    pages::show(ui, v.page, v.settings, v.page_state, v.wake);

    let total_h = (ui.y - start_y) + 20.0;
    let view_h = (win_h - TOOLBAR_H).max(1.0);
    let max_scroll = (total_h - view_h).max(0.0);
    ui.st.max_scroll = max_scroll;
    if ui.st.scroll_y > max_scroll {
        ui.st.scroll_y = max_scroll;
    }

    if max_scroll > 1.0 {
        let frac = (view_h / total_h).clamp(0.08, 1.0);
        let bar_h = (view_h * frac).max(28.0);
        let bar_y = TOOLBAR_H + (ui.st.scroll_y / max_scroll) * (view_h - bar_h);
        let bar_r = Rect::new(win_w - 9.0, bar_y + 3.0, 5.0, (bar_h - 6.0).max(12.0));
        ui.fill_rect(bar_r, 2.5, with_alpha(ui.pal.text, 70));
    }

    // 2. Toolbar: ‹ › and the pane title, hairline only once content scrolls under it.
    ui.clip = None;
    let tb = Rect::new(SIDEBAR_W, 0.0, content_w, TOOLBAR_H);
    ui.fill_rect(tb, 0.0, ui.pal.content);
    if ui.st.scroll_y > 0.5 {
        ui.line(SIDEBAR_W, TOOLBAR_H - 0.5, win_w, TOOLBAR_H - 0.5, 1.0, ui.pal.separator);
    }
    let cy = TOOLBAR_H * 0.5;
    let back_r = Rect::new(SIDEBAR_W + 12.0, cy - 13.0, 26.0, 26.0);
    let fwd_r = Rect::new(back_r.right() + 2.0, cy - 13.0, 26.0, 26.0);
    if toolbar_chevron(ui, back_r, true, can_back) {
        nav = Some(Nav::Back);
    }
    if toolbar_chevron(ui, fwd_r, false, can_fwd) {
        nav = Some(Nav::Forward);
    }
    ui.text_elided(
        v.page.title(),
        fwd_r.right() + 12.0,
        cy,
        content_w - 100.0,
        15.0,
        Weight::Bold,
        ui.pal.text,
    );

    // 3. Sidebar.
    let sb = Rect::new(0.0, 0.0, SIDEBAR_W, win_h);
    ui.fill_rect(sb, 0.0, ui.pal.sidebar);
    ui.line(SIDEBAR_W - 0.5, 0.0, SIDEBAR_W - 0.5, win_h, 1.0, ui.pal.group_border);

    let search_r = Rect::new(10.0, 12.0, SIDEBAR_W - 20.0, 28.0);
    let _ = ui.search_field(hash_id("sidebar-search"), search_r, v.search);

    let q = v.search.trim().to_lowercase();
    let mut sy = 52.0;
    if !q.is_empty() {
        let mut any = false;
        for &pg in ALL_PAGES {
            if pg.title().to_lowercase().contains(&q) || pg.keywords().contains(&q) {
                any = true;
                if draw_sidebar_item(ui, pg, v.page == pg, sy) {
                    nav = Some(Nav::Go(pg));
                }
                sy += ROW_H;
            }
        }
        if !any {
            ui.text_center("No Results", SIDEBAR_W * 0.5, sy + 24.0, 13.0, Weight::Regular, ui.pal.text2);
        }
    } else {
        if draw_account(ui, v.user, sy) {
            nav = Some(Nav::Go(Page::System));
        }
        sy += 62.0;
        for (i, section) in SIDEBAR.iter().enumerate() {
            if i > 0 {
                sy += 10.0;
            }
            for &pg in *section {
                if draw_sidebar_item(ui, pg, v.page == pg, sy) {
                    nav = Some(Nav::Go(pg));
                }
                sy += ROW_H;
            }
        }
    }

    // 4. Open pop-up menu on top of everything.
    ui.render_popup_overlay();
    nav
}

fn toolbar_chevron(ui: &mut Ui, r: Rect, left: bool, enabled: bool) -> bool {
    if enabled && ui.hovered(r) {
        ui.fill_rect(r, 6.0, ui.pal.hover);
    }
    let col = if enabled { ui.pal.text2 } else { with_alpha(ui.pal.text3, 120) };
    let (cx, cy) = (r.cx(), r.cy());
    let d = if left { 1.0 } else { -1.0 };
    ui.line(cx + 2.5 * d, cy - 6.0, cx - 3.0 * d, cy, 2.0, col);
    ui.line(cx - 3.0 * d, cy, cx + 2.5 * d, cy + 6.0, 2.0, col);
    enabled && ui.clicked(r)
}

/// The account card at the top of the sidebar (like the Apple Account entry).
fn draw_account(ui: &mut Ui, user: &(String, String), y: f32) -> bool {
    let r = Rect::new(10.0, y, SIDEBAR_W - 20.0, 52.0);
    if ui.hovered(r) {
        ui.fill_rect(r, 8.0, ui.pal.hover);
    }
    let (cx, cy) = (r.x + 24.0, r.cy());
    let top = [0xA8, 0xA8, 0xAE, 255];
    ui.circle_filled(cx, cy, 17.0, mix(top, [0x6E, 0x6E, 0x74, 255], 0.35));
    let initials: String = user
        .0
        .split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    ui.text_center(&initials, cx, cy, 13.5, Weight::Bold, [255, 255, 255, 255]);
    let tx = r.x + 48.0;
    let tw = r.w - 54.0;
    ui.text_elided(&user.0, tx, cy - 8.0, tw, 13.0, Weight::Bold, ui.pal.text);
    ui.text_elided(&user.1, tx, cy + 8.5, tw, 11.0, Weight::Regular, ui.pal.text2);
    ui.clicked(r)
}

fn draw_sidebar_item(ui: &mut Ui, pg: Page, selected: bool, y: f32) -> bool {
    let r = Rect::new(10.0, y, SIDEBAR_W - 20.0, ROW_H);
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

/// Full name (GECOS) and login of the current user, read without subprocesses.
fn current_user() -> (String, String) {
    let login = std::env::var("USER").unwrap_or_default();
    let gecos = std::fs::read_to_string("/etc/passwd").ok().and_then(|pw| {
        pw.lines().find_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            (f.len() > 4 && f[0] == login).then(|| f[4].split(',').next().unwrap_or("").trim().to_string())
        })
    });
    let name = gecos.filter(|g| !g.is_empty()).unwrap_or_else(|| login.clone());
    (name, "Local Account".into())
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
    let mode = settings.appearance.mode;
    let accent = settings.appearance.accent.rgb();
    let pal = Palette::new(resolve_dark(mode), accent);

    let pool = SlotPool::new((INIT_W * INIT_H * 4 * 4) as usize, &shm).expect("shm pool");
    let mut event_loop: EventLoop<App> = EventLoop::try_new().expect("event loop");
    let loop_handle = event_loop.handle();

    let (wake_tx, wake_rx) = channel::channel::<()>();
    loop_handle
        .insert_source(wake_rx, |_, _, app: &mut App| {
            app.request_redraw();
        })
        .expect("wake channel");

    WaylandSource::new(conn, event_queue)
        .insert(loop_handle.clone())
        .expect("wayland source");

    let mut app = App {
        loop_handle,
        qh: qh.clone(),
        registry,
        output,
        shm,
        seat,
        window,
        pool,
        pixmap: None,
        fonts: Fonts::load(),
        icons: Icons::default(),
        pal,
        styled_key: (mode, accent),
        saved: settings.clone(),
        settings,
        save_timer: None,
        compositor_live: None,
        page: start_page,
        back: Vec::new(),
        forward: Vec::new(),
        search: String::new(),
        user: current_user(),
        page_state: PageState::default(),
        ui_state: UiState::default(),
        wake: Wake(wake_tx),
        width: INIT_W,
        height: INIT_H,
        scale: 1,
        configured: false,
        exit: false,
        mods: Modifiers::default(),
        frame_pending: false,
        dirty: false,
        redraw_timer: None,
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
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.frame_pending = false;
        if self.dirty {
            self.render();
        }
    }
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
        if cmd && matches!(ev.keysym, Keysym::bracketleft | Keysym::bracketright) && self.ui_state.focused_input.is_none() {
            self.navigate(if ev.keysym == Keysym::bracketleft { Nav::Back } else { Nav::Forward });
            self.request_redraw();
            return;
        }
        if cmd && (ev.keysym == Keysym::f || ev.keysym == Keysym::F) {
            self.ui_state.focused_input = Some(hash_id("sidebar-search"));
            self.request_redraw();
            return;
        }
        if let Some(p) = self.ui_state.open_popup.as_mut() {
            match ev.keysym {
                Keysym::Escape => self.ui_state.open_popup = None,
                Keysym::Up => p.step(-1),
                Keysym::Down => p.step(1),
                Keysym::Home => p.highlight = Some(0),
                Keysym::End => p.highlight = Some(p.options.len().saturating_sub(1)),
                Keysym::Return | Keysym::KP_Enter | Keysym::space => {
                    if let Some(v) = p.chosen() {
                        self.ui_state.popup_result = Some((p.id, v));
                    }
                    self.ui_state.open_popup = None;
                }
                _ => {}
            }
            self.request_redraw();
            return;
        }
        if ev.keysym == Keysym::Escape {
            if self.ui_state.focused_input.is_some() {
                self.ui_state.text_events.push(TextEvent::Escape);
                self.request_redraw();
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
            self.request_redraw();
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
                // Mouse back/forward side buttons.
                PointerEventKind::Press { button: 0x113 | 0x116, .. } => {
                    self.navigate(Nav::Back);
                    redraw = true;
                }
                PointerEventKind::Press { button: 0x114 | 0x115, .. } => {
                    self.navigate(Nav::Forward);
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
            self.request_redraw();
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


