//! orchard-lock: the macOS-style lock screen for Orchard.
//!
//! An ext-session-lock client. It asks for the lock before anything else
//! (fonts load on other threads meanwhile), draws the clock, the account
//! picture and the password field on a transparent surface over the
//! compositor's blurred wallpaper, and checks the password through PAM.
//! On success it fades out and unlocks; the compositor then dissolves the
//! lock backdrop into the desktop.
//!
//! If this process dies, the session stays locked and Orchard starts a new one.

mod pam;
mod text;
mod ui;
mod user;

use std::time::{Duration, Instant};

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_output, delegate_pointer, delegate_registry, delegate_seat,
    delegate_session_lock, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            channel,
            timer::{TimeoutAction, Timer},
            EventLoop, LoopHandle,
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
        pointer::{PointerEvent, PointerEventKind, PointerHandler, BTN_LEFT},
        Capability, SeatHandler, SeatState,
    },
    session_lock::{SessionLock, SessionLockHandler, SessionLockState, SessionLockSurface, SessionLockSurfaceConfigure},
    shm::{
        slot::{Buffer, SlotPool},
        Shm, ShmHandler,
    },
};
use tiny_skia::Pixmap;

use crate::{
    text::{Fonts, TextCache},
    ui::{Hit, Layout, View, BAND_ALL, BAND_CLOCK, BAND_LOGIN, BAND_POWER},
};

const FADE_IN: Duration = Duration::from_millis(220);
const FADE_OUT: Duration = Duration::from_millis(150);
const SHAKE: Duration = Duration::from_millis(520);

/// Physical-pixel rectangle (x, y, w, h).
type Rect = (i32, i32, i32, i32);

struct Buf {
    buffer: Buffer,
    /// regions changed since this buffer last got the canvas
    stale: Vec<Rect>,
}

struct Screen {
    output: wl_output::WlOutput,
    surface: SessionLockSurface,
    /// logical size from the configure
    size: (u32, u32),
    scale: i32,
    canvas: Option<Pixmap>,
    pool: Option<SlotPool>,
    bufs: Vec<Buf>,
    /// damage not committed yet (no free buffer last time)
    pending: Vec<Rect>,
    dirty: u8,
    frame_pending: bool,
    /// commits so far (matches a fallback timer to its commit)
    commits: u64,
    configured: bool,
    primary: bool,
    avatar: Option<(i32, Pixmap)>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    Idle,
    Checking(Instant),
    Unlocking(Instant),
}

struct Strings {
    placeholder: &'static str,
    power: [&'static str; 3],
    unavailable: &'static str,
}

struct App {
    conn: Connection,
    loop_handle: LoopHandle<'static, App>,
    registry_state: RegistryState,
    output_state: OutputState,
    compositor: CompositorState,
    shm: Shm,
    seat_state: SeatState,
    /// keeps the ext-session-lock manager bound
    _lock_state: SessionLockState,
    qh: QueueHandle<App>,
    lock: Option<SessionLock>,
    screens: Vec<Screen>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    mods: Modifiers,
    password: Vec<u8>,
    chars: usize,
    phase: Phase,
    shown_at: Option<Instant>,
    shake_at: Option<Instant>,
    message: Option<String>,
    hover: Option<Hit>,
    pressed: Option<Hit>,
    pointer_surface: Option<wl_surface::WlSurface>,
    text: Option<TextCache>,
    fonts: Option<std::thread::JoinHandle<Option<Fonts>>>,
    user: user::User,
    picture: Option<image::RgbaImage>,
    picture_loader: Option<std::thread::JoinHandle<Option<image::RgbaImage>>>,
    auth_tx: channel::Sender<Result<(), String>>,
    strings: Strings,
    german: bool,
    h24: bool,
    primary_output: Option<String>,
    exit: bool,
}

fn german() -> bool {
    std::env::var("LC_ALL")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var("LC_TIME").ok().filter(|v| !v.is_empty()))
        .or_else(|| std::env::var("LANG").ok())
        .is_some_and(|l| l.starts_with("de"))
}

fn locale_24h() -> bool {
    let l = std::env::var("LC_TIME").or_else(|_| std::env::var("LANG")).unwrap_or_default();
    !["en_US", "en_CA", "en_AU", "en_IN", "en_PH", "en_NZ"].iter().any(|p| l.starts_with(p))
}

static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// `ORCHARD_LOCK_TRACE=1`: print startup timings to stderr.
fn trace(what: &str) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ON.get_or_init(|| std::env::var_os("ORCHARD_LOCK_TRACE").is_some()) {
        let ms = START.get().map(|t| t.elapsed().as_secs_f32() * 1000.0).unwrap_or(0.0);
        eprintln!("orchard-lock: {ms:7.1} ms  {what}");
    }
}

fn main() {
    START.get_or_init(Instant::now);
    // fonts and the account picture load while we talk to the compositor
    let fonts = Fonts::spawn();
    let user = user::User::current();
    let picture_loader = {
        let probe = user::User { login: user.login.clone(), display: String::new(), home: user.home.clone() };
        std::thread::spawn(move || probe.picture())
    };

    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("orchard-lock: no Wayland display: {e}");
            std::process::exit(1);
        }
    };
    trace("connected");
    let (globals, event_queue) = registry_queue_init(&conn).expect("registry");
    trace("registry");
    let qh: QueueHandle<App> = event_queue.handle();
    let mut event_loop: EventLoop<App> = EventLoop::try_new().expect("event loop");

    let lock_state = SessionLockState::new(&globals, &qh);
    // ask for the lock first: the compositor confirms it immediately
    let lock = match lock_state.lock(&qh) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("orchard-lock: the compositor does not support ext-session-lock: {e}");
            std::process::exit(1);
        }
    };
    let _ = conn.flush();
    trace("lock requested");

    let (auth_tx, auth_rx) = channel::channel::<Result<(), String>>();
    event_loop
        .handle()
        .insert_source(auth_rx, |ev, _, app| {
            if let channel::Event::Msg(res) = ev {
                app.auth_done(res);
            }
        })
        .expect("auth channel");

    let settings = orchard_config::Settings::load();
    let german = german();
    let strings = if german {
        Strings {
            placeholder: "Passwort eingeben",
            power: ["Ruhezustand", "Neustart", "Ausschalten"],
            unavailable: "PAM ist nicht verfügbar",
        }
    } else {
        Strings {
            placeholder: "Enter Password",
            power: ["Sleep", "Restart", "Shut Down"],
            unavailable: "Authentication is unavailable",
        }
    };

    let mut app = App {
        conn: conn.clone(),
        loop_handle: event_loop.handle(),
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        compositor: CompositorState::bind(&globals, &qh).expect("wl_compositor"),
        shm: Shm::bind(&globals, &qh).expect("wl_shm"),
        seat_state: SeatState::new(&globals, &qh),
        _lock_state: lock_state,
        qh: qh.clone(),
        lock: Some(lock),
        screens: Vec::new(),
        keyboard: None,
        pointer: None,
        mods: Modifiers::default(),
        password: Vec::with_capacity(512),
        chars: 0,
        phase: Phase::Idle,
        shown_at: None,
        shake_at: None,
        message: None,
        hover: None,
        pressed: None,
        pointer_surface: None,
        text: None,
        fonts: Some(fonts),
        user,
        picture: None,
        picture_loader: Some(picture_loader),
        auth_tx,
        strings,
        german,
        h24: settings.menu_bar.clock_24h.unwrap_or_else(locale_24h),
        primary_output: settings.displays.primary.clone(),
        exit: false,
    };

    WaylandSource::new(conn, event_queue).insert(event_loop.handle()).expect("wayland source");
    app.schedule_clock();

    while !app.exit {
        if event_loop.dispatch(None, &mut app).is_err() {
            break;
        }
    }
    pam::wipe(&mut app.password);
}

impl App {
    fn text(&mut self) -> &mut TextCache {
        if self.text.is_none() {
            trace("waiting for fonts");
            let fonts = self.fonts.take().and_then(|h| h.join().ok()).flatten();
            let Some(fonts) = fonts else {
                eprintln!("orchard-lock: no usable system font; cannot draw the lock screen");
                std::process::exit(1);
            };
            trace("fonts ready");
            self.text = Some(TextCache::new(fonts));
        }
        self.text.as_mut().expect("text cache initialized above")
    }

    fn picture(&mut self) -> Option<&image::RgbaImage> {
        if let Some(h) = self.picture_loader.take() {
            self.picture = h.join().ok().flatten();
        }
        self.picture.as_ref()
    }

    /// Redraw the clock on the next minute.
    fn schedule_clock(&mut self) {
        let now = chrono::Local::now();
        use chrono::Timelike;
        let ms = 60_000 - (now.second() as u64 * 1000 + now.timestamp_subsec_millis() as u64 % 1000) + 30;
        let _ = self.loop_handle.insert_source(Timer::from_duration(Duration::from_millis(ms)), |_, _, app| {
            app.mark(BAND_CLOCK);
            app.schedule_clock();
            TimeoutAction::Drop
        });
    }

    fn time_text(&self) -> (String, String) {
        use chrono::{Datelike, Timelike};
        let now = chrono::Local::now();
        let wd = now.weekday().num_days_from_monday() as usize;
        let m = now.month0() as usize;
        let date = if self.german {
            const D: [&str; 7] = ["Montag", "Dienstag", "Mittwoch", "Donnerstag", "Freitag", "Samstag", "Sonntag"];
            const M: [&str; 12] = [
                "Januar", "Februar", "März", "April", "Mai", "Juni", "Juli", "August", "September", "Oktober", "November",
                "Dezember",
            ];
            format!("{}, {}. {}", D[wd], now.day(), M[m])
        } else {
            const D: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
            const M: [&str; 12] = [
                "January", "February", "March", "April", "May", "June", "July", "August", "September", "October",
                "November", "December",
            ];
            format!("{} {} {}", D[wd], now.day(), M[m])
        };
        let time = if self.h24 {
            format!("{}:{:02}", now.hour(), now.minute())
        } else {
            format!("{}:{:02}", now.hour12().1, now.minute())
        };
        (date, time)
    }

    /// Overall opacity and whether it is still changing.
    fn alpha(&self) -> (f32, bool) {
        match self.phase {
            Phase::Unlocking(t) => {
                let p = (t.elapsed().as_secs_f32() / FADE_OUT.as_secs_f32()).min(1.0);
                (1.0 - p * p, p < 1.0)
            }
            _ => match self.shown_at {
                Some(t) => {
                    let p = (t.elapsed().as_secs_f32() / FADE_IN.as_secs_f32()).min(1.0);
                    (1.0 - (1.0 - p).powi(3), p < 1.0)
                }
                None => (0.0, true),
            },
        }
    }

    fn shake(&self) -> (f32, bool) {
        match self.shake_at {
            Some(t) if t.elapsed() < SHAKE => {
                let s = t.elapsed().as_secs_f32();
                ((-s * 7.0).exp() * (s * std::f32::consts::TAU * 7.5).sin() * 15.0, true)
            }
            _ => (0.0, false),
        }
    }

    /// Bands that keep changing every frame right now.
    fn animating(&self) -> u8 {
        let mut bits = 0;
        if self.alpha().1 {
            bits |= BAND_ALL;
        }
        if self.shake().1 || matches!(self.phase, Phase::Checking(_)) {
            bits |= BAND_LOGIN;
        }
        bits
    }

    fn mark(&mut self, bits: u8) {
        for s in &mut self.screens {
            s.dirty |= bits;
        }
        self.flush();
    }

    fn flush(&mut self) {
        for i in 0..self.screens.len() {
            let s = &self.screens[i];
            if s.configured && !s.frame_pending && (s.dirty != 0 || !s.pending.is_empty()) {
                self.render(i);
            }
        }
        let _ = self.conn.flush();
    }

    fn render(&mut self, i: usize) {
        let qh = self.qh.clone();
        let (date, time) = self.time_text();
        let (alpha, _) = self.alpha();
        let (shake, _) = self.shake();
        let spin = match self.phase {
            Phase::Checking(t) => t.elapsed().as_secs_f32(),
            _ => 0.0,
        };
        let busy = matches!(self.phase, Phase::Checking(_) | Phase::Unlocking(_));
        let name = self.user.display.clone();
        let message = self.message.clone();
        let (w, h, scale, primary) = {
            let s = &self.screens[i];
            (s.size.0, s.size.1, s.scale, s.primary)
        };
        // account picture at this scale (once)
        if primary && self.screens[i].avatar.as_ref().map(|a| a.0) != Some(scale) {
            let d = (72.0 * scale as f32).round() as u32;
            let pic = self.picture().cloned();
            trace("picture ready");
            self.screens[i].avatar = user::avatar(pic.as_ref(), d).map(|p| (scale, p));
        }
        self.text();
        let caps = self.mods.caps_lock;
        let chars = self.chars;
        let (hover, pressed) = (self.hover, self.pressed);
        let strings = (self.strings.placeholder, self.strings.power);

        let App { screens, text, shm, .. } = self;
        let text = text.as_mut().unwrap();
        let screen = &mut screens[i];
        let (pw, ph) = (w as i32 * scale, h as i32 * scale);
        if screen.canvas.as_ref().map(|c| (c.width() as i32, c.height() as i32)) != Some((pw, ph)) {
            screen.canvas = Pixmap::new(pw as u32, ph as u32);
            screen.bufs.clear();
            screen.pending.clear();
            screen.dirty = BAND_ALL;
            let len = (pw * ph * 4) as usize;
            match screen.pool.as_mut() {
                Some(pool) if pool.len() >= len * 2 => {}
                _ => screen.pool = SlotPool::new(len * 2, shm).ok(),
            }
        }
        let Some(canvas) = screen.canvas.as_mut() else { return };
        let layout = Layout::new(w as f32, h as f32, text);
        let view = View {
            alpha,
            date: &date,
            time: &time,
            name: &name,
            avatar: screen.avatar.as_ref().map(|a| &a.1),
            placeholder: strings.0,
            power_labels: strings.1,
            chars,
            caps,
            busy,
            spin,
            shake,
            hover,
            pressed,
            message: message.as_deref(),
        };
        let s = scale as f32;
        trace("drawing");
        let mut damage = Vec::new();
        for (band, (top, bottom)) in layout.bands(screen.dirty, primary) {
            let y0 = ((top * s).floor().max(0.0) as i32).min(ph);
            let y1 = ((bottom * s).ceil() as i32).clamp(y0, ph);
            if y1 > y0 {
                ui::draw_band(canvas, text, &layout, &view, band, s, (y0 as usize, y1 as usize));
                damage.push((0, y0, pw, y1 - y0));
            }
        }
        trace("drawn");
        screen.dirty = 0;
        screen.pending.extend(damage);
        if screen.pending.is_empty() {
            return;
        }
        present(screen, &qh);
        if screen.frame_pending {
            // A compositor that sends no frame callbacks to lock surfaces would
            // otherwise freeze the lock screen on its first (transparent) frame.
            let (surface, commit) = (screen.surface.wl_surface().clone(), screen.commits);
            let _ = self.loop_handle.insert_source(Timer::from_duration(Duration::from_millis(40)), move |_, _, app| {
                if app.screens.iter().any(|s| s.surface.wl_surface() == &surface && s.frame_pending && s.commits == commit) {
                    app.frame_done(&surface);
                }
                TimeoutAction::Drop
            });
        }
    }

    /// The compositor showed our last commit (or the fallback timer fired).
    fn frame_done(&mut self, surface: &wl_surface::WlSurface) {
        if let Phase::Unlocking(t) = self.phase {
            if t.elapsed() >= FADE_OUT {
                self.unlock();
                return;
            }
        }
        let anim = self.animating();
        if let Some(s) = self.screens.iter_mut().find(|s| s.surface.wl_surface() == surface) {
            s.frame_pending = false;
            s.dirty |= anim;
        }
        self.flush();
    }

    // ---- input ---------------------------------------------------------------

    fn key(&mut self, ev: &KeyEvent) {
        if !matches!(self.phase, Phase::Idle) {
            return;
        }
        let k = ev.keysym;
        if k == Keysym::Return || k == Keysym::KP_Enter || k == Keysym::ISO_Enter {
            self.submit();
            return;
        }
        if k == Keysym::Escape || (self.mods.ctrl && (k == Keysym::u || k == Keysym::U || k == Keysym::BackSpace)) {
            self.clear_password();
            self.message = None;
            self.mark(BAND_LOGIN);
            return;
        }
        if k == Keysym::BackSpace {
            if let Some((idx, _)) = std::str::from_utf8(&self.password).ok().and_then(|s| s.char_indices().next_back()) {
                for b in &mut self.password[idx..] {
                    *b = 0;
                }
                self.password.truncate(idx);
                self.chars -= 1;
                self.mark(BAND_LOGIN);
            }
            return;
        }
        if self.mods.ctrl || self.mods.logo {
            return;
        }
        if let Some(t) = ev.utf8.as_deref() {
            let t: String = t.chars().filter(|c| !c.is_control()).collect();
            if !t.is_empty() && self.password.len() + t.len() < self.password.capacity() {
                self.password.extend_from_slice(t.as_bytes());
                self.chars += t.chars().count();
                if self.message.is_some() {
                    self.message = None;
                }
                self.mark(BAND_LOGIN);
            }
        }
    }

    fn clear_password(&mut self) {
        let cap = self.password.capacity();
        pam::wipe(&mut self.password);
        debug_assert_eq!(self.password.capacity(), cap);
        self.chars = 0;
    }

    fn submit(&mut self) {
        if self.chars == 0 || !matches!(self.phase, Phase::Idle) {
            return;
        }
        self.phase = Phase::Checking(Instant::now());
        self.hover = None;
        let user = self.user.login.clone();
        let password = self.password.clone();
        let tx = self.auth_tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(pam::authenticate(&user, password));
        });
        self.mark(BAND_LOGIN);
    }

    fn auth_done(&mut self, res: Result<(), String>) {
        self.clear_password();
        match res {
            Ok(()) => {
                self.phase = Phase::Unlocking(Instant::now());
                self.mark(BAND_ALL);
                // The fade normally ends in frame_done, but there may be no
                // outputs (lid closed), no frame callbacks, or no buffer
                // allocation. Never leave an authenticated user locked out
                // just because nothing repainted.
                let _ = self.loop_handle.insert_source(
                    Timer::from_duration(FADE_OUT + Duration::from_millis(250)),
                    |_, _, app| {
                        if matches!(app.phase, Phase::Unlocking(_)) {
                            app.unlock();
                        }
                        TimeoutAction::Drop
                    },
                );
            }
            Err(e) => {
                eprintln!("orchard-lock: {e}");
                self.phase = Phase::Idle;
                self.shake_at = Some(Instant::now());
                if e.contains("libpam") || e.contains("pam_start") {
                    self.message = Some(self.strings.unavailable.into());
                }
                self.mark(BAND_LOGIN);
            }
        }
    }

    /// The fade-out finished: hand the session back.
    fn unlock(&mut self) {
        if let Some(lock) = self.lock.take() {
            lock.unlock();
        }
        let _ = self.conn.roundtrip();
        self.exit = true;
    }

    fn power(&mut self, hit: Hit) {
        let args: &[&str] = match hit {
            Hit::Sleep => &["suspend"],
            Hit::Restart => &["reboot"],
            Hit::ShutDown => &["poweroff"],
            Hit::Submit => return,
        };
        let _ = std::process::Command::new("systemctl").args(args).spawn().map(|mut c| {
            std::thread::spawn(move || c.wait());
        });
    }

    fn layout_for(&mut self, surface: &wl_surface::WlSurface) -> Option<(Layout, bool)> {
        let (w, h, primary) = self
            .screens
            .iter()
            .find(|s| s.surface.wl_surface() == surface)
            .map(|s| (s.size.0 as f32, s.size.1 as f32, s.primary))?;
        let text = self.text();
        Some((Layout::new(w, h, text), primary))
    }

    fn update_primary(&mut self) {
        let names: Vec<Option<String>> =
            self.screens.iter().map(|s| self.output_state.info(&s.output).and_then(|i| i.name)).collect();
        let want = self.primary_output.clone();
        let idx = names.iter().position(|n| n.is_some() && *n == want).unwrap_or(0);
        for (i, s) in self.screens.iter_mut().enumerate() {
            let p = i == idx;
            if s.primary != p {
                s.primary = p;
                s.dirty = BAND_ALL;
            }
        }
    }

    fn add_screen(&mut self, qh: &QueueHandle<App>, output: wl_output::WlOutput) {
        let Some(lock) = self.lock.as_ref() else { return };
        if !lock.is_locked() || self.screens.iter().any(|s| s.output == output) {
            return;
        }
        let surface = self.compositor.create_surface(qh);
        let scale = self.output_state.info(&output).map(|i| i.scale_factor).unwrap_or(1).max(1);
        surface.set_buffer_scale(scale);
        let surface = lock.create_lock_surface(surface, &output, qh);
        self.screens.push(Screen {
            output,
            surface,
            size: (0, 0),
            scale,
            canvas: None,
            pool: None,
            bufs: Vec::new(),
            pending: Vec::new(),
            dirty: BAND_ALL,
            frame_pending: false,
            commits: 0,
            configured: false,
            primary: false,
            avatar: None,
        });
        self.update_primary();
    }
}

/// Copy the changed rows into a free buffer and commit it.
fn present(screen: &mut Screen, qh: &QueueHandle<App>) {
    let Some(canvas) = screen.canvas.as_ref() else { return };
    let Some(pool) = screen.pool.as_mut() else { return };
    let (pw, ph) = (canvas.width() as i32, canvas.height() as i32);
    let damage = std::mem::take(&mut screen.pending);
    for b in &mut screen.bufs {
        b.stale.extend(damage.iter().copied());
    }
    // a buffer the compositor is not reading from, else a new one (max 3)
    let mut idx = screen.bufs.iter().position(|b| b.buffer.canvas(pool).is_some());
    if idx.is_none() && screen.bufs.len() < 3 {
        if let Ok((buffer, _)) = pool.create_buffer(pw, ph, pw * 4, wl_shm::Format::Argb8888) {
            screen.bufs.push(Buf { buffer, stale: vec![(0, 0, pw, ph)] });
            idx = Some(screen.bufs.len() - 1);
        }
    }
    let Some(idx) = idx else {
        // all buffers busy: try again on the next frame
        screen.pending = damage;
        return;
    };
    let buf = &mut screen.bufs[idx];
    let Some(dst) = buf.buffer.canvas(pool) else {
        screen.pending = damage;
        return;
    };
    let src = canvas.data();
    let stride = pw as usize * 4;
    for (_, y, _, h) in buf.stale.drain(..) {
        let (a, b) = (y.max(0) as usize * stride, ((y + h).min(ph)) as usize * stride);
        if a >= b {
            continue;
        }
        // premultiplied RGBA → ARGB8888 (BGRA in memory)
        for (d, s) in dst[a..b].chunks_exact_mut(4).zip(src[a..b].chunks_exact(4)) {
            d[0] = s[2];
            d[1] = s[1];
            d[2] = s[0];
            d[3] = s[3];
        }
    }
    let surface = screen.surface.wl_surface();
    if buf.buffer.attach_to(surface).is_err() {
        screen.pending = damage;
        return;
    }
    for (x, y, w, h) in &damage {
        surface.damage_buffer(*x, *y, *w, *h);
    }
    surface.set_buffer_scale(screen.scale);
    surface.frame(qh, surface.clone());
    surface.commit();
    screen.frame_pending = true;
    screen.commits += 1;
    static FIRST: std::sync::Once = std::sync::Once::new();
    FIRST.call_once(|| {
        let ms = START.get().map(|t| t.elapsed().as_millis()).unwrap_or(0);
        eprintln!("orchard-lock: first frame after {ms} ms");
    });
}

// ---- Wayland handlers -----------------------------------------------------------

impl SessionLockHandler for App {
    fn locked(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: SessionLock) {
        trace("locked");
        for output in self.output_state.outputs().collect::<Vec<_>>() {
            self.add_screen(qh, output);
        }
    }

    fn finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: SessionLock) {
        // another locker holds the session (or the compositor refused)
        eprintln!("orchard-lock: the session could not be locked (already locked?)");
        self.exit = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: SessionLockSurface,
        configure: SessionLockSurfaceConfigure,
        _: u32,
    ) {
        if self.shown_at.is_none() {
            trace("first configure");
            self.shown_at = Some(Instant::now());
        }
        if let Some(s) = self.screens.iter_mut().find(|s| s.surface.wl_surface() == surface.wl_surface()) {
            s.size = configure.new_size;
            s.configured = true;
            s.dirty = BAND_ALL;
        }
        self.flush();
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &wl_surface::WlSurface, factor: i32) {
        if let Some(s) = self.screens.iter_mut().find(|s| s.surface.wl_surface() == surface) {
            if s.scale != factor.max(1) {
                s.scale = factor.max(1);
                s.dirty = BAND_ALL;
            }
        }
        self.flush();
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &wl_surface::WlSurface, _: u32) {
        self.frame_done(surface);
    }
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.add_screen(qh, output);
    }
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        let scale = self.output_state.info(&output).map(|i| i.scale_factor).unwrap_or(1).max(1);
        if let Some(s) = self.screens.iter_mut().find(|s| s.output == output) {
            if s.scale != scale {
                s.scale = scale;
                s.dirty = BAND_ALL;
            }
        }
        self.update_primary();
        self.flush();
    }
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.screens.retain(|s| s.output != output);
        self.update_primary();
        self.flush();
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            let repeat: smithay_client_toolkit::seat::keyboard::repeat::RepeatCallback<App> = Box::new(|app: &mut App, _, ev| {
                // holding a key types it again (and Backspace deletes); Return never repeats
                if !matches!(ev.keysym, Keysym::Return | Keysym::KP_Enter | Keysym::Escape) {
                    app.key(&ev);
                }
            });
            self.keyboard = self.seat_state.get_keyboard_with_repeat(qh, &seat, None, self.loop_handle.clone(), repeat).ok();
        }
        if cap == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard {
            if let Some(k) = self.keyboard.take() {
                k.release();
            }
        }
        if cap == Capability::Pointer {
            if let Some(p) = self.pointer.take() {
                p.release();
            }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {}
    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.key(&event);
    }
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        modifiers: Modifiers,
        _: u32,
    ) {
        let caps_changed = modifiers.caps_lock != self.mods.caps_lock;
        self.mods = modifiers;
        if caps_changed {
            self.mark(BAND_LOGIN);
        }
    }
}

impl PointerHandler for App {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for ev in events {
            let (x, y) = (ev.position.0 as f32, ev.position.1 as f32);
            match ev.kind {
                PointerEventKind::Enter { .. } => self.pointer_surface = Some(ev.surface.clone()),
                PointerEventKind::Leave { .. } => {
                    self.pointer_surface = None;
                    if self.hover.take().is_some() || self.pressed.take().is_some() {
                        self.mark(BAND_LOGIN | BAND_POWER);
                    }
                    continue;
                }
                _ => {}
            }
            let can_submit = self.chars > 0 && matches!(self.phase, Phase::Idle);
            let hit = self.layout_for(&ev.surface).and_then(|(l, primary)| l.hit(x, y, primary, can_submit));
            match ev.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    if hit != self.hover {
                        self.hover = hit;
                        self.mark(BAND_LOGIN | BAND_POWER);
                    }
                }
                PointerEventKind::Press { button, .. } if button == BTN_LEFT => {
                    self.pressed = hit;
                    self.mark(BAND_LOGIN | BAND_POWER);
                }
                PointerEventKind::Release { button, .. } if button == BTN_LEFT => {
                    let pressed = self.pressed.take();
                    if pressed.is_some() && pressed == hit {
                        match hit {
                            Some(Hit::Submit) => self.submit(),
                            Some(h) if matches!(self.phase, Phase::Idle) => self.power(h),
                            _ => {}
                        }
                    }
                    self.mark(BAND_LOGIN | BAND_POWER);
                }
                _ => {}
            }
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(App);
delegate_output!(App);
delegate_shm!(App);
delegate_seat!(App);
delegate_keyboard!(App);
delegate_pointer!(App);
delegate_session_lock!(App);
delegate_registry!(App);
