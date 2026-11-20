//! orchard-shell: the menu bar and Dock for the Orchard compositor.
//!
//! Pure Rust client: layer-shell surfaces drawn on the CPU with tiny-skia.
//! Talks to the compositor over Orchard's JSON IPC for the window list,
//! activation and frosted-glass backdrops.

mod apps;
mod dock;
mod draw;
mod globalmenu;
mod ipc;
mod launcher;
mod launchpad;
mod menu;
mod hud;
mod menubar;
mod notify;
mod osd;
mod panel;
mod panel_app;
mod status;
mod switcher;
mod text;
mod theme;

use orchard_config::DockPosition;
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
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum MenuOwner {
    Bar(BarItem),
    Dock(usize),
}

struct App {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    pool: SlotPool,
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
    control_tx: Sender<panel::ControlState>,
    control_cache: panel::ControlState,
    last_slider_cmd: Instant,
    vol_tx: Sender<Option<(f32, bool)>>,
    vol_pending: Arc<AtomicBool>,
    status_tick: u32,

    /// global app menus
    gmenu: Option<globalmenu::GlobalMenu>,
    /// menu address of the focused window and the ids of its top-level menus
    menu_addr: Option<(String, String)>,
    app_menu_ids: Vec<i32>,
    gmenu_tx: Sender<(Option<(String, String)>, Vec<(i32, String)>)>,
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
    let pool = SlotPool::new(1920 * 200 * 4, &shm).expect("shm pool");

    let settings = orchard_config::Settings::load();
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
    let gmenu = globalmenu::GlobalMenu::start();
    let (gmenu_tx, gmenu_rx) = channel::channel::<(Option<(String, String)>, Vec<(i32, String)>)>();
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

    let mut app = App {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        layer_shell,
        shm,
        pool,
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
        dbus,
        panel: None,
        panel_surf: None,
        launchpad_surf: None,
        launchpad: None,
        launchpad_step: Instant::now(),
        launchpad_backdrop: -1.0,
        control_tx,
        control_cache: Default::default(),
        last_slider_cmd: Instant::now(),
        vol_tx,
        vol_pending: Arc::new(AtomicBool::new(false)),
        status_tick: 0,
        gmenu,
        menu_addr: None,
        app_menu_ids: Vec::new(),
        gmenu_tx,
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

    // status refresh (clock, battery, network, volume)
    loop_handle
        .insert_source(Timer::from_duration(Duration::from_secs(1)), |_, _, app: &mut App| {
            if app.menubar.refresh_status() {
                app.redraw(Which::Bar);
            }
            app.status_tick = app.status_tick.wrapping_add(1);
            if app.menubar.cfg.show_volume && app.status_tick.is_multiple_of(2) {
                app.refresh_volume();
            }
            // X11 apps register their menu some time after mapping
            app.refresh_app_menu();
            TimeoutAction::ToDuration(Duration::from_secs(1))
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
        if let Err(e) = event_loop.dispatch(Duration::from_millis(500), &mut app) {
            tracing::info!("compositor connection closed: {e}");
            return;
        }
        app.flush_draws();
    }
    let _ = qh;
}

impl App {
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
            self.close_launcher();
        }
        self.switcher = None;
        self.switcher_surf = None;
        self.osd = None;
        self.osd_surf = None;
        self.panel = None;
        self.panel_surf = None;
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
                    Some("settings_changed") => self.reload_settings(),
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
        tracing::info!("settings changed");
        if self.primary != s.displays.primary {
            self.primary = s.displays.primary.clone();
            self.retarget_output();
        }
        self.theme = Theme::from_settings(&s);
        let icons_changed = self.apps.set_icon_theme(s.appearance.icon_theme.clone());
        let was_showing_volume = self.menubar.cfg.show_volume;
        self.menubar.cfg = s.menu_bar.clone();
        self.menubar.refresh_status();
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
        self.dock.cfg = s.dock.clone();
        if !self.dock.cfg.autohide {
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
        self.last_backdrop = None;
        self.last_tiles.clear();
        self.redraw(Which::Bar);
        self.redraw(Which::Dock);
        self.send_dock_tiles();
        if self.menu.is_some() {
            self.redraw(Which::Menu);
        }
        if icons_changed {
            self.redraw_icon_surfaces();
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
    fn focused_menu_address(&self) -> Option<(String, String)> {
        let w = self.windows.iter().find(|w| w.focused)?;
        if let Some(m) = &w.menu {
            return Some((m.service.clone(), m.path.clone()));
        }
        let id = w.x11_id?;
        self.gmenu.as_ref()?.x11_menu(id)
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
            let tops = addr
                .as_ref()
                .and_then(|(s, p)| globalmenu::fetch(&conn, s, p, 0))
                .map(|root| {
                    root.children
                        .into_iter()
                        .filter(|c| c.visible && !c.separator && !c.label.is_empty())
                        .map(|c| (c.id, c.label))
                        .collect()
                })
                .unwrap_or_default();
            let _ = tx.send((addr, tops));
        });
    }

    fn on_app_menu(&mut self, addr: Option<(String, String)>, tops: Vec<(i32, String)>) {
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
        let msg = json!({"output": self.output_name, "edge": edge, "tiles": tiles});
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

    fn redraw(&mut self, which: Which) {
        if let Some(s) = self.surf(which) {
            s.dirty = true;
        }
    }

    /// Paint every dirty surface that is not waiting for a frame callback.
    fn flush_draws(&mut self) {
        const ALL: [Which; 9] = [
            Which::Launchpad,
            Which::Bar,
            Which::Dock,
            Which::Menu,
            Which::Switcher,
            Which::Launcher,
            Which::Osd,
            Which::Notify,
            Which::Panel,
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
        let (w, h, scale) = match self.surf(which) {
            Some(s) if s.configured => {
                s.dirty = false;
                (s.width, s.height, s.scale)
            }
            _ => return,
        };
        let pw = w as i32 * scale;
        let ph = h as i32 * scale;
        let Some(mut pm) = Pixmap::new(pw as u32, ph as u32) else { return };
        let fscale = scale as f32;

        let mut animating = false;
        match which {
            Which::Bar => self.menubar.paint(&mut pm, fscale, &self.fonts, &self.theme),
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
                }
            }
            Which::Switcher => {
                if let Some(sw) = &self.switcher {
                    sw.paint(&mut pm, fscale, &self.apps, &self.fonts, &self.theme);
                }
            }
            Which::Launcher => {
                if let Some(l) = &self.launcher {
                    let (x, y) = self.launcher_origin();
                    l.paint(&mut pm, x, y, fscale, &self.apps, &self.fonts, &self.theme, self.german);
                }
            }
            Which::Osd => {
                if let Some(o) = &self.osd {
                    o.paint(&mut pm, fscale, &self.theme);
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
                if let Some(p) = self.panel.as_mut() {
                    p.paint(&mut pm, fscale, w as f32, h as f32, &self.notes, &self.apps, &self.fonts, &self.theme, self.german);
                    p.remember_cards();
                    animating = p.is_animating();
                }
                self.send_panel_backdrop();
            }
        }

        let (buffer, canvas) = match self.pool.create_buffer(pw, ph, pw * 4, wl_shm::Format::Argb8888) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("shm buffer: {e}");
                return;
            }
        };
        draw::blit_to_shm(&pm, canvas);
        let qh = self.qh.clone();
        let Some(s) = self.surf(which) else { return };
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
                if hidden {
                    // auto-hidden: a thin strip at the screen edge brings it back
                    let strip = dock::REVEAL_STRIP as i32;
                    match self.dock.edge() {
                        DockPosition::Bottom => region.add(0, h - strip, w, strip),
                        DockPosition::Left => region.add(0, 0, strip, h),
                        DockPosition::Right => region.add(w - strip, 0, strip, h),
                    }
                } else {
                    // the bar plus room for magnified icons on the inner side
                    let grow = (self.dock.icon_max() - self.dock.icon()) as i32 + 4;
                    let (x, y, bw, bh) = (bg.0 as i32, bg.1 as i32, bg.2 as i32, bg.3 as i32);
                    match self.dock.edge() {
                        DockPosition::Bottom => region.add(x, y - grow, bw, bh + grow),
                        DockPosition::Left => region.add(x, y, bw + grow, bh),
                        DockPosition::Right => region.add(x - grow, y, bw + grow, bh),
                    }
                }
                s.layer.wl_surface().set_input_region(Some(region.wl_region()));
            }
        }
    }

    fn set_backdrop(&mut self, namespace: &str, rect: Option<(f32, f32, f32, f32, f32)>) {
        if let Some(ipc) = self.ipc.as_mut() {
            let rects: Vec<serde_json::Value> = rect
                .map(|(x, y, w, h, r)| vec![json!({"x": x, "y": y, "w": w, "h": h, "radius": r})])
                .unwrap_or_default();
            ipc.request("set_backdrop", json!({"namespace": namespace, "rects": rects}));
        }
    }

    // ---- switcher / launcher ---------------------------------------------

    fn show_switcher(&mut self, apps: Vec<String>, selected: usize) {
        let sw = switcher::Switcher { apps, selected };
        let (w, h) = sw.size();
        self.switcher = Some(sw);
        match self.switcher_surf.as_mut() {
            Some(s) => {
                if s.width != w || s.height != h {
                    s.layer.set_size(w, h);
                    s.layer.commit();
                } else {
                    self.draw(Which::Switcher);
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
        self.set_backdrop("orchard-switcher", Some((0.0, 0.0, w as f32, h as f32, 22.0)));
    }

    fn launcher_origin(&self) -> (f32, f32) {
        let (w, h) = self
            .launcher_surf
            .as_ref()
            .map(|s| (s.width as f32, s.height as f32))
            .unwrap_or((self.output_size.0 as f32, self.output_size.1 as f32));
        ((w - launcher::WIDTH) / 2.0, h * 0.22)
    }

    fn toggle_launcher(&mut self) {
        if self.launcher.is_some() {
            self.close_launcher();
            return;
        }
        self.launcher = Some(launcher::Launcher::default());
        let surface = self.compositor.create_surface(&self.qh);
        let layer = self.layer_shell.create_layer_surface(
            &self.qh,
            surface,
            Layer::Overlay,
            Some("orchard-launcher"),
            self.output.as_ref(),
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        layer.commit();
        self.launcher_surf = Some(Surf::new(layer));
    }

    fn close_launcher(&mut self) {
        self.launcher = None;
        self.launcher_surf = None;
        self.set_backdrop("orchard-launcher", None);
    }

    fn launcher_changed(&mut self) {
        if let Some(l) = self.launcher.as_mut() {
            l.update(&self.apps);
        }
        let (x, y) = self.launcher_origin();
        let h = self.launcher.as_ref().map(|l| l.height()).unwrap_or(0.0);
        self.set_backdrop("orchard-launcher", Some((x, y, launcher::WIDTH, h, 14.0)));
        self.redraw(Which::Launcher);
    }

    fn launch_selected(&mut self) {
        let cmd = self
            .launcher
            .as_ref()
            .and_then(|l| l.selected_entry(&self.apps))
            .and_then(|e| e.command());
        self.close_launcher();
        if let Some(cmd) = cmd {
            self.perform(Action::Launch(cmd));
        }
    }

    // ---- Launchpad --------------------------------------------------------

    fn toggle_launchpad(&mut self) {
        match self.launchpad.as_ref() {
            Some(l) if !l.closing => return self.close_launchpad(),
            Some(_) => {
                // reopen while it is still fading out
                if let Some(l) = self.launchpad.as_mut() {
                    l.closing = false;
                }
                self.redraw(Which::Launchpad);
                return;
            }
            None => {}
        }
        if self.launcher.is_some() {
            self.close_launcher();
        }
        self.close_menu();
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
            .map(|m| m.rects(&self.fonts))
            .unwrap_or_default()
            .into_iter()
            .map(|(x, y, w, h)| json!({"x": x, "y": y, "w": w, "h": h, "radius": 8.0}))
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
                    Action::Launch("orchard-settings --about-device || kinfocenter".into()),
                ),
                MenuItem::separator(),
                MenuItem::new(t("System Settings…", "Systemeinstellungen …"), Action::Launch("orchard-settings || systemsettings".into())),
                MenuItem::separator(),
                MenuItem::new(t("Force Quit…", "Sofort beenden …"), Action::Launch("plasma-systemmonitor || ksysguard".into()))
                    .with_shortcut("⌥⌘⎋"),
                MenuItem::separator(),
                MenuItem::new(t("Sleep", "Ruhezustand"), Action::System("systemctl suspend".into())),
                MenuItem::new(t("Restart…", "Neustart …"), Action::System("systemctl reboot".into())),
                MenuItem::new(t("Shut Down…", "Ausschalten …"), Action::System("systemctl poweroff".into())),
                MenuItem::separator(),
                MenuItem::new(t("Lock Screen", "Bildschirm sperren"), Action::System("${ORCHARD_LOCKER:-swaylock -f -c 1e1e1e}".into()))
                    .with_shortcut("⌃⌘Q"),
                MenuItem::new(t("Log Out", "Abmelden"), Action::LogOut).with_shortcut("⌥⇧⌘Q"),
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
                    None => vec![
                        MenuItem::new(t("Open Files", "Dateien öffnen"), Action::Launch("dolphin".into())),
                        MenuItem::new(t("Terminal", "Terminal"), Action::Launch("konsole".into())),
                    ],
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
            BarItem::AppMenu(i) => {
                let items = match (&self.menu_addr, self.app_menu_ids.get(i as usize), &self.gmenu) {
                    (Some((service, path)), Some(id), Some(g)) => {
                        g.fetch(service, path, *id).map(|n| globalmenu::to_items(&n, service, path)).unwrap_or_default()
                    }
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
                Action::Launch("merkuro-calendar || korganizer || gnome-calendar".into()),
            )],
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
            Action::OpenTrash => self.spawn("dolphin trash:/ || xdg-open trash:/"),
            Action::DbusMenu { service, path, id } => {
                if let Some(conn) = self.gmenu.as_ref().map(|g| g.conn.clone()) {
                    let _ = std::thread::Builder::new()
                        .name("menu-click".into())
                        .spawn(move || globalmenu::click(&conn, &service, &path, id));
                }
            }
            Action::LogOut => {
                if let Some(ipc) = self.ipc.as_mut() {
                    ipc.request("action", json!({ "name": "quit" }));
                }
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
        if let PointerEventKind::Press { button: BTN_LEFT, .. } = kind {
            match hit {
                Some(BarItem::Clock) => return self.toggle_panel(BarItem::Clock, panel::Kind::Notifications, None),
                Some(BarItem::Control) => return self.toggle_panel(BarItem::Control, panel::Kind::Control, None),
                Some(BarItem::Volume) => {
                    return self.toggle_panel(BarItem::Volume, panel::Kind::Control, Some(panel::Submenu::Sound))
                }
                Some(BarItem::Network) => {
                    return self.toggle_panel(BarItem::Network, panel::Kind::Control, Some(panel::Submenu::Wifi))
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
                self.dock.hover_x = Some(self.dock.unmap_point(x as f32, y as f32, sw, sh).0);
                if self.dock.drag_motion(x as f32, y as f32, sw, sh) {
                    self.dock.hovered = None;
                    self.redraw(Which::Dock);
                    return;
                }
                self.dock.hovered = self.dock.hit(x as f32, y as f32, sw, sh);
                self.redraw(Which::Dock);
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
                if !matches!(item, BarItem::Clock | BarItem::Control | BarItem::Volume | BarItem::Network)
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
            Some(Which::Launcher) => self.close_launcher(),
            Some(Which::Osd) => {
                self.osd = None;
                self.osd_surf = None;
            }
            Some(Which::Notify) => self.notes_surf = None,
            Some(Which::Panel) => self.close_panel(),
            Some(Which::Launchpad) => self.drop_launchpad(),
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
            self.keyboard = self.seat_state.get_keyboard(qh, &seat, None).ok();
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

impl App {
    fn launcher_pointer(&mut self, kind: &PointerEventKind, x: f64, y: f64) {
        let Some(l) = self.launcher.as_ref() else { return };
        let (ox, oy) = self.launcher_origin();
        let (lx, ly) = (x as f32 - ox, y as f32 - oy);
        let inside = lx >= 0.0 && lx <= launcher::WIDTH && ly >= 0.0 && ly <= l.height();
        match kind {
            PointerEventKind::Motion { .. } => {
                if let Some(row) = l.row_at(ly).filter(|_| inside) {
                    if row != l.selected {
                        self.launcher.as_mut().unwrap().selected = row;
                        self.redraw(Which::Launcher);
                    }
                }
            }
            PointerEventKind::Press { .. } => {
                if !inside {
                    self.close_launcher();
                }
            }
            PointerEventKind::Release { .. } => {
                if inside && l.row_at(ly).is_some() {
                    self.launch_selected();
                }
            }
            _ => {}
        }
    }
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
        if self.launchpad.as_ref().map(|l| !l.closing).unwrap_or(false) && self.launcher.is_none() {
            self.launchpad_key(event);
            return;
        }
        if self.launcher.is_none() {
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
            return;
        }
        match event.keysym {
            Keysym::Escape => self.close_launcher(),
            Keysym::Return | Keysym::KP_Enter => self.launch_selected(),
            Keysym::Down => {
                if let Some(l) = self.launcher.as_mut() {
                    if l.selected + 1 < l.results.len() {
                        l.selected += 1;
                    }
                }
                self.redraw(Which::Launcher);
            }
            Keysym::Up => {
                if let Some(l) = self.launcher.as_mut() {
                    l.selected = l.selected.saturating_sub(1);
                }
                self.redraw(Which::Launcher);
            }
            Keysym::BackSpace => {
                if let Some(l) = self.launcher.as_mut() {
                    l.query.pop();
                }
                self.launcher_changed();
            }
            _ => {
                if let Some(text) = event.utf8.filter(|t| !t.chars().any(|c| c.is_control())) {
                    if let Some(l) = self.launcher.as_mut() {
                        l.query.push_str(&text);
                    }
                    self.launcher_changed();
                }
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
        _: Modifiers,
        _: u32,
    ) {
    }
}
delegate_layer!(App);
delegate_registry!(App);
