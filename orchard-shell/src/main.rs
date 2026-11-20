//! orchard-shell: the menu bar and Dock for the Orchard compositor.
//!
//! Pure Rust client: layer-shell surfaces drawn on the CPU with tiny-skia.
//! Talks to the compositor over Orchard's JSON IPC for the window list,
//! activation and frosted-glass backdrops.

mod anim;
mod apps;
mod ddc;
mod desktop;
mod devbuild;
mod dialog;
mod dock;
mod draw;
mod globalmenu;
mod gtkmenu;
mod ipc;
mod launcher;
mod launchpad;
mod lowbattery;
mod menu;
mod hud;
mod menubar;
mod notify;
mod osd;
mod panel;
mod panel_app;
mod polkit;
mod screenshot;
mod status;
mod switcher;
mod text;
mod theme;
mod tray;

use orchard_config::{DockAutohide, DockPosition};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use serde_json::json;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat,
    delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            channel::{self, Sender},
            generic::Generic,
            timer::{TimeoutAction, Timer},
            EventLoop, Interest, LoopHandle, Mode, PostAction,
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
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use tiny_skia::Pixmap;

use crate::{
    apps::Apps,
    dock::{Dock, ItemKind},
    ipc::{Event, Ipc, WindowInfo},
    menu::{Action, Menu, MenuItem},
    menubar::{BarItem, MenuBar},
    text::Fonts,
    theme::Theme,
};

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Which {
    Bar,
    Dock,
    Menu,
    Switcher,
    Launcher,
    Osd,
    Notify,
    Panel,
    Launchpad,
    Screenshot,
    Dialog,
    Polkit,
    Desktop,
}

struct Surf {
    layer: LayerSurface,
    width: u32,
    height: u32,
    scale: i32,
    configured: bool,
    frame_pending: bool,
    /// When the outstanding frame callback was requested (stale-callback fallback).
    frame_since: Option<Instant>,
    /// Content changed; drawn by `flush_draws` / the next frame callback.
    dirty: bool,
    /// Per-surface SHM pool: dropped immediately when a transient overlay closes.
    pool: Option<SlotPool>,
    /// Reusable CPU render buffer so animations don't allocate/free 30-60 MB per frame.
    pixmap: Option<Pixmap>,
}

impl Surf {
    fn new(layer: LayerSurface) -> Surf {
        Surf {
            layer,
            width: 0,
            height: 0,
            scale: 1,
            configured: false,
            frame_pending: false,
            frame_since: None,
            dirty: false,
            pool: None,
            pixmap: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum MenuOwner {
    Bar(BarItem),
    Dock(usize),
    Desktop,
}

struct App {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,

    output: Option<wl_output::WlOutput>,
    output_name: String,
    output_size: (i32, i32),
    /// settings: output that should carry the menu bar and Dock
    primary: Option<String>,

    bar: Option<Surf>,
    dock_surf: Option<Surf>,
    menu_surf: Option<Surf>,
    switcher_surf: Option<Surf>,
    launcher_surf: Option<Surf>,
    switcher: Option<switcher::Switcher>,
    launcher: Option<launcher::Launcher>,
    /// Spotlight's file index, usage memory and background work
    spot: launcher::Spot,
    /// keyboard modifiers right now
    mods: Modifiers,
    qh: QueueHandle<App>,

    menubar: MenuBar,
    dock: Dock,
    menu: Option<Menu>,
    menu_owner: Option<MenuOwner>,
    pointer_pos: (f64, f64),
    pointer_on: Option<Which>,

    windows: Vec<WindowInfo>,
    apps: Apps,
    fonts: Fonts,
    theme: Theme,
    perf: orchard_config::PerformanceSettings,
    ipc: Option<Ipc>,
    last_step: Instant,
    last_backdrop: Option<(f32, f32, f32, f32)>,
    last_tiles: String,
    german: bool,
    exit: bool,

    loop_handle: LoopHandle<'static, App>,
    osd: Option<osd::Osd>,
    osd_surf: Option<Surf>,
    osd_tx: Sender<osd::Level>,
    notes: notify::Center,
    notes_surf: Option<Surf>,
    last_notes_backdrop: Option<Vec<(f32, f32, f32, f32)>>,
    /// session bus connection that owns org.freedesktop.Notifications
    dbus: Option<zbus::blocking::Connection>,

    /// Notification Center / Control Center
    panel: Option<panel::Panel>,
    panel_surf: Option<Surf>,
    launchpad_surf: Option<Surf>,
    launchpad: Option<launchpad::Launchpad>,
    launchpad_step: Instant,
    /// last backdrop opacity sent for Launchpad
    launchpad_backdrop: f32,
    /// macOS-style interactive screenshot tool (Print / ⌘⇧5)
    screenshot: Option<screenshot::ScreenshotTool>,
    screenshot_surf: Option<Surf>,
    screenshot_prefs: screenshot::Prefs,
    last_shot_backdrop: Vec<(f32, f32, f32, f32, f32)>,
    control_tx: Sender<panel::ControlState>,
    control_cache: panel::ControlState,
    last_slider_cmd: Instant,
    vol_tx: Sender<Option<(f32, bool)>>,
    vol_pending: Arc<AtomicBool>,
    status_tick: u32,
    low_battery: lowbattery::Watch,
    /// Game Mode: name of the full-screen game
    game: Option<String>,

    /// global app menus
    gmenu: Option<globalmenu::GlobalMenu>,
    /// menu address of the focused window and the ids of its top-level menus
    menu_addr: Option<globalmenu::MenuSource>,
    app_menu_ids: Vec<i32>,
    gmenu_tx: Sender<(Option<globalmenu::MenuSource>, Vec<(i32, String)>)>,

    /// power / session confirmation dialog
    dialog: Option<dialog::ConfirmDialog>,
    dialog_surf: Option<Surf>,
    /// Polkit authentication agent & macOS-style auth sheet
    polkit: Option<polkit::PolkitDialog>,
    polkit_surf: Option<Surf>,
    polkit_queue: std::collections::VecDeque<polkit::Request>,
    polkit_tx: Sender<polkit::Msg>,
    _polkit_agent: Option<polkit::AgentHandle>,
    /// system tray (StatusNotifierWatcher)
    tray: Option<tray::Tray>,
    /// desktop icons (Layer::Bottom)
    desktop: desktop::Desktop,
    desktop_surf: Option<Surf>,
}

fn main() {
    if let Ok(filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        tracing_subscriber::fmt().with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr())).with_env_filter(filter).init();
    } else {
        tracing_subscriber::fmt().with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr())).with_env_filter("info").init();
    }

    let conn = Connection::connect_to_env().expect("orchard-shell: no Wayland compositor");
    let (globals, event_queue) = registry_queue_init(&conn).expect("registry");
    let qh = event_queue.handle();
    let mut event_loop: EventLoop<'static, App> = EventLoop::try_new().expect("event loop");
    let loop_handle = event_loop.handle();
    WaylandSource::new(conn.clone(), event_queue)
        .insert(loop_handle.clone())
        .expect("wayland source");

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("layer shell");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm");

    let settings = orchard_config::Settings::load();
    anim::set_reduced(settings.appearance.reduce_motion);
    let apps = Apps::load(settings.appearance.icon_theme.clone());
    tracing::info!("{} desktop entries", apps.entries.len());
    let pinned = dock::load_pinned(&apps);
    let theme = Theme::from_settings(&settings);

    let ipc = match Ipc::connect() {
        Ok(ipc) => Some(ipc),
        Err(e) => {
            tracing::warn!("no Orchard IPC ({e}); running without window list");
            None
        }
    };

    // results of media keys (run on helper threads) and incoming notifications
    let (osd_tx, osd_rx) = channel::channel::<osd::Level>();
    loop_handle
        .insert_source(osd_rx, |ev, _, app: &mut App| {
            if let channel::Event::Msg(level) = ev {
                app.show_osd(level);
            }
        })
        .expect("osd channel");
    let (note_tx, note_rx) = channel::channel::<notify::Msg>();
    loop_handle
        .insert_source(note_rx, |ev, _, app: &mut App| {
            if let channel::Event::Msg(msg) = ev {
                app.on_notification(msg);
            }
        })
        .expect("notification channel");
    let dbus = notify::start(note_tx);
    let (polkit_tx, polkit_rx) = channel::channel::<polkit::Msg>();
    loop_handle
        .insert_source(polkit_rx, |ev, _, app: &mut App| {
            if let channel::Event::Msg(msg) = ev {
                app.on_polkit_msg(msg);
            }
        })
        .expect("polkit channel");
    let polkit_agent = polkit::start(polkit_tx.clone());
    let gmenu = globalmenu::GlobalMenu::start();
    let (gmenu_tx, gmenu_rx) = channel::channel::<(Option<globalmenu::MenuSource>, Vec<(i32, String)>)>();
    loop_handle
        .insert_source(gmenu_rx, |ev, _, app: &mut App| {
            if let channel::Event::Msg((addr, tops)) = ev {
                app.on_app_menu(addr, tops);
            }
        })
        .expect("global menu channel");
    let (control_tx, control_rx) = channel::channel::<panel::ControlState>();
    loop_handle
        .insert_source(control_rx, |ev, _, app: &mut App| {
            if let channel::Event::Msg(st) = ev {
                app.on_control_state(st);
            }
        })
        .expect("control channel");
    let (spot_tx, spot_rx) = channel::channel::<launcher::Msg>();
    loop_handle
        .insert_source(spot_rx, |ev, _, app: &mut App| {
            if let channel::Event::Msg(msg) = ev {
                app.on_spot_msg(msg);
            }
        })
        .expect("spotlight channel");
    let (vol_tx, vol_rx) = channel::channel::<Option<(f32, bool)>>();
    loop_handle
        .insert_source(vol_rx, |ev, _, app: &mut App| {
            if let channel::Event::Msg(vol) = ev {
                if app.menubar.volume != vol {
                    app.menubar.volume = vol;
                    app.redraw(Which::Bar);
                }
            }
        })
        .expect("volume channel");
    let (tray_tx, tray_rx) = channel::channel::<Vec<tray::TrayItem>>();
    loop_handle
        .insert_source(tray_rx, |ev, _, app: &mut App| {
            if let channel::Event::Msg(items) = ev {
                app.menubar.tray_items = items;
                app.redraw(Which::Bar);
            }
        })
        .expect("tray channel");
    let tray = tray::Tray::start(tray_tx);
    let desktop = desktop::Desktop::new(settings.windows.show_desktop_icons);

    let mut app = App {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        layer_shell,
        shm,
        pointer: None,
        keyboard: None,
        output: None,
        output_name: String::new(),
        output_size: (0, 0),
        primary: settings.displays.primary.clone(),
        bar: None,
        dock_surf: None,
        menu_surf: None,
        switcher_surf: None,
        launcher_surf: None,
        switcher: None,
        launcher: None,
        spot: launcher::Spot::new(spot_tx),
        mods: Modifiers::default(),
        qh: qh.clone(),
        menubar: MenuBar::new(),
        dock: Dock::new(pinned),
        menu: None,
        menu_owner: None,
        pointer_pos: (0.0, 0.0),
        pointer_on: None,
        windows: Vec::new(),
        apps,
        fonts: Fonts::load(),
        theme,
        perf: settings.performance.clone(),
        ipc,
        last_step: Instant::now(),
        last_backdrop: None,
        last_tiles: String::new(),
        german: std::env::var("LANG").map(|l| l.starts_with("de")).unwrap_or(false),
        exit: false,
        loop_handle: loop_handle.clone(),
        osd: None,
        osd_surf: None,
        osd_tx,
        notes: notify::Center::default(),
        notes_surf: None,
        last_notes_backdrop: None,
        dbus,
        panel: None,
        panel_surf: None,
        launchpad_surf: None,
        launchpad: None,
        launchpad_step: Instant::now(),
        launchpad_backdrop: -1.0,
        screenshot: None,
        screenshot_surf: None,
        screenshot_prefs: screenshot::Prefs::default(),
        last_shot_backdrop: Vec::new(),
        control_tx,
        control_cache: Default::default(),
        last_slider_cmd: Instant::now(),
        vol_tx,
        vol_pending: Arc::new(AtomicBool::new(false)),
        status_tick: 0,
        low_battery: lowbattery::Watch::default(),
        game: None,
        gmenu,
        menu_addr: None,
        app_menu_ids: Vec::new(),
        gmenu_tx,
        dialog: None,
        dialog_surf: None,
        polkit: None,
        polkit_surf: None,
        polkit_queue: std::collections::VecDeque::new(),
        polkit_tx,
        _polkit_agent: polkit_agent,
        tray,
        desktop,
        desktop_surf: None,
    };
    app.dock.cfg = settings.dock.clone();
    app.menubar.cfg = settings.menu_bar.clone();
    app.menubar.refresh_status();
    app.refresh_volume();
    app.notes.do_not_disturb = panel_app::load_dnd();
    app.menubar.dnd = app.notes.do_not_disturb;
    let apps_ref = &app.apps;
    app.dock.rebuild(apps_ref, &[]);

    // IPC events
    if let Some(ipc) = app.ipc.as_mut() {
        ipc.request("subscribe", json!({}));
        let fd = ipc.stream().try_clone().expect("clone ipc stream");
        loop_handle
            .insert_source(Generic::new(fd, Interest::READ, Mode::Level), |_, _, app: &mut App| {
                app.poll_ipc();
                Ok(PostAction::Continue)
            })
            .expect("ipc source");
    }

    // Event-driven volume monitor via `pactl subscribe` (replaces 2s wpctl polling)
    app.start_volume_watcher();

    // status refresh: 1s only when showing seconds or during an active dev build,
    // otherwise minute/10s-aligned (or 30s in Power Saver / on battery) to keep the shell asleep when idle.
    let mut last_slow_poll = Instant::now();
    loop_handle
        .insert_source(Timer::from_duration(Duration::from_secs(1)), move |_, _, app: &mut App| {
            let slow_secs = app.slow_poll_interval();
            let slow_due = last_slow_poll.elapsed() >= Duration::from_secs(slow_secs);
            let status_changed = if slow_due {
                last_slow_poll = Instant::now();
                app.refresh_apps();
                let changed = app.menubar.refresh_status_with(app.perf.battery_saver);
                app.check_low_battery();
                changed
            } else {
                app.menubar.refresh_clock()
            };
            if status_changed {
                app.redraw(Which::Bar);
            }
            app.status_tick = app.status_tick.wrapping_add(1);
            // X11 apps register their menu some time after mapping
            if app.app_menu_ids.is_empty() {
                app.refresh_app_menu();
            }
            // Rebuild item: pending sources / build state
            let mut dev_running = false;
            if let Some(dev) = app.menubar.dev.as_mut() {
                dev_running = dev.build == devbuild::Build::Running;
                if dev_running {
                    app.menubar.dev_spin = app.menubar.dev_spin.wrapping_add(1);
                }
                if (dev_running || slow_due) && (dev.refresh() | dev_running) {
                    app.redraw(Which::Bar);
                }
            }
            let mut dialog_active = false;
            if let Some(d) = app.dialog.as_ref() {
                if d.expired() {
                    app.confirm_dialog();
                } else {
                    dialog_active = true;
                    app.redraw(Which::Dialog);
                }
            }
            if slow_due && app.desktop.check_refresh() {
                app.redraw(Which::Desktop);
            }
            let next = if app.menubar.cfg.show_seconds || dev_running || dialog_active {
                Duration::from_secs(1)
            } else {
                // Wake on the next minute boundary or slow-poll boundary, whichever is sooner
                let now_secs = chrono::Local::now().timestamp().rem_euclid(60) as u64;
                let to_min = (60 - now_secs).max(1);
                Duration::from_secs(to_min.min(slow_secs))
            };
            TimeoutAction::ToDuration(next)
        })
        .expect("timer");

    // wait for outputs, then create surfaces
    // A dispatch error means the compositor connection is gone (it quit or crashed):
    // exit quietly, the compositor's supervisor starts a fresh shell if it is still up.
    while app.output.is_none() && !app.exit {
        if let Err(e) = event_loop.dispatch(Duration::from_millis(50), &mut app) {
            tracing::info!("compositor connection closed: {e}");
            return;
        }
        app.retarget_output();
        app.flush_draws();
    }

    while !app.exit {
        let timeout = app.next_dispatch_timeout();
        if let Err(e) = event_loop.dispatch(timeout, &mut app) {
            tracing::info!("compositor connection closed: {e}");
            return;
        }
        app.flush_draws();
    }
    let _ = qh;
}

impl App {
    /// Listen for PipeWire/PulseAudio sink changes via a single persistent `pactl subscribe`
    /// process instead of spawning `wpctl` every 2 seconds.
    fn start_volume_watcher(&self) {
        let tx = self.vol_tx.clone();
        let _ = std::thread::Builder::new().name("volume-watch".into()).spawn(move || {
            use std::io::{BufRead, BufReader};
            use std::process::{Command, Stdio};
            loop {
                let child = Command::new("pactl")
                    .arg("subscribe")
                    .env("LC_ALL", "C")
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn();
                let Ok(mut child) = child else {
                    // Fallback if pactl is unavailable: gentle 10s poll
                    std::thread::sleep(Duration::from_secs(10));
                    let _ = tx.send(status::volume());
                    continue;
                };
                if let Some(stdout) = child.stdout.take() {
                    let reader = BufReader::new(stdout);
                    let mut last_query = Instant::now() - Duration::from_secs(1);
                    for line in reader.lines().map_while(Result::ok) {
                        if (line.contains("sink") || line.contains("server"))
                            && last_query.elapsed() >= Duration::from_millis(80)
                        {
                            last_query = Instant::now();
                            let _ = tx.send(status::volume());
                        }
                    }
                }
                let _ = child.wait();
                std::thread::sleep(Duration::from_secs(5));
            }
        });
    }

    /// Query current default sink volume on a helper thread (at most one in flight).
    fn refresh_volume(&self) {
        if self.vol_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let tx = self.vol_tx.clone();
        let pending = self.vol_pending.clone();
        if std::thread::Builder::new()
            .name("volume-poll".into())
            .spawn(move || {
                let vol = status::volume();
                pending.store(false, Ordering::Release);
                let _ = tx.send(vol);
            })
            .is_err()
        {
            self.vol_pending.store(false, Ordering::Release);
        }
    }

    /// The output the menu bar and Dock belong on: the configured primary
    /// display if it is connected, else the first one with a known size.
    fn desired_output(&self, skip: Option<&wl_output::WlOutput>) -> Option<wl_output::WlOutput> {
        let ready: Vec<_> = self
            .output_state
            .outputs()
            .filter(|o| Some(o) != skip)
            .filter(|o| self.output_state.info(o).and_then(|i| i.logical_size).is_some())
            .collect();
        self.primary
            .as_ref()
            .and_then(|want| {
                ready
                    .iter()
                    .find(|o| self.output_state.info(o).and_then(|i| i.name).as_deref() == Some(want.as_str()))
                    .cloned()
            })
            .or_else(|| ready.first().cloned())
    }

    /// Move the menu bar and Dock to `desired_output()` if they are elsewhere.
    fn retarget_output(&mut self) {
        self.retarget_output_skipping(None);
    }

    fn retarget_output_skipping(&mut self, skip: Option<&wl_output::WlOutput>) {
        let Some(want) = self.desired_output(skip) else { return };
        if self.output.as_ref() == Some(&want) && self.bar.is_some() {
            return;
        }
        if self.output.is_some() {
            tracing::info!("moving menu bar and Dock to another output");
        }
        self.drop_surfaces();
        let qh = self.qh.clone();
        self.adopt_output(&qh, want);
    }

    /// The Dock's layer surface, on the edge chosen in Settings.
    fn create_dock_surface(&mut self, qh: &QueueHandle<Self>, output: &wl_output::WlOutput) {
        let surface = self.compositor.create_surface(qh);
        let dock = self
            .layer_shell
            .create_layer_surface(qh, surface, Layer::Top, Some("orchard-dock"), Some(output));
        let depth = self.dock.surface_height() as u32;
        match self.dock.edge() {
            DockPosition::Bottom => {
                dock.set_anchor(Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
                dock.set_size(0, depth);
            }
            DockPosition::Left => {
                dock.set_anchor(Anchor::LEFT | Anchor::TOP | Anchor::BOTTOM);
                dock.set_size(depth, 0);
            }
            DockPosition::Right => {
                dock.set_anchor(Anchor::RIGHT | Anchor::TOP | Anchor::BOTTOM);
                dock.set_size(depth, 0);
            }
        }
        dock.set_exclusive_zone(self.dock.exclusive_zone() as i32);
        dock.set_keyboard_interactivity(KeyboardInteractivity::None);
        dock.commit();
        self.dock_surf = Some(Surf::new(dock));
        self.last_backdrop = None;
        self.last_tiles.clear();
    }

    fn create_desktop_surface(&mut self, qh: &QueueHandle<Self>, output: &wl_output::WlOutput) {
        let surface = self.compositor.create_surface(qh);
        let layer = self.layer_shell.create_layer_surface(
            qh,
            surface,
            Layer::Bottom,
            Some("orchard-desktop"),
            Some(output),
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.commit();
        self.desktop_surf = Some(Surf::new(layer));
    }

    // ---- Power / Session confirmation dialogs ---------------------------

    pub(crate) fn open_dialog(&mut self, kind: dialog::DialogKind) {
        self.close_menu();
        self.close_panel();
        if self.launcher.is_some() {
            self.close_launcher_now();
        }
        self.dialog = Some(dialog::ConfirmDialog::new(kind));
        if self.dialog_surf.is_none() {
            let surface = self.compositor.create_surface(&self.qh);
            let layer = self.layer_shell.create_layer_surface(
                &self.qh,
                surface,
                Layer::Overlay,
                Some("orchard-dialog"),
                self.output.as_ref(),
            );
            layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
            layer.set_exclusive_zone(-1);
            layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
            layer.commit();
            self.dialog_surf = Some(Surf::new(layer));
        } else {
            self.redraw(Which::Dialog);
        }
    }

    fn close_dialog(&mut self) {
        if self.dialog.take().is_none() && self.dialog_surf.is_none() {
            return;
        }
        self.dialog_surf = None;
        self.set_backdrop("orchard-dialog", None);
    }

    fn confirm_dialog(&mut self) {
        let Some(d) = self.dialog.take() else { return };
        self.dialog_surf = None;
        self.set_backdrop("orchard-dialog", None);
        match d.kind {
            dialog::DialogKind::ShutDown => self.spawn("systemctl poweroff"),
            dialog::DialogKind::Restart => self.spawn("systemctl reboot"),
            dialog::DialogKind::Sleep => self.spawn("systemctl suspend"),
            dialog::DialogKind::LogOut => {
                if let Some(ipc) = self.ipc.as_mut() {
                    ipc.request("action", json!({ "name": "quit" }));
                }
            }
        }
    }

    fn dialog_pointer(&mut self, kind: &PointerEventKind, x: f64, y: f64) {
        let (x, y) = (x as f32, y as f32);
        match kind {
            PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                if let Some(d) = self.dialog.as_mut() {
                    let h = d.hit(x, y);
                    if h != d.hovered {
                        d.hovered = h;
                        self.redraw(Which::Dialog);
                    }
                }
            }
            PointerEventKind::Leave { .. } => {
                if let Some(d) = self.dialog.as_mut() {
                    if d.hovered.take().is_some() {
                        self.redraw(Which::Dialog);
                    }
                }
            }
            PointerEventKind::Press { button: BTN_LEFT, .. } => {
                let (hit, inside) = match self.dialog.as_ref() {
                    Some(d) => (d.hit(x, y), d.in_dialog(x, y)),
                    None => (None, false),
                };
                match hit {
                    Some(dialog::DialogButton::Cancel) => self.close_dialog(),
                    Some(dialog::DialogButton::Confirm) => self.confirm_dialog(),
                    None if !inside => self.close_dialog(),
                    None => {}
                }
            }
            _ => {}
        }
    }

    fn dialog_key(&mut self, event: &KeyEvent) {
        match event.keysym {
            Keysym::Escape => self.close_dialog(),
            Keysym::Return | Keysym::KP_Enter => self.confirm_dialog(),
            _ => {}
        }
    }

    // ---- Polkit authentication sheet ------------------------------------

    fn on_polkit_msg(&mut self, msg: polkit::Msg) {
        match msg {
            polkit::Msg::Begin(req) => {
                if self.polkit.is_some() {
                    self.polkit_queue.push_back(*req);
                } else {
                    self.open_polkit(*req);
                }
            }
            polkit::Msg::Cancel(cookie) => {
                if self.polkit.as_ref().is_some_and(|p| p.cookie() == cookie) {
                    self.close_polkit_and_advance(true);
                } else {
                    self.polkit_queue.retain(|r| r.cookie != cookie);
                }
            }
            polkit::Msg::HelperPrompt { cookie, attempt, echo, prompt } => {
                if let Some(p) = self.polkit.as_mut().filter(|p| p.cookie() == cookie) {
                    p.on_prompt(attempt, echo, prompt);
                    self.redraw(Which::Polkit);
                }
            }
            polkit::Msg::HelperInfo { cookie, attempt, text } => {
                if let Some(p) = self.polkit.as_mut().filter(|p| p.cookie() == cookie) {
                    p.on_info(attempt, text);
                    self.redraw(Which::Polkit);
                }
            }
            polkit::Msg::HelperError { cookie, attempt, text } => {
                if let Some(p) = self.polkit.as_mut().filter(|p| p.cookie() == cookie) {
                    p.on_error(attempt, text);
                    self.redraw(Which::Polkit);
                }
            }
            polkit::Msg::HelperFinished { cookie, attempt, success } => {
                let done = if let Some(p) = self.polkit.as_mut().filter(|p| p.cookie() == cookie) {
                    let ok = p.on_finished(attempt, success, self.german);
                    if !ok {
                        self.redraw(Which::Polkit);
                    }
                    ok
                } else {
                    false
                };
                if done {
                    self.close_polkit_and_advance(false);
                }
            }
        }
    }

    fn open_polkit(&mut self, req: polkit::Request) {
        self.close_menu();
        self.close_panel();
        if self.launcher.is_some() {
            self.close_launcher_now();
        }
        self.polkit = Some(polkit::PolkitDialog::new(req, self.polkit_tx.clone()));
        self.ensure_polkit_surface();
    }

    fn ensure_polkit_surface(&mut self) {
        if self.polkit_surf.is_none() {
            let surface = self.compositor.create_surface(&self.qh);
            let layer = self.layer_shell.create_layer_surface(
                &self.qh,
                surface,
                Layer::Overlay,
                Some("orchard-polkit"),
                self.output.as_ref(),
            );
            layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
            layer.set_exclusive_zone(-1);
            layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
            layer.commit();
            self.polkit_surf = Some(Surf::new(layer));
        } else {
            self.redraw(Which::Polkit);
        }
    }

    fn cancel_polkit(&mut self) {
        self.close_polkit_and_advance(true);
    }

    fn close_polkit_and_advance(&mut self, cancelled: bool) {
        if let Some(mut p) = self.polkit.take() {
            if cancelled {
                p.cancel();
            }
        }
        if let Some(next) = self.polkit_queue.pop_front() {
            self.polkit = Some(polkit::PolkitDialog::new(next, self.polkit_tx.clone()));
            self.ensure_polkit_surface();
        } else {
            self.polkit_surf = None;
            self.set_backdrop("orchard-polkit", None);
        }
    }

    fn polkit_pointer(&mut self, kind: &PointerEventKind, x: f64, y: f64) {
        let (x, y) = (x as f32, y as f32);
        match kind {
            PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                if let Some(p) = self.polkit.as_mut() {
                    let h = p.hit(x, y);
                    if h != p.hovered {
                        p.hovered = h;
                        self.redraw(Which::Polkit);
                    }
                }
            }
            PointerEventKind::Leave { .. } => {
                if let Some(p) = self.polkit.as_mut() {
                    if p.hovered.take().is_some() {
                        self.redraw(Which::Polkit);
                    }
                }
            }
            PointerEventKind::Press { button: BTN_LEFT, .. } => {
                let hit = self.polkit.as_ref().and_then(|p| p.hit(x, y));
                match hit {
                    Some(polkit::Hit::Cancel) => self.cancel_polkit(),
                    Some(polkit::Hit::Confirm) => {
                        if let Some(p) = self.polkit.as_mut() {
                            p.submit();
                            self.redraw(Which::Polkit);
                        }
                    }
                    Some(polkit::Hit::UserField) => {
                        if let Some(p) = self.polkit.as_mut() {
                            p.cycle_identity();
                            self.redraw(Which::Polkit);
                        }
                    }
                    Some(polkit::Hit::DetailsToggle) => {
                        if let Some(p) = self.polkit.as_mut() {
                            p.show_details = !p.show_details;
                            self.redraw(Which::Polkit);
                        }
                    }
                    Some(polkit::Hit::PasswordField) | None => {}
                }
            }
            _ => {}
        }
    }

    fn polkit_key(&mut self, event: &KeyEvent) {
        match event.keysym {
            Keysym::Escape => self.cancel_polkit(),
            Keysym::Return | Keysym::KP_Enter => {
                if let Some(p) = self.polkit.as_mut() {
                    p.submit();
                    self.redraw(Which::Polkit);
                }
            }
            Keysym::BackSpace => {
                if let Some(p) = self.polkit.as_mut() {
                    p.backspace(self.mods.ctrl);
                    self.redraw(Which::Polkit);
                }
            }
            Keysym::Tab => {
                if let Some(p) = self.polkit.as_mut() {
                    p.cycle_identity();
                    self.redraw(Which::Polkit);
                }
            }
            _ => {
                if self.mods.ctrl && matches!(event.keysym, Keysym::u | Keysym::U) {
                    if let Some(p) = self.polkit.as_mut() {
                        p.backspace(true);
                        self.redraw(Which::Polkit);
                    }
                    return;
                }
                if self.mods.ctrl || self.mods.alt || self.mods.logo {
                    return;
                }
                if let Some(text) = event.utf8.as_deref().filter(|t| !t.is_empty() && !t.chars().any(|c| c.is_control())) {
                    if let Some(p) = self.polkit.as_mut() {
                        p.push_str(text);
                        self.redraw(Which::Polkit);
                    }
                }
            }
        }
    }

    // ---- Desktop icons --------------------------------------------------

    fn open_desktop_path(&mut self, path: &std::path::Path) {
        if let Some(exec) = self
            .desktop
            .entries
            .iter()
            .find(|e| e.path == path)
            .and_then(|e| e.exec.clone())
        {
            self.spawn(&exec);
            return;
        }
        let quoted = path.to_string_lossy().replace('\'', "'\\''");
        self.spawn(&format!("xdg-open '{quoted}'"));
    }

    fn desktop_pointer(&mut self, qh: &QueueHandle<Self>, kind: &PointerEventKind, x: f64, y: f64) {
        let (x, y) = (x as f32, y as f32);
        match kind {
            PointerEventKind::Press { button: BTN_LEFT, .. } => {
                if self.menu.is_some() {
                    self.close_menu();
                }
                let (changed, open_idx) = self.desktop.left_click(x, y);
                if changed {
                    self.redraw(Which::Desktop);
                }
                if let Some(idx) = open_idx {
                    if let Some(path) = self.desktop.entries.get(idx).map(|e| e.path.clone()) {
                        self.open_desktop_path(&path);
                    }
                }
            }
            PointerEventKind::Press { button: BTN_RIGHT, .. } => {
                let hit = self.desktop.hit(x, y);
                if self.desktop.selected != hit {
                    self.desktop.selected = hit;
                    self.redraw(Which::Desktop);
                }
                let de = self.german;
                let t = |en: &str, de_s: &str| if de { de_s.to_string() } else { en.to_string() };
                let items = match hit.and_then(|i| self.desktop.entries.get(i).cloned()) {
                    Some(ent) => vec![
                        MenuItem::new(t("Open", "Öffnen"), Action::DesktopOpen(ent.path.clone())),
                        MenuItem::new(
                            t("Copy Path", "Pfad kopieren"),
                            Action::CopyText(ent.path.to_string_lossy().into_owned()),
                        ),
                        MenuItem::separator(),
                        MenuItem::new(t("Move to Trash", "In den Papierkorb legen"), Action::DesktopTrash(ent.path)),
                    ],
                    None => vec![
                        MenuItem::new(t("New Folder", "Neuer Ordner"), Action::DesktopNewFolder),
                        MenuItem::new(
                            t("Open Desktop in Files", "Schreibtisch in Dateien öffnen"),
                            Action::DesktopOpen(self.desktop.dir.clone()),
                        ),
                        MenuItem::separator(),
                        MenuItem::new(t("Sort by Name", "Nach Name sortieren"), Action::DesktopSort(desktop::SortBy::Name)),
                        MenuItem::new(t("Sort by Date", "Nach Datum sortieren"), Action::DesktopSort(desktop::SortBy::Date)),
                        MenuItem::new(t("Sort by Kind", "Nach Art sortieren"), Action::DesktopSort(desktop::SortBy::Kind)),
                        MenuItem::separator(),
                        MenuItem::new(
                            t("Change Wallpaper…", "Hintergrundbild ändern …"),
                            Action::Launch("orchard-settings Wallpaper".into()),
                        ),
                    ],
                };
                self.open_menu(qh, MenuOwner::Desktop, Menu::new(items, x, y));
            }
            _ => {}
        }
    }

    /// Top-left of the Dock surface on its output (logical px).
    fn dock_origin(&self) -> (f32, f32) {
        let Some(s) = self.dock_surf.as_ref() else { return (0.0, 0.0) };
        let x = if self.dock.edge() == DockPosition::Right {
            self.output_size.0 as f32 - s.width as f32
        } else {
            0.0
        };
        (x, self.output_size.1 as f32 - s.height as f32)
    }

    /// Destroy every layer surface (they are all tied to `self.output`).
    fn drop_surfaces(&mut self) {
        if self.menu.is_some() || self.menu_surf.is_some() {
            self.close_menu();
        }
        if self.launcher.is_some() {
            self.close_launcher_now();
        }
        self.switcher = None;
        self.switcher_surf = None;
        self.osd = None;
        self.osd_surf = None;
        self.set_backdrop("orchard-osd", None);
        self.panel = None;
        self.panel_surf = None;
        self.close_dialog();
        self.polkit_surf = None;
        self.set_backdrop("orchard-polkit", None);
        self.desktop_surf = None;
        self.close_screenshot_tool();
        // banners stay in the model and come back on the new output
        self.notes_surf = None;
        self.bar = None;
        self.dock_surf = None;
        self.output = None;
        self.pointer_on = None;
        self.last_backdrop = None;
        self.last_tiles.clear();
    }

    fn adopt_output(&mut self, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        let info = self.output_state.info(&output);
        if let Some(info) = &info {
            self.output_name = info.name.clone().unwrap_or_default();
            if let Some((w, h)) = info.logical_size {
                self.output_size = (w, h);
            }
        }
        tracing::info!("using output {:?} {:?}", self.output_name, self.output_size);

        let surface = self.compositor.create_surface(qh);
        let bar = self
            .layer_shell
            .create_layer_surface(qh, surface, Layer::Top, Some("orchard-menubar"), Some(&output));
        bar.set_anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT);
        bar.set_size(0, menubar::HEIGHT as u32);
        bar.set_exclusive_zone(menubar::HEIGHT as i32);
        bar.set_keyboard_interactivity(KeyboardInteractivity::None);
        bar.commit();
        self.bar = Some(Surf::new(bar));

        self.create_dock_surface(qh, &output);
        if self.desktop.enabled {
            self.create_desktop_surface(qh, &output);
        }

        if let Some(ipc) = self.ipc.as_mut() {
            ipc.request(
                "set_backdrop",
                json!({"namespace": "orchard-menubar", "rects": [{"x": 0, "y": 0, "w": 100000, "h": menubar::HEIGHT, "radius": 0}]}),
            );
        }
        self.output = Some(output);
        if !self.notes.banners.is_empty() {
            self.update_notes_surface();
        }
        if self.polkit.is_some() {
            self.ensure_polkit_surface();
        }
    }

    fn surf(&mut self, which: Which) -> Option<&mut Surf> {
        match which {
            Which::Bar => self.bar.as_mut(),
            Which::Dock => self.dock_surf.as_mut(),
            Which::Menu => self.menu_surf.as_mut(),
            Which::Switcher => self.switcher_surf.as_mut(),
            Which::Launcher => self.launcher_surf.as_mut(),
            Which::Osd => self.osd_surf.as_mut(),
            Which::Notify => self.notes_surf.as_mut(),
            Which::Panel => self.panel_surf.as_mut(),
            Which::Launchpad => self.launchpad_surf.as_mut(),
            Which::Screenshot => self.screenshot_surf.as_mut(),
            Which::Dialog => self.dialog_surf.as_mut(),
            Which::Polkit => self.polkit_surf.as_mut(),
            Which::Desktop => self.desktop_surf.as_mut(),
        }
    }

    fn which(&self, surface: &wl_surface::WlSurface) -> Option<Which> {
        if self.bar.as_ref().map(|s| s.layer.wl_surface() == surface).unwrap_or(false) {
            Some(Which::Bar)
        } else if self.dock_surf.as_ref().map(|s| s.layer.wl_surface() == surface).unwrap_or(false) {
            Some(Which::Dock)
        } else if self.menu_surf.as_ref().map(|s| s.layer.wl_surface() == surface).unwrap_or(false) {
            Some(Which::Menu)
        } else if self.switcher_surf.as_ref().map(|s| s.layer.wl_surface() == surface).unwrap_or(false) {
            Some(Which::Switcher)
        } else if self.launcher_surf.as_ref().map(|s| s.layer.wl_surface() == surface).unwrap_or(false) {
            Some(Which::Launcher)
        } else if self.osd_surf.as_ref().map(|s| s.layer.wl_surface() == surface).unwrap_or(false) {
            Some(Which::Osd)
        } else if self.notes_surf.as_ref().map(|s| s.layer.wl_surface() == surface).unwrap_or(false) {
            Some(Which::Notify)
        } else if self.panel_surf.as_ref().map(|s| s.layer.wl_surface() == surface).unwrap_or(false) {
            Some(Which::Panel)
        } else if self.launchpad_surf.as_ref().map(|s| s.layer.wl_surface() == surface).unwrap_or(false) {
            Some(Which::Launchpad)
        } else if self.screenshot_surf.as_ref().map(|s| s.layer.wl_surface() == surface).unwrap_or(false) {
            Some(Which::Screenshot)
        } else if self.dialog_surf.as_ref().map(|s| s.layer.wl_surface() == surface).unwrap_or(false) {
            Some(Which::Dialog)
        } else if self.polkit_surf.as_ref().map(|s| s.layer.wl_surface() == surface).unwrap_or(false) {
            Some(Which::Polkit)
        } else if self.desktop_surf.as_ref().map(|s| s.layer.wl_surface() == surface).unwrap_or(false) {
            Some(Which::Desktop)
        } else {
            None
        }
    }

    fn poll_ipc(&mut self) {
        let Some(ipc) = self.ipc.as_mut() else { return };
        let events = match ipc.poll() {
            Ok(ev) => ev,
            Err(()) => {
                tracing::warn!("compositor IPC closed");
                self.ipc = None;
                self.exit = true;
                return;
            }
        };
        for ev in events {
            match ev {
                Event::Windows(ws) => {
                    self.windows = ws;
                    self.on_windows_changed();
                }
                Event::Other(v) => match v["event"].as_str() {
                    Some("switcher") => {
                        let apps: Vec<String> = v["apps"]
                            .as_array()
                            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                            .unwrap_or_default();
                        let selected = v["selected"].as_u64().unwrap_or(0) as usize;
                        self.show_switcher(apps, selected);
                    }
                    Some("switcher_hide") => {
                        self.switcher = None;
                        self.switcher_surf = None;
                        self.set_backdrop("orchard-switcher", None);
                    }
                    Some("launcher_toggle") => self.toggle_launcher(),
                    Some("launchpad_toggle") => self.toggle_launchpad(),
                    Some("screenshot_tool") => {
                        let output = v["output"].as_str().unwrap_or_default().to_string();
                        let windows = v["windows"]
                            .as_array()
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|w| {
                                        Some(screenshot::ShotWindow {
                                            id: w["id"].as_u64()?,
                                            x: w["x"].as_f64()? as f32,
                                            y: w["y"].as_f64()? as f32,
                                            w: w["w"].as_f64()? as f32,
                                            h: w["h"].as_f64()? as f32,
                                            title: w["title"].as_str().unwrap_or_default().to_string(),
                                        })
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        self.toggle_screenshot_tool(output, windows);
                    }
                    Some("settings_changed") => self.reload_settings(),
                    Some("game_mode") => {
                        let active = v["active"].as_bool().unwrap_or(false);
                        let app_id = v["app_id"].as_str().map(str::to_string);
                        let silence = v["silence"].as_bool().unwrap_or(true);
                        self.on_game_mode(active, app_id, silence);
                    }
                    Some("dock_obstructed") => {
                        let obstructed = v["obstructed"].as_bool().unwrap_or(false);
                        if self.dock.obstructed != obstructed {
                            self.dock.obstructed = obstructed;
                            self.redraw(Which::Dock);
                        }
                    }
                    Some("media_key") => {
                        let key = v["key"].as_str().unwrap_or_default().to_string();
                        let fine = v["fine"].as_bool().unwrap_or(false);
                        let tx = self.osd_tx.clone();
                        let _ = std::thread::Builder::new().name("media-key".into()).spawn(move || {
                            if let Some(level) = osd::run_key(&key, fine) {
                                let _ = tx.send(level);
                            }
                        });
                    }
                    _ => {}
                },
            }
        }
    }

    /// orchard-settings saved: re-read settings.json and re-apply it.
    fn reload_settings(&mut self) {
        let s = orchard_config::Settings::load();
        anim::set_reduced(s.appearance.reduce_motion);
        tracing::info!("settings changed");
        if self.primary != s.displays.primary {
            self.primary = s.displays.primary.clone();
            self.retarget_output();
        }
        let old_theme = self.theme.clone();
        self.theme = Theme::from_settings(&s);
        let theme_changed = self.theme != old_theme;
        self.perf = s.performance.clone();
        let icons_changed = self.apps.set_icon_theme(s.appearance.icon_theme.clone());
        let was_showing_volume = self.menubar.cfg.show_volume;
        let old_bar_cfg = self.menubar.cfg.clone();
        self.menubar.cfg = s.menu_bar.clone();
        if let Some(p) = self.panel.as_mut() {
            p.night_shift = s.night_shift.active_now();
        }
        let status_changed = self.menubar.refresh_status_with(self.perf.battery_saver);
        if self.menubar.cfg.show_volume && !was_showing_volume {
            self.refresh_volume();
        }
        if (self.menubar.open == Some(BarItem::Volume) && !self.menubar.cfg.show_volume)
            || (self.menubar.open == Some(BarItem::Network) && !self.menubar.cfg.show_network)
        {
            self.close_panel();
        }
        let old_h = self.dock.surface_height();
        let old_zone = self.dock.exclusive_zone();
        let old_edge = self.dock.edge();
        let old_dock_cfg = self.dock.cfg.clone();
        self.dock.cfg = s.dock.clone();
        let dock_changed = self.dock.cfg != old_dock_cfg;
        if self.dock.cfg.autohide == DockAutohide::Never {
            self.dock.hidden = 0.0;
        }
        let (h, zone) = (self.dock.surface_height(), self.dock.exclusive_zone());
        if self.dock.edge() != old_edge {
            // another edge: a new surface with other anchors
            if let (Some(output), true) = (self.output.clone(), self.dock_surf.is_some()) {
                if matches!(self.menu_owner, Some(MenuOwner::Dock(_))) {
                    self.close_menu();
                }
                self.dock_surf = None;
                self.dock.hover_x = None;
                self.dock.last_hover_x = None;
                self.dock.hovered = None;
                self.dock.pressed = None;
                let qh = self.qh.clone();
                self.create_dock_surface(&qh, &output);
            }
        } else if let Some(surf) = self.dock_surf.as_ref() {
            if h != old_h || zone != old_zone {
                if self.dock.vertical() {
                    surf.layer.set_size(h as u32, 0);
                } else {
                    surf.layer.set_size(0, h as u32);
                }
                surf.layer.set_exclusive_zone(zone as i32);
                surf.layer.commit();
            }
        }
        let bar_changed = theme_changed || status_changed || self.menubar.cfg != old_bar_cfg;
        if bar_changed {
            self.redraw(Which::Bar);
        }
        if dock_changed || theme_changed || icons_changed {
            self.last_backdrop = None;
            self.last_tiles.clear();
            self.redraw(Which::Dock);
            self.send_dock_tiles();
        }
        if self.menu.is_some() {
            self.redraw(Which::Menu);
        }
        if icons_changed {
            self.desktop.clear_icon_cache();
            self.redraw_icon_surfaces();
        }
        let show_desk = s.windows.show_desktop_icons;
        if self.desktop.enabled != show_desk {
            self.desktop.enabled = show_desk;
            if show_desk {
                self.desktop.rescan();
                if let Some(output) = self.output.clone() {
                    let qh = self.qh.clone();
                    self.create_desktop_surface(&qh, &output);
                }
            } else {
                self.desktop_surf = None;
            }
        } else if show_desk {
            self.redraw(Which::Desktop);
        }
        self.flush_frames(&self.qh.clone());
    }

    fn on_windows_changed(&mut self) {
        let focused = self.windows.iter().find(|w| w.focused).cloned();
        match focused {
            Some(w) => {
                self.menubar.app_name = self
                    .apps
                    .for_app_id(&w.app_id)
                    .map(|e| e.name.clone())
                    .unwrap_or_else(|| if w.title.is_empty() { w.app_id.clone() } else { w.title.clone() });
                self.menubar.app_id = Some(w.app_id.clone());
            }
            None => {
                self.menubar.app_name = "Finder".into();
                self.menubar.app_id = None;
            }
        }
        let apps = &self.apps;
        self.dock.rebuild(apps, &self.windows);
        self.refresh_app_menu();
        self.redraw(Which::Bar);
        self.redraw(Which::Dock);
        self.send_dock_tiles();
    }

    /// Menu address of the focused window (Wayland: from the compositor, X11: registrar).
    fn focused_menu_address(&self) -> Option<globalmenu::MenuSource> {
        use globalmenu::MenuSource;
        let w = self.windows.iter().find(|w| w.focused)?;
        if let Some(m) = &w.menu {
            return Some(MenuSource::DbusMenu { service: m.service.clone(), path: m.path.clone() });
        }
        if let Some(g) = &w.gtk_menu {
            return Some(MenuSource::Gtk(g.clone()));
        }
        let gmenu = self.gmenu.as_ref()?;
        let found = match (w.x11_id, w.pid) {
            (Some(id), _) => gmenu.x11_menu(id),
            // Wayland Electron: registered for its process, not a window
            (None, Some(pid)) => {
                let mut siblings: Vec<u64> =
                    self.windows.iter().filter(|o| o.pid == Some(pid) && o.x11_id.is_none()).map(|o| o.id).collect();
                siblings.sort_unstable();
                let nth = siblings.iter().position(|id| *id == w.id).unwrap_or(0);
                gmenu.process_menu(pid, nth)
            }
            _ => None,
        };
        found.map(|(service, path)| MenuSource::DbusMenu { service, path })
    }

    /// Load the focused app's menu titles if its menu changed (on a helper thread:
    /// a hung app must not freeze the menu bar).
    fn refresh_app_menu(&mut self) {
        let addr = self.focused_menu_address();
        if addr == self.menu_addr {
            return;
        }
        self.menu_addr = addr.clone();
        if addr.is_none() {
            self.on_app_menu(None, Vec::new());
            return;
        }
        let Some(conn) = self.gmenu.as_ref().map(|g| g.conn.clone()) else { return };
        let tx = self.gmenu_tx.clone();
        let _ = std::thread::Builder::new().name("app-menu".into()).spawn(move || {
            let tops = addr.as_ref().map(|a| a.tops(&conn)).unwrap_or_default();
            let _ = tx.send((addr, tops));
        });
    }

    fn on_app_menu(&mut self, addr: Option<globalmenu::MenuSource>, tops: Vec<(i32, String)>) {
        if addr != self.menu_addr {
            return; // focus moved on meanwhile
        }
        let labels: Vec<String> = tops.iter().map(|(_, l)| l.clone()).collect();
        if labels == self.menubar.app_menus {
            return;
        }
        self.app_menu_ids = tops.into_iter().map(|(i, _)| i).collect();
        self.menubar.app_menus = labels;
        self.redraw(Which::Bar);
    }

    fn send_dock_tiles(&mut self) {
        let Some(s) = self.dock_surf.as_ref() else { return };
        if !s.configured {
            return;
        }
        let (sw, sh) = (s.width as f32, s.height as f32);
        let (ox, oy) = self.dock_origin();
        let (tiles, next) = self.dock.genie_tiles(sw, sh);
        let mut tiles: Vec<serde_json::Value> = tiles
            .into_iter()
            .filter_map(|(kind, (x, y, w, h))| {
                let rect = json!({"x": ox + x, "y": oy + y, "w": w, "h": h});
                match kind {
                    ItemKind::App { app_id, .. } => Some(json!({"app_id": app_id, "rect": rect})),
                    ItemKind::Minimized { app_id, window_id, .. } => {
                        Some(json!({"app_id": app_id, "window_id": window_id, "rect": rect}))
                    }
                    _ => None,
                }
            })
            .collect();
        // the tile a window minimized now gets: the genie aims there
        if let Some((x, y, w, h)) = next {
            tiles.push(json!({"app_id": "", "next_minimized": true, "rect": {"x": ox + x, "y": oy + y, "w": w, "h": h}}));
        }
        let edge = match self.dock.edge() {
            DockPosition::Left => "left",
            DockPosition::Bottom => "bottom",
            DockPosition::Right => "right",
        };
        let (rx, ry, rw, rh) = self.dock.resting_rect(sw, sh);
        let msg = json!({
            "output": self.output_name,
            "edge": edge,
            "tiles": tiles,
            "dock_rect": {"x": ox + rx, "y": oy + ry, "w": rw, "h": rh},
        });
        let key = msg.to_string();
        if key != self.last_tiles {
            self.last_tiles = key;
            if let Some(ipc) = self.ipc.as_mut() {
                ipc.request("set_dock_tiles", msg);
            }
        }
    }

    /// Schedule a repaint. Painting happens once per event-loop iteration
    /// (`flush_draws`) and at most once per compositor frame: a 1-8 kHz mouse
    /// over the Dock must not trigger one full repaint + buffer per motion
    /// event, or the shell falls behind, the compositor's send buffer to us
    /// overflows and libwayland drops the connection ("Data too big for buffer").
    /// Repaint everything that shows app icons (after an icon theme switch).
    fn redraw_icon_surfaces(&mut self) {
        for w in [Which::Switcher, Which::Launcher, Which::Launchpad, Which::Notify, Which::Panel] {
            self.redraw(w);
        }
    }

    /// Notify when the computer's or a wireless device's battery runs low.
    fn check_low_battery(&mut self) {
        let german = self.menubar.german;
        let mut notes: Vec<notify::Incoming> = Vec::new();
        let mut keys: Vec<String> = Vec::new();
        if let Some(b) = status::battery() {
            keys.push("system".into());
            if let Some(t) = self.low_battery.check("system", b.percent, b.charging, lowbattery::SYSTEM) {
                let critical = t <= 5;
                notes.push(low_battery_note(
                    lowbattery::notification_id("system"),
                    if german { "Batterie schwach".into() } else { "Low Battery".into() },
                    if german {
                        format!("Noch {} %. Schließe den Computer bald an das Stromnetz an.", b.percent)
                    } else {
                        format!("{}% remaining. Plug in your computer soon.", b.percent)
                    },
                    if critical { "battery-caution" } else { "battery-low" },
                    critical,
                ));
            }
        }
        for d in status::device_batteries() {
            keys.push(d.key.clone());
            if self.low_battery.check(&d.key, d.percent, d.charging, lowbattery::DEVICE).is_some() {
                notes.push(low_battery_note(
                    lowbattery::notification_id(&d.key),
                    if german { format!("Batterie von „{}“ schwach", d.name) } else { format!("{} Battery Low", d.name) },
                    if german {
                        format!("Noch {} %. Lade das Gerät bald auf.", d.percent)
                    } else {
                        format!("{}% remaining. Charge it soon.", d.percent)
                    },
                    "battery-low",
                    false,
                ));
            }
        }
        let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
        self.low_battery.retain(&keys);
        for n in notes {
            tracing::info!("low battery: {}", n.summary);
            // a lower level slides in again instead of silently updating the old banner
            self.notes.remove(n.id);
            self.on_notification(notify::Msg::Notify(Box::new(n)));
        }
    }

    /// Slow status-poll cadence (seconds): relaxed on battery or in Power Saver.
    fn slow_poll_interval(&self) -> u64 {
        let on_battery = self.menubar.battery.as_ref().map(|b| !b.charging).unwrap_or(false);
        // Game Mode: keep the background quiet while playing
        if self.game.is_some()
            || self.perf.preset == orchard_config::PerformancePreset::PowerSaver
            || (self.perf.battery_saver && on_battery)
        {
            30
        } else {
            10
        }
    }

    /// Only wake up on a timeout if a dirty surface is waiting on a potentially stale
    /// frame callback; otherwise sleep completely until the next Wayland/IPC/timer event.
    fn next_dispatch_timeout(&self) -> Option<Duration> {
        let surfaces = [
            self.bar.as_ref(),
            self.dock_surf.as_ref(),
            self.menu_surf.as_ref(),
            self.switcher_surf.as_ref(),
            self.launcher_surf.as_ref(),
            self.osd_surf.as_ref(),
            self.notes_surf.as_ref(),
            self.panel_surf.as_ref(),
            self.launchpad_surf.as_ref(),
            self.screenshot_surf.as_ref(),
            self.dialog_surf.as_ref(),
            self.polkit_surf.as_ref(),
        ];
        if surfaces.into_iter().flatten().any(|s| s.dirty && s.frame_pending) {
            Some(Duration::from_millis(260))
        } else {
            None
        }
    }

    fn redraw(&mut self, which: Which) {
        if let Some(s) = self.surf(which) {
            s.dirty = true;
        }
    }

    /// Paint every dirty surface that is not waiting for a frame callback.
    fn flush_draws(&mut self) {
        const ALL: [Which; 13] = [
            Which::Launchpad,
            Which::Bar,
            Which::Dock,
            Which::Menu,
            Which::Switcher,
            Which::Launcher,
            Which::Osd,
            Which::Notify,
            Which::Panel,
            Which::Screenshot,
            Which::Dialog,
            Which::Polkit,
            Which::Desktop,
        ];
        for which in ALL {
            let Some(s) = self.surf(which) else { continue };
            if !s.dirty || !s.configured || s.width == 0 || s.height == 0 {
                continue;
            }
            if s.frame_pending {
                // hidden/unmapped surfaces may never get their callback
                let stale = s.frame_since.map(|t| t.elapsed() > Duration::from_millis(250)).unwrap_or(true);
                if !stale {
                    continue;
                }
                s.frame_pending = false;
            }
            self.draw(which);
        }
        let qh = self.qh.clone();
        self.flush_frames(&qh);
    }

    fn draw(&mut self, which: Which) {
        let (w, h, scale, existing_pm) = match self.surf(which) {
            Some(s) if s.configured => {
                s.dirty = false;
                (s.width, s.height, s.scale, s.pixmap.take())
            }
            _ => return,
        };
        let pw = w as i32 * scale;
        let ph = h as i32 * scale;
        let mut pm = match existing_pm {
            Some(mut pm) if pm.width() == pw as u32 && pm.height() == ph as u32 => {
                pm.fill(tiny_skia::Color::TRANSPARENT);
                pm
            }
            _ => match Pixmap::new(pw as u32, ph as u32) {
                Some(pm) => pm,
                None => return,
            },
        };
        let fscale = scale as f32;

        let mut animating = false;
        match which {
            Which::Bar => self.menubar.paint(&mut pm, fscale, &self.fonts, &self.apps, &self.theme),
            Which::Dock => {
                let now = Instant::now();
                let dt = now.duration_since(self.last_step).as_secs_f32().min(0.05);
                self.last_step = now;
                self.dock.step(dt);
                let target = if self.dock.hover_x.is_some() { 1.0 } else { 0.0 };
                animating = self.dock.is_animating(target);
                self.dock.paint(&mut pm, fscale, &self.apps, &self.fonts, &self.theme);
                let bg = self.dock.surface_bg(w as f32, h as f32);
                self.update_dock_regions(bg);
            }
            Which::Menu => {
                if let Some(menu) = &self.menu {
                    menu.paint(&mut pm, fscale, &self.fonts, &self.theme);
                    animating = menu.is_animating();
                }
                self.send_menu_backdrop();
            }
            Which::Switcher => {
                if let Some(sw) = self.switcher.as_mut() {
                    sw.paint(&mut pm, fscale, &self.apps, &self.fonts, &self.theme);
                    animating = sw.is_animating();
                    let (sw_w, sw_h) = sw.size();
                    let a = sw.open_progress();
                    self.set_backdrop_alpha("orchard-switcher", Some((0.0, 0.0, sw_w as f32, sw_h as f32, 22.0, a)));
                }
            }
            Which::Launcher => match self.paint_launcher(&mut pm, fscale) {
                Some(a) => animating = a,
                None => return,
            },
            Which::Osd => {
                let Some(o) = self.osd.as_mut() else { return };
                o.step(Instant::now());
                if o.is_closed() {
                    self.osd = None;
                    self.osd_surf = None;
                    self.set_backdrop("orchard-osd", None);
                    return;
                }
                o.paint(&mut pm, fscale, &self.theme);
                animating = o.is_animating();
                if let Some((bx, by, bw, bh, br, ba)) = o.take_backdrop_update() {
                    if let Some(ipc) = self.ipc.as_mut() {
                        ipc.request(
                            "set_backdrop",
                            json!({"namespace": "orchard-osd", "rects": [{"x": bx, "y": by, "w": bw, "h": bh, "radius": br, "alpha": ba}]}),
                        );
                    }
                }
            }
            Which::Notify => {
                self.notes.paint(&mut pm, fscale, &self.apps, &self.fonts, &self.theme);
                animating = self.notes.is_animating();
                self.send_notes_backdrop();
            }
            Which::Launchpad => {
                let now = Instant::now();
                let dt = now.duration_since(self.launchpad_step).as_secs_f32().min(0.05);
                self.launchpad_step = now;
                let Some(lp) = self.launchpad.as_mut() else { return };
                lp.step(dt);
                if lp.is_closed() {
                    self.drop_launchpad();
                    return;
                }
                lp.paint(&mut pm, fscale, &self.apps, &self.fonts, &self.theme, self.german);
                animating = lp.is_animating();
                let a = lp.open;
                if (a - self.launchpad_backdrop).abs() > 0.01 || (a >= 1.0 && self.launchpad_backdrop < 1.0) {
                    self.launchpad_backdrop = a;
                    // blur only behind the centered panel
                    let (x, y, bw, bh, r) = lp.backdrop(w as f32, h as f32);
                    if let Some(ipc) = self.ipc.as_mut() {
                        ipc.request(
                            "set_backdrop",
                            json!({"namespace": "orchard-launchpad", "rects": [{"x": x, "y": y, "w": bw, "h": bh, "radius": r, "alpha": a}]}),
                        );
                    }
                }
            }
            Which::Panel => {
                if self.panel.as_ref().is_some_and(|p| p.is_closed()) {
                    self.drop_panel_now();
                    return;
                }
                if let Some(p) = self.panel.as_mut() {
                    p.paint(&mut pm, fscale, w as f32, h as f32, &self.notes, &self.apps, &self.fonts, &self.theme, self.german);
                    p.remember_cards();
                    animating = p.is_animating();
                }
                self.send_panel_backdrop();
            }
            Which::Screenshot => {
                if let Some(st) = self.screenshot.as_mut() {
                    st.paint(&mut pm, fscale, &self.fonts, &self.theme, self.german);
                }
                self.send_screenshot_backdrop();
            }
            Which::Dialog => {
                if let Some(d) = self.dialog.as_mut() {
                    d.paint(&mut pm, fscale, &self.fonts, &self.theme, self.german);
                    animating = d.is_animating();
                    let (x, y, dw, dh) = d.rect;
                    let a = d.open_progress();
                    self.set_backdrop_alpha("orchard-dialog", Some((x, y, dw, dh, 16.0, a)));
                }
            }
            Which::Polkit => {
                if let Some(p) = self.polkit.as_mut() {
                    p.paint(&mut pm, fscale, &self.fonts, &self.apps, &self.theme, self.mods.caps_lock, self.german);
                    animating = p.is_animating();
                    let (x, y, dw, dh) = p.rect;
                    self.set_backdrop("orchard-polkit", Some((x, y, dw, dh, 16.0)));
                }
            }
            Which::Desktop => {
                self.desktop.paint(&mut pm, fscale, &self.fonts, &mut self.apps, &self.theme);
            }
        }

        let qh = self.qh.clone();
        let shm = &self.shm;
        let s = match which {
            Which::Bar => self.bar.as_mut(),
            Which::Dock => self.dock_surf.as_mut(),
            Which::Menu => self.menu_surf.as_mut(),
            Which::Switcher => self.switcher_surf.as_mut(),
            Which::Launcher => self.launcher_surf.as_mut(),
            Which::Osd => self.osd_surf.as_mut(),
            Which::Notify => self.notes_surf.as_mut(),
            Which::Panel => self.panel_surf.as_mut(),
            Which::Launchpad => self.launchpad_surf.as_mut(),
            Which::Screenshot => self.screenshot_surf.as_mut(),
            Which::Dialog => self.dialog_surf.as_mut(),
            Which::Polkit => self.polkit_surf.as_mut(),
            Which::Desktop => self.desktop_surf.as_mut(),
        };
        let Some(s) = s else { return };
        let initial_bytes = ((pw as usize) * (ph as usize) * 4).max(4096);
        let pool = match s.pool.as_mut() {
            Some(p) => p,
            None => match SlotPool::new(initial_bytes, shm) {
                Ok(p) => s.pool.insert(p),
                Err(e) => {
                    tracing::warn!("shm pool: {e}");
                    s.pixmap = Some(pm);
                    return;
                }
            },
        };
        let (buffer, canvas) = match pool.create_buffer(pw, ph, pw * 4, wl_shm::Format::Argb8888) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("shm buffer: {e}");
                s.pixmap = Some(pm);
                return;
            }
        };
        draw::blit_to_shm(&pm, canvas);
        s.pixmap = Some(pm);
        let surface = s.layer.wl_surface().clone();
        surface.damage_buffer(0, 0, pw, ph);
        // Every commit asks for a frame callback: further repaints wait for it
        // (vsync-paced), and an animation continues from it (dirty stays set).
        if animating {
            s.dirty = true;
        }
        if !s.frame_pending {
            s.frame_pending = true;
            s.frame_since = Some(Instant::now());
            surface.frame(&qh, surface.clone());
        }
        let _ = buffer.attach_to(&surface);
        s.layer.commit();
    }

    fn update_dock_regions(&mut self, bg: (f32, f32, f32, f32)) {
        if self.last_backdrop == Some(bg) {
            return;
        }
        self.last_backdrop = Some(bg);
        let radius = self.dock.radius();
        let hidden = self.dock.is_hidden();
        if let Some(ipc) = self.ipc.as_mut() {
            let rects = if hidden {
                json!([])
            } else {
                json!([{"x": bg.0, "y": bg.1, "w": bg.2, "h": bg.3, "radius": radius}])
            };
            ipc.request("set_backdrop", json!({"namespace": "orchard-dock", "rects": rects}));
        }
        // input only over the dock (plus magnified icons), so clicks above pass through
        if let Some(s) = self.dock_surf.as_ref() {
            if let Ok(region) = Region::new(&self.compositor) {
                let (w, h) = (s.width as i32, s.height as i32);
                let strip = dock::REVEAL_STRIP as i32;
                if !hidden {
                    // the bar plus room for magnified icons on the inner side,
                    // extended all the way to the monitor edge (no dead gap)
                    let grow = (self.dock.icon_max() - self.dock.icon()).ceil() as i32 + 4;
                    let (x, y, bw, bh) = (bg.0.round() as i32, bg.1.round() as i32, bg.2.round() as i32, bg.3.round() as i32);
                    match self.dock.edge() {
                        DockPosition::Bottom => {
                            let top = (y - grow).clamp(0, h);
                            let rh = h - top;
                            if rh > 0 && bw > 0 {
                                region.add(x, top, bw, rh);
                            }
                        }
                        DockPosition::Left => {
                            let right = (x + bw + grow).clamp(0, w);
                            if right > 0 && bh > 0 {
                                region.add(0, y, right, bh);
                            }
                        }
                        DockPosition::Right => {
                            let left = (x - grow).clamp(0, w);
                            let rw = w - left;
                            if rw > 0 && bh > 0 {
                                region.add(left, y, rw, bh);
                            }
                        }
                    }
                }
                if hidden || self.dock.cfg.autohide != DockAutohide::Never {
                    // keep the edge reveal strip active while hidden and during
                    // reveal/hide so hitting the monitor edge anywhere opens the
                    // Dock and never drops pointer focus mid-animation
                    match self.dock.edge() {
                        DockPosition::Bottom => region.add(0, h - strip, w, strip),
                        DockPosition::Left => region.add(0, 0, strip, h),
                        DockPosition::Right => region.add(w - strip, 0, strip, h),
                    }
                }
                s.layer.wl_surface().set_input_region(Some(region.wl_region()));
            }
        }
    }

    fn set_backdrop(&mut self, namespace: &str, rect: Option<(f32, f32, f32, f32, f32)>) {
        self.set_backdrop_alpha(namespace, rect.map(|(x, y, w, h, r)| (x, y, w, h, r, 1.0)));
    }

    fn set_backdrop_alpha(&mut self, namespace: &str, rect: Option<(f32, f32, f32, f32, f32, f32)>) {
        if let Some(ipc) = self.ipc.as_mut() {
            let rects: Vec<serde_json::Value> = rect
                .map(|(x, y, w, h, r, a)| vec![json!({"x": x, "y": y, "w": w, "h": h, "radius": r, "alpha": a})])
                .unwrap_or_default();
            ipc.request("set_backdrop", json!({"namespace": namespace, "rects": rects}));
        }
    }

    // ---- macOS-style Screenshot tool (Print / ⌘⇧5) -------------------------

    fn toggle_screenshot_tool(&mut self, output_name: String, windows: Vec<screenshot::ShotWindow>) {
        if self.screenshot.is_some() {
            self.close_screenshot_tool();
            return;
        }
        if self.menu.is_some() {
            self.close_menu();
        }
        if self.panel.is_some() {
            self.close_panel();
        }
        if self.launcher.is_some() {
            self.close_launcher_now();
        }
        self.close_launchpad();

        let target_output = self
            .output_state
            .outputs()
            .find(|o| {
                self.output_state
                    .info(o)
                    .and_then(|i| i.name)
                    .as_deref()
                    == Some(output_name.as_str())
            })
            .or_else(|| self.output.clone());

        let tool = screenshot::ScreenshotTool::new(output_name, windows, &self.screenshot_prefs);
        self.screenshot = Some(tool);
        self.last_shot_backdrop.clear();

        let surface = self.compositor.create_surface(&self.qh);
        let layer = self.layer_shell.create_layer_surface(
            &self.qh,
            surface,
            Layer::Overlay,
            Some("orchard-screenshot"),
            target_output.as_ref(),
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        layer.commit();
        self.screenshot_surf = Some(Surf::new(layer));
    }

    fn close_screenshot_tool(&mut self) {
        if let Some(tool) = self.screenshot.take() {
            tool.save_prefs(&mut self.screenshot_prefs);
        }
        if self.screenshot_surf.take().is_some() {
            self.set_backdrop("orchard-screenshot", None);
            self.last_shot_backdrop.clear();
        }
    }

    fn send_screenshot_backdrop(&mut self) {
        let Some(tool) = self.screenshot.as_ref() else { return };
        let rects = tool.backdrop_rects();
        if rects == self.last_shot_backdrop {
            return;
        }
        self.last_shot_backdrop = rects.clone();
        if let Some(ipc) = self.ipc.as_mut() {
            let v: Vec<serde_json::Value> = rects
                .iter()
                .map(|(x, y, w, h, r)| json!({"x": x, "y": y, "w": w, "h": h, "radius": r}))
                .collect();
            ipc.request("set_backdrop", json!({"namespace": "orchard-screenshot", "rects": v}));
        }
    }

    fn execute_screenshot_capture(&mut self, spec: screenshot::CaptureSpec) {
        self.close_screenshot_tool();
        if let Some(ipc) = self.ipc.as_mut() {
            let rect = spec.rect.map(|(x, y, w, h)| json!({"x": x, "y": y, "w": w, "h": h}));
            ipc.request(
                "screenshot_capture",
                json!({
                    "mode": spec.mode.as_str(),
                    "output": spec.output,
                    "rect": rect,
                    "window_id": spec.window_id,
                    "target": spec.save_to.as_str(),
                    "include_cursor": spec.show_cursor,
                }),
            );
        }
    }

    fn handle_screenshot_action(&mut self, action: screenshot::Action) {
        match action {
            screenshot::Action::None => {}
            screenshot::Action::Redraw => self.redraw(Which::Screenshot),
            screenshot::Action::Close => self.close_screenshot_tool(),
            screenshot::Action::CaptureNow(spec) => self.execute_screenshot_capture(spec),
            screenshot::Action::StartCountdown => {
                self.redraw(Which::Screenshot);
                let _ = self.loop_handle.insert_source(
                    Timer::from_duration(Duration::from_millis(200)),
                    |_, _, app: &mut App| {
                        let ready = app.screenshot.as_mut().and_then(|s| s.take_ready_capture());
                        if let Some(spec) = ready {
                            app.execute_screenshot_capture(spec);
                            return TimeoutAction::Drop;
                        }
                        if app.screenshot.as_ref().is_some_and(|s| s.countdown.is_some()) {
                            app.redraw(Which::Screenshot);
                            TimeoutAction::ToDuration(Duration::from_millis(200))
                        } else {
                            TimeoutAction::Drop
                        }
                    },
                );
            }
        }
    }

    fn screenshot_pointer(&mut self, kind: &PointerEventKind, x: f64, y: f64) {
        let (x, y) = (x as f32, y as f32);
        let action = match self.screenshot.as_mut() {
            Some(tool) => match kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => tool.pointer_motion(x, y),
                PointerEventKind::Leave { .. } => {
                    tool.hover = None;
                    screenshot::Action::Redraw
                }
                PointerEventKind::Press { button: BTN_LEFT, .. } => tool.pointer_press(x, y),
                PointerEventKind::Release { button: BTN_LEFT, .. } => tool.pointer_release(),
                _ => screenshot::Action::None,
            },
            None => return,
        };
        self.handle_screenshot_action(action);
    }

    fn screenshot_key(&mut self, event: &KeyEvent) {
        let shift = self.mods.shift;
        let action = match self.screenshot.as_mut() {
            Some(tool) => tool.key_press(event.keysym, shift),
            None => return,
        };
        self.handle_screenshot_action(action);
    }

    // ---- switcher / launcher ---------------------------------------------

    fn show_switcher(&mut self, apps: Vec<String>, selected: usize) {
        let (w, h, alpha) = if let Some(sw) = self.switcher.as_mut() {
            sw.update(apps, selected);
            let (w, h) = sw.size();
            (w, h, sw.open_progress())
        } else {
            let sw = switcher::Switcher::new(apps, selected);
            let (w, h) = sw.size();
            let a = sw.open_progress();
            self.switcher = Some(sw);
            (w, h, a)
        };
        match self.switcher_surf.as_mut() {
            Some(s) => {
                if s.width != w || s.height != h {
                    s.layer.set_size(w, h);
                    s.layer.commit();
                } else {
                    self.redraw(Which::Switcher);
                }
            }
            None => {
                let surface = self.compositor.create_surface(&self.qh);
                let layer = self.layer_shell.create_layer_surface(
                    &self.qh,
                    surface,
                    Layer::Overlay,
                    Some("orchard-switcher"),
                    self.output.as_ref(),
                );
                layer.set_size(w, h);
                layer.set_keyboard_interactivity(KeyboardInteractivity::None);
                layer.commit();
                self.switcher_surf = Some(Surf::new(layer));
            }
        }
        self.set_backdrop_alpha("orchard-switcher", Some((0.0, 0.0, w as f32, h as f32, 22.0, alpha)));
    }

    /// Reload desktop entries and app icons if any application directory changed.
    pub(crate) fn refresh_apps(&mut self) -> bool {
        if !self.apps.refresh() {
            return false;
        }
        if let Some(lp) = self.launchpad.as_mut() {
            lp.refresh(&self.apps);
            self.redraw(Which::Launchpad);
        }
        if self.launcher.is_some() {
            self.launcher_changed();
        }
        let apps = &self.apps;
        self.dock.rebuild(apps, &self.windows);
        self.redraw(Which::Dock);
        self.send_dock_tiles();
        if let Some(w) = self.windows.iter().find(|w| w.focused) {
            let name = self
                .apps
                .for_app_id(&w.app_id)
                .map(|e| e.name.clone())
                .unwrap_or_else(|| if w.title.is_empty() { w.app_id.clone() } else { w.title.clone() });
            if self.menubar.app_name != name {
                self.menubar.app_name = name;
                self.redraw(Which::Bar);
            }
        }
        true
    }

    // ---- Launchpad --------------------------------------------------------

    fn toggle_launchpad(&mut self) {
        match self.launchpad.as_ref() {
            Some(l) if !l.closing => return self.close_launchpad(),
            Some(_) => {
                // reopen while it is still fading out
                self.refresh_apps();
                if let Some(l) = self.launchpad.as_mut() {
                    l.closing = false;
                }
                self.redraw(Which::Launchpad);
                return;
            }
            None => {}
        }
        if self.launcher.is_some() {
            self.close_launcher_now();
        }
        self.close_menu();
        self.refresh_apps();
        self.launchpad = Some(launchpad::Launchpad::new(&self.apps));
        self.launchpad_step = Instant::now();
        self.launchpad_backdrop = -1.0;
        let surface = self.compositor.create_surface(&self.qh);
        // Top layer, above windows and the menu bar; the compositor keeps the
        // Dock above it
        let layer = self.layer_shell.create_layer_surface(
            &self.qh,
            surface,
            Layer::Top,
            Some("orchard-launchpad"),
            self.output.as_ref(),
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        layer.commit();
        self.launchpad_surf = Some(Surf::new(layer));
        tracing::info!("launchpad opened");
    }

    /// Start the closing animation (the surface goes away when it ends).
    fn close_launchpad(&mut self) {
        if let Some(l) = self.launchpad.as_mut() {
            if !l.closing {
                l.closing = true;
                if let Some(s) = self.launchpad_surf.as_ref() {
                    // keyboard back to the apps right away
                    s.layer.set_keyboard_interactivity(KeyboardInteractivity::None);
                    s.layer.wl_surface().set_input_region(Region::new(&self.compositor).ok().as_ref().map(|r| r.wl_region()));
                    s.layer.commit();
                }
                self.redraw(Which::Launchpad);
            }
        }
    }

    fn drop_launchpad(&mut self) {
        self.launchpad = None;
        self.launchpad_surf = None;
        self.launchpad_backdrop = -1.0;
        self.set_backdrop("orchard-launchpad", None);
    }

    fn launchpad_launch(&mut self, result: usize) {
        let cmd = self
            .launchpad
            .as_ref()
            .and_then(|l| l.results.get(result))
            .and_then(|i| self.apps.entries.get(*i))
            .and_then(|e| e.command());
        self.close_launchpad();
        if let Some(cmd) = cmd {
            self.perform(Action::Launch(cmd));
        }
    }

    fn launchpad_size(&self) -> (f32, f32) {
        self.launchpad_surf
            .as_ref()
            .map(|s| (s.width as f32, s.height as f32))
            .unwrap_or((self.output_size.0 as f32, self.output_size.1 as f32))
    }

    fn launchpad_pointer(&mut self, kind: &PointerEventKind, x: f64, y: f64) {
        let (w, h) = self.launchpad_size();
        let Some(lp) = self.launchpad.as_mut() else { return };
        if lp.closing {
            return;
        }
        let (x, y) = (x as f32, y as f32);
        let hit = lp.hit(w, h, x, y);
        match kind {
            PointerEventKind::Motion { .. } | PointerEventKind::Enter { .. } => {
                let hov = match hit {
                    launchpad::Hit::App(i) => Some(i),
                    _ => None,
                };
                if hov != lp.hovered {
                    if hov.is_some() {
                        // the mouse takes over from the keyboard selection
                        lp.selected = None;
                    }
                    lp.hovered = hov;
                    self.redraw(Which::Launchpad);
                }
            }
            PointerEventKind::Leave { .. } => {
                if lp.hovered.take().is_some() {
                    self.redraw(Which::Launchpad);
                }
            }
            PointerEventKind::Press { button: BTN_LEFT, .. } => {
                lp.press(hit);
                self.redraw(Which::Launchpad);
            }
            PointerEventKind::Release { button: BTN_LEFT, .. } => {
                let clicked = lp.release(hit);
                self.redraw(Which::Launchpad);
                match clicked {
                    Some(launchpad::Hit::App(i)) => self.launchpad_launch(i),
                    Some(launchpad::Hit::Outside) => self.close_launchpad(),
                    _ => {}
                }
            }
            PointerEventKind::Axis { vertical, .. } => {
                let d = if vertical.absolute != 0.0 {
                    vertical.absolute as f32
                } else {
                    vertical.discrete as f32 * 48.0
                };
                if d != 0.0 {
                    lp.scroll_by(d);
                    lp.hovered = None;
                    self.redraw(Which::Launchpad);
                }
            }
            _ => {}
        }
    }

    fn launchpad_key(&mut self, event: KeyEvent) {
        let Some(lp) = self.launchpad.as_mut() else { return };
        match event.keysym {
            Keysym::Escape => {
                if lp.query.is_empty() {
                    self.close_launchpad();
                } else {
                    lp.set_query(&self.apps, String::new());
                }
            }
            Keysym::Return | Keysym::KP_Enter => {
                if let Some(i) = lp.selected.or(lp.hovered).or((!lp.query.is_empty() && !lp.results.is_empty()).then_some(0)) {
                    self.launchpad_launch(i);
                    return;
                }
            }
            Keysym::Left => lp.move_selection(-1, 0),
            Keysym::Right => lp.move_selection(1, 0),
            Keysym::Up => lp.move_selection(0, -1),
            Keysym::Down => lp.move_selection(0, 1),
            Keysym::Tab => lp.move_selection(1, 0),
            Keysym::Page_Down => lp.page(1),
            Keysym::Page_Up => lp.page(-1),
            Keysym::Home => lp.select_edge(false),
            Keysym::End => lp.select_edge(true),
            Keysym::BackSpace => {
                let mut q = lp.query.clone();
                q.pop();
                lp.set_query(&self.apps, q);
            }
            _ => {
                if let Some(text) = event.utf8.filter(|t| !t.chars().any(|c| c.is_control())) {
                    let q = format!("{}{}", lp.query, text);
                    lp.set_query(&self.apps, q);
                }
            }
        }
        self.redraw(Which::Launchpad);
    }

    // ---- menus ----------------------------------------------------------

    fn open_menu(&mut self, qh: &QueueHandle<Self>, owner: MenuOwner, mut menu: Menu) {
        menu.clamp(&self.fonts, self.output_size.0 as f32, self.output_size.1 as f32);
        if let MenuOwner::Bar(item) = &owner {
            self.menubar.open = Some(*item);
        }
        self.menu = Some(menu);
        self.dock.reveal = matches!(owner, MenuOwner::Dock(_));
        self.menu_owner = Some(owner);
        if self.menu_surf.is_none() {
            let surface = self.compositor.create_surface(qh);
            let layer = self.layer_shell.create_layer_surface(
                qh,
                surface,
                Layer::Overlay,
                Some("orchard-menu"),
                self.output.as_ref(),
            );
            layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
            layer.set_exclusive_zone(-1);
            layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
            layer.commit();
            self.menu_surf = Some(Surf::new(layer));
        } else {
            self.draw(Which::Menu);
        }
        self.send_menu_backdrop();
        self.redraw(Which::Bar);
    }

    fn send_menu_backdrop(&mut self) {
        let rects: Vec<serde_json::Value> = self
            .menu
            .as_ref()
            .map(|m| m.backdrop_rects(&self.fonts))
            .unwrap_or_default()
            .into_iter()
            .map(|(x, y, w, h, a)| json!({"x": x, "y": y, "w": w, "h": h, "radius": 8.0, "alpha": a}))
            .collect();
        if let Some(ipc) = self.ipc.as_mut() {
            ipc.request("set_backdrop", json!({"namespace": "orchard-menu", "rects": rects}));
        }
    }

    fn close_menu(&mut self) {
        self.menu = None;
        let was_dock = matches!(self.menu_owner, Some(MenuOwner::Dock(_)));
        self.menu_owner = None;
        if was_dock {
            self.dock.reveal = false;
            self.redraw(Which::Dock);
        }
        self.menubar.open = None;
        if let Some(s) = self.menu_surf.take() {
            drop(s);
        }
        self.send_menu_backdrop();
        self.redraw(Which::Bar);
    }

    fn bar_menu(&self, item: BarItem) -> Vec<MenuItem> {
        let de = self.german;
        let t = |en: &str, de_s: &str| if de { de_s.to_string() } else { en.to_string() };
        match item {
            BarItem::Logo => vec![
                MenuItem::new(
                    t("About This Device", "Über dieses Gerät"),
                    Action::Launch("orchard-settings --about-device".into()),
                ),
                MenuItem::separator(),
                MenuItem::new(t("System Settings…", "Systemeinstellungen …"), Action::Launch("orchard-settings".into())),
                MenuItem::separator(),
                MenuItem::new(t("Force Quit…", "Sofort beenden …"), Action::Launch(orchard_config::system_monitor_command()))
                    .with_shortcut("⌥⌘⎋"),
                MenuItem::separator(),
                MenuItem::new(t("Sleep", "Ruhezustand"), Action::Confirm(dialog::DialogKind::Sleep)),
                MenuItem::new(t("Restart…", "Neustart …"), Action::Confirm(dialog::DialogKind::Restart)),
                MenuItem::new(t("Shut Down…", "Ausschalten …"), Action::Confirm(dialog::DialogKind::ShutDown)),
                MenuItem::separator(),
                MenuItem::new(t("Lock Screen", "Bildschirm sperren"), Action::Wm("lock"))
                    .with_shortcut("⌃⌘Q"),
                MenuItem::new(t("Log Out…", "Abmelden …"), Action::Confirm(dialog::DialogKind::LogOut))
                    .with_shortcut("⌥⇧⌘Q"),
            ],
            BarItem::App => {
                let name = self.menubar.app_name.clone();
                match self.menubar.app_id.clone() {
                    Some(app_id) => vec![
                        MenuItem::disabled(format!("{} {name}", t("About", "Über"))),
                        MenuItem::separator(),
                        MenuItem::new(format!("{name} {}", t("hide", "ausblenden")), Action::HideApp(app_id.clone()))
                            .with_shortcut("⌘H"),
                        MenuItem::new(t("Hide Others", "Andere ausblenden"), Action::Wm("hide-others")).with_shortcut("⌥⌘H"),
                        MenuItem::separator(),
                        MenuItem::new(format!("{} {name}", t("Force Quit", "Sofort beenden:")), Action::Wm("force-quit"))
                            .with_shortcut("⌥⇧⌘⎋"),
                        MenuItem::new(format!("{name} {}", t("quit", "beenden")), Action::QuitApp(app_id))
                            .with_shortcut("⌘Q"),
                    ],
                    None => {
                        let term = orchard_config::resolve_terminal(&orchard_config::Settings::load().apps.terminal);
                        vec![
                            MenuItem::new(t("Open Files", "Dateien öffnen"), Action::Launch("xdg-open \"$HOME\"".into())),
                            MenuItem::new(t("Terminal", "Terminal"), Action::Launch(term)),
                        ]
                    }
                }
            }
            BarItem::Window => vec![
                MenuItem::new(t("Minimize", "Im Dock ablegen"), Action::Wm("minimize")).with_shortcut("⌘M"),
                MenuItem::new(t("Zoom", "Zoomen"), Action::Wm("zoom")).with_shortcut("⌃⌘F"),
                MenuItem::separator(),
                MenuItem::new(t("Cycle Through Windows", "Durch Fenster blättern"), Action::Wm("cycle-app"))
                    .with_shortcut("⌘^"),
                MenuItem::new(t("Close Window", "Fenster schließen"), Action::Wm("close")).with_shortcut("⌘W"),
                MenuItem::separator(),
                MenuItem::new(t("Restore Minimized", "Abgelegtes Fenster zurückholen"), Action::Wm("unminimize"))
                    .with_shortcut("⌥⌘M"),
            ],
            BarItem::Help => vec![MenuItem::new(
                t("Orchard Help", "Orchard-Hilfe"),
                Action::Launch("xdg-open https://github.com/Smithay/smithay".into()),
            )],
            BarItem::Control | BarItem::Volume | BarItem::Network => vec![],
            BarItem::Dev => self.menubar.dev.as_ref().map(|d| d.menu()).unwrap_or_default(),
            BarItem::AppMenu(i) => {
                let items = match (&self.menu_addr, self.app_menu_ids.get(i as usize), &self.gmenu) {
                    (Some(source), Some(id), Some(g)) => source.items(&g.conn, *id),
                    _ => Vec::new(),
                };
                if items.is_empty() {
                    vec![MenuItem::disabled(t("(empty)", "(leer)"))]
                } else {
                    items
                }
            }
            BarItem::Clock => vec![MenuItem::new(
                t("Open Calendar", "Kalender öffnen"),
                Action::Launch("gnome-calendar || thunderbird -calendar || evolution -c calendar".into()),
            )],
            BarItem::Tray(idx) => {
                let Some(item) = self.menubar.tray_items.get(idx) else {
                    return Vec::new();
                };
                if let (Some(menu_path), Some(g)) = (item.menu_path.as_deref(), self.gmenu.as_ref()) {
                    let items = g
                        .fetch(&item.service, menu_path, 0)
                        .map(|n| globalmenu::to_items(&n, &item.service, menu_path))
                        .unwrap_or_default();
                    if !items.is_empty() {
                        return items;
                    }
                }
                vec![MenuItem::disabled(item.title.clone())]
            }
        }
    }

    fn dock_menu(&self, idx: usize) -> Vec<MenuItem> {
        let de = self.german;
        let t = |en: &str, de_s: &str| if de { de_s.to_string() } else { en.to_string() };
        let Some(item) = self.dock.items.get(idx) else { return vec![] };
        match &item.kind {
            ItemKind::App { desktop_id, app_id, pinned } => {
                let mut v = Vec::new();
                for w in self.windows.iter().filter(|w| &w.app_id == app_id) {
                    let title = if w.title.is_empty() { item.name.clone() } else { w.title.clone() };
                    v.push(MenuItem::new(title, Action::ActivateWindow(w.id)));
                }
                if !v.is_empty() {
                    v.push(MenuItem::separator());
                }
                if let Some(id) = desktop_id {
                    if let Some(cmd) = self.apps.by_id(id).and_then(|e| e.command()) {
                        v.push(MenuItem::new(t("New Window", "Neues Fenster"), Action::Launch(cmd)));
                    }
                    v.push(MenuItem::new(
                        if *pinned { t("Remove from Dock", "Aus dem Dock entfernen") } else { t("Keep in Dock", "Im Dock behalten") },
                        Action::TogglePin(id.clone()),
                    ));
                }
                if item.running {
                    v.push(MenuItem::separator());
                    v.push(MenuItem::new(t("Quit", "Beenden"), Action::QuitApp(app_id.clone())));
                }
                v
            }
            ItemKind::Minimized { window_id, .. } => {
                vec![MenuItem::new(t("Restore", "Wiederherstellen"), Action::ActivateWindow(*window_id))]
            }
            ItemKind::Trash => vec![
                MenuItem::new(t("Open", "Öffnen"), Action::OpenTrash),
                MenuItem::new(t("Empty Trash", "Papierkorb entleeren"), Action::System("gio trash --empty".into())),
            ],
            ItemKind::Separator | ItemKind::Launchpad => vec![],
        }
    }

    fn perform(&mut self, action: Action) {
        tracing::info!("action {action:?}");
        match action {
            Action::Launch(cmd) | Action::System(cmd) => self.spawn(&cmd),
            Action::ActivateApp(app_id) => {
                if let Some(ipc) = self.ipc.as_mut() {
                    ipc.request("activate_app", json!({ "app_id": app_id }));
                }
            }
            Action::ActivateWindow(id) => {
                if let Some(ipc) = self.ipc.as_mut() {
                    ipc.request("activate", json!({ "id": id }));
                }
            }
            Action::QuitApp(app_id) => {
                if let Some(ipc) = self.ipc.as_mut() {
                    ipc.request("quit_app", json!({ "app_id": app_id }));
                }
            }
            Action::HideApp(app_id) => {
                // activate first so the compositor hides the right app
                if let Some(ipc) = self.ipc.as_mut() {
                    ipc.request("activate_app", json!({ "app_id": app_id }));
                    ipc.request("action", json!({ "name": "hide" }));
                }
            }
            Action::Wm(name) => {
                if let Some(ipc) = self.ipc.as_mut() {
                    ipc.request("action", json!({ "name": name }));
                }
            }
            Action::TogglePin(id) => {
                if let Some(pos) = self.dock.pinned.iter().position(|p| p == &id) {
                    self.dock.pinned.remove(pos);
                } else {
                    self.dock.pinned.push(id);
                }
                dock::save_pinned(&self.dock.pinned);
                let apps = &self.apps;
                self.dock.rebuild(apps, &self.windows);
                self.redraw(Which::Dock);
                self.send_dock_tiles();
            }
            Action::OpenTrash => self.spawn("gio open trash:/// || xdg-open trash:///"),
            Action::DbusMenu { service, path, id } => {
                if let Some(conn) = self.gmenu.as_ref().map(|g| g.conn.clone()) {
                    let _ = std::thread::Builder::new()
                        .name("menu-click".into())
                        .spawn(move || globalmenu::click(&conn, &service, &path, id));
                }
            }
            Action::GtkMenu { bus, act } => {
                if let Some(conn) = self.gmenu.as_ref().map(|g| g.conn.clone()) {
                    let _ = std::thread::Builder::new()
                        .name("menu-click".into())
                        .spawn(move || gtkmenu::activate(&conn, &bus, &act));
                }
            }
            Action::LogOut => {
                if let Some(ipc) = self.ipc.as_mut() {
                    ipc.request("action", json!({ "name": "quit" }));
                }
            }
            Action::Confirm(kind) => self.open_dialog(kind),
            Action::DesktopOpen(path) => self.open_desktop_path(&path),
            Action::DesktopTrash(path) => {
                // moving a big folder to the trash on another filesystem copies
                // it: never block the menu bar and Dock on that. The icon goes
                // away now; the Desktop folder poll picks up the real result.
                let p = path.clone();
                let failed = self.desktop.force_rescan.clone();
                let _ = std::thread::Builder::new().name("desktop-trash".into()).spawn(move || {
                    match std::process::Command::new("gio").arg("trash").arg(&p).status() {
                        Ok(st) if st.success() => {}
                        other => {
                            tracing::warn!("trash {}: {other:?}", p.display());
                            // bring the icon back
                            failed.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                });
                self.desktop.entries.retain(|e| e.path != path);
                self.desktop.selected = None;
                self.redraw(Which::Desktop);
            }
            Action::DesktopNewFolder => {
                let base = if self.german { "Neuer Ordner" } else { "New Folder" };
                let mut cand = self.desktop.dir.join(base);
                let mut n = 2u32;
                while cand.exists() {
                    cand = self.desktop.dir.join(format!("{base} {n}"));
                    n += 1;
                }
                let _ = std::fs::create_dir_all(&cand);
                self.desktop.rescan();
                self.redraw(Which::Desktop);
            }
            Action::DesktopSort(sort) => {
                self.desktop.set_sort(sort);
                self.redraw(Which::Desktop);
            }
            Action::CopyText(text) => {
                let _ = std::thread::Builder::new().name("wl-copy".into()).spawn(move || {
                    use std::io::Write;
                    if let Ok(mut child) = std::process::Command::new("wl-copy")
                        .stdin(std::process::Stdio::piped())
                        .spawn()
                    {
                        if let Some(mut stdin) = child.stdin.take() {
                            let _ = stdin.write_all(text.as_bytes());
                        }
                        let _ = child.wait();
                    }
                });
            }
        }
    }

    /// Keep the new Dock order, or stop keeping an app dragged out of it.
    fn apply_dock_drag(&mut self, outcome: Option<dock::DragOutcome>) {
        match outcome {
            Some(dock::DragOutcome::Pinned(order)) => {
                tracing::info!(?order, "dock: new order");
                self.dock.pinned = order;
            }
            Some(dock::DragOutcome::Remove(id)) => {
                tracing::info!(%id, "dock: removed");
                self.dock.pinned.retain(|p| p != &id);
            }
            None => {}
        }
        dock::save_pinned(&self.dock.pinned);
        let apps = &self.apps;
        self.dock.rebuild(apps, &self.windows);
        self.redraw(Which::Dock);
        self.send_dock_tiles();
    }

    fn dock_click(&mut self, idx: usize) {
        let Some(item) = self.dock.items.get(idx).cloned() else { return };
        match item.kind {
            ItemKind::App { desktop_id, app_id, .. } => {
                if item.running {
                    self.perform(Action::ActivateApp(app_id));
                } else if let Some(cmd) = desktop_id.as_deref().and_then(|id| self.apps.by_id(id)).and_then(|e| e.command()) {
                    self.dock.items[idx].bounce_start = Some(Instant::now());
                    self.perform(Action::Launch(cmd));
                    self.redraw(Which::Dock);
                }
            }
            ItemKind::Minimized { window_id, .. } => self.perform(Action::ActivateWindow(window_id)),
            ItemKind::Trash => self.perform(Action::OpenTrash),
            ItemKind::Launchpad => return self.toggle_launchpad(),
            ItemKind::Separator => {}
        }
        // using the Dock leaves Launchpad
        self.close_launchpad();
    }

    // ---- pointer --------------------------------------------------------

    fn bar_pointer(&mut self, qh: &QueueHandle<Self>, kind: &PointerEventKind, x: f64) {
        let width = self.bar.as_ref().map(|s| s.width).unwrap_or(0) as f32;
        let hit = self.menubar.hit(&self.fonts, x as f32, width);
        if let PointerEventKind::Press { button: BTN_RIGHT, .. } = kind {
            if let Some(BarItem::Tray(idx)) = hit {
                self.close_panel();
                let open_menu = self
                    .menubar
                    .tray_items
                    .get(idx)
                    .map(|i| i.menu_path.is_some())
                    .unwrap_or(false);
                if open_menu {
                    let tx = self
                        .menubar
                        .tray_ranges(&self.fonts, width)
                        .into_iter()
                        .find(|(i, _, _)| *i == idx)
                        .map(|(_, a, _)| a)
                        .unwrap_or(x as f32);
                    let items = self.bar_menu(BarItem::Tray(idx));
                    if !items.is_empty() {
                        self.open_menu(qh, MenuOwner::Bar(BarItem::Tray(idx)), Menu::new(items, tx, menubar::HEIGHT + 2.0));
                        return;
                    }
                }
                if let (Some(item), Some(tray)) = (self.menubar.tray_items.get(idx), self.tray.as_ref()) {
                    tray.context_menu(item.service.clone(), item.path.clone(), x as i32, menubar::HEIGHT as i32);
                }
                return;
            }
        }
        if let PointerEventKind::Press { button: BTN_LEFT, .. } = kind {
            match hit {
                Some(BarItem::Clock) => return self.toggle_panel(BarItem::Clock, panel::Kind::Notifications, None),
                Some(BarItem::Control) => return self.toggle_panel(BarItem::Control, panel::Kind::Control, None),
                Some(BarItem::Volume) => {
                    return self.toggle_panel(BarItem::Volume, panel::Kind::Control, Some(panel::Submenu::Sound))
                }
                Some(BarItem::Network) => {
                    let sub = match self.menubar.network {
                        status::Network::Wired => {
                            if self.control_cache.wifi_connected() {
                                panel::Submenu::Wifi
                            } else {
                                panel::Submenu::Ethernet
                            }
                        }
                        status::Network::Wifi(_) => panel::Submenu::Wifi,
                        status::Network::Offline => {
                            if self.control_cache.show_ethernet() {
                                panel::Submenu::Ethernet
                            } else {
                                panel::Submenu::Wifi
                            }
                        }
                    };
                    return self.toggle_panel(BarItem::Network, panel::Kind::Control, Some(sub));
                }
                Some(BarItem::Tray(idx)) => {
                    self.close_panel();
                    let Some(item) = self.menubar.tray_items.get(idx).cloned() else { return };
                    if item.item_is_menu && item.menu_path.is_some() {
                        let tx = self
                            .menubar
                            .tray_ranges(&self.fonts, width)
                            .into_iter()
                            .find(|(i, _, _)| *i == idx)
                            .map(|(_, a, _)| a)
                            .unwrap_or(x as f32);
                        let items = self.bar_menu(BarItem::Tray(idx));
                        if !items.is_empty() {
                            self.open_menu(qh, MenuOwner::Bar(BarItem::Tray(idx)), Menu::new(items, tx, menubar::HEIGHT + 2.0));
                            return;
                        }
                    }
                    if let Some(tray) = self.tray.as_ref() {
                        tray.activate(item.service, item.path, x as i32, menubar::HEIGHT as i32);
                    }
                    return;
                }
                Some(_) => self.close_panel(),
                None => {}
            }
            if let Some(item) = hit {
                let x = self
                    .menubar
                    .left_items(&self.fonts)
                    .into_iter()
                    .find(|(i, _, _)| *i == item)
                    .map(|(_, a, _)| a)
                    .or_else(|| match item {
                        BarItem::Dev => self.menubar.dev_range(&self.fonts, width).map(|(a, _)| a),
                        _ => None,
                    })
                    .unwrap_or_else(|| self.menubar.clock_range(&self.fonts, width).0);
                let items = self.bar_menu(item);
                self.open_menu(qh, MenuOwner::Bar(item), Menu::new(items, x, menubar::HEIGHT + 2.0));
            }
        }
    }

    fn dock_pointer(&mut self, qh: &QueueHandle<Self>, kind: &PointerEventKind, x: f64, y: f64) {
        let (sw, sh) = self
            .dock_surf
            .as_ref()
            .map(|s| (s.width as f32, s.height as f32))
            .unwrap_or((0.0, 0.0));
        match kind {
            PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                // magnification follows the position along the Dock
                let prev_hovered = self.dock.hovered;
                self.dock.hover_x = Some(self.dock.unmap_point(x as f32, y as f32, sw, sh).0);
                if self.dock.drag_motion(x as f32, y as f32, sw, sh) {
                    self.dock.hovered = None;
                    self.redraw(Which::Dock);
                    return;
                }
                self.dock.hovered = self.dock.hit(x as f32, y as f32, sw, sh);
                if self.dock.cfg.magnification
                    || self.dock.hovered != prev_hovered
                    || matches!(kind, PointerEventKind::Enter { .. })
                {
                    self.redraw(Which::Dock);
                }
            }
            PointerEventKind::Leave { .. } => {
                self.dock.hover_x = None;
                self.dock.hovered = None;
                self.dock.pressed = None;
                // a drag interrupted without a release: put everything back
                if self.dock.drag.take().map(|d| d.active).unwrap_or(false) {
                    let apps = &self.apps;
                    self.dock.rebuild(apps, &self.windows);
                }
                self.redraw(Which::Dock);
            }
            PointerEventKind::Press { button, .. } => {
                let hit = self.dock.hit(x as f32, y as f32, sw, sh);
                if *button == BTN_LEFT {
                    self.dock.pressed = hit;
                    match hit {
                        Some(i) => self.dock.press_for_drag(i, x as f32, y as f32, sw, sh),
                        None => self.dock.drag = None,
                    }
                    self.redraw(Which::Dock);
                } else if *button == BTN_RIGHT {
                    if let Some(i) = hit {
                        let items = self.dock_menu(i);
                        if !items.is_empty() {
                            let (placed, _) = self.dock.layout(sw, sh);
                            let (tx, ty, tw, th) = self.dock.tile_rect(&placed[i], sw, sh);
                            let (ox, oy) = self.dock_origin();
                            let (tx, ty) = (ox + tx, oy + ty);
                            let (mw, mh) = Menu::new(items.clone(), 0.0, 0.0).size(&self.fonts);
                            // above the icon, or beside it for a side Dock
                            let (mx, my) = match self.dock.edge() {
                                DockPosition::Bottom => (tx + tw / 2.0 - mw / 2.0, ty - mh - 10.0),
                                DockPosition::Left => (tx + tw + 10.0, ty + th / 2.0 - mh / 2.0),
                                DockPosition::Right => (tx - mw - 10.0, ty + th / 2.0 - mh / 2.0),
                            };
                            let menu = Menu::new(items, mx, my);
                            self.open_menu(qh, MenuOwner::Dock(i), menu);
                        }
                    }
                }
            }
            PointerEventKind::Release { button: BTN_LEFT, .. } => {
                if self.dock.drag.as_ref().map(|d| d.active).unwrap_or(false) {
                    let outcome = self.dock.finish_drag(sw, sh);
                    self.dock.pressed = None;
                    self.apply_dock_drag(outcome);
                    // the pointer may have been released outside the Dock
                    let inside = self.dock.hit(x as f32, y as f32, sw, sh).is_some();
                    if !inside {
                        self.dock.hover_x = None;
                        self.dock.hovered = None;
                    }
                    self.redraw(Which::Dock);
                    return;
                }
                self.dock.drag = None;
                let hit = self.dock.hit(x as f32, y as f32, sw, sh);
                let pressed = self.dock.pressed.take();
                if let (Some(a), Some(b)) = (pressed, hit) {
                    if a == b {
                        self.dock_click(a);
                    }
                }
                self.redraw(Which::Dock);
            }
            _ => {}
        }
    }

    fn menu_pointer(&mut self, qh: &QueueHandle<Self>, kind: &PointerEventKind, x: f64, y: f64) {
        let (x, y) = (x as f32, y as f32);
        // moving along the menu bar switches menus, like macOS
        if y < menubar::HEIGHT && matches!(self.menu_owner, Some(MenuOwner::Bar(_))) {
            let width = self.output_size.0 as f32;
            if let Some(item) = self.menubar.hit(&self.fonts, x, width) {
                if !matches!(
                    item,
                    BarItem::Clock | BarItem::Control | BarItem::Volume | BarItem::Network | BarItem::Dev
                )
                    && Some(MenuOwner::Bar(item)) != self.menu_owner
                {
                    if let PointerEventKind::Motion { .. } = kind {
                        let mx = self
                            .menubar
                            .left_items(&self.fonts)
                            .into_iter()
                            .find(|(i, _, _)| *i == item)
                            .map(|(_, a, _)| a)
                            .unwrap_or_else(|| self.menubar.clock_range(&self.fonts, width).0);
                        let items = self.bar_menu(item);
                        self.open_menu(qh, MenuOwner::Bar(item), Menu::new(items, mx, menubar::HEIGHT + 2.0));
                        return;
                    }
                }
            }
        }
        let (sw, sh) = (self.output_size.0 as f32, self.output_size.1 as f32);
        let Some(menu) = self.menu.as_mut() else { return };
        match kind {
            PointerEventKind::Motion { .. } | PointerEventKind::Enter { .. } => {
                if menu.pointer_moved(&self.fonts, x, y, sw, sh) {
                    self.redraw(Which::Menu);
                    self.send_menu_backdrop();
                }
            }
            PointerEventKind::Press { .. } => {
                if !menu.contains_any(&self.fonts, x, y) {
                    // click on the bar item that owns the menu toggles it closed
                    self.close_menu();
                }
            }
            PointerEventKind::Release { .. } => {
                if let Some(a) = menu.action_at(&self.fonts, x, y) {
                    self.close_menu();
                    self.perform(a);
                }
            }
            _ => {}
        }
    }
}

thread_local! {
    static PENDING_FRAMES: std::cell::RefCell<Vec<Which>> = const { std::cell::RefCell::new(Vec::new()) };
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &wl_surface::WlSurface, factor: i32) {
        if let Some(which) = self.which(surface) {
            if let Some(s) = self.surf(which) {
                s.scale = factor.max(1);
                let _ = s.layer.set_buffer_scale(s.scale as u32);
            }
            self.draw(which);
        }
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, surface: &wl_surface::WlSurface, _: u32) {
        if let Some(which) = self.which(surface) {
            let dirty = match self.surf(which) {
                Some(s) => {
                    s.frame_pending = false;
                    s.frame_since = None;
                    s.dirty
                }
                None => false,
            };
            if dirty {
                self.draw(which);
            }
            self.flush_frames(qh);
        }
    }
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl App {
    /// Issue wl_surface.frame for surfaces that asked for animation frames.
    fn flush_frames(&mut self, qh: &QueueHandle<Self>) {
        let pending: Vec<Which> = PENDING_FRAMES.with(|p| std::mem::take(&mut *p.borrow_mut()));
        for which in pending {
            if let Some(s) = self.surf(which) {
                let surface = s.layer.wl_surface().clone();
                surface.frame(qh, surface.clone());
                surface.commit();
            }
        }
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        if self.output.as_ref() == Some(&output) {
            if let Some(info) = self.output_state.info(&output) {
                if let Some((w, h)) = info.logical_size {
                    self.output_size = (w, h);
                }
                self.output_name = info.name.clone().unwrap_or_default();
            }
            self.send_dock_tiles();
        } else if self.output.is_some() {
            // the primary display may just have been plugged in
            self.retarget_output();
        }
    }
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        if self.output.as_ref() == Some(&output) {
            self.drop_surfaces();
            self.retarget_output_skipping(Some(&output));
        }
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        match self.which(layer.wl_surface()) {
            Some(Which::Menu) => self.menu_surf = None,
            Some(Which::Switcher) => self.switcher_surf = None,
            Some(Which::Launcher) => self.close_launcher_now(),
            Some(Which::Osd) => {
                self.osd = None;
                self.osd_surf = None;
                self.set_backdrop("orchard-osd", None);
            }
            Some(Which::Notify) => self.notes_surf = None,
            Some(Which::Panel) => self.close_panel(),
            Some(Which::Launchpad) => self.drop_launchpad(),
            Some(Which::Screenshot) => self.close_screenshot_tool(),
            Some(Which::Dialog) => self.close_dialog(),
            Some(Which::Polkit) => self.cancel_polkit(),
            Some(Which::Desktop) => self.desktop_surf = None,
            // the compositor closes layers of an output that went away
            Some(Which::Bar | Which::Dock) => {
                self.drop_surfaces();
                self.retarget_output();
            }
            _ => {}
        }
    }

    fn configure(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let Some(which) = self.which(layer.wl_surface()) else { return };
        let out = self.output_size;
        if let Some(s) = self.surf(which) {
            s.width = if configure.new_size.0 == 0 { out.0.max(1) as u32 } else { configure.new_size.0 };
            s.height = if configure.new_size.1 == 0 { out.1.max(1) as u32 } else { configure.new_size.1 };
            s.configured = true;
            s.frame_pending = false;
        }
        if which == Which::Menu {
            self.output_size.0 = self.output_size.0.max(self.menu_surf.as_ref().map(|s| s.width as i32).unwrap_or(0));
            self.output_size.1 = self.output_size.1.max(self.menu_surf.as_ref().map(|s| s.height as i32).unwrap_or(0));
        }
        self.draw(which);
        if which == Which::Dock {
            self.send_dock_tiles();
        }
        self.flush_frames(qh);
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            let repeat: smithay_client_toolkit::seat::keyboard::repeat::RepeatCallback<App> = Box::new(|app: &mut App, _, event| {
                // only text editing repeats; Escape or Return must not
                if app.launcher_active() && !matches!(event.keysym, Keysym::Escape | Keysym::Return | Keysym::KP_Enter) {
                    app.launcher_key(&event);
                    app.flush_draws();
                }
            });
            self.keyboard = self
                .seat_state
                .get_keyboard_with_repeat(qh, &seat, None, self.loop_handle.clone(), repeat)
                .ok();
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Pointer {
            if let Some(p) = self.pointer.take() {
                p.release();
            }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for App {
    fn pointer_frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for ev in events {
            let Some(which) = self.which(&ev.surface) else { continue };
            let (x, y) = ev.position;
            self.pointer_pos = (x, y);
            match ev.kind {
                PointerEventKind::Enter { .. } => self.pointer_on = Some(which),
                PointerEventKind::Leave { .. } => {
                    if self.pointer_on == Some(which) {
                        self.pointer_on = None;
                    }
                }
                _ => {}
            }
            match which {
                Which::Bar => self.bar_pointer(qh, &ev.kind, x),
                Which::Dock => self.dock_pointer(qh, &ev.kind, x, y),
                Which::Menu => self.menu_pointer(qh, &ev.kind, x, y),
                Which::Switcher | Which::Osd => {}
                Which::Launcher => self.launcher_pointer(&ev.kind, x, y),
                Which::Notify => self.notes_pointer(&ev.kind, x, y),
                Which::Panel => self.panel_pointer(&ev.kind, x, y),
                Which::Launchpad => self.launchpad_pointer(&ev.kind, x, y),
                Which::Screenshot => self.screenshot_pointer(&ev.kind, x, y),
                Which::Dialog => self.dialog_pointer(&ev.kind, x, y),
                Which::Polkit => self.polkit_pointer(&ev.kind, x, y),
                Which::Desktop => self.desktop_pointer(qh, &ev.kind, x, y),
            }
        }
        self.flush_frames(qh);
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
delegate_pointer!(App);
delegate_keyboard!(App);

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
        if self.polkit.is_some() {
            self.polkit_key(&event);
            return;
        }
        if self.dialog.is_some() {
            self.dialog_key(&event);
            return;
        }
        if self.screenshot.is_some() {
            self.screenshot_key(&event);
            return;
        }
        if self.launchpad.as_ref().map(|l| !l.closing).unwrap_or(false) && !self.launcher_active() {
            self.launchpad_key(event);
            return;
        }
        if self.launcher_active() {
            self.launcher_key(&event);
            return;
        }
        if event.keysym == Keysym::Escape && self.menu.is_some() {
            self.close_menu();
        }
        if event.keysym == Keysym::Escape && self.panel.is_some() {
            if self.panel.as_ref().is_some_and(|p| p.submenu.is_some() && !p.standalone) {
                if let Some(p) = self.panel.as_mut() {
                    p.close_submenu();
                }
                self.redraw(Which::Panel);
            } else {
                self.close_panel();
            }
        }
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
        if caps_changed && self.polkit.is_some() {
            self.redraw(Which::Polkit);
        }
    }
}
delegate_layer!(App);
delegate_registry!(App);

fn low_battery_note(id: u32, summary: String, body: String, icon: &str, critical: bool) -> notify::Incoming {
    notify::Incoming {
        id,
        app_name: "Orchard".into(),
        app_icon: icon.into(),
        summary,
        body,
        actions: Vec::new(),
        desktop_entry: None,
        image: None,
        image_path: None,
        critical,
        resident: false,
        timeout_ms: None,
    }
}
