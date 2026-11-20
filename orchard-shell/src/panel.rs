//! Notification Center (click the clock) and Control Center (the switches icon
//! in the menu bar). Both live on one full-screen, transparent overlay surface,
//! so a click anywhere outside the panel closes it, like on macOS.

use std::time::{Duration, Instant};

use tiny_skia::{FillRule, LineCap, LineJoin, PathBuilder, Pixmap, PixmapPaint, Stroke, Transform};

use crate::{
    anim::{self, ease_in_cubic, ease_out_cubic, lerp, lerp_rgba, tap_scale, SpringFloat, SpringParams},
    apps::Apps,
    draw::{composite, fill_circle, fill_rounded, paint, stroke_rounded},
    notify::{self, CardHit, Center, Hit},
    status::Network,
    text::{Fonts, Rgba, Weight},
    theme::Theme,
};

pub const NC_WIDTH: f32 = 356.0;
pub const CC_WIDTH: f32 = 320.0;
const MARGIN: f32 = 10.0;
const SLIDE_NC_OPEN: Duration = Duration::from_millis(240);
const SLIDE_NC_CLOSE: Duration = Duration::from_millis(175);
const CC_OPEN: Duration = Duration::from_millis(200);
const CC_CLOSE: Duration = Duration::from_millis(130);
const SUBMENU_ANIM: Duration = Duration::from_millis(190);

pub const PULSE_WIFI: usize = 0;
pub const PULSE_BT: usize = 1;
pub const PULSE_DND: usize = 2;
pub const PULSE_NIGHT: usize = 3;
pub const PULSE_STAGE: usize = 4;
pub const PULSE_GAME: usize = 5;

struct PanelAnim {
    initialized: bool,
    dnd: SpringFloat,
    night_shift: SpringFloat,
    stage_manager: SpringFloat,
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
    pulses: [Option<Instant>; 6],
}

impl PanelAnim {
    fn new() -> Self {
        Self {
            initialized: false,
            dnd: SpringFloat::new(0.0, SpringParams::SWITCH),
            night_shift: SpringFloat::new(0.0, SpringParams::SWITCH),
            stage_manager: SpringFloat::new(0.0, SpringParams::SWITCH),
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
            pulses: [None; 6],
        }
    }

    fn is_animating(&self, now: Instant) -> bool {
        self.dnd.is_animating(now)
            || self.night_shift.is_animating(now)
            || self.stage_manager.is_animating(now)
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
    StageManager,
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
    VolumeMute,
    /// `(display_index, min_x, max_x)`
    Brightness(usize, f32, f32),
    /// per-app volume: `(stream_index, min_x, max_x)`
    AppVolume(usize, f32, f32),
    AppMute(usize),
    MediaPrev,
    MediaPlay,
    MediaNext,
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
    /// Live menu-bar network state, used before the first async `query_control()` completes.
    pub network_hint: Network,
    /// slider being dragged
    pub dragging: Option<Target>,
    /// Night Shift on right now (schedule or manual switch)
    pub night_shift: bool,
    /// Stage Manager active right now
    pub stage_manager: bool,
    /// Game Mode switches on by itself for full-screen games
    pub game_mode_enabled: bool,
    /// the game Game Mode is on for
    pub game_active: Option<String>,
    anim: PanelAnim,
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
            stage_manager: false,
            game_mode_enabled: true,
            game_active: None,
            anim: PanelAnim::new(),
            targets: Vec::new(),
            cards: Vec::new(),
            panel_rect: (0.0, 0.0, 0.0, 0.0),
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

    pub fn is_animating(&self) -> bool {
        let now = Instant::now();
        if self.closing.is_some() {
            return !self.is_closed();
        }
        let open_dur = match self.kind {
            Kind::Notifications => SLIDE_NC_OPEN,
            Kind::Control => CC_OPEN,
        };
        (!anim::reduced() && (self.opened.elapsed() < open_dur || self.submenu_since.elapsed() < SUBMENU_ANIM))
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

    fn sync_springs(&mut self, center: &Center, now: Instant) {
        let dnd = if center.do_not_disturb { 1.0 } else { 0.0 };
        let night = if self.night_shift { 1.0 } else { 0.0 };
        let stage = if self.stage_manager { 1.0 } else { 0.0 };
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

        if !self.anim.initialized {
            self.anim.dnd.snap(dnd);
            self.anim.night_shift.snap(night);
            self.anim.stage_manager.snap(stage);
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
        self.anim.stage_manager.set_target(stage, now);
        self.anim.game_mode.set_target(game, now);
        self.anim.wifi.set_target(wifi, now);
        self.anim.ethernet.set_target(eth, now);
        self.anim.bluetooth.set_target(bt, now);
        self.anim.wifi_others.set_target(others, now);

        if matches!(self.dragging, Some(Target::Volume(..))) {
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
                if matches!(self.dragging, Some(Target::Brightness(i, ..)) if i == idx) {
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
        for ((rx, ry, rw, rh), t) in &self.targets {
            if x >= *rx && x < rx + rw && y >= *ry && y < ry + rh {
                return Some(t.clone());
            }
        }
        let (px, py, pw, ph) = self.panel_rect;
        (x >= px && x < px + pw && y >= py && y < py + ph).then_some(Target::Panel)
    }

    /// Slider value for pointer x.
    pub fn slider_value(target: &Target, x: f32) -> Option<f32> {
        match target {
            Target::Volume(a, b) | Target::Brightness(_, a, b) | Target::AppVolume(_, a, b) => {
                Some(((x - a) / (b - a)).clamp(0.0, 1.0))
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
        self.sync_springs(center, now);
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
        let y0 = 6.0 - (1.0 - factor) * 8.0;

        if factor < 0.999 {
            let pop_scale = 0.955 + 0.045 * factor;
            // Paint into a tight offscreen buffer at origin (0, 0) and composite anchored at top-right.
            let max_h = (h - MARGIN).max(1.0);
            let bw = (CC_WIDTH * s).ceil().max(1.0) as u32;
            let bh = (max_h * s).ceil().max(1.0) as u32;
            if let Some(mut buf) = Pixmap::new(bw, bh) {
                match self.submenu {
                    None => self.paint_cc(&mut buf, s, 0.0, 0.0, center, fonts, theme, german, now),
                    Some(sub) => self.paint_cc_submenu(&mut buf, s, 0.0, 0.0, max_h, sub, apps, fonts, theme, german, now),
                }
                let card_h = self.panel_rect.3;
                let scaled_w = CC_WIDTH * pop_scale;
                let scaled_h = card_h * pop_scale;
                // Top-right anchor matches the Control Center menu bar icon
                let draw_x = x0 + (CC_WIDTH - scaled_w);
                let draw_y = y0;
                self.panel_rect = (draw_x, draw_y, scaled_w, scaled_h);
                composite(pm, &buf, draw_x * s, draw_y * s, pop_scale, factor);
                if self.closing.is_some() {
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
        if self.closing.is_some() {
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

    // ---- Control Center (main view) ------------------------------------------

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
        let t = |en: &'static str, de: &'static str| if german { de } else { en };
        let dark = theme.bar_text[0] > 128;
        let tint: Rgba = if dark { [30, 30, 32, 160] } else { [242, 242, 245, 170] };
        let border: Rgba = if dark { [255, 255, 255, 34] } else { [0, 0, 0, 22] };
        let card: Rgba = if dark { [255, 255, 255, 22] } else { [255, 255, 255, 155] };
        let card_border: Rgba = if dark { [255, 255, 255, 18] } else { [0, 0, 0, 12] };
        let off: Rgba = if dark { [255, 255, 255, 40] } else { [0, 0, 0, 26] };
        let hover_bg: Rgba = if dark { [255, 255, 255, 18] } else { [0, 0, 0, 12] };
        let accent = theme.menu_selection;
        let text = theme.menu_text;
        let sub: Rgba = if dark { [235, 235, 235, 130] } else { [20, 20, 20, 120] };

        let pad = 10.0;
        let slide_x = if self.submenu_returning { -(1.0 - self.sub_progress()) * 10.0 } else { 0.0 };
        let inner_x = x0 + pad + slide_x;
        let inner_w = CC_WIDTH - 2.0 * pad;
        let half = (inner_w - pad) / 2.0;

        // compute target height and morph container smoothly from previous view
        let mut target_h = pad + 110.0 + pad;
        target_h += 50.0 + pad; // stage manager
        target_h += GAME_CARD_H + pad;
        let disp_count = self.control.displays.len();
        if disp_count == 1 {
            target_h += 64.0 + pad;
        } else if disp_count > 1 {
            target_h += 28.0 + disp_count as f32 * 44.0 + 6.0 + pad;
        }
        target_h += 64.0 + pad; // sound
        if self.control.media.is_some() {
            target_h += 64.0 + pad;
        }
        if !self.anim.panel_h_init {
            self.anim.panel_h.snap(target_h);
            self.anim.panel_h_init = true;
        } else {
            self.anim.panel_h.set_target(target_h, now);
        }
        let height = self.anim.panel_h.sample(now).max(120.0);
        self.panel_rect = (x0, y0, CC_WIDTH, height);
        fill_rounded(pm, x0 * s, y0 * s, CC_WIDTH * s, height * s, 18.0 * s, tint);
        stroke_rounded(pm, x0 * s, y0 * s, CC_WIDTH * s, height * s, 18.0 * s, 1.0 * s, border);

        let wifi_t = self.anim.wifi.sample(now).clamp(0.0, 1.0);
        let eth_t = self.anim.ethernet.sample(now).clamp(0.0, 1.0);
        let bt_t = self.anim.bluetooth.sample(now).clamp(0.0, 1.0);
        let dnd_t = self.anim.dnd.sample(now).clamp(0.0, 1.0);
        let night_t = self.anim.night_shift.sample(now).clamp(0.0, 1.0);
        let sm_t = self.anim.stage_manager.sample(now).clamp(0.0, 1.0);
        let game_t = self.anim.game_mode.sample(now).clamp(0.0, 1.0);
        let vol_val = self.anim.vol.sample(now).clamp(0.0, 1.0);

        // connectivity card: Wi-Fi / Ethernet (dynamic by active connection) + Bluetooth
        let y = y0 + pad;
        fill_rounded(pm, inner_x * s, y * s, half * s, 110.0 * s, 14.0 * s, card);
        stroke_rounded(pm, inner_x * s, y * s, half * s, 110.0 * s, 14.0 * s, 0.75 * s, card_border);

        let (wifi_on, ssid) = self.control.wifi.clone().unwrap_or((false, None));
        let bt_connected: Vec<&str> = self
            .control
            .bt_devices
            .iter()
            .filter(|d| d.connected)
            .map(|d| d.name.as_str())
            .collect();
        let bt_status = match self.control.bluetooth {
            Some(true) => match bt_connected.len() {
                0 => t("On", "Ein").to_string(),
                1 => bt_connected[0].to_string(),
                n => {
                    if german {
                        format!("{n} Geräte")
                    } else {
                        format!("{n} Devices")
                    }
                }
            },
            Some(false) => t("Off", "Aus").to_string(),
            None => t("Unavailable", "Nicht verfügbar").to_string(),
        };

        let show_eth = self.control.show_ethernet()
            || (self.control.wifi.is_none()
                && self.control.ethernet.is_empty()
                && self.network_hint == Network::Wired);
        let net_row = if show_eth {
            let eth_connected = self.control.ethernet_connected()
                || (self.control.ethernet.is_empty() && self.network_hint == Network::Wired);
            let primary = self.control.ethernet.first();
            let can_toggle = primary.is_some_and(|e| e.managed && e.plugged);
            let status = if eth_connected {
                t("Connected", "Verbunden").to_string()
            } else if primary.is_some_and(|e| e.plugged) {
                t("Disconnected", "Getrennt").to_string()
            } else {
                t("Unplugged", "Kein Kabel").to_string()
            };
            (
                can_toggle.then_some(Target::EthernetToggle),
                Submenu::Ethernet,
                PULSE_WIFI,
                "Ethernet".to_string(),
                status,
                eth_t,
                true,
            )
        } else {
            (
                self.control.wifi.is_some().then_some(Target::WifiToggle),
                Submenu::Wifi,
                PULSE_WIFI,
                "Wi-Fi".to_string(),
                if self.control.wifi.is_none() {
                    t("Unavailable", "Nicht verfügbar").to_string()
                } else if !wifi_on {
                    t("Off", "Aus").to_string()
                } else {
                    ssid.unwrap_or_else(|| t("Not Connected", "Nicht verbunden").into())
                },
                wifi_t,
                self.control.wifi.is_some(),
            )
        };

        let rows = [
            net_row,
            (
                self.control.bluetooth.is_some().then_some(Target::BluetoothToggle),
                Submenu::Bluetooth,
                PULSE_BT,
                "Bluetooth".to_string(),
                bt_status,
                bt_t,
                self.control.bluetooth.is_some(),
            ),
        ];

        for (i, (toggle_target, submenu, pulse_slot, title, status, on_t, available)) in rows.into_iter().enumerate() {
            let ry = y + 9.0 + i as f32 * 48.0;
            let row_rect = (inner_x + 4.0, ry, half - 8.0, 44.0);
            let circle_rect = (inner_x + 8.0, ry + 5.0, 32.0, 32.0);
            let open_rect = (inner_x + 40.0, ry, half - 42.0, 44.0);
            let hover_rect = if toggle_target.is_some() { open_rect } else { row_rect };
            let row_hovered = available && self.hovered(hover_rect.0, hover_rect.1, hover_rect.2, hover_rect.3);
            if row_hovered {
                fill_rounded(pm, row_rect.0 * s, row_rect.1 * s, row_rect.2 * s, row_rect.3 * s, 10.0 * s, hover_bg);
            }
            let (cx, cy) = (inner_x + 24.0, ry + 22.0);
            let (ps, _) = tap_scale(self.anim.pulses[pulse_slot], now);
            fill_circle(pm, cx * s, cy * s, 15.0 * ps * s, lerp_rgba(off, accent, on_t));
            let glyph = lerp_rgba(text, [255, 255, 255, 255], on_t);
            match submenu {
                Submenu::Wifi => crate::menubar::draw_network(pm, cx * s, (cy - 1.0) * s, s * 0.8 * ps, Network::Wifi(3), glyph),
                Submenu::Ethernet => crate::menubar::draw_network(pm, cx * s, cy * s, s * 1.0 * ps, Network::Wired, glyph),
                _ => bluetooth(pm, cx * s, cy * s, 7.0 * ps * s, glyph),
            }
            let tx = inner_x + 46.0;
            let maxw = half - 62.0;
            let b1 = fonts.baseline_centered((ry + 5.0) * s, 16.0 * s, 12.0 * s, Weight::Bold);
            fonts.draw(pm, &fonts.elide(&title, 12.0 * s, Weight::Bold, maxw * s), tx * s, b1, 12.0 * s, Weight::Bold, text);
            let b2 = fonts.baseline_centered((ry + 21.0) * s, 16.0 * s, 11.0 * s, Weight::Regular);
            fonts.draw(pm, &fonts.elide(&status, 11.0 * s, Weight::Regular, maxw * s), tx * s, b2, 11.0 * s, Weight::Regular, sub);
            if available {
                let chev_col = if row_hovered { text } else { sub };
                chevron_right(pm, (inner_x + half - 11.0) * s, (ry + 22.0) * s, 4.0 * s, chev_col);
                // Circle toggles directly when toggleable; rest of row opens submenu (1:1 macOS)
                if let Some(toggle) = toggle_target {
                    self.targets.push((circle_rect, toggle));
                    self.targets.push((open_rect, Target::OpenSubmenu(submenu)));
                } else {
                    self.targets.push((row_rect, Target::OpenSubmenu(submenu)));
                }
            }
        }

        // Do Not Disturb + Night Shift: two stacked cards on the right
        let fx = inner_x + half + pad;
        let card_h = (110.0 - pad) / 2.0;
        let dnd = center.do_not_disturb;
        let night = self.night_shift;
        let cards: [(Target, usize, &str, bool, f32, Rgba); 2] = [
            (Target::Dnd, PULSE_DND, t("Do Not Disturb", "Nicht stören"), dnd, dnd_t, [94, 92, 230, 255]),
            (Target::NightShift, PULSE_NIGHT, "Night Shift", night, night_t, [255, 149, 0, 255]),
        ];
        for (i, (target, pulse_slot, title, on, on_t, on_col)) in cards.into_iter().enumerate() {
            let cy0 = y + i as f32 * (card_h + pad);
            let hovered = self.hovered(fx, cy0, half, card_h);
            fill_rounded(pm, fx * s, cy0 * s, half * s, card_h * s, 14.0 * s, if hovered && !on { hover_bg } else { card });
            stroke_rounded(pm, fx * s, cy0 * s, half * s, card_h * s, 14.0 * s, 0.75 * s, card_border);
            let (cx, cy) = (fx + 22.0, cy0 + card_h / 2.0);
            let (ps, _) = tap_scale(self.anim.pulses[pulse_slot], now);
            fill_circle(pm, cx * s, cy * s, 14.0 * ps * s, lerp_rgba(off, on_col, on_t));
            let glyph = lerp_rgba(text, [255, 255, 255, 255], on_t);
            match target {
                Target::Dnd => moon(pm, cx * s, cy * s, 6.5 * ps * s, glyph),
                _ => sun(pm, cx * s, cy * s, 6.5 * ps * s, glyph),
            }
            let tx = fx + 42.0;
            let maxw = half - 48.0;
            let b1 = fonts.baseline_centered((cy - 15.0) * s, 16.0 * s, 12.0 * s, Weight::Bold);
            fonts.draw(pm, &fonts.elide(title, 12.0 * s, Weight::Bold, maxw * s), tx * s, b1, 12.0 * s, Weight::Bold, text);
            let b2 = fonts.baseline_centered((cy + 1.0) * s, 14.0 * s, 11.0 * s, Weight::Regular);
            fonts.draw(pm, if on { t("On", "Ein") } else { t("Off", "Aus") }, tx * s, b2, 11.0 * s, Weight::Regular, sub);
            self.targets.push(((fx, cy0, half, card_h), target));
        }

        // Stage Manager: quick toggle card (macOS Control Center)
        let sm_y = y + 110.0 + pad;
        let sm_h = 50.0;
        {
            let sm_on = self.stage_manager;
            let hovered = self.hovered(inner_x, sm_y, inner_w, sm_h);
            fill_rounded(pm, inner_x * s, sm_y * s, inner_w * s, sm_h * s, 14.0 * s, if hovered && !sm_on { hover_bg } else { card });
            stroke_rounded(pm, inner_x * s, sm_y * s, inner_w * s, sm_h * s, 14.0 * s, 0.75 * s, card_border);
            let (cx, cy) = (inner_x + 22.0, sm_y + sm_h / 2.0);
            let (ps, _) = tap_scale(self.anim.pulses[PULSE_STAGE], now);
            fill_circle(pm, cx * s, cy * s, 14.0 * ps * s, lerp_rgba(off, accent, sm_t));
            stage_manager_glyph(pm, cx * s, cy * s, 6.8 * ps * s, lerp_rgba(text, [255, 255, 255, 255], sm_t));
            let tx = inner_x + 42.0;
            let maxw = inner_w - 104.0;
            let b1 = fonts.baseline_centered((cy - 15.0) * s, 16.0 * s, 12.0 * s, Weight::Bold);
            fonts.draw(pm, &fonts.elide("Stage Manager", 12.0 * s, Weight::Bold, maxw * s), tx * s, b1, 12.0 * s, Weight::Bold, text);
            let b2 = fonts.baseline_centered((cy + 1.0) * s, 14.0 * s, 11.0 * s, Weight::Regular);
            fonts.draw(pm, if sm_on { t("On", "Ein") } else { t("Off", "Aus") }, tx * s, b2, 11.0 * s, Weight::Regular, sub);
            switch_anim(pm, s, inner_x + inner_w - 50.0, sm_y + (sm_h - 22.0) / 2.0, sm_t, accent, dark);
            self.targets.push(((inner_x, sm_y, inner_w, sm_h), Target::StageManager));
        }

        // Game Mode: full width under Stage Manager
        let gy = sm_y + sm_h + pad;
        {
            let on = self.game_mode_enabled;
            let hovered = self.hovered(inner_x, gy, inner_w, GAME_CARD_H);
            fill_rounded(pm, inner_x * s, gy * s, inner_w * s, GAME_CARD_H * s, 14.0 * s, if hovered && !on { hover_bg } else { card });
            stroke_rounded(pm, inner_x * s, gy * s, inner_w * s, GAME_CARD_H * s, 14.0 * s, 0.75 * s, card_border);
            let (cx, cy) = (inner_x + 22.0, gy + GAME_CARD_H / 2.0);
            let active = self.game_active.is_some();
            let (ps, _) = tap_scale(self.anim.pulses[PULSE_GAME], now);
            let on_col = if active { [52, 199, 89, 255] } else { accent };
            fill_circle(pm, cx * s, cy * s, 14.0 * ps * s, lerp_rgba(off, on_col, game_t));
            controller(pm, cx * s, cy * s, 7.0 * ps * s, lerp_rgba(text, [255, 255, 255, 255], game_t));
            let tx = inner_x + 42.0;
            let maxw = inner_w - 50.0;
            let b1 = fonts.baseline_centered((cy - 15.0) * s, 16.0 * s, 12.0 * s, Weight::Bold);
            fonts.draw(pm, t("Game Mode", "Spielmodus"), tx * s, b1, 12.0 * s, Weight::Bold, text);
            let status = match (&self.game_active, on) {
                (Some(game), _) => if german { format!("Ein · {game}") } else { format!("On · {game}") },
                (None, true) => t("Automatic: on while a game is full screen", "Automatisch: an, wenn ein Spiel im Vollbild läuft").to_string(),
                (None, false) => t("Off", "Aus").to_string(),
            };
            let b2 = fonts.baseline_centered((cy + 1.0) * s, 14.0 * s, 11.0 * s, Weight::Regular);
            fonts.draw(pm, &fonts.elide(&status, 11.0 * s, Weight::Regular, maxw * s), tx * s, b2, 11.0 * s, Weight::Regular, sub);
            self.targets.push(((inner_x, gy, inner_w, GAME_CARD_H), Target::GameMode));
        }

        // sliders
        let mut y = gy + GAME_CARD_H + pad;
        if self.control.displays.len() == 1 {
            let d_bus = self.control.displays[0].bus;
            let d_name = self.control.displays[0].name.clone();
            let d_val = self.anim.displays.get_mut(0).map(|sp| sp.sample(now)).unwrap_or(self.control.displays[0].value);
            fill_rounded(pm, inner_x * s, y * s, inner_w * s, 64.0 * s, 14.0 * s, card);
            stroke_rounded(pm, inner_x * s, y * s, inner_w * s, 64.0 * s, 14.0 * s, 0.75 * s, card_border);
            let b_base = fonts.baseline_centered((y + 6.0) * s, 18.0 * s, 12.0 * s, Weight::Bold);
            fonts.draw(pm, t("Display", "Display"), (inner_x + 12.0) * s, b_base, 12.0 * s, Weight::Bold, text);
            if d_bus.is_some() {
                let name = fonts.elide(&d_name, 11.0 * s, Weight::Regular, (inner_w - 86.0) * s);
                let nw = fonts.measure(&name, 11.0 * s, Weight::Regular);
                let nb = fonts.baseline_centered((y + 6.0) * s, 18.0 * s, 11.0 * s, Weight::Regular);
                fonts.draw(pm, &name, (inner_x + inner_w - 12.0) * s - nw, nb, 11.0 * s, Weight::Regular, sub);
            }
            let (tx, ty, tw, th) = (inner_x + 12.0, y + 32.0, inner_w - 24.0, 22.0);
            let hov = self.hovered(tx - 4.0, ty - 4.0, tw + 8.0, th + 8.0);
            let drag = matches!(self.dragging, Some(Target::Brightness(0, ..)));
            draw_pill_slider(pm, s, tx, ty, tw, th, d_val, 0, false, off, hov, drag);
            self.targets.push((
                (tx - 4.0, ty - 4.0, tw + 8.0, th + 8.0),
                Target::Brightness(0, tx + th / 2.0, tx + tw - th / 2.0),
            ));
            y += 64.0 + pad;
        } else if self.control.displays.len() > 1 {
            let count = self.control.displays.len();
            let card_h = 28.0 + count as f32 * 44.0 + 6.0;
            fill_rounded(pm, inner_x * s, y * s, inner_w * s, card_h * s, 14.0 * s, card);
            stroke_rounded(pm, inner_x * s, y * s, inner_w * s, card_h * s, 14.0 * s, 0.75 * s, card_border);
            let b_base = fonts.baseline_centered((y + 6.0) * s, 18.0 * s, 12.0 * s, Weight::Bold);
            fonts.draw(pm, t("Displays", "Displays"), (inner_x + 12.0) * s, b_base, 12.0 * s, Weight::Bold, text);
            for idx in 0..count {
                let d_name = self.control.displays[idx].name.clone();
                let d_val = self.anim.displays.get_mut(idx).map(|sp| sp.sample(now)).unwrap_or(self.control.displays[idx].value);
                let ry = y + 28.0 + idx as f32 * 44.0;
                let lb = fonts.baseline_centered(ry * s, 14.0 * s, 11.0 * s, Weight::Regular);
                let label = fonts.elide(&d_name, 11.0 * s, Weight::Regular, (inner_w - 24.0) * s);
                fonts.draw(pm, &label, (inner_x + 12.0) * s, lb, 11.0 * s, Weight::Regular, sub);
                let (tx, ty, tw, th) = (inner_x + 12.0, ry + 16.0, inner_w - 24.0, 22.0);
                let hov = self.hovered(tx - 4.0, ty - 4.0, tw + 8.0, th + 8.0);
                let drag = matches!(self.dragging, Some(Target::Brightness(i, ..)) if i == idx);
                draw_pill_slider(pm, s, tx, ty, tw, th, d_val, 0, false, off, hov, drag);
                self.targets.push((
                    (tx - 4.0, ty - 4.0, tw + 8.0, th + 8.0),
                    Target::Brightness(idx, tx + th / 2.0, tx + tw - th / 2.0),
                ));
            }
            y += card_h + pad;
        }

        // Sound card (header opens Sound submenu, slider adjusts volume)
        let (_, muted) = self.control.volume.unwrap_or((0.0, true));
        fill_rounded(pm, inner_x * s, y * s, inner_w * s, 64.0 * s, 14.0 * s, card);
        stroke_rounded(pm, inner_x * s, y * s, inner_w * s, 64.0 * s, 14.0 * s, 0.75 * s, card_border);
        let sound_head_rect = (inner_x + 4.0, y + 2.0, inner_w - 8.0, 26.0);
        let sound_hovered = self.hovered(sound_head_rect.0, sound_head_rect.1, sound_head_rect.2, sound_head_rect.3);
        if sound_hovered {
            fill_rounded(pm, sound_head_rect.0 * s, sound_head_rect.1 * s, sound_head_rect.2 * s, sound_head_rect.3 * s, 8.0 * s, hover_bg);
        }
        let b_base = fonts.baseline_centered((y + 6.0) * s, 18.0 * s, 12.0 * s, Weight::Bold);
        fonts.draw(pm, t("Sound", "Ton"), (inner_x + 12.0) * s, b_base, 12.0 * s, Weight::Bold, text);
        chevron_right(pm, (inner_x + inner_w - 14.0) * s, (y + 15.0) * s, 4.0 * s, if sound_hovered { text } else { sub });
        self.targets.push((sound_head_rect, Target::OpenSubmenu(Submenu::Sound)));

        let (tx, ty, tw, th) = (inner_x + 12.0, y + 32.0, inner_w - 24.0, 22.0);
        let vol_hov = self.hovered(tx - 4.0, ty - 4.0, tw + 8.0, th + 8.0);
        let vol_drag = matches!(self.dragging, Some(Target::Volume(..)));
        draw_pill_slider(pm, s, tx, ty, tw, th, vol_val, 1, muted, off, vol_hov, vol_drag);
        self.targets
            .push(((tx - 4.0, ty - 4.0, tw + 8.0, th + 8.0), Target::Volume(tx + th / 2.0, tx + tw - th / 2.0)));
        y += 64.0 + pad;

        // now playing
        if let Some(m) = self.control.media.clone() {
            fill_rounded(pm, inner_x * s, y * s, inner_w * s, 64.0 * s, 14.0 * s, card);
            stroke_rounded(pm, inner_x * s, y * s, inner_w * s, 64.0 * s, 14.0 * s, 0.75 * s, card_border);
            let maxw = inner_w - 130.0;
            let b1 = fonts.baseline_centered((y + 12.0) * s, 18.0 * s, 12.0 * s, Weight::Bold);
            let title = if m.title.is_empty() { t("Not Playing", "Keine Wiedergabe") } else { &m.title };
            fonts.draw(pm, &fonts.elide(title, 12.0 * s, Weight::Bold, maxw * s), (inner_x + 12.0) * s, b1, 12.0 * s, Weight::Bold, text);
            let b2 = fonts.baseline_centered((y + 32.0) * s, 16.0 * s, 11.0 * s, Weight::Regular);
            fonts.draw(pm, &fonts.elide(&m.artist, 11.0 * s, Weight::Regular, maxw * s), (inner_x + 12.0) * s, b2, 11.0 * s, Weight::Regular, sub);
            let cy = y + 32.0;
            let bx = inner_x + inner_w - 112.0;
            for (i, target) in [Target::MediaPrev, Target::MediaPlay, Target::MediaNext].into_iter().enumerate() {
                let cx = bx + 18.0 + i as f32 * 36.0;
                match target {
                    Target::MediaPrev => transport(pm, cx * s, cy * s, 6.0 * s, -1, text),
                    Target::MediaNext => transport(pm, cx * s, cy * s, 6.0 * s, 1, text),
                    _ if m.playing => pause(pm, cx * s, cy * s, 7.0 * s, text),
                    _ => play(pm, cx * s, cy * s, 8.0 * s, text),
                }
                self.targets.push(((cx - 16.0, cy - 16.0, 32.0, 32.0), target));
            }
        }
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
        let tint: Rgba = if dark { [30, 30, 32, 165] } else { [242, 242, 245, 178] };
        let border: Rgba = if dark { [255, 255, 255, 34] } else { [0, 0, 0, 22] };
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
        fill_rounded(pm, x0 * s, y0 * s, CC_WIDTH * s, total_h * s, 18.0 * s, tint);
        stroke_rounded(pm, x0 * s, y0 * s, CC_WIDTH * s, total_h * s, 18.0 * s, 1.0 * s, border);

        // ---- Header ----
        let hy = y0 + 4.0;
        let hb = fonts.baseline_centered(hy * s, header_h * s, 13.5 * s, Weight::Bold);
        if self.standalone {
            fonts.draw(pm, title, (x0 + 14.0) * s, hb, 13.5 * s, Weight::Bold, text);
        } else {
            let back_rect = (x0 + 6.0, hy + 4.0, if toggle_t.is_some() { CC_WIDTH - 68.0 } else { CC_WIDTH - 12.0 }, 36.0);
            let back_hovered = self.hovered(back_rect.0, back_rect.1, back_rect.2, back_rect.3);
            if back_hovered {
                fill_rounded(pm, back_rect.0 * s, back_rect.1 * s, back_rect.2 * s, back_rect.3 * s, 8.0 * s, hover_bg);
            }
            chevron_left(pm, (x0 + 18.0 + slide_x) * s, (hy + 22.0) * s, 4.5 * s, if back_hovered { text } else { sub_text });
            fonts.draw(pm, title, (x0 + 30.0 + slide_x) * s, hb, 13.5 * s, Weight::Bold, text);
            self.targets.push((back_rect, Target::SubmenuBack));
        }

        if let (Some(sw_t), Some(t_target)) = (toggle_t, toggle_target) {
            let sx = x0 + CC_WIDTH - 14.0 - 38.0;
            let sy = hy + 11.0;
            switch_anim(pm, s, sx, sy, sw_t, accent, dark);
            self.targets.push(((sx - 4.0, sy - 4.0, 46.0, 30.0), t_target));
        }

        // ---- Sound slider (directly under Sound header) ----
        let mut cur_y = hy + header_h;
        if submenu == Submenu::Sound {
            let (_, muted) = self.control.volume.unwrap_or((0.0, true));
            let (tx, ty, tw, th) = (x0 + 14.0, cur_y + 2.0, CC_WIDTH - 28.0, 22.0);
            let vol_hov = self.hovered(tx - 2.0, ty - 4.0, tw + 4.0, th + 8.0);
            let vol_drag = matches!(self.dragging, Some(Target::Volume(..)));
            draw_pill_slider(pm, s, tx, ty, tw, th, vol_val, 1, muted, off, vol_hov, vol_drag);
            // Clicking the speaker icon toggles mute; dragging anywhere else sets volume
            self.targets.push(((tx - 2.0, ty - 4.0, 24.0, th + 8.0), Target::VolumeMute));
            self.targets
                .push(((tx + 22.0, ty - 4.0, tw - 18.0, th + 8.0), Target::Volume(tx + th / 2.0, tx + tw - th / 2.0)));
            cur_y += sound_slider_h;
        }

        // ---- Scrollable body ----
        if content_h > 0.0 && body_h > 1.0 {
            draw_sep(pm, s, x0 + 14.0, cur_y, CC_WIDTH - 28.0, sep);
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
                                draw_section_header(&mut sub_pm, s, 14.0 + slide_x, ly, lbl, fonts, sub_text);
                                ly += 24.0;
                                for net in &known {
                                    let row = (6.0, ly, CC_WIDTH - 12.0, 38.0);
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
                                    draw_sep(&mut sub_pm, s, 14.0, ly + 4.0, CC_WIDTH - 28.0, sep);
                                    ly += 9.0;
                                }
                            }

                            if !others.is_empty() || known.is_empty() {
                                let hdr_rect = (6.0, ly, CC_WIDTH - 12.0, 26.0);
                                if is_hov(hdr_rect.0, hdr_rect.1, hdr_rect.2, hdr_rect.3) {
                                    fill_rounded(&mut sub_pm, hdr_rect.0 * s, hdr_rect.1 * s, hdr_rect.2 * s, hdr_rect.3 * s, 6.0 * s, hover_bg);
                                }
                                draw_section_header(&mut sub_pm, s, 14.0 + slide_x, ly + 1.0, t("Other Networks", "Andere Netzwerke"), fonts, sub_text);
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
                                            let row = (6.0, ly, CC_WIDTH - 12.0, 38.0);
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
                                draw_sep(&mut sub_pm, s, 14.0, ly + 4.0, CC_WIDTH - 28.0, sep);
                                ly += 9.0;
                            }
                            draw_section_header(&mut sub_pm, s, 14.0 + slide_x, ly, "Ethernet", fonts, sub_text);
                            ly += 24.0;
                            if let Some(dev) = self.control.ethernet.first() {
                                let row = (6.0, ly, CC_WIDTH - 12.0, 38.0);
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
                        draw_section_header(&mut sub_pm, s, 14.0 + slide_x, ly, t("Connections", "Verbindungen"), fonts, sub_text);
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
                                let row = (6.0, ly, CC_WIDTH - 12.0, 38.0);
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
                            draw_sep(&mut sub_pm, s, 14.0, ly + 4.0, CC_WIDTH - 28.0, sep);
                            ly += 9.0;
                            draw_section_header(&mut sub_pm, s, 14.0 + slide_x, ly, "Wi-Fi", fonts, sub_text);
                            ly += 24.0;
                            let row = (6.0, ly, CC_WIDTH - 12.0, 38.0);
                            let circle_rect = (12.0, ly + 5.0, 28.0, 28.0);
                            let open_rect = (42.0, ly, CC_WIDTH - 48.0, 38.0);
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
                        draw_section_header(&mut sub_pm, s, 14.0 + slide_x, ly, t("Devices", "Geräte"), fonts, sub_text);
                        ly += 24.0;
                        if self.control.bt_devices.is_empty() {
                            let b = fonts.baseline_centered(ly * s, 34.0 * s, 12.0 * s, Weight::Regular);
                            fonts.draw(&mut sub_pm, t("No Devices", "Keine Geräte"), (16.0 + slide_x) * s, b, 12.0 * s, Weight::Regular, sub_text);
                        } else {
                            for dev in &self.control.bt_devices {
                                let row = (6.0, ly, CC_WIDTH - 12.0, 38.0);
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
                        draw_section_header(&mut sub_pm, s, 14.0 + slide_x, ly, t("Output", "Ausgabe"), fonts, sub_text);
                        ly += 24.0;
                        if self.control.audio_sinks.is_empty() {
                            let b = fonts.baseline_centered(ly * s, 34.0 * s, 12.0 * s, Weight::Regular);
                            fonts.draw(&mut sub_pm, t("No Output Devices", "Keine Ausgabegeräte"), (16.0 + slide_x) * s, b, 12.0 * s, Weight::Regular, sub_text);
                        } else {
                            for sink in &self.control.audio_sinks {
                                let row = (6.0, ly, CC_WIDTH - 12.0, 38.0);
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
                            draw_sep(&mut sub_pm, s, 14.0, ly + 4.0, CC_WIDTH - 28.0, sep);
                            ly += 9.0;
                            draw_section_header(&mut sub_pm, s, 14.0 + slide_x, ly, t("Apps", "Apps"), fonts, sub_text);
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
                                    pm_draw_icon(&mut sub_pm, &icon, (14.0 + slide_x) * s, (ly + 3.0) * s, 16.0 * s);
                                    36.0
                                } else {
                                    14.0
                                };
                                let b = fonts.baseline_centered((ly + 2.0) * s, 18.0 * s, 12.0 * s, Weight::Regular);
                                let label = fonts.elide(&st_name, 12.0 * s, Weight::Regular, (CC_WIDTH - name_x - 18.0) * s);
                                fonts.draw(&mut sub_pm, &label, (name_x + slide_x) * s, b, 12.0 * s, Weight::Regular, text);
                                let (tx, ty, tw, th) = (14.0 + slide_x, ly + 22.0, CC_WIDTH - 28.0, 20.0);
                                let app_hov = is_hov(tx - 2.0, ty - 3.0, tw + 4.0, th + 6.0);
                                let app_drag = matches!(self.dragging, Some(Target::AppVolume(idx, ..)) if idx == i);
                                draw_pill_slider(&mut sub_pm, s, tx, ty, tw, th, shown, 1, st_muted, off, app_hov, app_drag);
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
        draw_sep(pm, s, x0 + 14.0, cur_y + 2.0, CC_WIDTH - 28.0, sep);
        let foot_rect = (x0 + 6.0, cur_y + 6.0, CC_WIDTH - 12.0, 30.0);
        if self.hovered(foot_rect.0, foot_rect.1, foot_rect.2, foot_rect.3) {
            fill_rounded(pm, foot_rect.0 * s, foot_rect.1 * s, foot_rect.2 * s, foot_rect.3 * s, 8.0 * s, hover_bg);
        }
        let fb = fonts.baseline_centered(foot_rect.1 * s, foot_rect.3 * s, 12.5 * s, Weight::Regular);
        fonts.draw(pm, settings_label, (x0 + 16.0) * s, fb, 12.5 * s, Weight::Regular, text);
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
) {
    fill_rounded(pm, tx * s, ty * s, tw * s, th * s, (th / 2.0) * s, off);
    let fill_w = (th + (tw - th) * value.clamp(0.0, 1.0)).max(th);
    let fill_alpha = if dragging { 252 } else if hovered { 246 } else { 236 };
    fill_rounded(pm, tx * s, ty * s, fill_w * s, th * s, (th / 2.0) * s, [255, 255, 255, fill_alpha]);
    let knob_x = tx + fill_w - th / 2.0;
    let knob_r = th / 2.0 - if dragging { 0.6 } else { 1.0 };
    if hovered || dragging {
        fill_circle(pm, knob_x * s, (ty + th / 2.0 + 0.6) * s, knob_r * s, [0, 0, 0, if dragging { 38 } else { 22 }]);
    }
    fill_circle(pm, knob_x * s, (ty + th / 2.0) * s, knob_r * s, [255, 255, 255, 255]);
    stroke_rounded(
        pm,
        (knob_x - knob_r) * s,
        (ty + th / 2.0 - knob_r) * s,
        (knob_r * 2.0) * s,
        (knob_r * 2.0) * s,
        knob_r * s,
        1.0 * s,
        [0, 0, 0, if dragging { 42 } else { 28 }],
    );
    let glyph: Rgba = [90, 90, 92, 255];
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

/// Height of the Control Center's Game Mode card.
const GAME_CARD_H: f32 = 50.0;

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

/// macOS Stage Manager glyph: three stacked recent tiles on the left + main window on the right.
pub fn stage_manager_glyph(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    let left_x = cx - r * 1.02;
    let tile_w = r * 0.42;
    let tile_h = r * 0.34;
    let gap = r * 0.16;
    let total_h = tile_h * 3.0 + gap * 2.0;
    let top_y = cy - total_h / 2.0;
    for i in 0..3 {
        let ty = top_y + i as f32 * (tile_h + gap);
        fill_rounded(pm, left_x, ty, tile_w, tile_h, r * 0.12, c);
    }
    let main_x = left_x + tile_w + r * 0.24;
    let main_w = r * 1.38;
    let main_h = total_h * 0.92;
    let main_y = cy - main_h / 2.0;
    stroke_rounded(pm, main_x, main_y, main_w, main_h, r * 0.24, (r * 0.22).max(1.2), c);
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

fn query_wifi_networks() -> Vec<WifiNetwork> {
    let list = cmd_localized("nmcli", &["-t", "-f", "IN-USE,SSID,SIGNAL,SECURITY", "dev", "wifi", "list"]).unwrap_or_default();
    let cons = cmd_localized("nmcli", &["-t", "-f", "NAME,TYPE", "con", "show"]).unwrap_or_default();
    parse_wifi_networks(&list, &cons)
}

fn query_bt_devices() -> Vec<BtDevice> {
    let paired = parse_bt_list(&cmd("bluetoothctl", &["devices", "Paired"]).unwrap_or_default());
    let connected: Vec<String> = parse_bt_list(&cmd("bluetoothctl", &["devices", "Connected"]).unwrap_or_default())
        .into_iter()
        .map(|(m, _)| m)
        .collect();
    let mut devices = Vec::new();
    for (mac, name) in paired {
        let is_connected = connected.contains(&mac);
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
        devices.push(BtDevice {
            mac,
            name,
            connected: is_connected,
            icon,
            battery,
        });
    }
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

fn query_audio_sinks() -> Vec<AudioSink> {
    let json = cmd_localized("pactl", &["-f", "json", "list", "sinks"]).unwrap_or_default();
    let default_sink = cmd("pactl", &["get-default-sink"]).unwrap_or_default().trim().to_string();
    parse_audio_sinks(&json, &default_sink)
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

pub fn query_control() -> ControlState {
    let (has_wifi, active_wifi_con, ethernet) =
        if let Some(devs) = cmd("nmcli", &["-t", "-f", "DEVICE,TYPE,STATE,CONNECTION", "dev"]) {
            let (hw, aw, mut eths) = parse_nm_devices(&devs);
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
            (hw, aw, eths)
        } else {
            (false, None, query_ethernet_sysfs())
        };
    let wifi_on = if has_wifi {
        cmd("nmcli", &["-t", "radio", "wifi"]).map(|r| r.trim() == "enabled")
    } else {
        None
    };
    let wifi_networks = if wifi_on == Some(true) {
        query_wifi_networks()
    } else {
        Vec::new()
    };
    let wifi = wifi_on.map(|on| {
        let ssid = if on {
            wifi_networks
                .iter()
                .find(|n| n.active)
                .map(|n| n.ssid.clone())
                .or(active_wifi_con)
        } else {
            None
        };
        (on, ssid)
    });

    let bluetooth = cmd("bluetoothctl", &["show"]).and_then(|o| {
        if o.contains("Powered: yes") {
            Some(true)
        } else if o.contains("Powered: no") {
            Some(false)
        } else {
            None
        }
    });
    let bt_devices = if bluetooth == Some(true) {
        query_bt_devices()
    } else {
        Vec::new()
    };

    let volume = cmd("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"]).and_then(|o| crate::osd::parse_wpctl(&o));
    let audio_sinks = query_audio_sinks();
    let streams = query_streams();

    let displays = crate::ddc::query_displays();

    let media = sh("playerctl metadata --format '{{status}}\t{{title}}\t{{artist}}'").and_then(|o| {
        let mut parts = o.trim_end_matches('\n').splitn(3, '\t');
        let status = parts.next()?;
        Some(Media {
            playing: status == "Playing",
            title: parts.next().unwrap_or_default().to_string(),
            artist: parts.next().unwrap_or_default().to_string(),
        })
    });

    ControlState {
        wifi,
        wifi_networks,
        ethernet,
        bluetooth,
        bt_devices,
        volume,
        audio_sinks,
        streams,
        displays,
        media,
    }
}

/// Fast path for sliders & mute: only applies the system change without running
/// the 15-subprocess full `query_control()`.
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
        Target::Volume(..) => {
            if let Some(v) = value {
                let _ = cmd("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", "0"]);
                let _ = cmd("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", &format!("{:.3}", v)]);
            }
        }
        Target::VolumeMute => {
            let _ = cmd("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"]);
        }
        Target::Brightness(idx, ..) => {
            if let (Some(v), Some(d)) = (value, displays.get(*idx)) {
                crate::ddc::set_brightness(d.bus, v);
            }
        }
        _ => {}
    }
}

/// Run a Control Center action; returns the new state.
pub fn run_control(target: &Target, value: Option<f32>, current: &ControlState) -> ControlState {
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
        Target::Volume(..) => {
            if let Some(v) = value {
                let _ = cmd("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", "0"]);
                let _ = cmd("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", &format!("{:.3}", v)]);
            }
        }
        Target::VolumeMute => {
            let _ = cmd("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"]);
        }
        Target::Brightness(idx, ..) => {
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
    query_control()
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

    #[test]
    fn quicksettings_switches_targets_between_wifi_and_ethernet() {
        let fonts = Fonts::load();
        let theme = Theme::new(crate::theme::Appearance::Dark);
        let center = Center::default();
        let apps = Apps::load(None);
        let mut pm = Pixmap::new(640, 800).unwrap();

        let mut panel = Panel::new(Kind::Control, 620.0);
        panel.opened = Instant::now() - Duration::from_secs(1);
        // Connected only via Ethernet -> main view exposes Ethernet toggle & submenu
        panel.control = ControlState {
            wifi: Some((false, None)),
            ethernet: vec![EthernetDevice {
                device: "enp5s0".into(),
                connection: Some("Wired connection 1".into()),
                ip: Some("10.0.0.5".into()),
                connected: true,
                plugged: true,
                managed: true,
            }],
            ..Default::default()
        };
        panel.paint(&mut pm, 1.0, 640.0, 800.0, &center, &apps, &fonts, &theme, false);
        assert!(panel.targets.iter().any(|(_, t)| *t == Target::EthernetToggle));
        assert!(panel.targets.iter().any(|(_, t)| *t == Target::OpenSubmenu(Submenu::Ethernet)));
        assert!(!panel.targets.iter().any(|(_, t)| *t == Target::WifiToggle));

        // Open Ethernet submenu -> exposes EthToggleDevice + quick-access Wi-Fi link
        panel.open_submenu(Submenu::Ethernet);
        panel.paint(&mut pm, 1.0, 640.0, 800.0, &center, &apps, &fonts, &theme, false);
        assert!(panel.targets.iter().any(|(_, t)| {
            matches!(t, Target::EthToggleDevice { device, connected: true } if device == "enp5s0")
        }));
        assert!(panel.targets.iter().any(|(_, t)| *t == Target::OpenSubmenu(Submenu::Wifi)));

        // Switch to Wi-Fi connected -> main view switches back to Wi-Fi
        panel.close_submenu();
        panel.control.wifi = Some((true, Some("HomeNet".into())));
        panel.control.ethernet[0].connected = false;
        panel.paint(&mut pm, 1.0, 640.0, 800.0, &center, &apps, &fonts, &theme, false);
        assert!(panel.targets.iter().any(|(_, t)| *t == Target::WifiToggle));
        assert!(panel.targets.iter().any(|(_, t)| *t == Target::OpenSubmenu(Submenu::Wifi)));
        assert!(!panel.targets.iter().any(|(_, t)| *t == Target::EthernetToggle));
    }
}
