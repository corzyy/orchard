//! Notification Center (click the clock) and Control Center (the switches icon
//! in the menu bar). Both live on one full-screen, transparent overlay surface,
//! so a click anywhere outside the panel closes it, like on macOS.

use std::time::{Duration, Instant};

use orchard_config::{ControlCenterSettings, ControlItem, ControlKind, ControlSize, Settings};
use tiny_skia::{FillRule, LineCap, LineJoin, PathBuilder, Pixmap, PixmapPaint, Stroke, Transform};

use crate::{
    anim::{
        self, ease_in_cubic, ease_out_cubic, lerp, lerp_rgba, scale_alpha, tap_scale, SpringFloat, SpringParams,
    },
    apps::Apps,
    draw::{composite, fill_circle, fill_rounded, paint, stroke_rounded},
    notify::{self, CardHit, Center, Hit},
    status::Network,
    text::{Fonts, Rgba, Weight},
    theme::Theme,
};

pub const NC_WIDTH: f32 = 356.0;
/// Width of the iOS-style Control Center grid: four 75 px columns, 10 px
/// gutters and 16 px side padding.
pub const CC_WIDTH: f32 = 362.0;
const MARGIN: f32 = 10.0;
const SLIDE_NC_OPEN: Duration = Duration::from_millis(240);
const SLIDE_NC_CLOSE: Duration = Duration::from_millis(175);
const CC_OPEN: Duration = Duration::from_millis(420);
const CC_CLOSE: Duration = Duration::from_millis(180);
const SUBMENU_ANIM: Duration = Duration::from_millis(210);
const PAGE_ANIM: Duration = Duration::from_millis(260);

const CC_COLS: usize = 4;
/// The panel uses generous side margins and compact controls, like the iOS
/// Control Center grid: 4 × 63 px cells, 14 px gutters, 34 px outer padding.
const CC_PAD: f32 = 34.0;
const CC_GAP: f32 = 14.0;
const CC_CELL: f32 = 63.0;
const CC_TOP: f32 = 20.0;
const CC_BOTTOM: f32 = 16.0;
const CC_FOOTER: f32 = 36.0;
const CC_RADIUS: f32 = 30.0;
const MODULE_RADIUS: f32 = 24.0;
/// Inner padding shared by 2 × 2 modules.
const MODULE_PAD: f32 = 14.0;
const GROUP_GAP: f32 = 8.0;
const GALLERY_HEADER: f32 = 34.0;
const PAGE_DOT: f32 = 3.5;

/// iOS-style per-module entrance delay while the panel opens.
const STAGGER_STEP: f32 = 0.018;
const STAGGER_LEN: f32 = 0.26;

pub const PULSE_WIFI: usize = 0;
pub const PULSE_BT: usize = 1;
pub const PULSE_DND: usize = 2;
pub const PULSE_NIGHT: usize = 3;
pub const PULSE_GAME: usize = 4;

struct PanelAnim {
    initialized: bool,
    dnd: SpringFloat,
    night_shift: SpringFloat,
    game_mode: SpringFloat,
    wifi: SpringFloat,
    ethernet: SpringFloat,
    bluetooth: SpringFloat,
    wifi_others: SpringFloat,
    panel_h: SpringFloat,
    panel_h_init: bool,
    vol: SpringFloat,
    displays: Vec<SpringFloat>,
    streams: Vec<SpringFloat>,
    pulses: [Option<Instant>; 5],
}

impl PanelAnim {
    fn new() -> Self {
        Self {
            initialized: false,
            dnd: SpringFloat::new(0.0, SpringParams::SWITCH),
            night_shift: SpringFloat::new(0.0, SpringParams::SWITCH),
            game_mode: SpringFloat::new(0.0, SpringParams::SWITCH),
            wifi: SpringFloat::new(0.0, SpringParams::SWITCH),
            ethernet: SpringFloat::new(0.0, SpringParams::SWITCH),
            bluetooth: SpringFloat::new(0.0, SpringParams::SWITCH),
            wifi_others: SpringFloat::new(1.0, SpringParams::CHEVRON),
            panel_h: SpringFloat::new(0.0, SpringParams::PANEL_HEIGHT),
            panel_h_init: false,
            vol: SpringFloat::new(0.0, SpringParams::SLIDER),
            displays: Vec::new(),
            streams: Vec::new(),
            pulses: [None; 5],
        }
    }

    fn is_animating(&self, now: Instant) -> bool {
        self.dnd.is_animating(now)
            || self.night_shift.is_animating(now)
            || self.game_mode.is_animating(now)
            || self.wifi.is_animating(now)
            || self.ethernet.is_animating(now)
            || self.bluetooth.is_animating(now)
            || self.wifi_others.is_animating(now)
            || self.panel_h.is_animating(now)
            || self.vol.is_animating(now)
            || self.displays.iter().any(|d| d.is_animating(now))
            || self.streams.iter().any(|st| st.is_animating(now))
            || self.pulses.iter().any(|p| tap_scale(*p, now).1)
    }
}

/// One control placed on the four-column grid (logical panel coordinates,
/// relative to the top-left of the page content).
#[derive(Debug, Clone, Copy, PartialEq)]
struct PlacedModule {
    kind: ControlKind,
    /// Index in the page's `Vec<ControlItem>`.
    item_index: usize,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

/// A control picked up in edit mode.
#[derive(Debug, Clone)]
struct ModuleDrag {
    kind: ControlKind,
    pos: (f32, f32),
}

/// iOS 18-style Control Center state: pages, edit mode, drag reordering and
/// the cached packed grid. Everything here is cheap to carry between frames.
struct ControlCenterUi {
    pages: Vec<Vec<ControlItem>>,
    page: usize,
    prev_page: usize,
    page_since: Instant,
    page_dir: i8,
    edit: bool,
    edit_since: Instant,
    gallery: bool,
    drag: Option<ModuleDrag>,
    placed: Vec<PlacedModule>,
    layout_dirty: bool,
    /// Surface position of the page content (top-left), from the last paint.
    content_origin: (f32, f32),
    /// Canvas → surface offset while painting a page into an offscreen buffer.
    hover_offset: (f32, f32),
    scroll: SpringFloat,
    scroll_target: f32,
    /// Panel height in logical px for the current view, from the last paint.
    height: f32,
    /// Last surface height seen by a paint, for snapping mode changes.
    screen_h: f32,
    /// Last painted panel rect (surface coords), for partial redraws.
    last_rect: (f32, f32, f32, f32),
}

impl ControlCenterUi {
    fn new(pages: Vec<Vec<ControlItem>>) -> Self {
        let now = Instant::now();
        Self {
            pages,
            page: 0,
            prev_page: 0,
            page_since: now - PAGE_ANIM,
            page_dir: 0,
            edit: false,
            edit_since: now,
            gallery: false,
            drag: None,
            placed: Vec::with_capacity(16),
            layout_dirty: true,
            content_origin: (0.0, 0.0),
            hover_offset: (0.0, 0.0),
            scroll: SpringFloat::new(0.0, SpringParams::SLIDER),
            scroll_target: 0.0,
            height: 0.0,
            screen_h: 800.0,
            last_rect: (0.0, 0.0, 0.0, 0.0),
        }
    }

    fn page_items(&self) -> &[ControlItem] {
        self.pages.get(self.page).map(Vec::as_slice).unwrap_or(&[])
    }

    fn page_transition(&self, now: Instant) -> f32 {
        if anim::reduced() || self.page_dir == 0 {
            return 1.0;
        }
        ease_out_cubic((now.saturating_duration_since(self.page_since).as_secs_f32() / PAGE_ANIM.as_secs_f32()).clamp(0.0, 1.0))
    }

    fn in_transition(&self, now: Instant) -> bool {
        self.page_dir != 0 && !anim::reduced() && now.saturating_duration_since(self.page_since) < PAGE_ANIM
    }

}

/// Pack controls into the four-column grid in list order: first free slot,
/// row by row. `Large` controls therefore get a 2 × 2 block, wide ones a 2 × 1
/// run, and so on, exactly like iOS Control Center.
fn pack_page(items: &[ControlItem]) -> Vec<PlacedModule> {
    let mut occupied: Vec<[bool; CC_COLS]> = Vec::new();
    let mut out: Vec<PlacedModule> = Vec::with_capacity(items.len());
    for (item_index, item) in items.iter().enumerate() {
        let (sw, sh) = {
            let (w, h) = item.size.span();
            (w.min(CC_COLS), h.max(1))
        };
        let mut placed = None;
        let mut row = 0usize;
        'search: loop {
            if row + sh > occupied.len() {
                occupied.resize(row + sh, [false; CC_COLS]);
            }
            for col in 0..=(CC_COLS - sw) {
                let free = (row..row + sh).all(|r| (col..col + sw).all(|c| !occupied[r][c]));
                if free {
                    for r in row..row + sh {
                        for c in col..col + sw {
                            occupied[r][c] = true;
                        }
                    }
                    placed = Some((col, row));
                    break 'search;
                }
            }
            row += 1;
            if row > 512 {
                // Defensive: a malformed size can never loop forever.
                break;
            }
        }
        let Some((col, row)) = placed else { continue };
        out.push(PlacedModule {
            kind: item.kind,
            item_index,
            x: CC_PAD + col as f32 * (CC_CELL + CC_GAP),
            y: row as f32 * (CC_CELL + CC_GAP),
            w: sw as f32 * CC_CELL + (sw.saturating_sub(1)) as f32 * CC_GAP,
            h: sh as f32 * CC_CELL + (sh.saturating_sub(1)) as f32 * CC_GAP,
        });
    }
    out
}

fn content_rows(placed: &[PlacedModule]) -> usize {
    placed
        .iter()
        .map(|p| ((p.y + p.h) / (CC_CELL + CC_GAP)).ceil() as usize)
        .max()
        .unwrap_or(0)
}

fn content_height(placed: &[PlacedModule]) -> f32 {
    let rows = content_rows(placed);
    if rows == 0 {
        0.0
    } else {
        rows as f32 * CC_CELL + (rows - 1) as f32 * CC_GAP
    }
}

/// Semantic colours for the Control Center modules, pre-faded for the
/// staggered entrance so no per-shape alpha maths is needed later.
#[derive(Clone, Copy)]
struct CcLook {
    dark: bool,
    text: Rgba,
    sub: Rgba,
    faint: Rgba,
    card: Rgba,
    card_hover: Rgba,
    border: Rgba,
    off: Rgba,
    accent: Rgba,
}

impl CcLook {
    fn new(theme: &Theme, alpha: f32) -> Self {
        let alpha = alpha.clamp(0.0, 1.0);
        let dark = theme.bar_text[0] > 128;
        let f = |c: Rgba| scale_alpha(c, alpha);
        if dark {
            Self {
                dark: true,
                text: f([245, 245, 247, 250]),
                sub: f([235, 235, 240, 155]),
                faint: f([235, 235, 240, 90]),
                card: f([255, 255, 255, 30]),
                card_hover: f([255, 255, 255, 46]),
                border: f([255, 255, 255, 18]),
                off: f([120, 120, 128, 125]),
                accent: f(theme.menu_selection),
            }
        } else {
            Self {
                dark: false,
                text: f([20, 20, 22, 255]),
                sub: f([60, 60, 67, 145]),
                faint: f([60, 60, 67, 85]),
                card: f([255, 255, 255, 178]),
                card_hover: f([255, 255, 255, 232]),
                border: f([0, 0, 0, 20]),
                off: f([106, 106, 113, 150]),
                accent: f(theme.menu_selection),
            }
        }
    }
}

fn next_module_size(kind: ControlKind, size: ControlSize) -> ControlSize {
    match kind {
        ControlKind::Connectivity | ControlKind::NowPlaying => match size {
            ControlSize::Large => ControlSize::Wide,
            _ => ControlSize::Large,
        },
        ControlKind::Brightness | ControlKind::Volume => ControlSize::Tall,
        _ => match size {
            ControlSize::Small => ControlSize::Wide,
            _ => ControlSize::Small,
        },
    }
}

fn module_name(kind: ControlKind, german: bool) -> &'static str {
    match kind {
        ControlKind::Connectivity => {
            if german {
                "Konnektivität"
            } else {
                "Connectivity"
            }
        }
        ControlKind::Wifi => "Wi-Fi",
        ControlKind::Bluetooth => "Bluetooth",
        ControlKind::NowPlaying => {
            if german {
                "Wiedergabe"
            } else {
                "Now Playing"
            }
        }
        ControlKind::Focus => {
            if german {
                "Fokus"
            } else {
                "Focus"
            }
        }
        ControlKind::NightShift => "Night Shift",
        ControlKind::GameMode => {
            if german {
                "Spielmodus"
            } else {
                "Game Mode"
            }
        }
        ControlKind::Brightness => {
            if german {
                "Helligkeit"
            } else {
                "Brightness"
            }
        }
        ControlKind::Volume => {
            if german {
                "Lautstärke"
            } else {
                "Volume"
            }
        }
        ControlKind::Network => {
            if german {
                "Netzwerk"
            } else {
                "Network"
            }
        }
        ControlKind::Sound => {
            if german {
                "Ton"
            } else {
                "Sound"
            }
        }
        ControlKind::Displays => "Displays",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Notifications,
    Control,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Submenu {
    Wifi,
    Ethernet,
    Bluetooth,
    Sound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NetRow {
    Wifi,
    Ethernet,
    Bluetooth,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EthernetDevice {
    /// Kernel interface name (e.g. "enp5s0")
    pub device: String,
    /// Active NetworkManager connection profile name, if connected
    pub connection: Option<String>,
    /// IPv4 address without CIDR prefix, if connected
    pub ip: Option<String>,
    pub connected: bool,
    /// False when cable is unplugged (`unavailable`)
    pub plugged: bool,
    /// True when NetworkManager can connect/disconnect this interface
    pub managed: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct WifiNetwork {
    pub ssid: String,
    /// 0..=100
    pub signal: u8,
    pub secure: bool,
    pub active: bool,
    pub known: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BtDevice {
    pub mac: String,
    pub name: String,
    pub connected: bool,
    pub icon: String,
    pub battery: Option<u8>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AudioIcon {
    #[default]
    Speaker,
    Headphones,
    Bluetooth,
    Display,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioSink {
    pub name: String,
    pub desc: String,
    pub active: bool,
    pub icon: AudioIcon,
}

/// What the Control Center shows (queried on a helper thread).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ControlState {
    /// Wi-Fi radio on, connected network
    pub wifi: Option<(bool, Option<String>)>,
    pub wifi_networks: Vec<WifiNetwork>,
    pub ethernet: Vec<EthernetDevice>,
    pub bluetooth: Option<bool>,
    pub bt_devices: Vec<BtDevice>,
    /// 0..1, muted
    pub volume: Option<(f32, bool)>,
    pub audio_sinks: Vec<AudioSink>,
    /// apps playing sound (per-app volume)
    pub streams: Vec<AppStream>,
    /// Controllable displays (built-in backlight + DDC/CI external monitors)
    pub displays: Vec<crate::ddc::DisplayBrightness>,
    pub media: Option<Media>,
}

impl ControlState {
    pub fn wifi_connected(&self) -> bool {
        self.wifi
            .as_ref()
            .and_then(|(on, ssid)| on.then_some(ssid.as_ref()).flatten())
            .is_some()
    }

    pub fn ethernet_connected(&self) -> bool {
        self.ethernet.iter().any(|e| e.connected)
    }

    /// True when Quick Settings should display Ethernet instead of Wi-Fi:
    /// connected only via Ethernet, or Ethernet hardware is present without Wi-Fi.
    pub fn show_ethernet(&self) -> bool {
        if self.ethernet_connected() && !self.wifi_connected() {
            true
        } else {
            self.wifi.is_none() && !self.ethernet.is_empty()
        }
    }
}

/// Which parts of a [`ControlState`] were actually read on this pass. A helper
/// daemon that is briefly busy (or a `timeout` killed command) leaves its bit
/// unset, and the previous value is kept instead of the tile flickering away.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ControlFresh {
    pub wifi: bool,
    pub wifi_networks: bool,
    pub ethernet: bool,
    pub bluetooth: bool,
    pub volume: bool,
    pub audio_sinks: bool,
    pub displays: bool,
}

/// A Control Center job for the background worker. Queries and actions are
/// processed one at a time so a slow state read can never race an action and
/// paint a stale switch back over it.
pub struct ControlRequest {
    /// Monotonic request id; results older than the newest applied one are dropped.
    pub seq: u64,
    pub target: Option<Target>,
    pub value: Option<f32>,
    /// State the UI knows at request time (used to resolve what an action toggles).
    pub current: ControlState,
}

/// A finished Control Center job, on its way back to the UI thread.
pub struct ControlUpdate {
    pub seq: u64,
    pub state: ControlState,
    pub fresh: ControlFresh,
}

impl ControlUpdate {
    /// Fill fields that could not be read this round from the last known state,
    /// so a busy NetworkManager/BlueZ/PipeWire never makes a tile disappear.
    pub fn merged(self, prev: &ControlState) -> ControlState {
        let mut st = self.state;
        if !self.fresh.ethernet {
            st.ethernet = prev.ethernet.clone();
        }
        if !self.fresh.wifi {
            st.wifi = prev.wifi.clone();
        }
        if !self.fresh.wifi_networks {
            st.wifi_networks = prev.wifi_networks.clone();
            // keep showing the SSID we are on when only the scan failed
            if matches!(st.wifi, Some((true, None))) {
                if let Some((true, Some(ssid))) = &prev.wifi {
                    st.wifi = Some((true, Some(ssid.clone())));
                }
            }
        }
        if !self.fresh.bluetooth {
            st.bluetooth = prev.bluetooth;
            st.bt_devices = prev.bt_devices.clone();
        }
        if !self.fresh.volume {
            st.volume = prev.volume;
        }
        if !self.fresh.audio_sinks {
            st.audio_sinks = prev.audio_sinks.clone();
        }
        if !self.fresh.displays {
            st.displays = prev.displays.clone();
        }
        st
    }
}

/// Run Control Center jobs on one helper thread, in order.
///
/// Serialising them means a full state query always finishes before the
/// next action starts, so the state that reaches the UI reflects the last
/// click instead of whichever thread happened to finish last. Queued plain
/// refreshes are collapsed: only the newest one is worth running.
pub fn spawn_control_worker(requests: std::sync::mpsc::Receiver<ControlRequest>, send: impl Fn(ControlUpdate) + Send + 'static) {
    let _ = std::thread::Builder::new().name("control-center".into()).spawn(move || {
        let mut pending: std::collections::VecDeque<ControlRequest> = std::collections::VecDeque::new();
        loop {
            if pending.is_empty() {
                match requests.recv() {
                    Ok(r) => pending.push_back(r),
                    Err(_) => return,
                }
            }
            while let Ok(r) = requests.try_recv() {
                pending.push_back(r);
            }

            // Actions keep their order; when only refreshes are left, the newest
            // one supersedes the older snapshots still waiting in the queue.
            let req = match pending.iter().position(|r| r.target.is_some()) {
                Some(i) => pending.remove(i).expect("index from position"),
                None => pending.pop_back().expect("queue not empty"),
            };
            // Anything older than a request that has now been handled is stale.
            pending.retain(|r| r.seq > req.seq);

            let (state, fresh) = match req.target.as_ref() {
                Some(t) => run_control(t, req.value, &req.current),
                None => query_control_fresh(),
            };
            send(ControlUpdate { seq: req.seq, state, fresh });
        }
    });
}

/// One app's playback streams (a browser's tabs share one row).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppStream {
    pub name: String,
    /// icon name or binary, for the app's icon
    pub icon: String,
    /// pactl sink-input indices
    pub indices: Vec<u32>,
    /// 0..1+ (loudest stream)
    pub volume: f32,
    /// every stream muted
    pub muted: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Media {
    pub title: String,
    pub artist: String,
    pub playing: bool,
}

/// Interactive parts of the panel (panel-surface coordinates).
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Card(Hit),
    ClearAll,
    Dnd,
    NightShift,
    GameMode,
    WifiToggle,
    EthernetToggle,
    BluetoothToggle,
    OpenSubmenu(Submenu),
    SubmenuBack,
    WifiConnect {
        ssid: String,
        known: bool,
        secure: bool,
    },
    WifiToggleOthers,
    EthToggleDevice {
        device: String,
        connected: bool,
    },
    BtToggleDevice {
        mac: String,
        connected: bool,
    },
    AudioSelectSink(String),
    OpenSettings(&'static str),
    /// slider: x range of the track
    Volume(f32, f32),
    /// vertical slider: `(top_y, bottom_y)`
    VolumeVertical(f32, f32),
    VolumeMute,
    /// `(display_index, min_x, max_x)`
    Brightness(usize, f32, f32),
    /// `(display_index, top_y, bottom_y)`
    BrightnessVertical(usize, f32, f32),
    /// per-app volume: `(stream_index, min_x, max_x)`
    AppVolume(usize, f32, f32),
    AppMute(usize),
    MediaPrev,
    MediaPlay,
    MediaNext,
    /// Control Center page switched from a dot.
    Page(usize),
    /// Edit-mode toolbar.
    ToggleEdit,
    OpenGallery,
    CloseGallery,
    AddModule(ControlKind),
    RemoveModule(ControlKind),
    ResizeModule(ControlKind),
    GrabModule(ControlKind),
    PageAdd,
    PageRemove,
    /// inside the panel, nothing interactive
    Panel,
}

pub struct Panel {
    pub kind: Kind,
    pub opened: Instant,
    /// Set while the panel is playing its exit animation before surface teardown.
    pub closing: Option<Instant>,
    pub submenu: Option<Submenu>,
    /// True when returning from a submenu back to the main Control Center view.
    submenu_returning: bool,
    /// True when opened directly from a menu bar icon (Sound / Network) rather than via Control Center.
    pub standalone: bool,
    /// True while a standalone Network submenu should align itself to the first fresh `ControlState`.
    pub auto_network_submenu: bool,
    pub submenu_since: Instant,
    pub wifi_others_open: bool,
    /// SSID, MAC, or Ethernet interface currently connecting / toggling
    pub busy_item: Option<String>,
    pub scroll: f32,
    pub hover: Option<(f32, f32)>,
    /// x of the right edge of the Control Center icon (Control Center anchors there)
    pub anchor_right: f32,
    pub control: ControlState,
    /// Live menu-bar network state, used before the first async query completes.
    pub network_hint: Network,
    /// slider being dragged
    pub dragging: Option<Target>,
    /// Night Shift on right now (schedule or manual switch)
    pub night_shift: bool,
    /// Game Mode switches on by itself for full-screen games
    pub game_mode_enabled: bool,
    /// the game Game Mode is on for
    pub game_active: Option<String>,
    anim: PanelAnim,
    /// iOS-style grid, edit mode and page state.
    cc: ControlCenterUi,
    /// Reused offscreen buffer for the panel's scale/fade opening animation.
    cc_buf: Option<Pixmap>,
    /// Reused buffer for clipped/scrolled/paging page content.
    body_buf: Option<Pixmap>,
    // filled by paint, used for hit tests
    targets: Vec<((f32, f32, f32, f32), Target)>,
    cards: Vec<CardHit>,
    panel_rect: (f32, f32, f32, f32),
}

impl Panel {
    pub fn new(kind: Kind, anchor_right: f32) -> Panel {
        let now = Instant::now();
        Panel {
            kind,
            opened: now,
            closing: None,
            submenu: None,
            submenu_returning: false,
            standalone: false,
            auto_network_submenu: false,
            submenu_since: now - SUBMENU_ANIM,
            wifi_others_open: true,
            busy_item: None,
            scroll: 0.0,
            hover: None,
            anchor_right,
            control: ControlState::default(),
            network_hint: Network::Offline,
            dragging: None,
            night_shift: false,
            game_mode_enabled: true,
            game_active: None,
            anim: PanelAnim::new(),
            cc: ControlCenterUi::new(ControlCenterSettings::default().pages),
            cc_buf: None,
            body_buf: None,
            targets: Vec::new(),
            cards: Vec::new(),
            panel_rect: (0.0, 0.0, 0.0, 0.0),
        }
    }

    /// Load the user's Control Center layout. Kept separate from [`Panel::new`]
    /// so unit tests and the default UI stay deterministic.
    pub fn apply_settings(&mut self, settings: &Settings) {
        let mut pages = settings.control_center.sanitized().pages;
        if pages.is_empty() {
            pages = ControlCenterSettings::default().pages;
        }
        // Brightness and volume are fixed iOS-style vertical sliders.
        for page in &mut pages {
            for item in page.iter_mut() {
                if matches!(item.kind, ControlKind::Brightness | ControlKind::Volume) {
                    item.size = ControlSize::Tall;
                }
            }
        }
        if pages.is_empty() {
            pages.push(Vec::new());
        }
        if pages != self.cc.pages {
            self.cc.pages = pages;
            self.cc.page = self.cc.page.min(self.cc.pages.len().saturating_sub(1));
            self.cc.layout_dirty = true;
        }
    }

    /// Triggers a subtle macOS tap pulse on one of the circular toggle icons.
    pub fn pulse(&mut self, slot: usize) {
        if let Some(p) = self.anim.pulses.get_mut(slot) {
            *p = Some(Instant::now());
        }
    }

    pub fn start_close(&mut self) {
        if self.closing.is_none() {
            self.closing = Some(Instant::now());
            self.dragging = None;
            self.hover = None;
        }
    }

    pub fn is_closed(&self) -> bool {
        let Some(c) = self.closing else { return false };
        if anim::reduced() {
            return true;
        }
        let dur = match self.kind {
            Kind::Notifications => SLIDE_NC_CLOSE,
            Kind::Control => CC_CLOSE,
        };
        c.elapsed() >= dur
    }

    pub fn open_submenu(&mut self, sub: Submenu) {
        self.submenu = Some(sub);
        self.submenu_returning = false;
        self.submenu_since = Instant::now();
        self.scroll = 0.0;
        self.dragging = None;
        if sub == Submenu::Wifi {
            // If user already has known networks, start with Other Networks expanded if not connected
            let connected = self.control.wifi.as_ref().and_then(|(_, s)| s.as_ref()).is_some();
            let has_known = self.control.wifi_networks.iter().any(|n| n.known || n.active);
            self.wifi_others_open = !connected || !has_known;
            self.anim.wifi_others.snap(if self.wifi_others_open { 1.0 } else { 0.0 });
        }
    }

    pub fn close_submenu(&mut self) {
        self.submenu = None;
        self.submenu_returning = true;
        self.submenu_since = Instant::now();
        self.scroll = 0.0;
        self.dragging = None;
    }

    // ---- Control Center layout / edit mode ----------------------------------

    fn ensure_layout(&mut self) {
        if !self.cc.layout_dirty {
            return;
        }
        self.cc.placed = if self.cc.gallery {
            let items: Vec<ControlItem> = ControlKind::ALL
                .into_iter()
                .map(|k| ControlItem::new(k, ControlSize::Small))
                .collect();
            pack_page(&items)
        } else {
            pack_page(self.cc.page_items())
        };
        self.cc.layout_dirty = false;
    }

    /// Height of the current page / gallery, clamped to the space available
    /// below the menu bar.
    fn cc_height(&mut self, screen_h: f32) -> f32 {
        self.ensure_layout();
        let content = if self.cc.gallery {
            let rows = content_rows(&self.cc.placed);
            if rows == 0 {
                96.0
            } else {
                rows as f32 * CC_CELL + (rows - 1).max(0) as f32 * CC_GAP + GALLERY_HEADER + 20.0
            }
        } else if self.cc.placed.is_empty() {
            132.0
        } else {
            content_height(&self.cc.placed)
        };
        let max_content =
            (screen_h - 6.0 - MARGIN - CC_TOP - CC_FOOTER - CC_BOTTOM).max(CC_CELL + 12.0);
        let body = content.min(max_content);
        self.cc.height = CC_TOP + body + CC_FOOTER + CC_BOTTOM;
        self.cc.height
    }

    fn body_height(&self) -> f32 {
        (self.cc.height - CC_TOP - CC_FOOTER - CC_BOTTOM).max(0.0)
    }

    fn update_scroll_target(&mut self) {
        let content = if self.cc.gallery {
            let rows = content_rows(&self.cc.placed);
            (rows as f32 * CC_CELL + (rows - 1).max(0) as f32 * CC_GAP + GALLERY_HEADER + 20.0).max(0.0)
        } else if self.cc.placed.is_empty() {
            132.0
        } else {
            content_height(&self.cc.placed)
        };
        let max = (content - self.body_height()).max(0.0);
        self.cc.scroll_target = self.cc.scroll_target.clamp(0.0, max);
    }

    pub fn cc_toggle_edit(&mut self) {
        self.cc.edit = !self.cc.edit;
        self.cc.edit_since = Instant::now();
        self.cc.gallery = false;
        self.cc.drag = None;
        self.cc.layout_dirty = true;
    }

    pub fn cc_open_gallery(&mut self) {
        self.cc.gallery = true;
        self.cc.edit = true;
        self.cc.drag = None;
        self.cc.scroll_target = 0.0;
        self.cc.scroll.snap(0.0);
        self.cc.layout_dirty = true;
        // A mode switch, not a content morph: make the sheet large enough on
        // the very first frame instead of letting rows clip while it grows.
        let target = self.cc_height(self.cc.screen_h);
        self.anim.panel_h.snap(target);
        self.anim.panel_h_init = true;
    }

    pub fn cc_close_gallery(&mut self) {
        self.cc.gallery = false;
        self.cc.layout_dirty = true;
        self.cc.scroll_target = 0.0;
        self.cc.scroll.snap(0.0);
    }

    pub fn cc_add_module(&mut self, kind: ControlKind) {
        // A control lives on exactly one page. Adding it from the gallery of
        // another page moves it here instead of silently doing nothing.
        for page in &mut self.cc.pages {
            page.retain(|c| c.kind != kind);
        }
        if self.cc.pages.is_empty() {
            self.cc.pages.push(Vec::new());
        }
        let page = self.cc.page.min(self.cc.pages.len() - 1);
        self.cc.page = page;
        self.cc.pages[page].push(ControlItem::with_default_size(kind));
        self.cc_close_gallery();
        self.cc.layout_dirty = true;
        self.save_layout();
    }

    pub fn cc_remove_module(&mut self, kind: ControlKind) {
        // Controls are unique across pages, so a remove from the gallery or
        // an edit badge removes it wherever it lives.
        for page in &mut self.cc.pages {
            page.retain(|c| c.kind != kind);
        }
        self.cc.layout_dirty = true;
        self.save_layout();
    }

    pub fn cc_resize_module(&mut self, kind: ControlKind) {
        if let Some(item) = self.cc.pages.get_mut(self.cc.page).and_then(|p| p.iter_mut().find(|c| c.kind == kind)) {
            item.size = next_module_size(item.kind, item.size);
        }
        self.cc.layout_dirty = true;
        self.save_layout();
    }

    pub fn cc_set_page(&mut self, page: usize) {
        let page = page.min(self.cc.pages.len().saturating_sub(1));
        if page == self.cc.page {
            return;
        }
        self.cc.prev_page = self.cc.page;
        self.cc.page_dir = if page > self.cc.page { 1 } else { -1 };
        self.cc.page = page;
        self.cc.page_since = Instant::now();
        self.cc.scroll_target = 0.0;
        self.cc.scroll.snap(0.0);
        self.cc.layout_dirty = true;
        self.cc.gallery = false;
    }

    pub fn cc_page_step(&mut self, step: i32) {
        if step == 0 || self.cc.pages.is_empty() {
            return;
        }
        let next = (self.cc.page as i32 + step).clamp(0, self.cc.pages.len() as i32 - 1) as usize;
        self.cc_set_page(next);
    }

    pub fn cc_add_page(&mut self) {
        self.cc.prev_page = self.cc.page;
        self.cc.page_dir = 1;
        self.cc.pages.push(Vec::new());
        self.cc.page = self.cc.pages.len() - 1;
        self.cc.page_since = Instant::now();
        self.cc.scroll_target = 0.0;
        self.cc.scroll.snap(0.0);
        self.cc.layout_dirty = true;
        self.cc.gallery = true;
        self.cc.edit = true;
        self.save_layout();
    }

    pub fn cc_remove_page(&mut self) {
        if self.cc.pages.len() <= 1 {
            return;
        }
        self.cc.pages.remove(self.cc.page);
        self.cc.prev_page = self.cc.page;
        self.cc.page = self.cc.page.min(self.cc.pages.len() - 1);
        self.cc.page_dir = -1;
        self.cc.page_since = Instant::now();
        self.cc.scroll_target = 0.0;
        self.cc.scroll.snap(0.0);
        self.cc.layout_dirty = true;
        self.save_layout();
    }

    pub fn cc_scroll_by(&mut self, dy: f32) {
        self.cc.scroll_target = (self.cc.scroll_target + dy).max(0.0);
        self.update_scroll_target();
    }

    pub fn cc_begin_module_drag(&mut self, kind: ControlKind, x: f32, y: f32) {
        self.cc.drag = Some(ModuleDrag { kind, pos: (x, y) });
    }

    pub fn cc_drag_module_to(&mut self, x: f32, y: f32) {
        let Some(drag) = self.cc.drag.as_ref() else { return };
        let kind = drag.kind;
        let (ox, oy) = self.cc.content_origin;
        let px = x - ox;
        let py = y - oy + self.cc.scroll.value(Instant::now());
        // Nearest other module in the last packed grid. Live reflow is smoother
        // than a drop indicator and needs no extra state.
        let target = self
            .cc
            .placed
            .iter()
            .filter(|p| p.kind != kind)
            .min_by(|a, b| {
                let da = (a.x + a.w / 2.0 - px).powi(2) + (a.y + a.h / 2.0 - py).powi(2);
                let db = (b.x + b.w / 2.0 - px).powi(2) + (b.y + b.h / 2.0 - py).powi(2);
                da.total_cmp(&db)
            })
            .copied();
        if let Some(drag) = self.cc.drag.as_mut() {
            drag.pos = (x, y);
        }
        if let Some(target) = target {
            let Some(page) = self.cc.pages.get_mut(self.cc.page) else { return };
            let Some(from) = page.iter().position(|c| c.kind == kind) else { return };
            if from != target.item_index {
                let item = page.remove(from);
                let insert = if from < target.item_index { target.item_index - 1 } else { target.item_index };
                page.insert(insert.min(page.len()), item);
                self.cc.layout_dirty = true;
            }
        }
    }

    pub fn cc_end_module_drag(&mut self) {
        if self.cc.drag.take().is_some() {
            self.cc.layout_dirty = true;
            self.save_layout();
        }
    }

    pub fn cc_dragging(&self) -> bool {
        self.cc.drag.is_some()
    }

    /// Escape leaves the innermost Control Center mode. Returns `false` when
    /// the panel itself should close instead.
    pub fn cc_escape(&mut self) -> bool {
        if self.cc.drag.is_some() {
            self.cc_end_module_drag();
            true
        } else if self.cc.gallery {
            self.cc_close_gallery();
            true
        } else if self.cc.edit {
            self.cc_toggle_edit();
            true
        } else {
            false
        }
    }

    fn save_layout(&self) {
        let mut settings = Settings::load();
        settings.control_center.pages = self.cc.pages.clone();
        if let Err(e) = settings.save() {
            tracing::warn!("saving Control Center layout: {e}");
        }
    }

    /// Rect of the last painted panel, for partial surface repaints.
    pub fn last_rect(&self) -> (f32, f32, f32, f32) {
        self.cc.last_rect
    }

    pub fn is_animating(&self) -> bool {
        let now = Instant::now();
        if self.closing.is_some() {
            return !self.is_closed();
        }
        let open_dur = match self.kind {
            Kind::Notifications => SLIDE_NC_OPEN,
            Kind::Control => CC_OPEN,
        };
        (!anim::reduced()
            && (self.opened.elapsed() < open_dur
                || self.submenu_since.elapsed() < SUBMENU_ANIM
                || self.cc.in_transition(now)))
            || self.cc.scroll.is_animating(now)
            || (self.kind == Kind::Control && self.cc.edit && self.submenu.is_none() && !self.cc.gallery)
            || self.anim.is_animating(now)
    }

    /// 0 = hidden, 1 = fully visible (combines open and close animations).
    fn open_factor(&self) -> f32 {
        if anim::reduced() {
            return if self.closing.is_some() { 0.0 } else { 1.0 };
        }
        let open_dur = match self.kind {
            Kind::Notifications => SLIDE_NC_OPEN,
            Kind::Control => CC_OPEN,
        };
        let open = ease_out_cubic((self.opened.elapsed().as_secs_f32() / open_dur.as_secs_f32()).clamp(0.0, 1.0));
        if let Some(c) = self.closing {
            let close_dur = match self.kind {
                Kind::Notifications => SLIDE_NC_CLOSE,
                Kind::Control => CC_CLOSE,
            };
            let close = 1.0 - ease_in_cubic((c.elapsed().as_secs_f32() / close_dur.as_secs_f32()).clamp(0.0, 1.0));
            (open * close).clamp(0.0, 1.0)
        } else {
            open
        }
    }

    /// Alpha for the compositor's frosted-glass backdrop behind this panel.
    pub fn backdrop_alpha(&self) -> f32 {
        match self.kind {
            Kind::Notifications => 1.0,
            Kind::Control => self.open_factor(),
        }
    }

    fn sub_progress(&self) -> f32 {
        if anim::reduced() {
            return 1.0;
        }
        let t = (self.submenu_since.elapsed().as_secs_f32() / SUBMENU_ANIM.as_secs_f32()).clamp(0.0, 1.0);
        ease_out_cubic(t)
    }

    fn sync_springs(&mut self, center: &Center, now: Instant, screen_h: f32) {
        self.cc.screen_h = screen_h;
        let dnd = if center.do_not_disturb { 1.0 } else { 0.0 };
        let night = if self.night_shift { 1.0 } else { 0.0 };
        let game = if self.game_mode_enabled || self.game_active.is_some() { 1.0 } else { 0.0 };
        let wifi = if self.control.wifi.as_ref().map(|w| w.0).unwrap_or(false) { 1.0 } else { 0.0 };
        let eth = if self.control.ethernet_connected() || (self.control.ethernet.is_empty() && self.network_hint == Network::Wired) {
            1.0
        } else {
            0.0
        };
        let bt = if self.control.bluetooth == Some(true) { 1.0 } else { 0.0 };
        let others = if self.wifi_others_open { 1.0 } else { 0.0 };
        let (vol_raw, muted) = self.control.volume.unwrap_or((0.0, true));
        let vol = if muted { 0.0 } else { vol_raw.clamp(0.0, 1.0) };

        // Pack the page before the spring initialisation early-return below,
        // so the very first frame already knows its modules and height.
        if self.kind == Kind::Control && self.submenu.is_none() {
            let target = self.cc_height(screen_h);
            if !self.anim.panel_h_init {
                self.anim.panel_h.snap(target);
                self.anim.panel_h_init = true;
            } else {
                self.anim.panel_h.set_target(target, now);
            }
            self.update_scroll_target();
            self.cc.scroll.set_target(self.cc.scroll_target, now);
        }

        if !self.anim.initialized {
            self.anim.dnd.snap(dnd);
            self.anim.night_shift.snap(night);
            self.anim.game_mode.snap(game);
            self.anim.wifi.snap(wifi);
            self.anim.ethernet.snap(eth);
            self.anim.bluetooth.snap(bt);
            self.anim.wifi_others.snap(others);
            self.anim.vol.snap(vol);
            self.anim.displays = self
                .control
                .displays
                .iter()
                .map(|d| SpringFloat::new(d.value.clamp(0.0, 1.0), SpringParams::SLIDER))
                .collect();
            self.anim.streams = self
                .control
                .streams
                .iter()
                .map(|st| SpringFloat::new(if st.muted { 0.0 } else { st.volume.clamp(0.0, 1.0) }, SpringParams::SLIDER))
                .collect();
            self.anim.initialized = true;
            return;
        }

        self.anim.dnd.set_target(dnd, now);
        self.anim.night_shift.set_target(night, now);
        self.anim.game_mode.set_target(game, now);
        self.anim.wifi.set_target(wifi, now);
        self.anim.ethernet.set_target(eth, now);
        self.anim.bluetooth.set_target(bt, now);
        self.anim.wifi_others.set_target(others, now);

        if matches!(self.dragging, Some(Target::Volume(..) | Target::VolumeVertical(..))) {
            self.anim.vol.snap(vol);
        } else {
            self.anim.vol.set_target(vol, now);
        }

        if self.anim.displays.len() != self.control.displays.len() {
            self.anim.displays = self
                .control
                .displays
                .iter()
                .map(|d| SpringFloat::new(d.value.clamp(0.0, 1.0), SpringParams::SLIDER))
                .collect();
        } else {
            for (idx, (sp, d)) in self.anim.displays.iter_mut().zip(&self.control.displays).enumerate() {
                let target = d.value.clamp(0.0, 1.0);
                if matches!(self.dragging, Some(Target::Brightness(i, ..) | Target::BrightnessVertical(i, ..)) if i == idx) {
                    sp.snap(target);
                } else {
                    sp.set_target(target, now);
                }
            }
        }

        if self.anim.streams.len() != self.control.streams.len() {
            self.anim.streams = self
                .control
                .streams
                .iter()
                .map(|st| SpringFloat::new(if st.muted { 0.0 } else { st.volume.clamp(0.0, 1.0) }, SpringParams::SLIDER))
                .collect();
        } else {
            for (idx, (sp, st)) in self.anim.streams.iter_mut().zip(&self.control.streams).enumerate() {
                let target = if st.muted { 0.0 } else { st.volume.clamp(0.0, 1.0) };
                if matches!(self.dragging, Some(Target::AppVolume(i, ..)) if i == idx) {
                    sp.snap(target);
                } else {
                    sp.set_target(target, now);
                }
            }
        }

    }

    fn hovered(&self, rx: f32, ry: f32, rw: f32, rh: f32) -> bool {
        self.hover
            .map(|(hx, hy)| hx >= rx && hx < rx + rw && hy >= ry && hy < ry + rh)
            .unwrap_or(false)
    }

    /// Hover test for a rect in the current page canvas (which may be an
    /// offscreen buffer while scrolling or paging).
    fn module_hovered(&self, rx: f32, ry: f32, rw: f32, rh: f32) -> bool {
        self.hovered(
            rx + self.cc.hover_offset.0,
            ry + self.cc.hover_offset.1,
            rw,
            rh,
        )
    }

    /// The panel's rect (for the frosted backdrop), surface coordinates.
    pub fn rect(&self) -> (f32, f32, f32, f32) {
        self.panel_rect
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<Target> {
        for c in &self.cards {
            if let Some(h) = c.hit(x, y) {
                return Some(Target::Card(h));
            }
        }
        // Last painted wins, so edit badges and small controls layered over a
        // module body can take the click.
        for ((rx, ry, rw, rh), t) in self.targets.iter().rev() {
            if x >= *rx && x < rx + rw && y >= *ry && y < ry + rh {
                return Some(t.clone());
            }
        }
        let (px, py, pw, ph) = self.panel_rect;
        (x >= px && x < px + pw && y >= py && y < py + ph).then_some(Target::Panel)
    }

    /// Slider value for a pointer position.
    pub fn slider_value(target: &Target, x: f32, y: f32) -> Option<f32> {
        match target {
            Target::Volume(a, b) | Target::Brightness(_, a, b) | Target::AppVolume(_, a, b) => {
                Some(((x - a) / (b - a)).clamp(0.0, 1.0))
            }
            Target::VolumeVertical(top, bottom) | Target::BrightnessVertical(_, top, bottom) => {
                Some(((bottom - y) / (bottom - top)).clamp(0.0, 1.0))
            }
            _ => None,
        }
    }

    pub fn scroll_by(&mut self, dy: f32) {
        self.scroll = (self.scroll + dy).max(0.0);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &mut self,
        pm: &mut Pixmap,
        s: f32,
        w: f32,
        h: f32,
        center: &Center,
        apps: &Apps,
        fonts: &Fonts,
        theme: &Theme,
        german: bool,
    ) {
        let now = Instant::now();
        self.targets.clear();
        self.cards.clear();
        self.sync_springs(center, now, h);
        match self.kind {
            Kind::Notifications => self.paint_nc(pm, s, w, h, center, apps, fonts, theme, german, now),
            Kind::Control => self.paint_control_wrapped(pm, s, w, h, center, apps, fonts, theme, german, now),
        }
    }

    /// Renders Control Center (main or submenu), applying the macOS top-right
    /// popover scale + fade animation while opening or closing.
    #[allow(clippy::too_many_arguments)]
    fn paint_control_wrapped(
        &mut self,
        pm: &mut Pixmap,
        s: f32,
        w: f32,
        h: f32,
        center: &Center,
        apps: &Apps,
        fonts: &Fonts,
        theme: &Theme,
        german: bool,
        now: Instant,
    ) {
        let factor = self.open_factor();
        if factor <= 0.005 {
            return;
        }
        let x0 = (self.anchor_right - CC_WIDTH).clamp(MARGIN, (w - CC_WIDTH - MARGIN).max(MARGIN));
        let y0 = 6.0 - (1.0 - factor) * 10.0;

        if factor < 0.999 {
            // macOS Control Center: a quick top-right anchored pop, with the
            // modules themselves cascading in one after another.
            let pop_scale = 0.84 + 0.16 * factor;
            let max_h = (h - MARGIN).max(1.0);
            let content_h = match self.submenu {
                Some(_) => max_h,
                None => self.anim.panel_h.value(now).clamp(110.0, max_h),
            };
            let bw = (CC_WIDTH * s).ceil().max(1.0) as u32;
            let bh = (content_h * s).ceil().max(1.0) as u32;
            if self
                .cc_buf
                .as_ref()
                .map(|b| b.width() != bw || b.height() != bh)
                .unwrap_or(true)
            {
                self.cc_buf = Pixmap::new(bw, bh);
            }
            if let Some(mut buf) = self.cc_buf.take() {
                buf.fill(tiny_skia::Color::TRANSPARENT);
                match self.submenu {
                    None => self.paint_cc(&mut buf, s, 0.0, 0.0, center, fonts, theme, german, now),
                    Some(sub) => {
                        self.paint_cc_submenu(&mut buf, s, 0.0, 0.0, max_h, sub, apps, fonts, theme, german, now)
                    }
                }
                let card_h = self.panel_rect.3;
                let scaled_w = CC_WIDTH * pop_scale;
                let scaled_h = card_h * pop_scale;
                // Top-right anchor matches the Control Center menu bar icon
                let draw_x = x0 + (CC_WIDTH - scaled_w);
                let draw_y = y0;
                self.panel_rect = (draw_x, draw_y, scaled_w, scaled_h);
                self.cc.last_rect = self.panel_rect;
                composite(pm, &buf, draw_x * s, draw_y * s, pop_scale, factor);
                self.cc_buf = Some(buf);
                if self.closing.is_some() || self.cc.in_transition(now) {
                    self.targets.clear();
                } else {
                    for ((rx, ry, rw, rh), _) in &mut self.targets {
                        *rx = draw_x + *rx * pop_scale;
                        *ry = draw_y + *ry * pop_scale;
                        *rw *= pop_scale;
                        *rh *= pop_scale;
                    }
                }
                return;
            }
        }

        match self.submenu {
            None => self.paint_cc(pm, s, x0, y0, center, fonts, theme, german, now),
            Some(sub) => self.paint_cc_submenu(pm, s, x0, y0, h, sub, apps, fonts, theme, german, now),
        }
        self.cc.last_rect = self.panel_rect;
        if self.closing.is_some() || self.cc.in_transition(now) {
            self.targets.clear();
        }
    }

    // ---- Notification Center -------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn paint_nc(
        &mut self,
        pm: &mut Pixmap,
        s: f32,
        w: f32,
        h: f32,
        center: &Center,
        apps: &Apps,
        fonts: &Fonts,
        theme: &Theme,
        german: bool,
        now: Instant,
    ) {
        let t = |en: &'static str, de: &'static str| if german { de } else { en };
        let slide = (1.0 - self.open_factor()) * (NC_WIDTH + MARGIN);
        let x0 = w - NC_WIDTH - MARGIN + slide;
        let (top, bottom) = (MARGIN, h - MARGIN);
        self.panel_rect = (x0, top, NC_WIDTH, bottom - top);
        self.cc.last_rect = self.panel_rect;
        let dark = theme.bar_text[0] > 128;
        let tint: Rgba = if dark { [30, 30, 32, 150] } else { [240, 240, 242, 150] };
        let card: Rgba = if dark { [255, 255, 255, 22] } else { [255, 255, 255, 150] };
        fill_rounded(pm, x0 * s, top * s, NC_WIDTH * s, (bottom - top) * s, 18.0 * s, tint);

        let inner_x = x0 + 12.0;
        let inner_w = NC_WIDTH - 24.0;
        // date
        let date = date_label(german);
        let base = fonts.baseline_centered((top + 12.0) * s, 28.0 * s, 18.0 * s, Weight::Bold);
        fonts.draw(pm, &date, inner_x * s, base, 18.0 * s, Weight::Bold, theme.menu_text);

        // Do Not Disturb row
        let row_y = top + 50.0;
        fill_rounded(pm, inner_x * s, row_y * s, inner_w * s, 44.0 * s, 14.0 * s, card);
        let accent = theme.menu_selection;
        let dnd_t = self.anim.dnd.sample(now).clamp(0.0, 1.0);
        let (dnd_scale, _) = tap_scale(self.anim.pulses[PULSE_DND], now);
        let moon_bg = lerp_rgba([128, 128, 128, 90], [94, 92, 230, 255], dnd_t);
        fill_circle(pm, (inner_x + 24.0) * s, (row_y + 22.0) * s, 14.0 * dnd_scale * s, moon_bg);
        moon(pm, (inner_x + 24.0) * s, (row_y + 22.0) * s, 7.0 * dnd_scale * s, [255, 255, 255, 255]);
        let base = fonts.baseline_centered(row_y * s, 44.0 * s, 13.0 * s, Weight::Bold);
        fonts.draw(pm, t("Do Not Disturb", "Nicht stören"), (inner_x + 48.0) * s, base, 13.0 * s, Weight::Bold, theme.menu_text);
        switch_anim(pm, s, inner_x + inner_w - 52.0, row_y + 11.0, dnd_t, accent, dark);
        self.targets.push(((inner_x, row_y, inner_w, 44.0), Target::Dnd));

        // section header
        let head_y = row_y + 58.0;
        let base = fonts.baseline_centered(head_y * s, 22.0 * s, 13.0 * s, Weight::Bold);
        fonts.draw(pm, t("Notifications", "Mitteilungen"), inner_x * s, base, 13.0 * s, Weight::Bold, theme.menu_text_disabled);
        if !center.history.is_empty() {
            let label = t("Clear All", "Alle löschen");
            let lw = fonts.measure(label, 12.0 * s, Weight::Regular) / s;
            let lx = inner_x + inner_w - lw - 4.0;
            fonts.draw(pm, label, lx * s, base, 12.0 * s, Weight::Regular, accent);
            self.targets.push(((lx - 6.0, head_y, lw + 12.0, 22.0), Target::ClearAll));
        }

        // the list, clipped to its area by drawing into a separate pixmap
        let list_y = head_y + 30.0;
        let list_h = (bottom - 8.0 - list_y).max(0.0);
        if center.history.is_empty() {
            let msg = t("No Notifications", "Keine Mitteilungen");
            let mw = fonts.measure(msg, 13.0 * s, Weight::Regular) / s;
            let base = fonts.baseline_centered((list_y + 40.0) * s, 20.0 * s, 13.0 * s, Weight::Regular);
            fonts.draw(pm, msg, (inner_x + (inner_w - mw) / 2.0) * s, base, 13.0 * s, Weight::Regular, theme.menu_text_disabled);
            return;
        }
        let layouts: Vec<_> = center.history.iter().map(|p| notify::card_layout(fonts, &p.n, inner_w)).collect();
        let total: f32 = layouts.iter().map(|l| l.h + 10.0).sum();
        self.scroll = self.scroll.min((total - list_h).max(0.0));
        let (Some(mut sub), true) = (
            Pixmap::new((inner_w * s).ceil().max(1.0) as u32 + 2, (list_h * s).ceil().max(1.0) as u32),
            list_h > 1.0,
        ) else {
            return;
        };
        let hover_id = self.hover.and_then(|(hx, hy)| self.cards_prev_hover(hx, hy));
        let mut y = -self.scroll + 10.0;
        let mut cards = Vec::new();
        for (p, l) in center.history.iter().zip(&layouts) {
            if y + l.h >= 0.0 && y <= list_h {
                let hit = notify::paint_card(
                    &mut sub,
                    s,
                    1.0,
                    y,
                    inner_w - 2.0,
                    &p.n,
                    l,
                    p.received.elapsed(),
                    hover_id == Some(p.n.id),
                    apps,
                    fonts,
                    theme,
                );
                // back to surface coordinates; cards outside the visible list don't count
                let mut hit = hit;
                hit.rect = (hit.rect.0 + inner_x - 1.0, hit.rect.1 + list_y, hit.rect.2, hit.rect.3);
                hit.close = (hit.close.0 + inner_x - 1.0, hit.close.1 + list_y, hit.close.2);
                for (_, r) in hit.buttons.iter_mut() {
                    r.0 += inner_x - 1.0;
                    r.1 += list_y;
                }
                cards.push(hit);
            }
            y += l.h + 10.0;
        }
        pm.draw_pixmap(
            ((inner_x - 1.0) * s) as i32,
            (list_y * s) as i32,
            sub.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
        // only the visible part of a card is clickable
        for c in cards.iter_mut() {
            let (x, y, w, h) = c.rect;
            let y2 = (y + h).min(list_y + list_h);
            let y1 = y.max(list_y);
            c.rect = (x, y1, w, (y2 - y1).max(0.0));
            if c.close.1 < list_y {
                c.close.2 = -100.0;
            }
        }
        self.cards = cards;
    }

    /// The card under the pointer, from the last paint.
    fn cards_prev_hover(&self, x: f32, y: f32) -> Option<u32> {
        PREV_CARDS.with(|c| {
            c.borrow()
                .iter()
                .find(|(_, (cx, cy, cw, ch))| x >= *cx && x < cx + cw && y >= *cy && y < cy + ch)
                .map(|(id, _)| *id)
        })
    }

    pub fn remember_cards(&self) {
        PREV_CARDS.with(|c| *c.borrow_mut() = self.cards.iter().map(|h| (h.id, h.rect)).collect());
    }

    // ---- Control Center (iOS 18 module grid) ---------------------------------

    #[allow(clippy::too_many_arguments)]
    fn paint_cc(
        &mut self,
        pm: &mut Pixmap,
        s: f32,
        x0: f32,
        y0: f32,
        center: &Center,
        fonts: &Fonts,
        theme: &Theme,
        german: bool,
        now: Instant,
    ) {
        let _ = center;
        let dark = theme.bar_text[0] > 128;
        let solid = theme.menu_bg[3] > 200;
        let height = self.anim.panel_h.sample(now).clamp(110.0, 4000.0);
        self.cc.height = height;
        self.panel_rect = (x0, y0, CC_WIDTH, height);

        let tint: Rgba = if dark {
            if solid { [28, 28, 30, 248] } else { [18, 18, 22, 122] }
        } else if solid {
            [244, 244, 247, 250]
        } else {
            [246, 246, 250, 150]
        };
        let edge: Rgba = if dark { [255, 255, 255, 30] } else { [0, 0, 0, 18] };
        fill_rounded(pm, x0 * s, y0 * s, CC_WIDTH * s, height * s, CC_RADIUS * s, tint);
        stroke_rounded(pm, x0 * s, y0 * s, CC_WIDTH * s, height * s, CC_RADIUS * s, 1.0 * s, edge);

        let body_h = (height - CC_TOP - CC_FOOTER - CC_BOTTOM).max(0.0);
        let content_y = y0 + CC_TOP;
        let scroll = self.cc.scroll.sample(now).clamp(0.0, 100000.0);
        let trans = self.cc.in_transition(now);
        let page_p = self.cc.page_transition(now);
        let dir = self.cc.page_dir as f32;
        // Sliding back from a detail sheet: the grid re-enters from the left.
        let return_slide = if self.submenu_returning {
            -(1.0 - self.sub_progress()) * 16.0
        } else {
            0.0
        };

        if self.cc.gallery {
            self.paint_gallery(pm, s, x0, content_y, body_h, theme, fonts, german, now);
        } else if self.cc.placed.is_empty() {
            self.paint_empty_page(pm, s, x0, content_y, body_h, theme, fonts, german);
        } else {
            let content = self.ensure_scroll_height();
            let needs_body = trans || content > body_h + 0.5 || scroll > 0.5;
            if needs_body {
                let bw = (CC_WIDTH * s).ceil().max(1.0) as u32;
                let bh = (body_h * s).ceil().max(1.0) as u32;
                if self
                    .body_buf
                    .as_ref()
                    .map(|b| b.width() != bw || b.height() != bh)
                    .unwrap_or(true)
                {
                    self.body_buf = Pixmap::new(bw, bh);
                }
                if let Some(mut buf) = self.body_buf.take() {
                    buf.fill(tiny_skia::Color::TRANSPARENT);
                    let mut local: Vec<((f32, f32, f32, f32), Target)> = Vec::new();
                    if trans {
                        let prev = pack_page(self.cc.pages.get(self.cc.prev_page).map(Vec::as_slice).unwrap_or(&[]));
                        let current = std::mem::take(&mut self.cc.placed);
                        self.paint_page(
                            &mut buf,
                            s,
                            &prev,
                            (-dir * page_p * CC_WIDTH, 0.0),
                            (x0, content_y),
                            fonts,
                            theme,
                            german,
                            now,
                            false,
                            &mut local,
                        );
                        self.paint_page(
                            &mut buf,
                            s,
                            &current,
                            (dir * (1.0 - page_p) * CC_WIDTH, -scroll),
                            (x0, content_y),
                            fonts,
                            theme,
                            german,
                            now,
                            false,
                            &mut local,
                        );
                        self.cc.placed = current;
                    } else {
                        // Current page, drawn into the clipped body buffer.
                        self.paint_page_current(
                            &mut buf,
                            s,
                            (return_slide, -scroll),
                            (x0, content_y),
                            fonts,
                            theme,
                            german,
                            now,
                            true,
                            &mut local,
                        );
                    }
                    composite(pm, &buf, x0 * s, content_y * s, 1.0, 1.0);
                    self.body_buf = Some(buf);
                    // A scrolled-out module must not leave a clickable strip
                    // above or below the visible body.
                    let (vx0, vy0) = (x0, content_y);
                    let (vx1, vy1) = (x0 + CC_WIDTH, content_y + body_h);
                    for ((rx, ry, rw, rh), t) in local {
                        let x1 = (rx + rw).min(vx1);
                        let y1 = (ry + rh).min(vy1);
                        let x0 = rx.max(vx0);
                        let y0 = ry.max(vy0);
                        if x1 > x0 && y1 > y0 {
                            self.targets.push(((x0, y0, x1 - x0, y1 - y0), t));
                        }
                    }
                }
            } else {
                let mut local: Vec<((f32, f32, f32, f32), Target)> = Vec::new();
                self.paint_page_current(
                    pm,
                    s,
                    (x0 + return_slide, content_y),
                    (0.0, 0.0),
                    fonts,
                    theme,
                    german,
                    now,
                    true,
                    &mut local,
                );
                self.targets.extend(local);
                self.cc.content_origin = (x0, content_y);
            }
        }

        // Page dots / edit controls.
        self.cc.hover_offset = (0.0, 0.0);
        self.paint_cc_footer(pm, s, x0, y0 + height - CC_BOTTOM - CC_FOOTER, theme, fonts);

        // The picked-up module floats under the pointer while reordering.
        if let Some(drag) = self.cc.drag.clone() {
            let (dw, dh) = self
                .cc
                .placed
                .iter()
                .find(|p| p.kind == drag.kind)
                .map(|p| (p.w, p.h))
                .unwrap_or((CC_CELL, CC_CELL));
            let (mx, my) = (drag.pos.0 - dw / 2.0, drag.pos.1 - dh / 2.0);
            let look = CcLook::new(theme, 0.92);
            fill_rounded(pm, (mx + 1.0) * s, (my + 4.0) * s, dw * s, dh * s, MODULE_RADIUS * s, [0, 0, 0, 60]);
            self.paint_module(
                pm,
                s,
                drag.kind,
                (mx, my, dw, dh),
                look,
                fonts,
                theme,
                german,
                now,
                false,
                &mut Vec::new(),
            );
        }
    }

    /// Visible body height, used by the scroll spring.
    fn ensure_scroll_height(&mut self) -> f32 {
        self.ensure_layout();
        content_height(&self.cc.placed)
    }

    fn paint_page_current(
        &mut self,
        pm: &mut Pixmap,
        s: f32,
        offset: (f32, f32),
        target_origin: (f32, f32),
        fonts: &Fonts,
        theme: &Theme,
        german: bool,
        now: Instant,
        interactive: bool,
        targets: &mut Vec<((f32, f32, f32, f32), Target)>,
    ) {
        self.ensure_layout();
        let placed = std::mem::take(&mut self.cc.placed);
        self.paint_page(
            pm,
            s,
            &placed,
            offset,
            target_origin,
            fonts,
            theme,
            german,
            now,
            interactive,
            targets,
        );
        self.cc.placed = placed;
    }

    /// Draw one page of modules. `offset` is the page origin inside the target
    /// canvas; `target_origin` maps canvas coordinates back to the surface.
    /// `interactive = false` is used for the outgoing page in a paging
    /// transition (and for the floating drag preview).
    #[allow(clippy::too_many_arguments)]
    fn paint_page(
        &mut self,
        pm: &mut Pixmap,
        s: f32,
        placed: &[PlacedModule],
        offset: (f32, f32),
        target_origin: (f32, f32),
        fonts: &Fonts,
        theme: &Theme,
        german: bool,
        now: Instant,
        interactive: bool,
        targets: &mut Vec<((f32, f32, f32, f32), Target)>,
    ) {
        let opening = self.opened.elapsed();
        let opening = !anim::reduced() && opening < CC_OPEN && self.closing.is_none() && self.submenu.is_none();
        let open_t = self.opened.elapsed().as_secs_f32();
        let edit_sway = if self.cc.edit && !anim::reduced() {
            let t = now.saturating_duration_since(self.cc.edit_since).as_secs_f32();
            Some(t)
        } else {
            None
        };
        let ox = offset.0 + target_origin.0;
        let oy = offset.1 + target_origin.1;
        self.cc.content_origin = (ox, oy);
        self.cc.hover_offset = target_origin;

        let drag_kind = self.cc.drag.as_ref().map(|d| d.kind);
        for (i, m) in placed.iter().enumerate() {
            if interactive && drag_kind == Some(m.kind) {
                // The picked-up control is drawn under the pointer instead.
                continue;
            }
            // Staggered entrance from the menu bar (macOS Control Center).
            let (e, alpha) = if opening {
                let local = ((open_t - i as f32 * STAGGER_STEP) / STAGGER_LEN).clamp(0.0, 1.0);
                (ease_out_cubic(local), ease_out_cubic(local))
            } else {
                (1.0, 1.0)
            };
            if e <= 0.001 {
                continue;
            }
            let sway = edit_sway
                .map(|t| ((t * 5.0 + i as f32 * 0.85).sin() * 1.3) as f32)
                .unwrap_or(0.0);
            let rect = (
                offset.0 + m.x + sway,
                offset.1 + m.y + (1.0 - e) * 16.0,
                m.w,
                m.h,
            );
            let look = CcLook::new(theme, alpha);
            let module_interactive = interactive && alpha > 0.999 && !self.cc.edit;
            self.paint_module(
                pm,
                s,
                m.kind,
                rect,
                look,
                fonts,
                theme,
                german,
                now,
                module_interactive,
                targets,
            );
            // Targets are always surface coordinates, even when the pixels
            // were drawn into a scrolled/paging offscreen buffer.
            let gx = target_origin.0 + rect.0;
            let gy = target_origin.1 + rect.1;

            if self.cc.edit && interactive && alpha > 0.999 {
                // The whole card grabs; the badges below win the hit test.
                targets.push(((gx, gy, rect.2, rect.3), Target::GrabModule(m.kind)));
                self.paint_edit_badges(pm, s, (gx, gy, rect.2, rect.3), m.kind, theme, targets);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_edit_badges(
        &mut self,
        pm: &mut Pixmap,
        s: f32,
        rect: (f32, f32, f32, f32),
        kind: ControlKind,
        theme: &Theme,
        targets: &mut Vec<((f32, f32, f32, f32), Target)>,
    ) {
        let (x, y, w, h) = rect;
        // Remove (inset so it stays inside the panel corner).
        let (cx, cy) = (x + 14.0, y + 14.0);
        fill_circle(pm, cx * s, (cy + 0.7) * s, 11.0 * s, [0, 0, 0, 40]);
        fill_circle(pm, cx * s, cy * s, 10.5 * s, [255, 69, 58, 255]);
        minus(pm, cx * s, cy * s, 4.2 * s, [255, 255, 255, 255]);
        targets.push(((cx - 13.0, cy - 13.0, 26.0, 26.0), Target::RemoveModule(kind)));
        // Resize handle appears on hover. Sliders only have their iOS size.
        if !matches!(kind, ControlKind::Brightness | ControlKind::Volume)
            && self.hovered(x, y, w, h)
        {
            let (rx, ry) = (x + w - 14.0, y + h - 14.0);
            fill_circle(pm, rx * s, (ry + 0.7) * s, 11.0 * s, [0, 0, 0, 40]);
            fill_circle(pm, rx * s, ry * s, 10.5 * s, [242, 242, 247, 255]);
            resize_glyph(pm, rx * s, ry * s, 4.6 * s, [40, 40, 43, 255]);
            targets.push(((rx - 13.0, ry - 13.0, 26.0, 26.0), Target::ResizeModule(kind)));
        }
        let _ = theme;
    }

    fn paint_empty_page(
        &mut self,
        pm: &mut Pixmap,
        s: f32,
        x0: f32,
        y: f32,
        body_h: f32,
        theme: &Theme,
        fonts: &Fonts,
        german: bool,
    ) {
        let look = CcLook::new(theme, 1.0);
        let t = |en: &'static str, de: &'static str| if german { de } else { en };
        let rect = (x0 + CC_PAD, y, CC_WIDTH - 2.0 * CC_PAD, (body_h - 6.0).max(60.0));
        let hovered = self.hovered(rect.0, rect.1, rect.2, rect.3);
        fill_rounded(pm, rect.0 * s, rect.1 * s, rect.2 * s, rect.3 * s, MODULE_RADIUS * s, if hovered { look.card_hover } else { look.card });
        stroke_rounded(pm, rect.0 * s, rect.1 * s, rect.2 * s, rect.3 * s, MODULE_RADIUS * s, 1.0 * s, look.border);
        let (cx, cy) = (rect.0 + rect.2 / 2.0, rect.1 + rect.3 / 2.0 - 12.0);
        fill_circle(pm, cx * s, cy * s, 18.0 * s, look.off);
        plus(pm, cx * s, cy * s, 7.5 * s, look.sub);
        let msg = t("No Controls", "Keine Bedienelemente");
        let mw = fonts.measure(msg, 13.0 * s, Weight::Bold) / s;
        let base = fonts.baseline_centered((cy + 24.0) * s, 18.0 * s, 13.0 * s, Weight::Bold);
        fonts.draw(pm, msg, (rect.0 + (rect.2 - mw) / 2.0) * s, base, 13.0 * s, Weight::Bold, look.text);
        let hint = t("Click to add controls", "Klicken, um Bedienelemente hinzuzufügen");
        let hw = fonts.measure(hint, 11.0 * s, Weight::Regular) / s;
        let base = fonts.baseline_centered((cy + 42.0) * s, 16.0 * s, 11.0 * s, Weight::Regular);
        fonts.draw(pm, hint, (rect.0 + (rect.2 - hw) / 2.0) * s, base, 11.0 * s, Weight::Regular, look.sub);
        self.targets.push((rect, Target::OpenGallery));
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_gallery(
        &mut self,
        pm: &mut Pixmap,
        s: f32,
        x0: f32,
        y: f32,
        body_h: f32,
        theme: &Theme,
        fonts: &Fonts,
        german: bool,
        now: Instant,
    ) {
        let t = |en: &'static str, de: &'static str| if german { de } else { en };
        let look = CcLook::new(theme, 1.0);
        self.ensure_layout();
        let title = t("Add Control", "Bedienelement hinzufügen");
        let base = fonts.baseline_centered(y * s, 26.0 * s, 15.0 * s, Weight::Bold);
        fonts.draw(pm, title, (x0 + CC_PAD) * s, base, 15.0 * s, Weight::Bold, look.text);
        // "New Page" pill
        let np_w = fonts.measure(t("New Page", "Neue Seite"), 11.0 * s, Weight::Bold) / s + 24.0;
        let np = (x0 + CC_WIDTH - CC_PAD - np_w, y + 3.0, np_w, 24.0);
        let np_hover = self.hovered(np.0, np.1, np.2, np.3);
        fill_rounded(pm, np.0 * s, np.1 * s, np.2 * s, np.3 * s, 12.0 * s, if np_hover { look.card_hover } else { look.card });
        let nb = fonts.baseline_centered(np.1 * s, np.3 * s, 11.0 * s, Weight::Bold);
        let label = t("New Page", "Neue Seite");
        fonts.draw(pm, label, (np.0 + 12.0) * s, nb, 11.0 * s, Weight::Bold, look.text);
        self.targets.push((np, Target::PageAdd));

        let grid_y = y + GALLERY_HEADER;
        let scroll = self.cc.scroll.sample(now);
        let placed = std::mem::take(&mut self.cc.placed);
        if placed.is_empty() {
            self.cc.placed = placed;
            let msg = t("All controls added", "Alle Bedienelemente hinzugefügt");
            let mw = fonts.measure(msg, 12.0 * s, Weight::Regular) / s;
            let base = fonts.baseline_centered((grid_y + 16.0) * s, 20.0 * s, 12.0 * s, Weight::Regular);
            fonts.draw(pm, msg, (x0 + (CC_WIDTH - mw) / 2.0) * s, base, 12.0 * s, Weight::Regular, look.sub);
            return;
        }
        // The panel height springs between views; don't let gallery tiles
        // escape the current (still growing) panel body in the meantime.
        let body_bottom = y + body_h;
        for m in placed.iter() {
            let rect = (
                x0 + m.x,
                grid_y + m.y - scroll,
                m.w,
                m.h,
            );
            if rect.1 < grid_y - 0.5 || rect.1 + rect.3 > body_bottom + 0.5 {
                continue;
            }
            let hovered = self.hovered(rect.0, rect.1, rect.2, rect.3);
            let added = self.cc.pages.iter().flatten().any(|c| c.kind == m.kind);
            let on_this_page = self.cc.pages.get(self.cc.page).is_some_and(|p| p.iter().any(|c| c.kind == m.kind));
            let l = CcLook::new(theme, if added && on_this_page { 0.62 } else { 1.0 });
            self.paint_gallery_tile(pm, s, rect, m.kind, l, hovered, added, fonts, german);
            let target = if on_this_page {
                Target::RemoveModule(m.kind)
            } else {
                Target::AddModule(m.kind)
            };
            self.targets.push((rect, target));
        }
        self.cc.placed = placed;
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_gallery_tile(
        &mut self,
        pm: &mut Pixmap,
        s: f32,
        rect: (f32, f32, f32, f32),
        kind: ControlKind,
        look: CcLook,
        hovered: bool,
        added: bool,
        fonts: &Fonts,
        german: bool,
    ) {
        let (x, y, w, h) = rect;
        fill_rounded(
            pm,
            x * s,
            y * s,
            w * s,
            h * s,
            MODULE_RADIUS * s,
            if hovered { look.card_hover } else { look.card },
        );
        stroke_rounded(pm, x * s, y * s, w * s, h * s, MODULE_RADIUS * s, 1.0 * s, look.border);
        let (cx, cy) = (x + w / 2.0, y + 22.0);
        fill_circle(pm, cx * s, cy * s, 13.5 * s, look.off);
        module_glyph(pm, s, kind, cx, cy, 5.8, look.text);
        let label = fonts.elide(module_name(kind, german), 9.5 * s, Weight::Bold, (w - 8.0) * s);
        let lw = fonts.measure(&label, 9.5 * s, Weight::Bold) / s;
        let base = fonts.baseline_centered((y + 38.0) * s, 20.0 * s, 9.5 * s, Weight::Bold);
        fonts.draw(pm, &label, (x + (w - lw) / 2.0) * s, base, 9.5 * s, Weight::Bold, look.text);
        // Add / already-added badge.
        let (bx, by) = (x + w - 9.0, y + 9.0);
        if added {
            fill_circle(pm, bx * s, by * s, 6.5 * s, [52, 199, 89, 255]);
            check(pm, bx * s, by * s, 3.1 * s, [255, 255, 255, 255]);
        } else {
            fill_circle(pm, bx * s, by * s, 6.5 * s, look.accent);
            plus(pm, bx * s, by * s, 3.2 * s, [255, 255, 255, 255]);
        }
    }

    fn paint_cc_footer(
        &mut self,
        pm: &mut Pixmap,
        s: f32,
        x0: f32,
        y: f32,
        theme: &Theme,
        fonts: &Fonts,
    ) {
        let look = CcLook::new(theme, 1.0);
        let h = CC_FOOTER;
        if self.cc.gallery {
            let label = "Done";
            let w = fonts.measure(label, 11.5 * s, Weight::Bold) / s + 26.0;
            let r = (x0 + CC_WIDTH - CC_PAD - w, y + 4.0, w, 26.0);
            let hov = self.hovered(r.0, r.1, r.2, r.3);
            fill_rounded(pm, r.0 * s, r.1 * s, r.2 * s, r.3 * s, 13.0 * s, if hov { look.accent } else { look.card_hover });
            let b = fonts.baseline_centered(r.1 * s, r.3 * s, 11.5 * s, Weight::Bold);
            fonts.draw(pm, label, (r.0 + 13.0) * s, b, 11.5 * s, Weight::Bold, if hov { [255, 255, 255, 255] } else { look.text });
            self.targets.push((r, Target::CloseGallery));
            return;
        }

        // Page dots, centred.
        let n = self.cc.pages.len();
        let dot_gap = 15.0;
        let total = if n == 0 { 0.0 } else { (n - 1) as f32 * dot_gap + 6.0 };
        let mut dx = x0 + (CC_WIDTH - total) / 2.0;
        for i in 0..n {
            let active = i == self.cc.page;
            let rect = if active {
                let r = (dx - 6.0, y + h / 2.0 - 3.0, 12.0, 6.0);
                fill_rounded(pm, r.0 * s, r.1 * s, r.2 * s, r.3 * s, 3.0 * s, look.text);
                r
            } else {
                let r = (dx - 4.5, y + h / 2.0 - PAGE_DOT - 4.0, 12.0, 14.0);
                fill_circle(pm, dx * s, (y + h / 2.0) * s, PAGE_DOT * s, look.faint);
                r
            };
            self.targets.push((rect, Target::Page(i)));
            dx += dot_gap;
        }

        if self.cc.edit {
            if self.cc.pages.len() > 1 {
                let r = (x0 + CC_PAD, y + 5.0, 24.0, 24.0);
                let hov = self.hovered(r.0, r.1, r.2, r.3);
                fill_circle(pm, (r.0 + 12.0) * s, (r.1 + 12.0) * s, 12.0 * s, if hov { [255, 69, 58, 45] } else { [0, 0, 0, 0] });
                trash(pm, (r.0 + 12.0) * s, (r.1 + 12.0) * s, 5.6 * s, if hov { [255, 69, 58, 255] } else { look.sub });
                self.targets.push((r, Target::PageRemove));
            }
            let plus_r = (x0 + CC_WIDTH - CC_PAD - 24.0, y + 5.0, 24.0, 24.0);
            let hov = self.hovered(plus_r.0, plus_r.1, plus_r.2, plus_r.3);
            fill_circle(pm, (plus_r.0 + 12.0) * s, (plus_r.1 + 12.0) * s, 12.0 * s, if hov { look.accent } else { look.card_hover });
            plus(pm, (plus_r.0 + 12.0) * s, (plus_r.1 + 12.0) * s, 5.0 * s, if hov { [255, 255, 255, 255] } else { look.text });
            self.targets.push((plus_r, Target::OpenGallery));
            let done = (x0 + CC_WIDTH - CC_PAD - 24.0 - 30.0, y + 5.0, 24.0, 24.0);
            let hov = self.hovered(done.0, done.1, done.2, done.3);
            fill_circle(pm, (done.0 + 12.0) * s, (done.1 + 12.0) * s, 12.0 * s, if hov { look.accent } else { look.card_hover });
            check(pm, (done.0 + 12.0) * s, (done.1 + 12.0) * s, 5.0 * s, if hov { [255, 255, 255, 255] } else { look.text });
            self.targets.push((done, Target::ToggleEdit));
        } else {
            let r = (x0 + CC_WIDTH - CC_PAD - 24.0, y + 5.0, 24.0, 24.0);
            let hov = self.hovered(r.0, r.1, r.2, r.3);
            fill_circle(pm, (r.0 + 12.0) * s, (r.1 + 12.0) * s, 12.0 * s, if hov { look.card_hover } else { [0, 0, 0, 0] });
            pencil(pm, (r.0 + 12.0) * s, (r.1 + 12.0) * s, 5.4 * s, if hov { look.text } else { look.sub });
            self.targets.push((r, Target::ToggleEdit));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_module(
        &mut self,
        pm: &mut Pixmap,
        s: f32,
        kind: ControlKind,
        rect: (f32, f32, f32, f32),
        look: CcLook,
        fonts: &Fonts,
        theme: &Theme,
        german: bool,
        now: Instant,
        interactive: bool,
        targets: &mut Vec<((f32, f32, f32, f32), Target)>,
    ) {
        let (x, y, w, h) = rect;
        // iOS 18: a 1 × 1 control is just one large round button with the
        // symbol centred. Titles and status text only appear from 2 × 1 up.
        if w <= CC_CELL + 1.0 && h <= CC_CELL + 1.0 {
            self.paint_round_button(pm, s, rect, kind, look, german, interactive, targets, now);
            return;
        }
        // Connectivity is composed of iOS pill rows, and the tall sliders are
        // capsules themselves; neither sits on a square module card.
        let vertical_slider = matches!(kind, ControlKind::Brightness | ControlKind::Volume) && h > w + 1.0;
        if kind == ControlKind::Connectivity {
            self.paint_connectivity(pm, s, rect, look, fonts, german, interactive, targets, now);
            return;
        }
        if vertical_slider {
            self.paint_vertical_slider(pm, s, rect, kind, look, interactive, targets, now);
            return;
        }
        let hovered = self.module_hovered(x, y, w, h);
        let card = if hovered { look.card_hover } else { look.card };
        fill_rounded(pm, x * s, y * s, w * s, h * s, MODULE_RADIUS * s, card);
        stroke_rounded(pm, x * s, y * s, w * s, h * s, MODULE_RADIUS * s, 1.0 * s, look.border);
        match kind {
            ControlKind::Connectivity => {}
            ControlKind::NowPlaying => {
                self.paint_now_playing(pm, s, rect, look, fonts, theme, german, interactive, targets);
            }
            ControlKind::Brightness => {
                self.paint_slider_module(pm, s, rect, kind, look, fonts, german, now, interactive, targets);
            }
            ControlKind::Volume => {
                self.paint_slider_module(pm, s, rect, kind, look, fonts, german, now, interactive, targets);
            }
            ControlKind::Wifi
            | ControlKind::Bluetooth
            | ControlKind::Focus
            | ControlKind::NightShift
            | ControlKind::GameMode
            | ControlKind::Displays
            | ControlKind::Network
            | ControlKind::Sound => {
                self.paint_small_module(pm, s, rect, kind, look, fonts, german, interactive, targets, now);
            }
        }
    }

    /// The iOS 18 small control: one circle filling the grid cell, symbol only.
    #[allow(clippy::too_many_arguments)]
    fn paint_round_button(
        &mut self,
        pm: &mut Pixmap,
        s: f32,
        rect: (f32, f32, f32, f32),
        kind: ControlKind,
        look: CcLook,
        german: bool,
        interactive: bool,
        targets: &mut Vec<((f32, f32, f32, f32), Target)>,
        now: Instant,
    ) {
        let (x, y, w, h) = rect;
        let (on_t, on_col, pulse, target) = match kind {
            ControlKind::Wifi => (
                self.anim.wifi.sample(now).clamp(0.0, 1.0),
                look.accent,
                PULSE_WIFI,
                if self.control.wifi.is_some() {
                    Target::WifiToggle
                } else {
                    Target::OpenSubmenu(Submenu::Wifi)
                },
            ),
            ControlKind::Bluetooth => (
                self.anim.bluetooth.sample(now).clamp(0.0, 1.0),
                look.accent,
                PULSE_BT,
                if self.control.bluetooth.is_some() {
                    Target::BluetoothToggle
                } else {
                    Target::OpenSubmenu(Submenu::Bluetooth)
                },
            ),
            ControlKind::Focus => (
                self.anim.dnd.sample(now).clamp(0.0, 1.0),
                [94, 92, 230, 255],
                PULSE_DND,
                Target::Dnd,
            ),
            ControlKind::NightShift => (
                self.anim.night_shift.sample(now).clamp(0.0, 1.0),
                [255, 149, 0, 255],
                PULSE_NIGHT,
                Target::NightShift,
            ),
            ControlKind::GameMode => (
                self.anim.game_mode.sample(now).clamp(0.0, 1.0),
                [52, 199, 89, 255],
                PULSE_GAME,
                Target::GameMode,
            ),
            ControlKind::Displays => (0.0, look.accent, PULSE_GAME, Target::OpenSettings("displays")),
            ControlKind::Network => (0.0, look.accent, PULSE_WIFI, Target::OpenSubmenu(Submenu::Wifi)),
            ControlKind::Sound => (0.0, look.accent, PULSE_WIFI, Target::OpenSubmenu(Submenu::Sound)),
            ControlKind::Connectivity => (
                self.anim.wifi.sample(now).clamp(0.0, 1.0),
                look.accent,
                PULSE_WIFI,
                Target::OpenSubmenu(Submenu::Wifi),
            ),
            ControlKind::NowPlaying => (
                if self.control.media.as_ref().is_some_and(|m| m.playing) {
                    1.0
                } else {
                    0.0
                },
                look.accent,
                PULSE_WIFI,
                Target::MediaPlay,
            ),
            ControlKind::Brightness => (
                if self.control.displays.is_empty() { 0.0 } else { 1.0 },
                look.accent,
                PULSE_GAME,
                Target::OpenSettings("displays"),
            ),
            ControlKind::Volume => (
                if self.control.volume.is_some_and(|(_, muted)| !muted) {
                    1.0
                } else {
                    0.0
                },
                look.accent,
                PULSE_WIFI,
                Target::VolumeMute,
            ),
        };
        let hovered = interactive && self.module_hovered(x, y, w, h);
        let white = [250, 250, 252, look.text[3]];
        let off_glyph = [255, 255, 255, look.text[3]];
        let (mut circle, glyph) = if look.dark {
            (lerp_rgba(look.off, white, on_t), lerp_rgba(off_glyph, on_col, on_t))
        } else {
            (lerp_rgba(look.off, on_col, on_t), off_glyph)
        };
        if hovered {
            circle[3] = circle[3].saturating_add(20);
        }
        let d = w.min(h);
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        let r = (d / 2.0 - 1.0).max(1.0);
        let (ps, _) = tap_scale(self.anim.pulses[pulse], now);
        fill_circle(pm, cx * s, cy * s, r * ps * s, circle);
        module_glyph(pm, s, kind, cx, cy, 10.5 * ps, glyph);
        if interactive {
            targets.push(((cx - r, cy - r, r * 2.0, r * 2.0), target));
        }
        let _ = german;
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_connectivity(
        &mut self,
        pm: &mut Pixmap,
        s: f32,
        rect: (f32, f32, f32, f32),
        look: CcLook,
        fonts: &Fonts,
        german: bool,
        interactive: bool,
        targets: &mut Vec<((f32, f32, f32, f32), Target)>,
        now: Instant,
    ) {
        let (x, y, w, h) = rect;
        let network_eth = self.control.show_ethernet() && !self.control.wifi_connected();
        let first = if network_eth { NetRow::Ethernet } else { NetRow::Wifi };
        let second = NetRow::Bluetooth;
        let stacked = h > w * 0.72;
        if stacked {
            let row_h = ((h - GROUP_GAP) / 2.0).max(1.0);
            let top = (x, y, w, row_h);
            let bottom = (x, y + row_h + GROUP_GAP, w, row_h);
            self.paint_connectivity_row(pm, s, top, first, look, fonts, german, interactive, targets, now);
            self.paint_connectivity_row(pm, s, bottom, second, look, fonts, german, interactive, targets, now);
        } else if w > CC_CELL * 3.0 {
            // Full width: two pill rows side by side, like iOS landscape.
            let col_w = (w - GROUP_GAP) / 2.0;
            self.paint_connectivity_row(pm, s, (x, y, col_w, h), first, look, fonts, german, interactive, targets, now);
            self.paint_connectivity_row(
                pm,
                s,
                (x + col_w + GROUP_GAP, y, col_w, h),
                second,
                look,
                fonts,
                german,
                interactive,
                targets,
                now,
            );
        } else {
            self.paint_connectivity_row(pm, s, rect, first, look, fonts, german, interactive, targets, now);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_connectivity_row(
        &mut self,
        pm: &mut Pixmap,
        s: f32,
        rect: (f32, f32, f32, f32),
        row: NetRow,
        look: CcLook,
        fonts: &Fonts,
        german: bool,
        interactive: bool,
        targets: &mut Vec<((f32, f32, f32, f32), Target)>,
        now: Instant,
    ) {
        let t = |en: &'static str, de: &'static str| if german { de } else { en };
        let (x, y, w, h) = rect;
        let radius = (h / 2.0).min(w / 2.0);
        let hovered = interactive && self.module_hovered(x, y, w, h);
        fill_rounded(
            pm,
            x * s,
            y * s,
            w * s,
            h * s,
            radius * s,
            if hovered { look.card_hover } else { look.card },
        );
        stroke_rounded(pm, x * s, y * s, w * s, h * s, radius * s, 1.0 * s, look.border);

        let (on_t, tint, title, status, available, toggle, detail, pulse, icon) = match row {
            NetRow::Wifi => {
                let (title, status) = match &self.control.wifi {
                    Some((true, Some(ssid))) => (ssid.clone(), t("Connected", "Verbunden").to_string()),
                    Some((true, None)) => ("Wi-Fi".into(), t("Not Connected", "Nicht verbunden").to_string()),
                    Some((false, _)) => ("Wi-Fi".into(), t("Off", "Aus").to_string()),
                    None => ("Wi-Fi".into(), t("Unavailable", "Nicht verfügbar").to_string()),
                };
                (
                    self.anim.wifi.sample(now).clamp(0.0, 1.0),
                    look.accent,
                    title,
                    status,
                    self.control.wifi.is_some(),
                    Target::WifiToggle,
                    Submenu::Wifi,
                    PULSE_WIFI,
                    NetRow::Wifi,
                )
            }
            NetRow::Ethernet => {
                let dev = self.control.ethernet.first();
                let title = dev
                    .and_then(|d| d.connection.clone())
                    .or_else(|| dev.map(|d| d.device.clone()))
                    .unwrap_or_else(|| "Ethernet".into());
                let status = match dev {
                    Some(d) if d.connected => d
                        .ip
                        .clone()
                        .unwrap_or_else(|| t("Connected", "Verbunden").to_string()),
                    Some(d) if d.plugged => t("Disconnected", "Getrennt").to_string(),
                    Some(_) => t("Cable unplugged", "Kein Kabel").to_string(),
                    None => t("Unavailable", "Nicht verfügbar").to_string(),
                };
                (
                    self.anim.ethernet.sample(now).clamp(0.0, 1.0),
                    look.accent,
                    title,
                    status,
                    dev.is_some_and(|d| d.managed && d.plugged),
                    Target::EthernetToggle,
                    Submenu::Ethernet,
                    PULSE_WIFI,
                    NetRow::Ethernet,
                )
            }
            NetRow::Bluetooth => {
                let connected: Vec<&str> = self
                    .control
                    .bt_devices
                    .iter()
                    .filter(|d| d.connected)
                    .map(|d| d.name.as_str())
                    .collect();
                let status = match self.control.bluetooth {
                    Some(true) => match connected.len() {
                        0 => t("On", "Ein").to_string(),
                        1 => connected[0].to_string(),
                        n => {
                            if german {
                                format!("{n} Geräte")
                            } else {
                                format!("{n} devices")
                            }
                        }
                    },
                    Some(false) => t("Off", "Aus").to_string(),
                    None => t("Unavailable", "Nicht verfügbar").to_string(),
                };
                (
                    self.anim.bluetooth.sample(now).clamp(0.0, 1.0),
                    look.accent,
                    "Bluetooth".to_string(),
                    status,
                    self.control.bluetooth.is_some(),
                    Target::BluetoothToggle,
                    Submenu::Bluetooth,
                    PULSE_BT,
                    NetRow::Bluetooth,
                )
            }
        };

        // White circle + tinted symbol when on, grey circle + white symbol off.
        let icon_d = (h - 14.0).clamp(24.0, 54.0);
        let icon_r = icon_d / 2.0;
        let (icx, icy) = (x + icon_r + 7.0, y + h / 2.0);
        let white = [250, 250, 252, look.text[3]];
        let off_glyph = [255, 255, 255, look.text[3]];
        let circle = if look.dark {
            lerp_rgba(look.off, white, on_t)
        } else {
            lerp_rgba(look.off, tint, on_t)
        };
        let glyph = if look.dark {
            lerp_rgba(off_glyph, tint, on_t)
        } else {
            off_glyph
        };
        let (ps, _) = tap_scale(self.anim.pulses[pulse], now);
        fill_circle(pm, icx * s, icy * s, icon_r * ps * s, circle);
        match icon {
            NetRow::Wifi => {
                crate::menubar::draw_network(pm, icx * s, (icy - 1.0) * s, s * 0.8 * ps, Network::Wifi(3), glyph)
            }
            NetRow::Ethernet => {
                crate::menubar::draw_network(pm, icx * s, icy * s, s * 0.92 * ps, Network::Wired, glyph)
            }
            NetRow::Bluetooth => bluetooth(pm, icx * s, icy * s, 7.4 * ps * s, glyph),
        }

        let tx = icx + icon_r + 9.0;
        let maxw = (x + w - 12.0 - tx).max(24.0);
        let title_base = fonts.baseline_centered((y + h / 2.0 - 15.0) * s, 18.0 * s, 11.5 * s, Weight::Bold);
        fonts.draw(
            pm,
            &fonts.elide(&title, 11.5 * s, Weight::Bold, maxw * s),
            tx * s,
            title_base,
            11.5 * s,
            Weight::Bold,
            look.text,
        );
        let status_base = fonts.baseline_centered((y + h / 2.0 + 3.0) * s, 16.0 * s, 10.0 * s, Weight::Regular);
        fonts.draw(
            pm,
            &fonts.elide(&status, 10.0 * s, Weight::Regular, maxw * s),
            tx * s,
            status_base,
            10.0 * s,
            Weight::Regular,
            look.sub,
        );

        if interactive {
            targets.push((rect, Target::OpenSubmenu(detail)));
            if available {
                targets.push(((icx - icon_r, icy - icon_r, icon_r * 2.0, icon_r * 2.0), toggle));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_now_playing(
        &mut self,
        pm: &mut Pixmap,
        s: f32,
        rect: (f32, f32, f32, f32),
        look: CcLook,
        fonts: &Fonts,
        theme: &Theme,
        german: bool,
        interactive: bool,
        targets: &mut Vec<((f32, f32, f32, f32), Target)>,
    ) {
        let t = |en: &'static str, de: &'static str| if german { de } else { en };
        let (x, y, w, h) = rect;
        let large = h > CC_CELL + 10.0;
        let media = self.control.media.clone();
        let has = media.is_some();
        let art = 52.0;
        if large {
            let ax = x + MODULE_PAD;
            let ay = y + MODULE_PAD;
            let art_col = if has { scale_alpha(look.accent, 0.92) } else { look.off };
            fill_rounded(pm, ax * s, ay * s, art * s, art * s, 14.0 * s, art_col);
            let fade = look.text[3] as f32 / 255.0;
            fill_rounded(
                pm,
                (ax + 4.0) * s,
                (ay + 4.0) * s,
                (art - 8.0) * s,
                (art - 8.0) * s,
                10.0 * s,
                scale_alpha([255, 255, 255, 255], (if has { 0.16 } else { 0.06 }) * fade),
            );
            music_note(
                pm,
                (ax + art / 2.0) * s,
                (ay + art / 2.0) * s,
                (art * 0.19) * s,
                if has { [255, 255, 255, look.text[3]] } else { look.faint },
            );
        }

        let (title, artist, playing) = match &media {
            Some(m) => (
                if m.title.is_empty() {
                    t("Not Playing", "Keine Wiedergabe").to_string()
                } else {
                    m.title.clone()
                },
                m.artist.clone(),
                m.playing,
            ),
            None => (t("Not Playing", "Keine Wiedergabe").to_string(), String::new(), false),
        };
        let (text_x, title_base, artist_base, maxw) = if large {
            (
                x + MODULE_PAD,
                fonts.baseline_centered((y + 70.0) * s, 20.0 * s, 12.5 * s, Weight::Bold),
                fonts.baseline_centered((y + 87.0) * s, 16.0 * s, 10.5 * s, Weight::Regular),
                w - MODULE_PAD * 2.0,
            )
        } else {
            (
                x + MODULE_PAD,
                fonts.baseline_centered((y + 12.0) * s, 18.0 * s, 12.5 * s, Weight::Bold),
                fonts.baseline_centered((y + 31.0) * s, 16.0 * s, 10.5 * s, Weight::Regular),
                w - MODULE_PAD - 48.0,
            )
        };
        let title = fonts.elide(&title, 12.5 * s, Weight::Bold, maxw.max(30.0) * s);
        fonts.draw(pm, &title, text_x * s, title_base, 12.5 * s, Weight::Bold, look.text);
        let artist = fonts.elide(if artist.is_empty() { t("Nothing playing", "Nichts läuft") } else { &artist }, 10.5 * s, Weight::Regular, maxw.max(30.0) * s);
        fonts.draw(pm, &artist, text_x * s, artist_base, 10.5 * s, Weight::Regular, look.sub);

        // Plain transport glyphs, like the iOS music control.
        if large {
            let (bx, cy, step) = (x + w / 2.0 - 34.0, y + h - 22.0, 34.0);
            for (i, target) in [Target::MediaPrev, Target::MediaPlay, Target::MediaNext].into_iter().enumerate() {
                let cx = bx + i as f32 * step;
                let glyph = if has { look.text } else { look.faint };
                match target {
                    Target::MediaPrev => transport(pm, cx * s, cy * s, 6.0 * s, -1, glyph),
                    Target::MediaNext => transport(pm, cx * s, cy * s, 6.0 * s, 1, glyph),
                    _ if playing => pause(pm, cx * s, cy * s, 7.0 * s, glyph),
                    _ => play(pm, cx * s, cy * s, 7.6 * s, glyph),
                }
                if interactive && has {
                    targets.push(((cx - 16.0, cy - 16.0, 32.0, 32.0), target));
                }
            }
        } else {
            let (cx, cy) = (x + w - 26.0, y + h / 2.0);
            let glyph = if has { look.text } else { look.faint };
            if playing {
                pause(pm, cx * s, cy * s, 6.4 * s, glyph);
            } else {
                play(pm, cx * s, cy * s, 7.0 * s, glyph);
            }
            if interactive && has {
                targets.push(((cx - 18.0, cy - 18.0, 36.0, 36.0), Target::MediaPlay));
            }
        }
        let _ = theme;
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_vertical_slider(
        &mut self,
        pm: &mut Pixmap,
        s: f32,
        rect: (f32, f32, f32, f32),
        kind: ControlKind,
        look: CcLook,
        interactive: bool,
        targets: &mut Vec<((f32, f32, f32, f32), Target)>,
        now: Instant,
    ) {
        let (x, y, w, h) = rect;
        let brightness = kind == ControlKind::Brightness;
        let (value, muted, available) = if brightness {
            let animated = self.anim.displays.first_mut().map(|sp| sp.sample(now));
            let value = animated
                .or_else(|| self.control.displays.first().map(|d| d.value))
                .unwrap_or(0.0);
            (value.clamp(0.0, 1.0), false, !self.control.displays.is_empty())
        } else {
            let (_, muted) = self.control.volume.unwrap_or((0.0, true));
            (self.anim.vol.sample(now).clamp(0.0, 1.0), muted, self.control.volume.is_some())
        };
        let hovered = interactive && self.module_hovered(x, y, w, h);
        let r = w / 2.0;
        let alpha = look.text[3] as f32 / 255.0;
        let white = [252, 252, 255, (248.0 * alpha).round().clamp(0.0, 255.0) as u8];

        fill_rounded(pm, x * s, y * s, w * s, h * s, r * s, look.off);
        if hovered {
            fill_rounded(
                pm,
                x * s,
                y * s,
                w * s,
                h * s,
                r * s,
                [255, 255, 255, (14.0 * alpha).round() as u8],
            );
        }
        let fh = h * value;
        fill_vertical_slider(pm, x * s, (y + h - fh) * s, w * s, fh * s, white);
        stroke_rounded(pm, x * s, y * s, w * s, h * s, r * s, 1.0 * s, look.border);

        let icon_cy = y + h - 23.0;
        let icon = if brightness {
            [255, 204, 0, look.text[3]]
        } else {
            [look.accent[0], look.accent[1], look.accent[2], look.text[3]]
        };
        if brightness {
            crate::osd::sun(pm, (x + w / 2.0) * s, icon_cy * s, 8.4 * s, value, icon);
        } else {
            crate::osd::speaker(
                pm,
                (x + w / 2.0) * s,
                icon_cy * s,
                7.8 * s,
                value,
                muted || !available,
                icon,
            );
        }

        if interactive && available {
            let target = if brightness {
                Target::BrightnessVertical(0, y + 4.0, y + h - 4.0)
            } else {
                Target::VolumeVertical(y + 4.0, y + h - 4.0)
            };
            targets.push(((x, y, w, h), target));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_slider_module(
        &mut self,
        pm: &mut Pixmap,
        s: f32,
        rect: (f32, f32, f32, f32),
        kind: ControlKind,
        look: CcLook,
        fonts: &Fonts,
        german: bool,
        now: Instant,
        interactive: bool,
        targets: &mut Vec<((f32, f32, f32, f32), Target)>,
    ) {
        let t = |en: &'static str, de: &'static str| if german { de } else { en };
        let (x, y, w, h) = rect;
        let brightness = kind == ControlKind::Brightness;
        let (value, muted, available) = if brightness {
            let animated = self.anim.displays.first_mut().map(|sp| sp.sample(now));
            let value = animated
                .or_else(|| self.control.displays.first().map(|d| d.value))
                .unwrap_or(0.0);
            (value.clamp(0.0, 1.0), false, !self.control.displays.is_empty())
        } else {
            let (_, muted) = self.control.volume.unwrap_or((0.0, true));
            (self.anim.vol.sample(now).clamp(0.0, 1.0), muted, self.control.volume.is_some())
        };
        let title = if brightness { t("Display", "Display") } else { t("Sound", "Ton") };
        // Leave room for the edit-mode remove/resize badges.
        let inset = if self.cc.edit { 18.0 } else { 0.0 };
        let base = fonts.baseline_centered((y + 12.0) * s, 18.0 * s, 11.5 * s, Weight::Bold);
        fonts.draw(pm, title, (x + MODULE_PAD + inset) * s, base, 11.5 * s, Weight::Bold, look.text);
        let value_text = if !available {
            "—".to_string()
        } else if muted {
            t("Muted", "Stumm").to_string()
        } else {
            format!("{}%", (value * 100.0).round() as u32)
        };
        let vw = fonts.measure(&value_text, 10.0 * s, Weight::Regular) / s;
        let chev = !self.cc.edit && if brightness { self.control.displays.len() > 1 } else { true };
        let vx = x + w - MODULE_PAD - inset - vw - if chev { 12.0 } else { 0.0 };
        let vb = fonts.baseline_centered((y + 12.0) * s, 18.0 * s, 10.0 * s, Weight::Regular);
        fonts.draw(pm, &value_text, vx * s, vb, 10.0 * s, Weight::Regular, look.sub);
        if chev {
            chevron_right(pm, (x + w - MODULE_PAD - 2.0 - inset) * s, (y + 21.0) * s, 3.5 * s, look.faint);
        }

        let (tx, ty, tw, th) = (
            x + MODULE_PAD + inset,
            y + 38.0,
            w - MODULE_PAD * 2.0 - inset * 2.0,
            23.0,
        );
        let track_hover = self.module_hovered(tx - 4.0, ty - 4.0, tw + 8.0, th + 8.0);
        let dragging = matches!(self.dragging, Some(Target::Volume(..)) if !brightness)
            || matches!(self.dragging, Some(Target::Brightness(0, ..)) if brightness);
        draw_pill_slider(
            pm,
            s,
            tx,
            ty,
            tw,
            th,
            value,
            if brightness { 0 } else { 1 },
            muted,
            look.off,
            track_hover,
            dragging,
            look.text[3] as f32 / 255.0,
        );
        if interactive {
            if available {
                let target = if brightness {
                    Target::Brightness(0, tx + th / 2.0, tx + tw - th / 2.0)
                } else {
                    Target::Volume(tx + th / 2.0, tx + tw - th / 2.0)
                };
                targets.push(((tx - 4.0, ty - 4.0, tw + 8.0, th + 8.0), target));
                if !brightness {
                    // The little speaker inside the track toggles mute.
                    targets.push(((tx - 3.0, ty - 4.0, 28.0, th + 8.0), Target::VolumeMute));
                }
            }
            // Header chevron opens the detail sheet.
            if chev {
                let detail = if brightness {
                    Target::OpenSettings("displays")
                } else {
                    Target::OpenSubmenu(Submenu::Sound)
                };
                targets.push(((x + w - MODULE_PAD - 26.0, y + 4.0, 34.0, 28.0), detail));
            }
        }
        let _ = h;
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_small_module(
        &mut self,
        pm: &mut Pixmap,
        s: f32,
        rect: (f32, f32, f32, f32),
        kind: ControlKind,
        look: CcLook,
        fonts: &Fonts,
        german: bool,
        interactive: bool,
        targets: &mut Vec<((f32, f32, f32, f32), Target)>,
        now: Instant,
    ) {
        let (x, y, w, h) = rect;
        let (on_t, on_col, pulse, target) = match kind {
            ControlKind::Wifi => (
                self.anim.wifi.sample(now).clamp(0.0, 1.0),
                look.accent,
                PULSE_WIFI,
                if self.control.wifi.is_some() {
                    Target::WifiToggle
                } else {
                    Target::OpenSubmenu(Submenu::Wifi)
                },
            ),
            ControlKind::Bluetooth => (
                self.anim.bluetooth.sample(now).clamp(0.0, 1.0),
                look.accent,
                PULSE_BT,
                if self.control.bluetooth.is_some() {
                    Target::BluetoothToggle
                } else {
                    Target::OpenSubmenu(Submenu::Bluetooth)
                },
            ),
            ControlKind::Focus => (
                self.anim.dnd.sample(now).clamp(0.0, 1.0),
                [94, 92, 230, 255],
                PULSE_DND,
                Target::Dnd,
            ),
            ControlKind::NightShift => (
                self.anim.night_shift.sample(now).clamp(0.0, 1.0),
                [255, 149, 0, 255],
                PULSE_NIGHT,
                Target::NightShift,
            ),
            ControlKind::GameMode => (
                self.anim.game_mode.sample(now).clamp(0.0, 1.0),
                [52, 199, 89, 255],
                PULSE_GAME,
                Target::GameMode,
            ),
            ControlKind::Displays => (0.0, look.accent, PULSE_GAME, Target::OpenSettings("displays")),
            ControlKind::Network => (0.0, look.accent, PULSE_WIFI, Target::OpenSubmenu(Submenu::Wifi)),
            ControlKind::Sound => (0.0, look.accent, PULSE_WIFI, Target::OpenSubmenu(Submenu::Sound)),
            _ => (0.0, look.accent, PULSE_WIFI, Target::Panel),
        };
        let white = [250, 250, 252, look.text[3]];
        let circle = if look.dark {
            lerp_rgba(look.off, white, on_t)
        } else {
            lerp_rgba(look.off, on_col, on_t)
        };
        let wide = w > CC_CELL + 10.0;
        let tall = h > CC_CELL + 10.0;
        let icon_r = if wide { (h / 2.0 - 9.0).clamp(12.0, 26.0) } else { 16.0 };
        let (cx, cy) = if wide {
            (x + 7.0 + icon_r, y + h / 2.0)
        } else if tall {
            (x + w / 2.0, y + h / 2.0 - 12.0)
        } else {
            (x + w / 2.0, y + 26.0)
        };
        let (ps, _) = tap_scale(self.anim.pulses[pulse], now);
        fill_circle(pm, cx * s, cy * s, icon_r * ps * s, circle);
        let off_glyph = [255, 255, 255, look.text[3]];
        let glyph = if look.dark {
            lerp_rgba(off_glyph, on_col, on_t)
        } else {
            off_glyph
        };
        let glyph_r = 6.5 * (icon_r / 16.0);
        module_glyph(pm, s, kind, cx, cy, glyph_r * ps, glyph);
        let label = module_name(kind, german);
        if wide {
            let tx = x + 7.0 + icon_r * 2.0 + 8.0;
            let maxw = (x + w - 10.0 - tx).max(30.0);
            let base = fonts.baseline_centered((y + 19.0) * s, 18.0 * s, 11.0 * s, Weight::Bold);
            fonts.draw(pm, &fonts.elide(label, 11.0 * s, Weight::Bold, maxw * s), tx * s, base, 11.0 * s, Weight::Bold, look.text);
            let status = match kind {
                ControlKind::Displays => {
                    if german {
                        "Anpassen"
                    } else {
                        "Adjust"
                    }
                }
                ControlKind::Network => "Details",
                ControlKind::Sound => {
                    if german {
                        "Ausgabe"
                    } else {
                        "Output"
                    }
                }
                _ => {
                    if on_t > 0.5 {
                        if german {
                            "Ein"
                        } else {
                            "On"
                        }
                    } else if german {
                        "Aus"
                    } else {
                        "Off"
                    }
                }
            };
            let sb = fonts.baseline_centered((y + 39.0) * s, 16.0 * s, 9.5 * s, Weight::Regular);
            fonts.draw(pm, status, tx * s, sb, 9.5 * s, Weight::Regular, look.sub);
        } else {
            let lw = fonts.measure(label, 10.0 * s, Weight::Bold) / s;
            let top = if tall { y + h / 2.0 + 10.0 } else { y + 46.0 };
            let base = fonts.baseline_centered(top * s, 22.0 * s, 10.0 * s, Weight::Bold);
            fonts.draw(pm, label, (x + (w - lw) / 2.0) * s, base, 10.0 * s, Weight::Bold, look.text);
        }
        if interactive {
            targets.push(((x, y, w, h), target));
        }
        let _ = (h, pulse);
    }


    // ---- Control Center submenus (1:1 macOS) ---------------------------------

    #[allow(clippy::too_many_arguments)]
    fn paint_cc_submenu(
        &mut self,
        pm: &mut Pixmap,
        s: f32,
        x0: f32,
        y0: f32,
        h: f32,
        submenu: Submenu,
        apps: &Apps,
        fonts: &Fonts,
        theme: &Theme,
        german: bool,
        now: Instant,
    ) {
        let t = |en: &'static str, de: &'static str| if german { de } else { en };
        let dark = theme.bar_text[0] > 128;
        let solid = theme.menu_bg[3] > 200;
        let tint: Rgba = if dark {
            if solid { [28, 28, 30, 248] } else { [18, 18, 22, 128] }
        } else if solid {
            [244, 244, 247, 250]
        } else {
            [246, 246, 250, 155]
        };
        let border: Rgba = if dark { [255, 255, 255, 30] } else { [0, 0, 0, 18] };
        let off: Rgba = if dark { [255, 255, 255, 36] } else { [0, 0, 0, 22] };
        let hover_bg: Rgba = if dark { [255, 255, 255, 22] } else { [0, 0, 0, 13] };
        let sep: Rgba = if dark { [255, 255, 255, 26] } else { [0, 0, 0, 22] };
        let accent = theme.menu_selection;
        let text = theme.menu_text;
        let sub_text: Rgba = if dark { [235, 235, 235, 138] } else { [20, 20, 20, 128] };

        let slide_x = if self.standalone { 0.0 } else { (1.0 - self.sub_progress()) * 14.0 };
        let wifi_t = self.anim.wifi.sample(now).clamp(0.0, 1.0);
        let eth_t = self.anim.ethernet.sample(now).clamp(0.0, 1.0);
        let bt_t = self.anim.bluetooth.sample(now).clamp(0.0, 1.0);
        let others_t = self.anim.wifi_others.sample(now).clamp(0.0, 1.0);
        let vol_val = self.anim.vol.sample(now).clamp(0.0, 1.0);

        // Pre-build the scrollable content items for each submenu
        let (title, toggle_t, toggle_target, settings_label, settings_page) = match submenu {
            Submenu::Wifi => {
                if self.control.wifi.is_some() {
                    (
                        "Wi-Fi",
                        Some(wifi_t),
                        Some(Target::WifiToggle),
                        t("Wi-Fi Settings…", "WLAN-Einstellungen …"),
                        "wifi",
                    )
                } else {
                    (
                        t("Network", "Netzwerk"),
                        None,
                        None,
                        t("Network Settings…", "Netzwerkeinstellungen …"),
                        "network",
                    )
                }
            }
            Submenu::Ethernet => {
                let eth_toggleable = self.control.ethernet.iter().any(|e| e.managed && e.plugged);
                (
                    "Ethernet",
                    eth_toggleable.then_some(eth_t),
                    eth_toggleable.then_some(Target::EthernetToggle),
                    t("Network Settings…", "Netzwerkeinstellungen …"),
                    "network",
                )
            }
            Submenu::Bluetooth => (
                "Bluetooth",
                Some(bt_t),
                Some(Target::BluetoothToggle),
                t("Bluetooth Settings…", "Bluetooth-Einstellungen …"),
                "bluetooth",
            ),
            Submenu::Sound => (
                t("Sound", "Ton"),
                None,
                None,
                t("Sound Settings…", "Toneinstellungen …"),
                "sound",
            ),
        };

        // Header geometry
        let header_h = 44.0;
        // Sound submenu puts the pill slider directly below the header (no divider above slider)
        let sound_slider_h = if submenu == Submenu::Sound { 34.0 } else { 0.0 };
        let footer_h = 42.0; // divider (8px) + button (30px) + bottom pad (4px)

        // Compute content height for the list area
        let content_h = match submenu {
            Submenu::Wifi => {
                let wifi_on = self.control.wifi.as_ref().map(|w| w.0).unwrap_or(false);
                let show_eth_link = self.control.ethernet.iter().any(|e| e.connected || e.plugged);
                if !wifi_on && !show_eth_link {
                    0.0
                } else {
                    let mut ch = 4.0;
                    if wifi_on {
                        let known_count = self.control.wifi_networks.iter().filter(|n| n.active || n.known).count();
                        let other_count = self.control.wifi_networks.iter().filter(|n| !n.active && !n.known).count();
                        if known_count > 0 {
                            ch += 24.0 + known_count as f32 * 38.0 + 4.0;
                            if other_count > 0 {
                                ch += 9.0; // divider
                            }
                        }
                        if other_count > 0 || known_count == 0 {
                            ch += 26.0; // "Other Networks" header row
                            let other_rows_h = if other_count == 0 { 34.0 } else { other_count as f32 * 38.0 };
                            ch += other_rows_h * others_t;
                        }
                    }
                    if show_eth_link {
                        if wifi_on {
                            ch += 4.0 + 9.0; // padding + divider
                        }
                        ch += 24.0 + 38.0; // "Ethernet" header + summary row
                    }
                    ch + 4.0
                }
            }
            Submenu::Ethernet => {
                let count = self.control.ethernet.len();
                let mut ch = 4.0 + 24.0 + if count == 0 { 34.0 } else { count as f32 * 38.0 };
                if self.control.wifi.is_some() {
                    ch += 4.0 + 9.0 + 24.0 + 38.0; // pad + divider + "Wi-Fi" header + row
                }
                ch + 4.0
            }
            Submenu::Bluetooth => {
                let bt_on = self.control.bluetooth == Some(true);
                if !bt_on {
                    0.0
                } else {
                    let count = self.control.bt_devices.len();
                    4.0 + 24.0 + if count == 0 { 34.0 } else { count as f32 * 38.0 } + 4.0
                }
            }
            Submenu::Sound => {
                let count = self.control.audio_sinks.len();
                let apps_h = if self.control.streams.is_empty() {
                    0.0
                } else {
                    9.0 + 24.0 + self.control.streams.len() as f32 * APP_ROW_H
                };
                4.0 + 24.0 + if count == 0 { 34.0 } else { count as f32 * 38.0 } + apps_h + 4.0
            }
        };

        let top_chrome = 4.0 + header_h + sound_slider_h + if content_h > 0.0 { 1.0 } else { 0.0 };
        let max_list_h = (h - y0 - MARGIN - top_chrome - footer_h).clamp(110.0, 380.0);
        let visible_list_h = content_h.min(max_list_h);
        let target_h = top_chrome + visible_list_h + footer_h;
        if !self.anim.panel_h_init {
            self.anim.panel_h.snap(target_h);
            self.anim.panel_h_init = true;
        } else {
            self.anim.panel_h.set_target(target_h, now);
        }
        let total_h = self.anim.panel_h.sample(now).max(top_chrome + footer_h);
        let body_h = (total_h - top_chrome - footer_h).max(0.0);

        self.panel_rect = (x0, y0, CC_WIDTH, total_h);
        fill_rounded(pm, x0 * s, y0 * s, CC_WIDTH * s, total_h * s, CC_RADIUS * s, tint);
        stroke_rounded(pm, x0 * s, y0 * s, CC_WIDTH * s, total_h * s, CC_RADIUS * s, 1.0 * s, border);

        // ---- Header ----
        let hy = y0 + 4.0;
        let hb = fonts.baseline_centered(hy * s, header_h * s, 13.5 * s, Weight::Bold);
        if self.standalone {
            fonts.draw(pm, title, (x0 + 16.0) * s, hb, 14.5 * s, Weight::Bold, text);
        } else {
            let back_rect = (x0 + 8.0, hy + 4.0, if toggle_t.is_some() { CC_WIDTH - 72.0 } else { CC_WIDTH - 16.0 }, 36.0);
            let back_hovered = self.hovered(back_rect.0, back_rect.1, back_rect.2, back_rect.3);
            if back_hovered {
                fill_rounded(pm, back_rect.0 * s, back_rect.1 * s, back_rect.2 * s, back_rect.3 * s, 8.0 * s, hover_bg);
            }
            chevron_left(pm, (x0 + 20.0 + slide_x) * s, (hy + 22.0) * s, 4.5 * s, if back_hovered { text } else { sub_text });
            fonts.draw(pm, title, (x0 + 34.0 + slide_x) * s, hb, 14.5 * s, Weight::Bold, text);
            self.targets.push((back_rect, Target::SubmenuBack));
        }

        if let (Some(sw_t), Some(t_target)) = (toggle_t, toggle_target) {
            let sx = x0 + CC_WIDTH - 16.0 - 38.0;
            let sy = hy + 11.0;
            switch_anim(pm, s, sx, sy, sw_t, accent, dark);
            self.targets.push(((sx - 4.0, sy - 4.0, 46.0, 30.0), t_target));
        }

        // ---- Sound slider (directly under Sound header) ----
        let mut cur_y = hy + header_h;
        if submenu == Submenu::Sound {
            let (_, muted) = self.control.volume.unwrap_or((0.0, true));
            let (tx, ty, tw, th) = (x0 + 16.0, cur_y + 2.0, CC_WIDTH - 32.0, 22.0);
            let vol_hov = self.hovered(tx - 2.0, ty - 4.0, tw + 4.0, th + 8.0);
            let vol_drag = matches!(self.dragging, Some(Target::Volume(..)));
            draw_pill_slider(pm, s, tx, ty, tw, th, vol_val, 1, muted, off, vol_hov, vol_drag, 1.0);
            // Clicking the speaker icon toggles mute; dragging anywhere else sets volume
            self.targets.push(((tx - 2.0, ty - 4.0, 24.0, th + 8.0), Target::VolumeMute));
            self.targets
                .push(((tx + 22.0, ty - 4.0, tw - 18.0, th + 8.0), Target::Volume(tx + th / 2.0, tx + tw - th / 2.0)));
            cur_y += sound_slider_h;
        }

        // ---- Scrollable body ----
        if content_h > 0.0 && body_h > 1.0 {
            draw_sep(pm, s, x0 + 16.0, cur_y, CC_WIDTH - 32.0, sep);
            cur_y += 1.0;

            let list_y = cur_y;
            self.scroll = self.scroll.min((content_h - body_h).max(0.0));
            let scroll = self.scroll;

            if let Some(mut sub_pm) = Pixmap::new((CC_WIDTH * s).ceil().max(1.0) as u32, (body_h * s).ceil().max(1.0) as u32) {
                let mut local_targets: Vec<((f32, f32, f32, f32), Target)> = Vec::new();
                let hover_local = self.hover.and_then(|(hx, hy)| {
                    if hy >= list_y && hy < list_y + body_h && hx >= x0 && hx < x0 + CC_WIDTH {
                        Some((hx - x0, hy - list_y))
                    } else {
                        None
                    }
                });
                let is_hov = |rx: f32, ry: f32, rw: f32, rh: f32| {
                    hover_local
                        .map(|(hx, hy)| hx >= rx && hx < rx + rw && hy >= ry && hy < ry + rh)
                        .unwrap_or(false)
                };

                let mut ly = 4.0 - scroll;

                match submenu {
                    Submenu::Wifi => {
                        let wifi_on = self.control.wifi.as_ref().map(|w| w.0).unwrap_or(false);
                        let show_eth_link = self.control.ethernet.iter().any(|e| e.connected || e.plugged);
                        if wifi_on {
                            let known: Vec<WifiNetwork> = self
                                .control
                                .wifi_networks
                                .iter()
                                .filter(|n| n.active || n.known)
                                .cloned()
                                .collect();
                            let others: Vec<WifiNetwork> = self
                                .control
                                .wifi_networks
                                .iter()
                                .filter(|n| !n.active && !n.known)
                                .cloned()
                                .collect();

                            if !known.is_empty() {
                                let lbl = if known.len() == 1 {
                                    t("Known Network", "Bekanntes Netzwerk")
                                } else {
                                    t("Known Networks", "Bekannte Netzwerke")
                                };
                                draw_section_header(&mut sub_pm, s, 16.0 + slide_x, ly, lbl, fonts, sub_text);
                                ly += 24.0;
                                for net in &known {
                                    let row = (8.0, ly, CC_WIDTH - 16.0, 38.0);
                                    if is_hov(row.0, row.1, row.2, row.3) {
                                        fill_rounded(&mut sub_pm, row.0 * s, row.1 * s, row.2 * s, row.3 * s, 8.0 * s, hover_bg);
                                    }
                                    draw_wifi_row(
                                        &mut sub_pm,
                                        s,
                                        slide_x,
                                        ly,
                                        net,
                                        self.busy_item.as_deref() == Some(net.ssid.as_str()),
                                        fonts,
                                        accent,
                                        off,
                                        text,
                                        sub_text,
                                        german,
                                    );
                                    local_targets.push((
                                        row,
                                        Target::WifiConnect {
                                            ssid: net.ssid.clone(),
                                            known: true,
                                            secure: net.secure,
                                        },
                                    ));
                                    ly += 38.0;
                                }
                                ly += 4.0;
                                if !others.is_empty() {
                                    draw_sep(&mut sub_pm, s, 16.0, ly + 4.0, CC_WIDTH - 32.0, sep);
                                    ly += 9.0;
                                }
                            }

                            if !others.is_empty() || known.is_empty() {
                                let hdr_rect = (6.0, ly, CC_WIDTH - 12.0, 26.0);
                                if is_hov(hdr_rect.0, hdr_rect.1, hdr_rect.2, hdr_rect.3) {
                                    fill_rounded(&mut sub_pm, hdr_rect.0 * s, hdr_rect.1 * s, hdr_rect.2 * s, hdr_rect.3 * s, 6.0 * s, hover_bg);
                                }
                                draw_section_header(&mut sub_pm, s, 16.0 + slide_x, ly + 1.0, t("Other Networks", "Andere Netzwerke"), fonts, sub_text);
                                chevron_rotated(
                                    &mut sub_pm,
                                    (CC_WIDTH - 18.0) * s,
                                    (ly + 13.0) * s,
                                    4.0 * s,
                                    others_t * std::f32::consts::FRAC_PI_2,
                                    sub_text,
                                );
                                local_targets.push((hdr_rect, Target::WifiToggleOthers));
                                ly += 26.0;

                                if others_t > 0.02 {
                                    if others.is_empty() {
                                        let b = fonts.baseline_centered(ly * s, 34.0 * s, 12.0 * s, Weight::Regular);
                                        fonts.draw(&mut sub_pm, t("No Networks Found", "Keine Netzwerke gefunden"), (16.0 + slide_x) * s, b, 12.0 * s, Weight::Regular, sub_text);
                                        ly += 34.0 * others_t;
                                    } else {
                                        for net in &others {
                                            let row = (8.0, ly, CC_WIDTH - 16.0, 38.0);
                                            if is_hov(row.0, row.1, row.2, row.3) {
                                                fill_rounded(&mut sub_pm, row.0 * s, row.1 * s, row.2 * s, row.3 * s, 8.0 * s, hover_bg);
                                            }
                                            draw_wifi_row(
                                                &mut sub_pm,
                                                s,
                                                slide_x,
                                                ly,
                                                net,
                                                self.busy_item.as_deref() == Some(net.ssid.as_str()),
                                                fonts,
                                                accent,
                                                off,
                                                text,
                                                sub_text,
                                                german,
                                            );
                                            if self.wifi_others_open {
                                                local_targets.push((
                                                    row,
                                                    Target::WifiConnect {
                                                        ssid: net.ssid.clone(),
                                                        known: false,
                                                        secure: net.secure,
                                                    },
                                                ));
                                            }
                                            ly += 38.0;
                                        }
                                    }
                                }
                            }
                        }

                        if show_eth_link {
                            if wifi_on {
                                ly += 4.0;
                                draw_sep(&mut sub_pm, s, 16.0, ly + 4.0, CC_WIDTH - 32.0, sep);
                                ly += 9.0;
                            }
                            draw_section_header(&mut sub_pm, s, 16.0 + slide_x, ly, "Ethernet", fonts, sub_text);
                            ly += 24.0;
                            if let Some(dev) = self.control.ethernet.first() {
                                let row = (8.0, ly, CC_WIDTH - 16.0, 38.0);
                                if is_hov(row.0, row.1, row.2, row.3) {
                                    fill_rounded(&mut sub_pm, row.0 * s, row.1 * s, row.2 * s, row.3 * s, 8.0 * s, hover_bg);
                                }
                                let status = if dev.connected {
                                    dev.ip.as_deref().unwrap_or_else(|| t("Connected", "Verbunden"))
                                } else {
                                    t("Disconnected", "Getrennt")
                                };
                                let name = dev.connection.as_deref().unwrap_or(dev.device.as_str());
                                draw_network_link_row(
                                    &mut sub_pm,
                                    s,
                                    slide_x,
                                    ly,
                                    Network::Wired,
                                    dev.connected,
                                    name,
                                    status,
                                    fonts,
                                    accent,
                                    off,
                                    text,
                                    sub_text,
                                );
                                local_targets.push((row, Target::OpenSubmenu(Submenu::Ethernet)));
                            }
                        }
                    }

                    Submenu::Ethernet => {
                        draw_section_header(&mut sub_pm, s, 16.0 + slide_x, ly, t("Connections", "Verbindungen"), fonts, sub_text);
                        ly += 24.0;
                        if self.control.ethernet.is_empty() {
                            let msg = if self.network_hint == Network::Wired {
                                t("Connected", "Verbunden")
                            } else {
                                t("No Ethernet Interfaces", "Keine Ethernet-Schnittstellen")
                            };
                            let b = fonts.baseline_centered(ly * s, 34.0 * s, 12.0 * s, Weight::Regular);
                            fonts.draw(&mut sub_pm, msg, (16.0 + slide_x) * s, b, 12.0 * s, Weight::Regular, sub_text);
                            ly += 34.0;
                        } else {
                            for dev in &self.control.ethernet {
                                let row = (8.0, ly, CC_WIDTH - 16.0, 38.0);
                                let clickable = dev.managed && dev.plugged;
                                if clickable && is_hov(row.0, row.1, row.2, row.3) {
                                    fill_rounded(&mut sub_pm, row.0 * s, row.1 * s, row.2 * s, row.3 * s, 8.0 * s, hover_bg);
                                }
                                let busy = self.busy_item.as_deref() == Some(dev.device.as_str());
                                draw_eth_row(&mut sub_pm, s, slide_x, ly, dev, busy, fonts, accent, off, text, sub_text, german);
                                if clickable {
                                    local_targets.push((
                                        row,
                                        Target::EthToggleDevice {
                                            device: dev.device.clone(),
                                            connected: dev.connected,
                                        },
                                    ));
                                }
                                ly += 38.0;
                            }
                        }

                        if let Some((wifi_on, ref ssid)) = self.control.wifi {
                            ly += 4.0;
                            draw_sep(&mut sub_pm, s, 16.0, ly + 4.0, CC_WIDTH - 32.0, sep);
                            ly += 9.0;
                            draw_section_header(&mut sub_pm, s, 16.0 + slide_x, ly, "Wi-Fi", fonts, sub_text);
                            ly += 24.0;
                            let row = (8.0, ly, CC_WIDTH - 16.0, 38.0);
                            let circle_rect = (14.0, ly + 5.0, 28.0, 28.0);
                            let open_rect = (44.0, ly, CC_WIDTH - 52.0, 38.0);
                            if is_hov(open_rect.0, open_rect.1, open_rect.2, open_rect.3) {
                                fill_rounded(&mut sub_pm, row.0 * s, row.1 * s, row.2 * s, row.3 * s, 8.0 * s, hover_bg);
                            }
                            let status = if !wifi_on {
                                t("Off", "Aus")
                            } else {
                                ssid.as_deref().unwrap_or_else(|| t("Not Connected", "Nicht verbunden"))
                            };
                            draw_network_link_row(
                                &mut sub_pm,
                                s,
                                slide_x,
                                ly,
                                Network::Wifi(3),
                                wifi_on,
                                "Wi-Fi",
                                status,
                                fonts,
                                accent,
                                off,
                                text,
                                sub_text,
                            );
                            local_targets.push((circle_rect, Target::WifiToggle));
                            local_targets.push((open_rect, Target::OpenSubmenu(Submenu::Wifi)));
                        }
                    }

                    Submenu::Bluetooth => {
                        draw_section_header(&mut sub_pm, s, 16.0 + slide_x, ly, t("Devices", "Geräte"), fonts, sub_text);
                        ly += 24.0;
                        if self.control.bt_devices.is_empty() {
                            let b = fonts.baseline_centered(ly * s, 34.0 * s, 12.0 * s, Weight::Regular);
                            fonts.draw(&mut sub_pm, t("No Devices", "Keine Geräte"), (16.0 + slide_x) * s, b, 12.0 * s, Weight::Regular, sub_text);
                        } else {
                            for dev in &self.control.bt_devices {
                                let row = (8.0, ly, CC_WIDTH - 16.0, 38.0);
                                if is_hov(row.0, row.1, row.2, row.3) {
                                    fill_rounded(&mut sub_pm, row.0 * s, row.1 * s, row.2 * s, row.3 * s, 8.0 * s, hover_bg);
                                }
                                let busy = self.busy_item.as_deref() == Some(dev.mac.as_str());
                                draw_bt_row(&mut sub_pm, s, slide_x, ly, dev, busy, fonts, accent, off, text, sub_text, german);
                                local_targets.push((
                                    row,
                                    Target::BtToggleDevice {
                                        mac: dev.mac.clone(),
                                        connected: dev.connected,
                                    },
                                ));
                                ly += 38.0;
                            }
                        }
                    }

                    Submenu::Sound => {
                        draw_section_header(&mut sub_pm, s, 16.0 + slide_x, ly, t("Output", "Ausgabe"), fonts, sub_text);
                        ly += 24.0;
                        if self.control.audio_sinks.is_empty() {
                            let b = fonts.baseline_centered(ly * s, 34.0 * s, 12.0 * s, Weight::Regular);
                            fonts.draw(&mut sub_pm, t("No Output Devices", "Keine Ausgabegeräte"), (16.0 + slide_x) * s, b, 12.0 * s, Weight::Regular, sub_text);
                        } else {
                            for sink in &self.control.audio_sinks {
                                let row = (8.0, ly, CC_WIDTH - 16.0, 38.0);
                                if is_hov(row.0, row.1, row.2, row.3) {
                                    fill_rounded(&mut sub_pm, row.0 * s, row.1 * s, row.2 * s, row.3 * s, 8.0 * s, hover_bg);
                                }
                                draw_sink_row(&mut sub_pm, s, slide_x, ly, sink, fonts, accent, off, text);
                                local_targets.push((row, Target::AudioSelectSink(sink.name.clone())));
                                ly += 38.0;
                            }
                        }
                        // per-app volume
                        if !self.control.streams.is_empty() {
                            draw_sep(&mut sub_pm, s, 16.0, ly + 4.0, CC_WIDTH - 32.0, sep);
                            ly += 9.0;
                            draw_section_header(&mut sub_pm, s, 16.0 + slide_x, ly, t("Apps", "Apps"), fonts, sub_text);
                            ly += 24.0;
                            let stream_count = self.control.streams.len();
                            for i in 0..stream_count {
                                let st_icon = self.control.streams[i].icon.clone();
                                let st_name = self.control.streams[i].name.clone();
                                let st_muted = self.control.streams[i].muted;
                                let shown = self
                                    .anim
                                    .streams
                                    .get_mut(i)
                                    .map(|sp| sp.sample(now))
                                    .unwrap_or(if st_muted { 0.0 } else { self.control.streams[i].volume.min(1.0) });
                                let icon_px = (16.0 * s).round().max(1.0) as u32;
                                let name_x = if let Some(icon) = apps.icon(&st_icon, icon_px) {
                                    pm_draw_icon(&mut sub_pm, &icon, (16.0 + slide_x) * s, (ly + 3.0) * s, 16.0 * s);
                                    38.0
                                } else {
                                    16.0
                                };
                                let b = fonts.baseline_centered((ly + 2.0) * s, 18.0 * s, 12.0 * s, Weight::Regular);
                                let label = fonts.elide(&st_name, 12.0 * s, Weight::Regular, (CC_WIDTH - name_x - 18.0) * s);
                                fonts.draw(&mut sub_pm, &label, (name_x + slide_x) * s, b, 12.0 * s, Weight::Regular, text);
                                let (tx, ty, tw, th) = (16.0 + slide_x, ly + 22.0, CC_WIDTH - 32.0, 20.0);
                                let app_hov = is_hov(tx - 2.0, ty - 3.0, tw + 4.0, th + 6.0);
                                let app_drag = matches!(self.dragging, Some(Target::AppVolume(idx, ..)) if idx == i);
                                draw_pill_slider(&mut sub_pm, s, tx, ty, tw, th, shown, 1, st_muted, off, app_hov, app_drag, 1.0);
                                // the speaker mutes; the rest of the track sets the level.
                                // local targets get x0 added, slider ranges are absolute.
                                local_targets.push(((tx - 2.0, ty - 3.0, 24.0, th + 6.0), Target::AppMute(i)));
                                local_targets.push((
                                    (tx + 22.0, ty - 3.0, tw - 18.0, th + 6.0),
                                    Target::AppVolume(i, x0 + tx + th / 2.0, x0 + tx + tw - th / 2.0),
                                ));
                                ly += APP_ROW_H;
                            }
                        }
                    }
                }

                pm.draw_pixmap(
                    (x0 * s) as i32,
                    (list_y * s) as i32,
                    sub_pm.as_ref(),
                    &PixmapPaint::default(),
                    Transform::identity(),
                    None,
                );

                // Translate & clip local targets to the visible list viewport
                for ((rx, ry, rw, rh), target) in local_targets {
                    let y1 = ry.max(0.0);
                    let y2 = (ry + rh).min(body_h);
                    if y2 > y1 {
                        self.targets.push(((x0 + rx, list_y + y1, rw, y2 - y1), target));
                    }
                }
            }

            cur_y += body_h;
        }

        // ---- Footer ("… Settings…") ----
        draw_sep(pm, s, x0 + 16.0, cur_y + 2.0, CC_WIDTH - 32.0, sep);
        let foot_rect = (x0 + 8.0, cur_y + 6.0, CC_WIDTH - 16.0, 30.0);
        if self.hovered(foot_rect.0, foot_rect.1, foot_rect.2, foot_rect.3) {
            fill_rounded(pm, foot_rect.0 * s, foot_rect.1 * s, foot_rect.2 * s, foot_rect.3 * s, 8.0 * s, hover_bg);
        }
        let fb = fonts.baseline_centered(foot_rect.1 * s, foot_rect.3 * s, 12.5 * s, Weight::Regular);
        fonts.draw(pm, settings_label, (x0 + 18.0) * s, fb, 12.5 * s, Weight::Regular, text);
        self.targets.push((foot_rect, Target::OpenSettings(settings_page)));
    }
}

/// Height of one per-app volume row (name + slider).
const APP_ROW_H: f32 = 48.0;

/// Draw an icon pixmap scaled into a `size` square at (x, y) (device px).
fn pm_draw_icon(pm: &mut Pixmap, icon: &Pixmap, x: f32, y: f32, size: f32) {
    let k = size / icon.width().max(1) as f32;
    pm.draw_pixmap(
        0,
        0,
        icon.as_ref(),
        &PixmapPaint { quality: tiny_skia::FilterQuality::Bilinear, ..Default::default() },
        Transform::from_row(k, 0.0, 0.0, k, x, y),
        None,
    );
}

/// White fill of an iOS vertical slider: a straight top with soft corners and
/// a full semicircular bottom. One path avoids the double-alpha seam that two
/// overlapping shapes would leave on the translucent module.
fn fill_vertical_slider(pm: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, c: Rgba) {
    if w <= 0.5 || h <= 0.5 {
        return;
    }
    let r = w / 2.0;
    if h <= r {
        fill_rounded(pm, x, y, w, h, h / 2.0, c);
        return;
    }
    let x1 = x + w;
    let y1 = y + h;
    let top_r = (w * 0.10).min(w * 0.25).min(h * 0.25);
    let mut pb = PathBuilder::new();
    pb.move_to(x + top_r, y);
    pb.line_to(x1 - top_r, y);
    pb.quad_to(x1, y, x1, y + top_r);
    pb.line_to(x1, y1 - r);
    let steps = 16;
    for k in 1..=steps {
        let a = std::f32::consts::PI * k as f32 / steps as f32;
        pb.line_to(x + r + r * a.cos(), y1 - r + r * a.sin());
    }
    pb.line_to(x, y + top_r);
    pb.quad_to(x, y, x + top_r, y);
    pb.close();
    if let Some(path) = pb.finish() {
        pm.fill_path(&path, &paint(c), FillRule::Winding, Transform::identity(), None);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_pill_slider(
    pm: &mut Pixmap,
    s: f32,
    tx: f32,
    ty: f32,
    tw: f32,
    th: f32,
    value: f32,
    icon: u8,
    muted: bool,
    off: Rgba,
    hovered: bool,
    dragging: bool,
    opacity: f32,
) {
    let op = opacity.clamp(0.0, 1.0);
    let a = |v: u8| (v as f32 * op).round().clamp(0.0, 255.0) as u8;
    fill_rounded(pm, tx * s, ty * s, tw * s, th * s, (th / 2.0) * s, off);
    let fill_w = (th + (tw - th) * value.clamp(0.0, 1.0)).max(th);
    let fill_alpha = if dragging { 252 } else if hovered { 246 } else { 236 };
    fill_rounded(pm, tx * s, ty * s, fill_w * s, th * s, (th / 2.0) * s, [255, 255, 255, a(fill_alpha)]);
    let knob_x = tx + fill_w - th / 2.0;
    let knob_r = th / 2.0 - if dragging { 0.6 } else { 1.0 };
    if hovered || dragging {
        fill_circle(
            pm,
            knob_x * s,
            (ty + th / 2.0 + 0.6) * s,
            knob_r * s,
            [0, 0, 0, a(if dragging { 38 } else { 22 })],
        );
    }
    fill_circle(pm, knob_x * s, (ty + th / 2.0) * s, knob_r * s, [255, 255, 255, a(255)]);
    stroke_rounded(
        pm,
        (knob_x - knob_r) * s,
        (ty + th / 2.0 - knob_r) * s,
        (knob_r * 2.0) * s,
        (knob_r * 2.0) * s,
        knob_r * s,
        1.0 * s,
        [0, 0, 0, a(if dragging { 42 } else { 28 })],
    );
    let glyph: Rgba = [90, 90, 92, a(255)];
    match icon {
        0 => crate::osd::sun(pm, (tx + th / 2.0) * s, (ty + th / 2.0) * s, 8.0 * s, value, glyph),
        _ => crate::osd::speaker(pm, (tx + th / 2.0 + 1.0) * s, (ty + th / 2.0) * s, 7.0 * s, value, muted || value <= 0.001, glyph),
    }
}

fn draw_sep(pm: &mut Pixmap, s: f32, x: f32, y: f32, w: f32, c: Rgba) {
    fill_rounded(pm, x * s, y * s, w * s, (0.75 * s).max(1.0), 0.0, c);
}

fn draw_section_header(pm: &mut Pixmap, s: f32, x: f32, y: f32, label: &str, fonts: &Fonts, c: Rgba) {
    let b = fonts.baseline_centered(y * s, 24.0 * s, 11.5 * s, Weight::Bold);
    fonts.draw(pm, label, x * s, b, 11.5 * s, Weight::Bold, c);
}

#[allow(clippy::too_many_arguments)]
fn draw_wifi_row(
    pm: &mut Pixmap,
    s: f32,
    slide_x: f32,
    ly: f32,
    net: &WifiNetwork,
    busy: bool,
    fonts: &Fonts,
    accent: Rgba,
    off: Rgba,
    text: Rgba,
    sub_text: Rgba,
    german: bool,
) {
    let (cx, cy) = (27.0 + slide_x, ly + 19.0);
    fill_circle(pm, cx * s, cy * s, 13.0 * s, if net.active { accent } else { off });
    let glyph_col = if net.active { [255, 255, 255, 255] } else { text };
    let bars = if net.signal >= 67 {
        3
    } else if net.signal >= 34 {
        2
    } else {
        1
    };
    crate::menubar::draw_network(pm, cx * s, (cy - 1.0) * s, s * 0.72, Network::Wifi(bars), glyph_col);

    let right_reserve = if busy {
        86.0
    } else if net.secure {
        32.0
    } else {
        16.0
    };
    let maxw = (CC_WIDTH - 48.0 - right_reserve).max(40.0);
    let b = fonts.baseline_centered(ly * s, 38.0 * s, 13.0 * s, Weight::Regular);
    let label = fonts.elide(&net.ssid, 13.0 * s, Weight::Regular, maxw * s);
    fonts.draw(pm, &label, (48.0 + slide_x) * s, b, 13.0 * s, Weight::Regular, text);

    if busy {
        let msg = if german { "Verbinden…" } else { "Connecting…" };
        let mw = fonts.measure(msg, 11.0 * s, Weight::Regular) / s;
        let mb = fonts.baseline_centered(ly * s, 38.0 * s, 11.0 * s, Weight::Regular);
        fonts.draw(pm, msg, (CC_WIDTH - 14.0 - mw) * s, mb, 11.0 * s, Weight::Regular, sub_text);
    } else if net.secure {
        lock_glyph(pm, (CC_WIDTH - 22.0) * s, cy * s, 5.5 * s, sub_text);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_eth_row(
    pm: &mut Pixmap,
    s: f32,
    slide_x: f32,
    ly: f32,
    dev: &EthernetDevice,
    busy: bool,
    fonts: &Fonts,
    accent: Rgba,
    off: Rgba,
    text: Rgba,
    sub_text: Rgba,
    german: bool,
) {
    let (cx, cy) = (27.0 + slide_x, ly + 19.0);
    fill_circle(pm, cx * s, cy * s, 13.0 * s, if dev.connected { accent } else { off });
    let glyph_col = if dev.connected { [255, 255, 255, 255] } else { text };
    crate::menubar::draw_network(pm, cx * s, cy * s, s * 0.9, Network::Wired, glyph_col);

    let status: String = if busy {
        if dev.connected {
            if german { "Trennen…" } else { "Disconnecting…" }
        } else if german {
            "Verbinden…"
        } else {
            "Connecting…"
        }
        .into()
    } else if dev.connected {
        dev.ip
            .clone()
            .unwrap_or_else(|| if german { "Verbunden" } else { "Connected" }.into())
    } else if dev.plugged {
        if german { "Getrennt" } else { "Disconnected" }.into()
    } else if german {
        "Kein Kabel".into()
    } else {
        "Cable unplugged".into()
    };

    let sw = fonts.measure(&status, 11.0 * s, Weight::Regular) / s;
    let right_reserve = sw + 22.0;
    let maxw = (CC_WIDTH - 48.0 - right_reserve).max(40.0);
    let name = dev.connection.as_deref().unwrap_or(dev.device.as_str());
    let b = fonts.baseline_centered(ly * s, 38.0 * s, 13.0 * s, Weight::Regular);
    let label = fonts.elide(name, 13.0 * s, Weight::Regular, maxw * s);
    fonts.draw(pm, &label, (48.0 + slide_x) * s, b, 13.0 * s, Weight::Regular, text);

    let sb = fonts.baseline_centered(ly * s, 38.0 * s, 11.0 * s, Weight::Regular);
    fonts.draw(pm, &status, (CC_WIDTH - 14.0 - sw) * s, sb, 11.0 * s, Weight::Regular, sub_text);
}

#[allow(clippy::too_many_arguments)]
fn draw_network_link_row(
    pm: &mut Pixmap,
    s: f32,
    slide_x: f32,
    ly: f32,
    icon: Network,
    active: bool,
    title: &str,
    status: &str,
    fonts: &Fonts,
    accent: Rgba,
    off: Rgba,
    text: Rgba,
    sub_text: Rgba,
) {
    let (cx, cy) = (27.0 + slide_x, ly + 19.0);
    fill_circle(pm, cx * s, cy * s, 13.0 * s, if active { accent } else { off });
    let glyph_col = if active { [255, 255, 255, 255] } else { text };
    match icon {
        Network::Wired => crate::menubar::draw_network(pm, cx * s, cy * s, s * 0.9, Network::Wired, glyph_col),
        _ => crate::menubar::draw_network(pm, cx * s, (cy - 1.0) * s, s * 0.72, Network::Wifi(3), glyph_col),
    }

    let sw = fonts.measure(status, 11.0 * s, Weight::Regular) / s;
    let right_reserve = sw + 34.0;
    let maxw = (CC_WIDTH - 48.0 - right_reserve).max(40.0);
    let b = fonts.baseline_centered(ly * s, 38.0 * s, 13.0 * s, Weight::Regular);
    let label = fonts.elide(title, 13.0 * s, Weight::Regular, maxw * s);
    fonts.draw(pm, &label, (48.0 + slide_x) * s, b, 13.0 * s, Weight::Regular, text);

    let sb = fonts.baseline_centered(ly * s, 38.0 * s, 11.0 * s, Weight::Regular);
    fonts.draw(pm, status, (CC_WIDTH - 28.0 - sw) * s, sb, 11.0 * s, Weight::Regular, sub_text);
    chevron_right(pm, (CC_WIDTH - 16.0) * s, cy * s, 4.0 * s, sub_text);
}

#[allow(clippy::too_many_arguments)]
fn draw_bt_row(
    pm: &mut Pixmap,
    s: f32,
    slide_x: f32,
    ly: f32,
    dev: &BtDevice,
    busy: bool,
    fonts: &Fonts,
    accent: Rgba,
    off: Rgba,
    text: Rgba,
    sub_text: Rgba,
    german: bool,
) {
    let (cx, cy) = (27.0 + slide_x, ly + 19.0);
    fill_circle(pm, cx * s, cy * s, 13.0 * s, if dev.connected { accent } else { off });
    let glyph_col = if dev.connected { [255, 255, 255, 255] } else { text };
    let icon = dev.icon.as_str();
    if icon.contains("headphone") || icon.contains("headset") {
        headphones(pm, cx * s, cy * s, 6.8 * s, glyph_col);
    } else if icon.contains("keyboard") {
        keyboard_glyph(pm, cx * s, cy * s, 6.8 * s, glyph_col);
    } else if icon.contains("mouse") || icon.contains("touchpad") {
        mouse_glyph(pm, cx * s, cy * s, 6.8 * s, glyph_col);
    } else if icon.contains("audio") || icon.contains("speaker") {
        crate::osd::speaker(pm, (cx + 1.0) * s, cy * s, 6.8 * s, 0.8, false, glyph_col);
    } else {
        bluetooth(pm, cx * s, cy * s, 6.5 * s, glyph_col);
    }

    let right_reserve = if busy {
        88.0
    } else if dev.connected && dev.battery.is_some() {
        64.0
    } else {
        16.0
    };
    let maxw = (CC_WIDTH - 48.0 - right_reserve).max(40.0);
    let b = fonts.baseline_centered(ly * s, 38.0 * s, 13.0 * s, Weight::Regular);
    let label = fonts.elide(&dev.name, 13.0 * s, Weight::Regular, maxw * s);
    fonts.draw(pm, &label, (48.0 + slide_x) * s, b, 13.0 * s, Weight::Regular, text);

    if busy {
        let msg = if dev.connected {
            if german { "Trennen…" } else { "Disconnecting…" }
        } else if german {
            "Verbinden…"
        } else {
            "Connecting…"
        };
        let mw = fonts.measure(msg, 11.0 * s, Weight::Regular) / s;
        let mb = fonts.baseline_centered(ly * s, 38.0 * s, 11.0 * s, Weight::Regular);
        fonts.draw(pm, msg, (CC_WIDTH - 14.0 - mw) * s, mb, 11.0 * s, Weight::Regular, sub_text);
    } else if dev.connected {
        if let Some(pct) = dev.battery {
            let pct_str = format!("{pct}%");
            let pw = fonts.measure(&pct_str, 11.0 * s, Weight::Regular) / s;
            let bx = CC_WIDTH - 34.0;
            let pb = fonts.baseline_centered(ly * s, 38.0 * s, 11.0 * s, Weight::Regular);
            fonts.draw(pm, &pct_str, (bx - pw - 5.0) * s, pb, 11.0 * s, Weight::Regular, sub_text);
            mini_battery(pm, bx * s, cy * s, s, pct, sub_text);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_sink_row(
    pm: &mut Pixmap,
    s: f32,
    slide_x: f32,
    ly: f32,
    sink: &AudioSink,
    fonts: &Fonts,
    accent: Rgba,
    off: Rgba,
    text: Rgba,
) {
    let (cx, cy) = (27.0 + slide_x, ly + 19.0);
    fill_circle(pm, cx * s, cy * s, 13.0 * s, if sink.active { accent } else { off });
    let glyph_col = if sink.active { [255, 255, 255, 255] } else { text };
    match sink.icon {
        AudioIcon::Headphones => headphones(pm, cx * s, cy * s, 6.8 * s, glyph_col),
        AudioIcon::Bluetooth => bluetooth(pm, cx * s, cy * s, 6.5 * s, glyph_col),
        AudioIcon::Display => display_glyph(pm, cx * s, cy * s, 6.8 * s, glyph_col),
        AudioIcon::Speaker => crate::osd::speaker(pm, (cx + 1.0) * s, cy * s, 6.8 * s, 0.8, false, glyph_col),
    }

    let maxw = (CC_WIDTH - 48.0 - 16.0).max(40.0);
    let b = fonts.baseline_centered(ly * s, 38.0 * s, 13.0 * s, Weight::Regular);
    let label = fonts.elide(&sink.desc, 13.0 * s, Weight::Regular, maxw * s);
    fonts.draw(pm, &label, (48.0 + slide_x) * s, b, 13.0 * s, Weight::Regular, text);
}

thread_local! {
    static PREV_CARDS: std::cell::RefCell<Vec<(u32, (f32, f32, f32, f32))>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// "Thursday, 19 November" / "Donnerstag, 19. November"
fn date_label(german: bool) -> String {
    use chrono::Datelike;
    let now = chrono::Local::now();
    let wd = now.weekday().num_days_from_monday() as usize;
    let m = now.month0() as usize;
    if german {
        const D: [&str; 7] = ["Montag", "Dienstag", "Mittwoch", "Donnerstag", "Freitag", "Samstag", "Sonntag"];
        const M: [&str; 12] = [
            "Januar", "Februar", "März", "April", "Mai", "Juni", "Juli", "August", "September", "Oktober", "November", "Dezember",
        ];
        format!("{}, {}. {}", D[wd], now.day(), M[m])
    } else {
        const D: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
        const M: [&str; 12] = [
            "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December",
        ];
        format!("{}, {} {}", D[wd], now.day(), M[m])
    }
}

// ---- glyphs --------------------------------------------------------------------

/// A crescent moon (Do Not Disturb / Focus).
pub fn moon(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    let mut pb = PathBuilder::new();
    let n = 24;
    // outer arc of the full circle, then the inner arc of the cut-out circle back
    for k in 0..=n {
        let a = (50.0 + 260.0 * k as f32 / n as f32).to_radians();
        let (x, y) = (cx + r * a.cos(), cy + r * a.sin());
        if k == 0 {
            pb.move_to(x, y);
        } else {
            pb.line_to(x, y);
        }
    }
    let (ox, oy, rr) = (cx + r * 0.55, cy - r * 0.45, r * 0.8);
    for k in 0..=n {
        let a = (200.0 - 200.0 * k as f32 / n as f32 + 30.0).to_radians();
        pb.line_to(ox + rr * a.cos(), oy + rr * a.sin());
    }
    pb.close();
    if let Some(p) = pb.finish() {
        pm.fill_path(&p, &paint(c), FillRule::EvenOdd, Transform::identity(), None);
    }
}

/// A game controller outline with a d-pad and two buttons (Game Mode).
pub fn controller(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    let (w, h) = (r * 2.3, r * 1.45);
    crate::draw::stroke_rounded(pm, cx - w / 2.0, cy - h / 2.0, w, h, h * 0.45, r * 0.2, c);
    let mut pb = PathBuilder::new();
    let (dx, d) = (cx - w * 0.24, r * 0.32);
    pb.move_to(dx - d, cy);
    pb.line_to(dx + d, cy);
    pb.move_to(dx, cy - d);
    pb.line_to(dx, cy + d);
    if let Some(p) = pb.finish() {
        let st = Stroke { width: r * 0.18, line_cap: LineCap::Round, ..Default::default() };
        pm.stroke_path(&p, &paint(c), &st, Transform::identity(), None);
    }
    fill_circle(pm, cx + w * 0.2, cy - r * 0.12, r * 0.14, c);
    fill_circle(pm, cx + w * 0.3, cy + r * 0.16, r * 0.14, c);
}

/// A sun: a disc with eight short rays (Night Shift).
pub fn sun(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    fill_circle(pm, cx, cy, r * 0.5, c);
    let mut pb = PathBuilder::new();
    for k in 0..8 {
        let a = (k as f32 * 45.0).to_radians();
        let (sa, ca) = a.sin_cos();
        pb.move_to(cx + ca * r * 0.78, cy + sa * r * 0.78);
        pb.line_to(cx + ca * r * 1.08, cy + sa * r * 1.08);
    }
    if let Some(p) = pb.finish() {
        let st = Stroke {
            width: r * 0.2,
            line_cap: LineCap::Round,
            ..Default::default()
        };
        pm.stroke_path(&p, &paint(c), &st, Transform::identity(), None);
    }
}

fn bluetooth(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    let mut pb = PathBuilder::new();
    let (x, h) = (cx - r * 0.1, r);
    pb.move_to(cx - r * 0.55, cy - h * 0.45);
    pb.line_to(cx + r * 0.5, cy + h * 0.5);
    pb.line_to(x, cy + h);
    pb.line_to(x, cy - h);
    pb.line_to(cx + r * 0.5, cy - h * 0.5);
    pb.line_to(cx - r * 0.55, cy + h * 0.45);
    if let Some(p) = pb.finish() {
        let st = Stroke {
            width: r * 0.22,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Default::default()
        };
        pm.stroke_path(&p, &paint(c), &st, Transform::identity(), None);
    }
}

fn chevron_right(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    let mut pb = PathBuilder::new();
    pb.move_to(cx - r * 0.45, cy - r);
    pb.line_to(cx + r * 0.45, cy);
    pb.line_to(cx - r * 0.45, cy + r);
    if let Some(p) = pb.finish() {
        let st = Stroke {
            width: (r * 0.38).max(1.3),
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Default::default()
        };
        pm.stroke_path(&p, &paint(c), &st, Transform::identity(), None);
    }
}

fn chevron_left(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    let mut pb = PathBuilder::new();
    pb.move_to(cx + r * 0.45, cy - r);
    pb.line_to(cx - r * 0.45, cy);
    pb.line_to(cx + r * 0.45, cy + r);
    if let Some(p) = pb.finish() {
        let st = Stroke {
            width: (r * 0.38).max(1.4),
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Default::default()
        };
        pm.stroke_path(&p, &paint(c), &st, Transform::identity(), None);
    }
}

fn chevron_rotated(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, angle: f32, c: Rgba) {
    let (sa, ca) = angle.sin_cos();
    let rot = |x: f32, y: f32| (cx + x * ca - y * sa, cy + x * sa + y * ca);
    let (x0, y0) = rot(-r * 0.45, -r);
    let (x1, y1) = rot(r * 0.45, 0.0);
    let (x2, y2) = rot(-r * 0.45, r);
    let mut pb = PathBuilder::new();
    pb.move_to(x0, y0);
    pb.line_to(x1, y1);
    pb.line_to(x2, y2);
    if let Some(p) = pb.finish() {
        let st = Stroke {
            width: (r * 0.38).max(1.3),
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Default::default()
        };
        pm.stroke_path(&p, &paint(c), &st, Transform::identity(), None);
    }
}

fn lock_glyph(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    // Padlock body
    fill_rounded(pm, cx - r * 0.65, cy - r * 0.1, r * 1.3, r * 1.05, r * 0.25, c);
    // Shackle arch
    let mut pb = PathBuilder::new();
    let arch_r = r * 0.4;
    let base_y = cy - r * 0.1;
    let top_y = cy - r * 0.55;
    pb.move_to(cx - arch_r, base_y);
    pb.line_to(cx - arch_r, top_y);
    for k in 0..=12 {
        let a = std::f32::consts::PI + std::f32::consts::PI * (k as f32 / 12.0);
        pb.line_to(cx + arch_r * a.cos(), top_y + arch_r * a.sin());
    }
    pb.line_to(cx + arch_r, base_y);
    if let Some(p) = pb.finish() {
        let st = Stroke {
            width: (r * 0.24).max(1.1),
            line_cap: LineCap::Round,
            ..Default::default()
        };
        pm.stroke_path(&p, &paint(c), &st, Transform::identity(), None);
    }
}

fn headphones(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    // Headband arc
    let mut pb = PathBuilder::new();
    let band_r = r * 0.82;
    for k in 0..=16 {
        let a = std::f32::consts::PI + std::f32::consts::PI * (k as f32 / 16.0);
        let (x, y) = (cx + band_r * a.cos(), cy + r * 0.15 + band_r * a.sin());
        if k == 0 {
            pb.move_to(x, y);
        } else {
            pb.line_to(x, y);
        }
    }
    if let Some(p) = pb.finish() {
        let st = Stroke {
            width: (r * 0.22).max(1.3),
            line_cap: LineCap::Round,
            ..Default::default()
        };
        pm.stroke_path(&p, &paint(c), &st, Transform::identity(), None);
    }
    // Left and right ear cups
    fill_rounded(pm, cx - r * 0.96, cy + r * 0.02, r * 0.42, r * 0.85, r * 0.2, c);
    fill_rounded(pm, cx + r * 0.54, cy + r * 0.02, r * 0.42, r * 0.85, r * 0.2, c);
}

fn keyboard_glyph(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    stroke_rounded(pm, cx - r, cy - r * 0.62, r * 2.0, r * 1.24, r * 0.25, (r * 0.18).max(1.1), c);
    // Tiny key dots
    let kw = r * 0.24;
    let kh = r * 0.2;
    for row in [-0.26f32, 0.08] {
        for col in [-0.62f32, -0.22, 0.18, 0.56] {
            fill_rounded(pm, cx + col * r - kw / 2.0, cy + row * r - kh / 2.0, kw, kh, r * 0.05, c);
        }
    }
    fill_rounded(pm, cx - r * 0.42, cy + r * 0.3, r * 0.84, kh * 0.85, r * 0.05, c);
}

fn mouse_glyph(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    stroke_rounded(pm, cx - r * 0.58, cy - r * 0.88, r * 1.16, r * 1.76, r * 0.56, (r * 0.2).max(1.2), c);
    fill_rounded(pm, cx - r * 0.09, cy - r * 0.56, r * 0.18, r * 0.42, r * 0.09, c);
}

fn display_glyph(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    stroke_rounded(pm, cx - r * 0.92, cy - r * 0.78, r * 1.84, r * 1.24, r * 0.2, (r * 0.18).max(1.2), c);
    fill_rounded(pm, cx - r * 0.12, cy + r * 0.48, r * 0.24, r * 0.26, 0.0, c);
    fill_rounded(pm, cx - r * 0.46, cy + r * 0.72, r * 0.92, r * 0.16, r * 0.08, c);
}

fn mini_battery(pm: &mut Pixmap, x: f32, cy: f32, s: f32, pct: u8, c: Rgba) {
    let w = 18.0 * s;
    let h = 9.0 * s;
    let y = cy - h / 2.0;
    stroke_rounded(pm, x, y, w, h, 2.4 * s, 1.0 * s, c);
    fill_rounded(pm, x + w + 1.0 * s, cy - 1.6 * s, 1.3 * s, 3.2 * s, 0.6 * s, c);
    let fill_w = (w - 3.6 * s) * (pct as f32 / 100.0).clamp(0.08, 1.0);
    let fill_c = if pct <= 15 { [255, 69, 58, 255] } else { c };
    fill_rounded(pm, x + 1.8 * s, y + 1.8 * s, fill_w, h - 3.6 * s, 1.2 * s, fill_c);
}

fn play(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    let mut pb = PathBuilder::new();
    pb.move_to(cx - r * 0.6, cy - r);
    pb.line_to(cx + r, cy);
    pb.line_to(cx - r * 0.6, cy + r);
    pb.close();
    if let Some(p) = pb.finish() {
        pm.fill_path(&p, &paint(c), FillRule::Winding, Transform::identity(), None);
    }
}

fn pause(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    fill_rounded(pm, cx - r * 0.8, cy - r, r * 0.55, r * 2.0, r * 0.15, c);
    fill_rounded(pm, cx + r * 0.25, cy - r, r * 0.55, r * 2.0, r * 0.15, c);
}

/// ⏮ / ⏭: two triangles pointing in `dir`.
fn transport(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, dir: i32, c: Rgba) {
    let d = dir as f32;
    for off in [-r * 0.9, r * 0.1] {
        let base = cx + off * d;
        let mut pb = PathBuilder::new();
        pb.move_to(base, cy - r * 0.8);
        pb.line_to(base + r * 0.9 * d, cy);
        pb.line_to(base, cy + r * 0.8);
        pb.close();
        if let Some(p) = pb.finish() {
            pm.fill_path(&p, &paint(c), FillRule::Winding, Transform::identity(), None);
        }
    }
}

/// Draw the glyph for a Control Center module at logical (cx, cy).
fn module_glyph(pm: &mut Pixmap, s: f32, kind: ControlKind, cx: f32, cy: f32, r: f32, c: Rgba) {
    let ps = (r / 6.5).clamp(0.1, 1.5);
    match kind {
        ControlKind::Connectivity | ControlKind::Network | ControlKind::Wifi => {
            crate::menubar::draw_network(pm, cx * s, (cy - 1.0) * s, s * 0.8 * ps, Network::Wifi(3), c)
        }
        ControlKind::Bluetooth => bluetooth(pm, cx * s, cy * s, r * 1.08 * s, c),
        ControlKind::NowPlaying => music_note(pm, cx * s, cy * s, r * 1.15 * s, c),
        ControlKind::Focus => moon(pm, cx * s, cy * s, r * s, c),
        ControlKind::NightShift | ControlKind::Brightness => sun(pm, cx * s, cy * s, r * s, c),
        ControlKind::GameMode => controller(pm, cx * s, cy * s, r * s, c),
        ControlKind::Displays => display_glyph(pm, cx * s, cy * s, r * s, c),
        ControlKind::Sound | ControlKind::Volume => {
            crate::osd::speaker(pm, (cx + 1.0) * s, cy * s, r * s, 0.8, false, c)
        }
    }
}

fn music_note(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    fill_circle(pm, cx - r * 0.35, cy + r * 0.55, r * 0.38, c);
    fill_rounded(pm, cx + r * 0.22, cy - r * 0.85, r * 0.17, r * 1.42, r * 0.08, c);
    let mut pb = PathBuilder::new();
    pb.move_to(cx + r * 0.39, cy - r * 0.82);
    pb.line_to(cx + r * 0.72, cy - r * 0.45);
    pb.line_to(cx + r * 0.39, cy - r * 0.30);
    pb.close();
    if let Some(p) = pb.finish() {
        pm.fill_path(&p, &paint(c), FillRule::Winding, Transform::identity(), None);
    }
}

fn plus(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    let mut pb = PathBuilder::new();
    pb.move_to(cx - r, cy);
    pb.line_to(cx + r, cy);
    pb.move_to(cx, cy - r);
    pb.line_to(cx, cy + r);
    if let Some(p) = pb.finish() {
        let st = Stroke {
            width: (r * 0.42).max(1.4),
            line_cap: LineCap::Round,
            ..Default::default()
        };
        pm.stroke_path(&p, &paint(c), &st, Transform::identity(), None);
    }
}

fn minus(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    let mut pb = PathBuilder::new();
    pb.move_to(cx - r, cy);
    pb.line_to(cx + r, cy);
    if let Some(p) = pb.finish() {
        let st = Stroke {
            width: (r * 0.42).max(1.5),
            line_cap: LineCap::Round,
            ..Default::default()
        };
        pm.stroke_path(&p, &paint(c), &st, Transform::identity(), None);
    }
}

fn check(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    let mut pb = PathBuilder::new();
    pb.move_to(cx - r, cy);
    pb.line_to(cx - r * 0.2, cy + r * 0.8);
    pb.line_to(cx + r, cy - r * 0.8);
    if let Some(p) = pb.finish() {
        let st = Stroke {
            width: (r * 0.38).max(1.4),
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Default::default()
        };
        pm.stroke_path(&p, &paint(c), &st, Transform::identity(), None);
    }
}

fn pencil(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    let mut pb = PathBuilder::new();
    pb.move_to(cx + r * 0.55, cy - r * 0.95);
    pb.line_to(cx + r * 0.95, cy - r * 0.55);
    pb.line_to(cx - r * 0.35, cy + r);
    pb.line_to(cx - r, cy + r * 0.55);
    pb.close();
    if let Some(p) = pb.finish() {
        pm.fill_path(&p, &paint(c), FillRule::Winding, Transform::identity(), None);
    }
}

fn trash(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    fill_rounded(pm, cx - r * 0.75, cy - r * 0.55, r * 1.5, r * 0.26, r * 0.08, c);
    fill_rounded(pm, cx - r * 0.38, cy - r * 0.92, r * 0.76, r * 0.25, r * 0.08, c);
    stroke_rounded(pm, cx - r * 0.58, cy - r * 0.25, r * 1.16, r * 1.2, r * 0.22, (r * 0.18).max(1.2), c);
}

fn resize_glyph(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    let mut pb = PathBuilder::new();
    pb.move_to(cx - r, cy + r);
    pb.line_to(cx + r, cy - r);
    pb.move_to(cx - r * 0.15, cy - r);
    pb.line_to(cx + r, cy - r);
    pb.line_to(cx + r, cy + r * 0.15);
    pb.move_to(cx - r, cy + r * 0.15);
    pb.line_to(cx - r, cy + r);
    pb.line_to(cx - r * 0.15, cy + r);
    if let Some(p) = pb.finish() {
        let st = Stroke {
            width: (r * 0.28).max(1.1),
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Default::default()
        };
        pm.stroke_path(&p, &paint(c), &st, Transform::identity(), None);
    }
}

/// Animated macOS / iOS-style switch at (x, y), 38×22 (`t` in `0..=1`).
fn switch_anim(pm: &mut Pixmap, s: f32, x: f32, y: f32, t: f32, accent: Rgba, dark: bool) {
    let off_bg = if dark { [120, 120, 128, 100] } else { [0, 0, 0, 42] };
    let bg = lerp_rgba(off_bg, accent, t);
    fill_rounded(pm, x * s, y * s, 38.0 * s, 22.0 * s, 11.0 * s, bg);
    let kx = lerp(x + 11.0, x + 27.0, t.clamp(0.0, 1.0));
    // Soft knob drop shadow
    fill_circle(pm, kx * s, (y + 11.8) * s, 9.0 * s, [0, 0, 0, if dark { 42 } else { 26 }]);
    fill_circle(pm, kx * s, (y + 11.0) * s, 9.0 * s, [255, 255, 255, 255]);
    stroke_rounded(pm, (kx - 9.0) * s, (y + 2.0) * s, 18.0 * s, 18.0 * s, 9.0 * s, 0.75 * s, [0, 0, 0, 26]);
}

// ---- system state (helper thread) ---------------------------------------------

fn cmd(prog: &str, args: &[&str]) -> Option<String> {
    // a hung daemon (bluetoothd, NetworkManager …) must not freeze the panel
    let out = std::process::Command::new("timeout")
        .arg("4")
        .arg(prog)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn cmd_localized(prog: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("timeout").arg("4").arg(prog).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn sh(script: &str) -> Option<String> {
    let out = std::process::Command::new("timeout")
        .arg("4")
        .arg("sh")
        .arg("-c")
        .arg(script)
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Split one `nmcli -t` line, honouring `\:` escapes.
fn nm_fields(line: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    out.last_mut().unwrap().push(n);
                }
            }
            ':' => out.push(String::new()),
            c => out.last_mut().unwrap().push(c),
        }
    }
    out
}

fn parse_wifi_networks(list: &str, known_cons: &str) -> Vec<WifiNetwork> {
    let known: Vec<String> = known_cons
        .lines()
        .map(nm_fields)
        .filter(|f| f.get(1).map(|t| t == "802-11-wireless").unwrap_or(false))
        .map(|f| f[0].clone())
        .collect();

    let mut nets: Vec<WifiNetwork> = Vec::new();
    for f in list.lines().map(nm_fields) {
        if f.len() < 4 || f[1].is_empty() {
            continue;
        }
        let ssid = f[1].clone();
        let active = f[0].trim() == "*";
        let signal: u8 = f[2].parse().unwrap_or(0);
        let secure = !f[3].trim().is_empty() && f[3].trim() != "--";
        let is_known = active || known.iter().any(|k| k == &ssid);
        match nets.iter_mut().find(|n| n.ssid == ssid) {
            Some(n) => {
                n.active |= active;
                n.signal = n.signal.max(signal);
                n.secure |= secure;
                n.known |= is_known;
            }
            None => nets.push(WifiNetwork {
                ssid,
                signal,
                secure,
                active,
                known: is_known,
            }),
        }
    }
    nets.sort_by(|a, b| {
        b.active
            .cmp(&a.active)
            .then(b.known.cmp(&a.known))
            .then(b.signal.cmp(&a.signal))
            .then(a.ssid.cmp(&b.ssid))
    });
    nets
}

fn parse_bt_list(out: &str) -> Vec<(String, String)> {
    out.lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("Device ")?;
            let (mac, name) = rest.split_once(' ')?;
            Some((mac.to_string(), name.to_string()))
        })
        .collect()
}

fn infer_audio_icon(name: &str, desc: &str, active_port: &str) -> AudioIcon {
    let combined = format!("{} {} {}", name, desc, active_port).to_lowercase();
    if combined.contains("headphone")
        || combined.contains("headset")
        || combined.contains("earphone")
        || combined.contains("airpod")
        || combined.contains("buds")
    {
        AudioIcon::Headphones
    } else if combined.contains("bluez") || combined.contains("bluetooth") {
        AudioIcon::Bluetooth
    } else if combined.contains("hdmi") || combined.contains("displayport") || combined.contains("dp-") || combined.contains("monitor") {
        AudioIcon::Display
    } else {
        AudioIcon::Speaker
    }
}

fn parse_audio_sinks(json: &str, default_sink: &str) -> Vec<AudioSink> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let mut sinks: Vec<AudioSink> = v
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|d| {
                    let name = d["name"].as_str()?.to_string();
                    if name.ends_with(".monitor") || name == "auto_null" {
                        return None;
                    }
                    let desc = d["description"].as_str().unwrap_or(&name).to_string();
                    let active_port = d["active_port"].as_str().unwrap_or_default();
                    let active = name == default_sink;
                    let icon = infer_audio_icon(&name, &desc, active_port);
                    Some(AudioSink { name, desc, active, icon })
                })
                .collect()
        })
        .unwrap_or_default();
    if !sinks.iter().any(|s| s.active) {
        if let Some(first) = sinks.first_mut() {
            first.active = true;
        }
    }
    sinks
}

/// Scan results, plus whether the scan itself could be read (a failed command
/// must not look like "there are no networks around").
fn query_wifi_networks() -> (Vec<WifiNetwork>, bool) {
    match cmd_localized("nmcli", &["-t", "-f", "IN-USE,SSID,SIGNAL,SECURITY", "dev", "wifi", "list"]) {
        Some(list) => {
            let cons = cmd_localized("nmcli", &["-t", "-f", "NAME,TYPE", "con", "show"]).unwrap_or_default();
            (parse_wifi_networks(&list, &cons), true)
        }
        None => (Vec::new(), false),
    }
}

fn query_bt_devices() -> Vec<BtDevice> {
    let paired = parse_bt_list(&cmd("bluetoothctl", &["devices", "Paired"]).unwrap_or_default());
    let connected: Vec<String> = parse_bt_list(&cmd("bluetoothctl", &["devices", "Connected"]).unwrap_or_default())
        .into_iter()
        .map(|(m, _)| m)
        .collect();
    // One `info` round trip per paired device; asking in parallel keeps a slow
    // or busy bluetoothd from stalling the whole Control Center refresh.
    let mut devices: Vec<BtDevice> = std::thread::scope(|s| {
        let handles: Vec<_> = paired
            .into_iter()
            .map(|(mac, name)| {
                let connected = &connected;
                s.spawn(move || {
                    let mut icon = String::new();
                    let mut battery = None;
                    if let Some(detail) = cmd("bluetoothctl", &["info", &mac]) {
                        for l in detail.lines().map(str::trim) {
                            if let Some(i) = l.strip_prefix("Icon: ") {
                                icon = i.to_string();
                            }
                            if let Some(b) = l.strip_prefix("Battery Percentage: ") {
                                // "0x5a (90)"
                                battery = b.split('(').nth(1).and_then(|x| x.trim_end_matches(')').parse().ok());
                            }
                        }
                    }
                    let is_connected = connected.contains(&mac);
                    BtDevice {
                        mac,
                        name,
                        connected: is_connected,
                        icon,
                        battery,
                    }
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    devices.sort_by(|a, b| b.connected.cmp(&a.connected).then(a.name.cmp(&b.name)));
    devices
}

/// `pactl -f json list sink-inputs` → one entry per app (sorted by name, so
/// rows keep their place while the list refreshes). System sounds are left out.
fn parse_sink_inputs(json: &str) -> Vec<AppStream> {
    let Ok(serde_json::Value::Array(list)) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let mut apps: Vec<AppStream> = Vec::new();
    for si in &list {
        let Some(index) = si["index"].as_u64() else { continue };
        let props = &si["properties"];
        let prop = |k: &str| props[k].as_str().filter(|v| !v.is_empty());
        // event sounds and level meters (pavucontrol peak detection)
        if prop("media.role") == Some("event") || props["stream.monitor"].as_str() == Some("true") {
            continue;
        }
        let name = prop("application.name")
            .or(prop("application.process.binary"))
            .or(prop("node.name"))
            .unwrap_or("Unknown")
            .to_string();
        let icon = prop("application.icon_name")
            .or(prop("application.id"))
            .or(prop("application.process.binary"))
            .unwrap_or_default()
            .to_lowercase();
        let volume = si["volume"]
            .as_object()
            .map(|chans| {
                chans
                    .values()
                    .filter_map(|c| c["value"].as_f64())
                    .fold(0.0f64, f64::max)
                    / 65536.0
            })
            .unwrap_or(0.0) as f32;
        let muted = si["mute"].as_bool().unwrap_or(false);
        match apps.iter_mut().find(|a| a.name == name) {
            Some(a) => {
                a.indices.push(index as u32);
                a.volume = a.volume.max(volume);
                a.muted &= muted;
            }
            None => apps.push(AppStream { name, icon, indices: vec![index as u32], volume, muted }),
        }
    }
    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps
}

pub fn query_streams() -> Vec<AppStream> {
    parse_sink_inputs(&cmd_localized("pactl", &["-f", "json", "list", "sink-inputs"]).unwrap_or_default())
}

fn query_audio_sinks() -> Option<Vec<AudioSink>> {
    let json = cmd_localized("pactl", &["-f", "json", "list", "sinks"])?;
    let default_sink = cmd("pactl", &["get-default-sink"]).unwrap_or_default().trim().to_string();
    Some(parse_audio_sinks(&json, &default_sink))
}

fn parse_nm_devices(out: &str) -> (bool, Option<String>, Vec<EthernetDevice>) {
    let mut has_wifi = false;
    let mut active_wifi_con = None;
    let mut eths = Vec::new();
    for f in out.lines().map(nm_fields) {
        if f.len() < 3 {
            continue;
        }
        let dev = &f[0];
        let kind = f[1].as_str();
        let state = f[2].as_str();
        let con = f
            .get(3)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty() && *s != "--");
        let connected = state == "connected" || state.starts_with("connected ");
        match kind {
            "wifi" => {
                has_wifi = true;
                if connected && active_wifi_con.is_none() {
                    active_wifi_con = con.map(str::to_string);
                }
            }
            "ethernet" => {
                if crate::status::is_virtual_iface(dev) {
                    continue;
                }
                let external = state.contains("externally");
                let unmanaged = state == "unmanaged";
                let plugged = state != "unavailable";
                let managed = !external && !unmanaged;
                eths.push(EthernetDevice {
                    device: dev.clone(),
                    connection: if connected { con.map(str::to_string) } else { None },
                    ip: None,
                    connected,
                    plugged,
                    managed,
                });
            }
            _ => {}
        }
    }
    eths.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then(b.plugged.cmp(&a.plugged))
            .then(a.device.cmp(&b.device))
    });
    (has_wifi, active_wifi_con, eths)
}

fn query_ethernet_sysfs() -> Vec<EthernetDevice> {
    let Ok(rd) = std::fs::read_dir("/sys/class/net") else {
        return Vec::new();
    };
    let mut eths = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if crate::status::is_virtual_iface(&name) || e.path().join("wireless").exists() {
            continue;
        }
        let state = std::fs::read_to_string(e.path().join("operstate")).unwrap_or_default();
        let carrier = std::fs::read_to_string(e.path().join("carrier")).unwrap_or_default();
        let connected = state.trim() == "up";
        let plugged = connected || carrier.trim() == "1";
        eths.push(EthernetDevice {
            device: name,
            connection: None,
            ip: None,
            connected,
            plugged,
            managed: false,
        });
    }
    eths.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then(b.plugged.cmp(&a.plugged))
            .then(a.device.cmp(&b.device))
    });
    eths
}

/// Wi-Fi hardware as seen by the kernel, so a failing `nmcli` alone never makes
/// the Wi-Fi tile vanish.
fn has_wifi_sysfs() -> bool {
    std::fs::read_dir("/sys/class/net")
        .map(|rd| rd.flatten().any(|e| e.path().join("wireless").exists()))
        .unwrap_or(false)
}

#[derive(Default)]
struct NetQuery {
    wifi: Option<(bool, Option<String>)>,
    wifi_networks: Vec<WifiNetwork>,
    ethernet: Vec<EthernetDevice>,
    fresh_wifi: bool,
    fresh_networks: bool,
    fresh_ethernet: bool,
}

fn query_network() -> NetQuery {
    let mut q = NetQuery::default();
    let mut has_wifi = has_wifi_sysfs();
    let mut active_wifi_con = None;

    match cmd("nmcli", &["-t", "-f", "DEVICE,TYPE,STATE,CONNECTION", "dev"]) {
        Some(devs) => {
            let (hw, aw, mut eths) = parse_nm_devices(&devs);
            has_wifi |= hw;
            active_wifi_con = aw;
            for dev in &mut eths {
                if !dev.managed && !dev.connected {
                    let up = std::fs::read_to_string(format!("/sys/class/net/{}/operstate", dev.device))
                        .map(|s| s.trim() == "up")
                        .unwrap_or(false);
                    if up {
                        dev.connected = true;
                        dev.plugged = true;
                    }
                }
                if dev.connected {
                    dev.ip = cmd("nmcli", &["-g", "IP4.ADDRESS", "dev", "show", &dev.device]).and_then(|s| {
                        s.lines()
                            .next()
                            .and_then(|l| l.split('|').next())
                            .and_then(|a| a.trim().split('/').next())
                            .map(str::trim)
                            .filter(|a| !a.is_empty())
                            .map(str::to_string)
                    });
                }
            }
            eths.sort_by(|a, b| {
                b.connected
                    .cmp(&a.connected)
                    .then(b.plugged.cmp(&a.plugged))
                    .then(a.device.cmp(&b.device))
            });
            q.ethernet = eths;
            q.fresh_ethernet = true;
        }
        None => {
            q.ethernet = query_ethernet_sysfs();
            q.fresh_ethernet = !q.ethernet.is_empty();
        }
    }

    if !has_wifi {
        // No radio at all: "off / no networks" is the real answer, not a failure.
        q.fresh_wifi = true;
        q.fresh_networks = true;
        return q;
    }

    match cmd("nmcli", &["-t", "radio", "wifi"]) {
        Some(out) => {
            let on = out.trim() == "enabled";
            q.fresh_wifi = true;
            if on {
                let (nets, ok) = query_wifi_networks();
                let ssid = nets
                    .iter()
                    .find(|n| n.active)
                    .map(|n| n.ssid.clone())
                    .or(active_wifi_con);
                q.wifi_networks = nets;
                q.fresh_networks = ok;
                q.wifi = Some((true, ssid));
            } else {
                q.fresh_networks = true;
                q.wifi = Some((false, None));
            }
        }
        None => {
            // Radio state unknown this round (busy NetworkManager); the previous
            // value is kept by the merge instead of showing "Unavailable".
        }
    }
    q
}

fn query_bluetooth() -> (Option<bool>, Vec<BtDevice>, bool) {
    match cmd("bluetoothctl", &["show"]) {
        Some(out) => {
            let powered = if out.contains("Powered: yes") {
                Some(true)
            } else if out.contains("Powered: no") {
                Some(false)
            } else {
                None
            };
            let devices = if powered == Some(true) { query_bt_devices() } else { Vec::new() };
            (powered, devices, true)
        }
        None => (None, Vec::new(), false),
    }
}

#[derive(Default)]
struct AudioQuery {
    volume: Option<(f32, bool)>,
    sinks: Vec<AudioSink>,
    streams: Vec<AppStream>,
    fresh_volume: bool,
    fresh_sinks: bool,
}

fn query_audio() -> AudioQuery {
    let mut q = AudioQuery::default();
    if let Some(out) = cmd("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"]) {
        q.volume = crate::osd::parse_wpctl(&out);
        q.fresh_volume = q.volume.is_some();
    }
    if let Some(sinks) = query_audio_sinks() {
        q.sinks = sinks;
        q.fresh_sinks = true;
    }
    q.streams = query_streams();
    q
}

fn query_media() -> Option<Media> {
    sh("playerctl metadata --format '{{status}}\t{{title}}\t{{artist}}'").and_then(|o| {
        let mut parts = o.trim_end_matches('\n').splitn(3, '\t');
        let status = parts.next()?;
        Some(Media {
            playing: status == "Playing",
            title: parts.next().unwrap_or_default().to_string(),
            artist: parts.next().unwrap_or_default().to_string(),
        })
    })
}

/// Read the whole Control Center state. The independent groups (network,
/// Bluetooth, audio, displays, media) run in parallel, so a slow DDC probe or
/// Wi-Fi scan no longer delays the tiles it has nothing to do with.
pub fn query_control_fresh() -> (ControlState, ControlFresh) {
    let (net, (bluetooth, bt_devices, bt_ok), audio, (displays, displays_ok), media) = std::thread::scope(|s| {
        let net = s.spawn(query_network);
        let bt = s.spawn(query_bluetooth);
        let audio = s.spawn(query_audio);
        let displays = s.spawn(crate::ddc::query_displays);
        let media = s.spawn(query_media);
        (
            net.join().unwrap_or_default(),
            bt.join().unwrap_or_default(),
            audio.join().unwrap_or_default(),
            displays.join().unwrap_or_default(),
            media.join().unwrap_or_default(),
        )
    });

    let state = ControlState {
        wifi: net.wifi,
        wifi_networks: net.wifi_networks,
        ethernet: net.ethernet,
        bluetooth,
        bt_devices,
        volume: audio.volume,
        audio_sinks: audio.sinks,
        streams: audio.streams,
        displays,
        media,
    };
    let fresh = ControlFresh {
        wifi: net.fresh_wifi,
        wifi_networks: net.fresh_networks,
        ethernet: net.fresh_ethernet,
        bluetooth: bt_ok,
        volume: audio.fresh_volume,
        audio_sinks: audio.fresh_sinks,
        displays: displays_ok,
    };
    (state, fresh)
}

/// Fast path for sliders & mute: only applies the system change without running
/// the full `query_control_fresh()`.
pub fn run_control_slider(
    target: &Target,
    value: Option<f32>,
    displays: &[crate::ddc::DisplayBrightness],
    streams: &[AppStream],
) {
    match target {
        Target::AppVolume(idx, ..) => {
            if let (Some(v), Some(app)) = (value, streams.get(*idx)) {
                let pct = format!("{}%", (v * 100.0).round() as u32);
                for i in &app.indices {
                    let i = i.to_string();
                    let _ = cmd("pactl", &["set-sink-input-mute", &i, "0"]);
                    let _ = cmd("pactl", &["set-sink-input-volume", &i, &pct]);
                }
            }
        }
        Target::AppMute(idx) => {
            if let Some(app) = streams.get(*idx) {
                // all streams of the app follow the row's new state
                let mute = if app.muted { "1" } else { "0" };
                for i in &app.indices {
                    let _ = cmd("pactl", &["set-sink-input-mute", &i.to_string(), mute]);
                }
            }
        }
        Target::Volume(..) | Target::VolumeVertical(..) => {
            if let Some(v) = value {
                let _ = cmd("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", "0"]);
                let _ = cmd("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", &format!("{:.3}", v)]);
            }
        }
        Target::VolumeMute => {
            let _ = cmd("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"]);
        }
        Target::Brightness(idx, ..) | Target::BrightnessVertical(idx, ..) => {
            if let (Some(v), Some(d)) = (value, displays.get(*idx)) {
                crate::ddc::set_brightness(d.bus, v);
            }
        }
        _ => {}
    }
}

/// Run a Control Center action; returns the new state.
pub fn run_control(target: &Target, value: Option<f32>, current: &ControlState) -> (ControlState, ControlFresh) {
    match target {
        Target::WifiToggle => {
            let on = current.wifi.as_ref().map(|w| w.0).unwrap_or(false);
            cmd("nmcli", &["radio", "wifi", if on { "off" } else { "on" }]);
        }
        Target::EthernetToggle => {
            let connected: Vec<&str> = current
                .ethernet
                .iter()
                .filter(|e| e.managed && e.connected)
                .map(|e| e.device.as_str())
                .collect();
            if !connected.is_empty() {
                for dev in connected {
                    cmd("nmcli", &["dev", "disconnect", dev]);
                }
            } else if let Some(dev) = current.ethernet.iter().find(|e| e.managed && e.plugged) {
                cmd("nmcli", &["dev", "connect", &dev.device]);
            }
        }
        Target::EthToggleDevice { device, connected } => {
            let verb = if *connected { "disconnect" } else { "connect" };
            cmd("nmcli", &["dev", verb, device]);
        }
        Target::WifiConnect { ssid, known, .. } => {
            if *known {
                if cmd("nmcli", &["con", "up", "id", ssid]).is_none() {
                    cmd("nmcli", &["dev", "wifi", "connect", ssid]);
                }
            } else {
                cmd("nmcli", &["dev", "wifi", "connect", ssid]);
            }
        }
        Target::BluetoothToggle => {
            let on = current.bluetooth == Some(true);
            // a soft-blocked adapter has to be unblocked before it can power on
            if on {
                cmd("bluetoothctl", &["power", "off"]);
            } else {
                let _ = cmd("rfkill", &["unblock", "bluetooth"]);
                cmd("bluetoothctl", &["power", "on"]);
            }
        }
        Target::BtToggleDevice { mac, connected } => {
            let verb = if *connected { "disconnect" } else { "connect" };
            let _ = std::process::Command::new("timeout")
                .arg("8")
                .arg("bluetoothctl")
                .args([verb, mac])
                .output();
        }
        Target::AudioSelectSink(name) => {
            cmd("pactl", &["set-default-sink", name]);
        }
        Target::Volume(..) | Target::VolumeVertical(..) => {
            if let Some(v) = value {
                let _ = cmd("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", "0"]);
                let _ = cmd("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", &format!("{:.3}", v)]);
            }
        }
        Target::VolumeMute => {
            let _ = cmd("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"]);
        }
        Target::Brightness(idx, ..) | Target::BrightnessVertical(idx, ..) => {
            if let (Some(v), Some(d)) = (value, current.displays.get(*idx)) {
                crate::ddc::set_brightness(d.bus, v);
            }
        }
        Target::MediaPrev => {
            cmd("playerctl", &["previous"]);
        }
        Target::MediaPlay => {
            cmd("playerctl", &["play-pause"]);
        }
        Target::MediaNext => {
            cmd("playerctl", &["next"]);
        }
        _ => {}
    }
    // players update their metadata a moment later
    if matches!(target, Target::MediaPrev | Target::MediaNext | Target::MediaPlay) {
        std::thread::sleep(Duration::from_millis(250));
    }
    // NetworkManager / BlueZ publish the new state over D-Bus just after the
    // command returns; give them a moment so the read-back isn't the old one.
    if matches!(target, Target::WifiToggle | Target::EthernetToggle | Target::BluetoothToggle) {
        std::thread::sleep(Duration::from_millis(150));
    }
    query_control_fresh()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wifi_and_sorts() {
        let list = " :CoffeeShop:45:WPA2\n*:Home\\:5G:82:WPA2\n :Home\\:5G:60:WPA2\n :FreeWifi:90:--\n";
        let known = "Home\\:5G:802-11-wireless\nCoffeeShop:802-11-wireless\nWired:802-3-ethernet\n";
        let nets = parse_wifi_networks(list, known);
        assert_eq!(nets.len(), 3);
        // Active first, then known, then other
        assert_eq!(nets[0].ssid, "Home:5G");
        assert!(nets[0].active);
        assert_eq!(nets[0].signal, 82);
        assert!(nets[0].secure);
        assert_eq!(nets[1].ssid, "CoffeeShop");
        assert!(nets[1].known);
        assert!(!nets[1].active);
        assert_eq!(nets[2].ssid, "FreeWifi");
        assert!(!nets[2].known);
        assert!(!nets[2].secure);
    }

    #[test]
    fn merge_keeps_fields_that_failed_to_read() {
        let prev = ControlState {
            wifi: Some((true, Some("HomeNet".into()))),
            bluetooth: Some(true),
            volume: Some((0.4, false)),
            audio_sinks: vec![AudioSink {
                name: "alsa_output.hdmi".into(),
                desc: "HDMI".into(),
                active: true,
                icon: AudioIcon::Display,
            }],
            displays: vec![crate::ddc::DisplayBrightness {
                bus: Some(4),
                name: "DELL".into(),
                value: 0.5,
            }],
            ..Default::default()
        };

        // Every helper timed out: nothing may flicker away.
        let all_failed = ControlUpdate {
            seq: 1,
            state: ControlState::default(),
            fresh: ControlFresh::default(),
        };
        assert_eq!(all_failed.merged(&prev), prev);

        // Only volume was read: it replaces, everything else is kept.
        let volume_only = ControlUpdate {
            seq: 2,
            state: ControlState {
                volume: Some((0.9, true)),
                ..Default::default()
            },
            fresh: ControlFresh {
                volume: true,
                ..Default::default()
            },
        };
        let merged = volume_only.merged(&prev);
        assert_eq!(merged.volume, Some((0.9, true)));
        assert_eq!(merged.wifi, prev.wifi);
        assert_eq!(merged.displays, prev.displays);
        assert_eq!(merged.audio_sinks, prev.audio_sinks);
    }

    #[test]
    fn parses_bt_devices() {
        let out = "Device AA:BB:CC:DD:EE:FF AirPods Pro\nDevice 11:22:33:44:55:66 Magic Mouse\n";
        let devs = parse_bt_list(out);
        assert_eq!(devs.len(), 2);
        assert_eq!(devs[0], ("AA:BB:CC:DD:EE:FF".into(), "AirPods Pro".into()));
        assert_eq!(devs[1], ("11:22:33:44:55:66".into(), "Magic Mouse".into()));
    }

    /// Real PipeWire round trip with a silent stream (zeros):
    /// `cargo test -p orchard-shell app_volume_round_trip -- --ignored`
    #[test]
    #[ignore]
    fn app_volume_round_trip() {
        let mut child = std::process::Command::new("paplay")
            .args(["--raw", "--volume=0", "--client-name=Orchard Volume Test", "/dev/zero"])
            .spawn()
            .expect("paplay");
        std::thread::sleep(std::time::Duration::from_millis(800));
        let find = || query_streams().into_iter().position(|a| a.name == "Orchard Volume Test");
        let streams = query_streams();
        let idx = find().expect("test stream listed");
        run_control_slider(&Target::AppVolume(idx, 0.0, 1.0), Some(0.37), &[], &streams);
        let now = query_streams();
        let app = &now[find().unwrap()];
        assert!((app.volume - 0.37).abs() < 0.01, "{}", app.volume);
        assert!(!app.muted);
        // the row's new state is what gets applied
        let mut muted = now.clone();
        muted[idx].muted = true;
        run_control_slider(&Target::AppMute(idx), None, &[], &muted);
        assert!(query_streams()[find().unwrap()].muted);
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn parses_sink_inputs_per_app() {
        // shape of real `pactl -f json list sink-inputs` output
        let json = r#"[
          {"index":170,"mute":false,"volume":{"front-left":{"value":32768,"value_percent":"50%"},"front-right":{"value":39321,"value_percent":"60%"}},
           "properties":{"application.name":"Firefox","application.icon_name":"firefox","media.name":"Tab 1"}},
          {"index":171,"mute":true,"volume":{"mono":{"value":65536,"value_percent":"100%"}},
           "properties":{"application.name":"Firefox","media.name":"Tab 2"}},
          {"index":172,"mute":true,"volume":{"mono":{"value":65536}},
           "properties":{"application.process.binary":"mpv"}},
          {"index":173,"mute":false,"volume":{"mono":{"value":65536}},
           "properties":{"application.name":"Bell","media.role":"event"}}
        ]"#;
        let apps = parse_sink_inputs(json);
        assert_eq!(apps.len(), 2, "event sounds are left out");
        assert_eq!(apps[0].name, "Firefox");
        assert_eq!(apps[0].indices, [170, 171]);
        assert!((apps[0].volume - 1.0).abs() < 1e-3, "loudest stream wins");
        assert!(!apps[0].muted, "one stream still audible");
        assert_eq!(apps[0].icon, "firefox");
        assert_eq!(apps[1].name, "mpv");
        assert!(apps[1].muted);
        assert!(parse_sink_inputs("[]").is_empty());
        assert!(parse_sink_inputs("garbage").is_empty());
    }

    #[test]
    fn parses_audio_sinks_and_icons() {
        let json = r#"[
            {"name": "alsa_output.pci.analog", "description": "Built-in Audio", "active_port": "analog-output-speaker"},
            {"name": "bluez_output.AA_BB", "description": "AirPods Pro", "active_port": "headphone-output"},
            {"name": "alsa_output.hdmi", "description": "HDMI / DisplayPort", "active_port": "hdmi-output-0"},
            {"name": "auto_null", "description": "Dummy Output"},
            {"name": "alsa_output.pci.analog.monitor", "description": "Monitor"}
        ]"#;
        let sinks = parse_audio_sinks(json, "bluez_output.AA_BB");
        assert_eq!(sinks.len(), 3);
        assert!(!sinks[0].active);
        assert_eq!(sinks[0].icon, AudioIcon::Speaker);
        assert!(sinks[1].active);
        assert_eq!(sinks[1].icon, AudioIcon::Headphones);
        assert!(!sinks[2].active);
        assert_eq!(sinks[2].icon, AudioIcon::Display);
    }

    #[test]
    fn parses_nm_devices_and_filters_virtual() {
        let out = "\
lo:loopback:connected (externally):lo
veth1234:ethernet:unmanaged:
docker0:bridge:connected (externally):docker0
enp5s0:ethernet:unavailable:
enp6s0:ethernet:disconnected:
enp4s0:ethernet:connected:Wired\\:1
enp7s0:ethernet:connected (externally):enp7s0
wlan0:wifi:connected:OfficeWiFi
";
        let (has_wifi, active_wifi, eths) = parse_nm_devices(out);
        assert!(has_wifi);
        assert_eq!(active_wifi.as_deref(), Some("OfficeWiFi"));
        assert_eq!(eths.len(), 4);
        // Connected interfaces come first, then plugged disconnected, then unplugged
        assert_eq!(eths[0].device, "enp4s0");
        assert!(eths[0].connected);
        assert!(eths[0].managed);
        assert_eq!(eths[0].connection.as_deref(), Some("Wired:1"));
        assert_eq!(eths[1].device, "enp7s0");
        assert!(eths[1].connected);
        assert!(!eths[1].managed);
        assert_eq!(eths[2].device, "enp6s0");
        assert!(!eths[2].connected);
        assert!(eths[2].plugged);
        assert!(eths[2].managed);
        assert_eq!(eths[3].device, "enp5s0");
        assert!(!eths[3].connected);
        assert!(!eths[3].plugged);
    }

    #[test]
    fn show_ethernet_picks_active_connection() {
        let eth = |connected: bool, plugged: bool| EthernetDevice {
            device: "enp5s0".into(),
            connection: connected.then(|| "Wired connection 1".into()),
            ip: connected.then(|| "192.168.1.10".into()),
            connected,
            plugged,
            managed: true,
        };

        // Only Ethernet connected (Wi-Fi off) -> Ethernet
        let st = ControlState {
            wifi: Some((false, None)),
            ethernet: vec![eth(true, true)],
            ..Default::default()
        };
        assert!(st.show_ethernet());

        // Only Ethernet connected (Wi-Fi on, not connected) -> Ethernet
        let st = ControlState {
            wifi: Some((true, None)),
            ethernet: vec![eth(true, true)],
            ..Default::default()
        };
        assert!(st.show_ethernet());

        // Only Ethernet connected (no Wi-Fi hardware) -> Ethernet
        let st = ControlState {
            wifi: None,
            ethernet: vec![eth(true, true)],
            ..Default::default()
        };
        assert!(st.show_ethernet());

        // Only Wi-Fi connected -> Wi-Fi
        let st = ControlState {
            wifi: Some((true, Some("HomeNet".into()))),
            ethernet: vec![eth(false, false)],
            ..Default::default()
        };
        assert!(!st.show_ethernet());

        // Both Wi-Fi and Ethernet connected -> Wi-Fi (not exclusively Ethernet)
        let st = ControlState {
            wifi: Some((true, Some("HomeNet".into()))),
            ethernet: vec![eth(true, true)],
            ..Default::default()
        };
        assert!(!st.show_ethernet());

        // Neither connected, Wi-Fi hardware present -> Wi-Fi
        let st = ControlState {
            wifi: Some((true, None)),
            ethernet: vec![eth(false, true)],
            ..Default::default()
        };
        assert!(!st.show_ethernet());

        // Neither connected, no Wi-Fi hardware, Ethernet present -> Ethernet
        let st = ControlState {
            wifi: None,
            ethernet: vec![eth(false, false)],
            ..Default::default()
        };
        assert!(st.show_ethernet());

        // Neither present -> fallback (false)
        assert!(!ControlState::default().show_ethernet());
    }

    /// Renders the Control Center to `/tmp/orchard-cc-preview.png` for visual
    /// inspection: `cargo test -p orchard-shell render_cc_preview -- --ignored`.
    #[test]
    #[ignore]
    fn render_cc_preview() {
        let fonts = Fonts::load();
        let center = Center::default();
        let apps = Apps::load(None);
        let render = |appearance,
                      edit: bool,
                      gallery: bool,
                      open_ms: u64,
                      submenu: Option<Submenu>,
                      layout: Option<ControlCenterSettings>,
                      path: &str| {
            let theme = Theme::new(appearance);
            let mut pm = Pixmap::new(1500, 1100).unwrap();
            let mut panel = Panel::new(Kind::Control, 1480.0);
            panel.opened = Instant::now() - Duration::from_millis(open_ms);
            if let Some(layout) = layout {
                let mut settings = Settings::default();
                settings.control_center = layout;
                panel.apply_settings(&settings);
            }
            panel.control = ControlState {
                wifi: Some((true, Some("Home 5G".into()))),
                bluetooth: Some(true),
                ethernet: Vec::new(),
                volume: Some((0.62, false)),
                displays: vec![crate::ddc::DisplayBrightness {
                    bus: None,
                    name: "Built-in Display".into(),
                    value: 0.82,
                }],
                media: Some(Media {
                    title: "Midnight City".into(),
                    artist: "M83".into(),
                    playing: true,
                }),
                ..Default::default()
            };
            panel.game_mode_enabled = true;
            if edit {
                panel.cc_toggle_edit();
            }
            if gallery {
                panel.cc_open_gallery();
            }
            if let Some(sub) = submenu {
                panel.open_submenu(sub);
            }
            panel.paint(&mut pm, 2.0, 750.0, 550.0, &center, &apps, &fonts, &theme, false);
            pm.save_png(path).unwrap();
        };
        render(crate::theme::Appearance::Dark, false, false, 2000, None, None, "/tmp/orchard-cc-dark.png");
        render(crate::theme::Appearance::Light, false, false, 2000, None, None, "/tmp/orchard-cc-light.png");
        render(crate::theme::Appearance::Dark, true, false, 2000, None, None, "/tmp/orchard-cc-edit.png");
        render(crate::theme::Appearance::Dark, true, true, 2000, None, None, "/tmp/orchard-cc-gallery.png");
        render(crate::theme::Appearance::Dark, false, false, 170, None, None, "/tmp/orchard-cc-open.png");
        render(
            crate::theme::Appearance::Dark,
            false,
            false,
            2000,
            Some(Submenu::Wifi),
            None,
            "/tmp/orchard-cc-wifi.png",
        );
        render(
            crate::theme::Appearance::Dark,
            false,
            false,
            2000,
            None,
            Some(ControlCenterSettings {
                pages: vec![vec![
                    ControlItem::new(ControlKind::NowPlaying, ControlSize::Wide),
                    ControlItem::new(ControlKind::Wifi, ControlSize::Small),
                    ControlItem::new(ControlKind::Bluetooth, ControlSize::Small),
                    ControlItem::new(ControlKind::Focus, ControlSize::Wide),
                    ControlItem::new(ControlKind::GameMode, ControlSize::Wide),
                    ControlItem::new(ControlKind::Network, ControlSize::Wide),
                    ControlItem::new(ControlKind::Sound, ControlSize::Small),
                    ControlItem::new(ControlKind::Displays, ControlSize::Small),
                    ControlItem::new(ControlKind::Brightness, ControlSize::Full),
                    ControlItem::new(ControlKind::Volume, ControlSize::Full),
                ]],
            }),
            "/tmp/orchard-cc-custom.png",
        );
    }

    #[test]
    fn pack_page_uses_a_four_column_ios_grid() {
        let pages = ControlCenterSettings::default().pages;
        let placed = pack_page(&pages[0]);
        assert_eq!(placed.len(), 12);
        assert_eq!((placed[0].x, placed[0].y, placed[0].w, placed[0].h), (34.0, 0.0, 140.0, 140.0));
        assert_eq!((placed[1].x, placed[1].y), (188.0, 0.0));
        assert_eq!((placed[2].x, placed[2].y), (34.0, 154.0));
        assert_eq!((placed[3].x, placed[3].y), (111.0, 154.0));
        assert_eq!((placed[4].x, placed[4].y, placed[4].w, placed[4].h), (188.0, 154.0, 63.0, 140.0));
        assert_eq!((placed[5].x, placed[5].y, placed[5].w, placed[5].h), (265.0, 154.0, 63.0, 140.0));
        assert_eq!((placed[6].x, placed[6].y), (34.0, 231.0));
        assert_eq!((placed[7].x, placed[7].y), (111.0, 231.0));
        assert_eq!((placed[8].x, placed[8].y), (34.0, 308.0));
        assert_eq!((placed[9].x, placed[9].y), (111.0, 308.0));
        assert_eq!((placed[10].x, placed[10].y), (188.0, 308.0));
        assert_eq!((placed[11].x, placed[11].y), (265.0, 308.0));
        assert_eq!(content_rows(&placed), 5);
        assert_eq!(content_height(&placed), 371.0);
    }

    #[test]
    fn paints_every_control_at_every_size_without_panicking() {
        let fonts = Fonts::load();
        let center = Center::default();
        let apps = Apps::load(None);
        let mut pm = Pixmap::new(500, 600).unwrap();
        let mut panel = Panel::new(Kind::Control, 490.0);
        panel.control = ControlState {
            wifi: Some((true, Some("Home".into()))),
            bluetooth: Some(true),
            volume: Some((0.4, false)),
            displays: vec![crate::ddc::DisplayBrightness {
                bus: None,
                name: "Built-in".into(),
                value: 0.5,
            }],
            media: Some(Media {
                title: "A very long media title that will be elided".into(),
                artist: "Artist".into(),
                playing: false,
            }),
            ..Default::default()
        };
        for appearance in [crate::theme::Appearance::Dark, crate::theme::Appearance::Light] {
            let theme = Theme::new(appearance);
            for kind in ControlKind::ALL {
                for size in [
                    ControlSize::Small,
                    ControlSize::Wide,
                    ControlSize::Tall,
                    ControlSize::Large,
                    ControlSize::Full,
                ] {
                    panel.cc.pages = vec![vec![ControlItem::new(kind, size)]];
                    panel.cc.page = 0;
                    panel.cc.layout_dirty = true;
                    panel.cc.edit = false;
                    panel.cc.gallery = false;
                    panel.opened = Instant::now() - Duration::from_secs(1);
                    panel.paint(&mut pm, 1.0, 500.0, 600.0, &center, &apps, &fonts, &theme, false);
                }
            }
        }
        // Gallery and edit rendering for the default layout.
        let theme = Theme::new(crate::theme::Appearance::Dark);
        panel.cc.pages = ControlCenterSettings::default().pages;
        panel.cc.layout_dirty = true;
        panel.cc_toggle_edit();
        panel.paint(&mut pm, 1.0, 500.0, 600.0, &center, &apps, &fonts, &theme, false);
        panel.cc_open_gallery();
        panel.paint(&mut pm, 1.0, 500.0, 600.0, &center, &apps, &fonts, &theme, false);
    }

    #[test]
    fn resize_cycles_to_sensible_sizes() {
        assert_eq!(next_module_size(ControlKind::Connectivity, ControlSize::Large), ControlSize::Wide);
        assert_eq!(next_module_size(ControlKind::Connectivity, ControlSize::Wide), ControlSize::Large);
        assert_eq!(next_module_size(ControlKind::Brightness, ControlSize::Tall), ControlSize::Tall);
        assert_eq!(next_module_size(ControlKind::Volume, ControlSize::Wide), ControlSize::Tall);
        assert_eq!(next_module_size(ControlKind::Focus, ControlSize::Small), ControlSize::Wide);
        assert_eq!(next_module_size(ControlKind::Focus, ControlSize::Wide), ControlSize::Small);
    }

    #[test]
    fn control_center_exposes_quick_settings_and_media_targets() {
        let fonts = Fonts::load();
        let theme = Theme::new(crate::theme::Appearance::Dark);
        let center = Center::default();
        let apps = Apps::load(None);
        let mut pm = Pixmap::new(750, 800).unwrap();
        let mut panel = Panel::new(Kind::Control, 740.0);
        panel.opened = Instant::now() - Duration::from_secs(1);
        panel.control = ControlState {
            wifi: Some((true, Some("HomeNet".into()))),
            bluetooth: Some(true),
            ethernet: vec![EthernetDevice {
                device: "enp5s0".into(),
                connection: Some("Wired connection 1".into()),
                ip: Some("10.0.0.5".into()),
                connected: true,
                plugged: true,
                managed: true,
            }],
            volume: Some((0.5, false)),
            displays: vec![crate::ddc::DisplayBrightness {
                bus: None,
                name: "Built-in".into(),
                value: 0.7,
            }],
            media: Some(Media {
                title: "Song".into(),
                artist: "Artist".into(),
                playing: true,
            }),
            ..Default::default()
        };
        panel.paint(&mut pm, 1.0, 750.0, 800.0, &center, &apps, &fonts, &theme, false);
        macro_rules! has {
            ($t:expr) => {
                panel.targets.iter().any(|(_, x)| x == &$t)
            };
        }
        assert!(has!(Target::WifiToggle));
        assert!(has!(Target::BluetoothToggle));
        assert!(has!(Target::OpenSubmenu(Submenu::Wifi)));
        assert!(has!(Target::OpenSubmenu(Submenu::Bluetooth)));
        assert!(has!(Target::Dnd));
        assert!(has!(Target::NightShift));
        assert!(has!(Target::GameMode));
        assert!(has!(Target::OpenSettings("displays")));
        assert!(panel
            .targets
            .iter()
            .any(|(_, t)| matches!(t, Target::VolumeVertical(..) | Target::Volume(..))));
        assert!(panel
            .targets
            .iter()
            .any(|(_, t)| matches!(t, Target::BrightnessVertical(0, ..) | Target::Brightness(0, ..))));
        assert!(has!(Target::MediaPlay));
        assert!(has!(Target::ToggleEdit));
        let (_, _, w, h) = panel.rect();
        assert_eq!(w, CC_WIDTH);
        assert!(h > 300.0);

        // Edit mode turns every module into a draggable, removable card.
        panel.cc_toggle_edit();
        panel.paint(&mut pm, 1.0, 750.0, 800.0, &center, &apps, &fonts, &theme, false);
        assert!(has!(Target::GrabModule(ControlKind::Connectivity)));
        assert!(has!(Target::GrabModule(ControlKind::NowPlaying)));
        assert!(has!(Target::RemoveModule(ControlKind::Focus)));
        assert!(!has!(Target::WifiToggle), "module sub-targets are disabled while editing");

        // The gallery lists added and addable controls.
        panel.cc_open_gallery();
        panel.paint(&mut pm, 1.0, 750.0, 800.0, &center, &apps, &fonts, &theme, false);
        assert!(has!(Target::RemoveModule(ControlKind::Wifi)));
        assert!(has!(Target::RemoveModule(ControlKind::Connectivity)));
        assert!(has!(Target::PageAdd));
        assert!(has!(Target::CloseGallery));
    }
}
