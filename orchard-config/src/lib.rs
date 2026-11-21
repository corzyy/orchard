//! Orchard's settings file: `~/.config/orchard/settings.json`.
//!
//! Written by `orchard-settings`, read by the compositor and `orchard-shell`.
//! After saving, a writer sends `{"request":"reload_settings"}` on the IPC
//! socket; the compositor re-applies everything and broadcasts a
//! `settings_changed` event so the shell can do the same.
//!
//! Every field has a default, so older or partial files keep working.

use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    /// follow `org.gnome.desktop.interface color-scheme`
    #[default]
    Auto,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Accent {
    #[default]
    Blue,
    Purple,
    Pink,
    Red,
    Orange,
    Yellow,
    Green,
    Graphite,
    Wallpaper,
}

impl Accent {
    pub const ALL: [Accent; 8] = [
        Accent::Blue,
        Accent::Purple,
        Accent::Pink,
        Accent::Red,
        Accent::Orange,
        Accent::Yellow,
        Accent::Green,
        Accent::Graphite,
    ];
    /// sRGB colour of the accent.
    pub fn rgb(self) -> [u8; 3] {
        match self {
            Accent::Blue => [0x00, 0x7A, 0xFF],
            Accent::Purple => [0x95, 0x3D, 0x96],
            Accent::Pink => [0xF7, 0x4F, 0x9E],
            Accent::Red => [0xE0, 0x38, 0x3E],
            Accent::Orange => [0xF7, 0x82, 0x1B],
            Accent::Yellow => [0xFC, 0xB8, 0x27],
            Accent::Green => [0x62, 0xBA, 0x46],
            Accent::Graphite => [0x8C, 0x8C, 0x8C],
            Accent::Wallpaper => [0x6D, 0x6B, 0xC4],
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Accent::Blue => "Blue",
            Accent::Purple => "Purple",
            Accent::Pink => "Pink",
            Accent::Red => "Red",
            Accent::Orange => "Orange",
            Accent::Yellow => "Yellow",
            Accent::Green => "Green",
            Accent::Graphite => "Graphite",
            Accent::Wallpaper => "Wallpaper",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceSettings {
    pub mode: ThemeMode,
    pub accent: Accent,
    /// Accent colour extracted from the current wallpaper when `accent == Accent::Wallpaper`.
    pub wallpaper_accent: Option<[u8; 3]>,
    /// Replace spring / genie / slide animations with quick fades.
    pub reduce_motion: bool,
    /// Draw panels with solid colours instead of frosted glass.
    pub reduce_transparency: bool,
    /// Window corner radius in logical px.
    pub corner_radius: f32,
    /// Icon theme for Orchard and the apps running in it.
    /// `None` = [`DEFAULT_ICON_THEME`].
    pub icon_theme: Option<String>,
    /// Pointer theme for Orchard and the apps running in it.
    /// `None` = [`DEFAULT_CURSOR_THEME`].
    pub cursor_theme: Option<String>,
    /// Pointer size in logical px.
    pub cursor_size: u32,
}

pub const DEFAULT_ICON_THEME: &str = "Adwaita";
pub const DEFAULT_CURSOR_THEME: &str = "Adwaita";

impl AppearanceSettings {
    /// Effective sRGB accent colour (using `wallpaper_accent` when `accent == Accent::Wallpaper`).
    pub fn accent_rgb(&self) -> [u8; 3] {
        match self.accent {
            Accent::Wallpaper => self.wallpaper_accent.unwrap_or_else(|| Accent::Wallpaper.rgb()),
            other => other.rgb(),
        }
    }
    pub fn icon_theme(&self) -> &str {
        self.icon_theme.as_deref().filter(|t| !t.is_empty()).unwrap_or(DEFAULT_ICON_THEME)
    }
    pub fn cursor_theme(&self) -> &str {
        self.cursor_theme.as_deref().filter(|t| !t.is_empty()).unwrap_or(DEFAULT_CURSOR_THEME)
    }
    pub fn cursor_size(&self) -> u32 {
        if (8..=256).contains(&self.cursor_size) { self.cursor_size } else { 24 }
    }
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            mode: ThemeMode::Auto,
            accent: Accent::Blue,
            wallpaper_accent: None,
            reduce_motion: false,
            reduce_transparency: false,
            corner_radius: 10.0,
            icon_theme: None,
            cursor_theme: None,
            cursor_size: 24,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TitlebarDoubleClick {
    #[default]
    Zoom,
    Minimize,
    Nothing,
}

/// Floating-mode window snapping.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TilingSettings {
    /// Margin between tiles and the screen edges, logical px.
    pub outer_gap: i32,
    /// Space between two tiles, logical px.
    pub inner_gap: i32,
    /// Dragging a window against a screen edge tiles it.
    pub snap_to_edges: bool,
    /// Show the translucent drop preview while dragging.
    pub snap_preview: bool,
    /// Corners give quarter tiles.
    pub snap_corners: bool,
    /// How close to the edge (px) the pointer must get to snap.
    pub edge_zone: i32,
}

impl TilingSettings {
    pub const GAP_MAX: i32 = 48;

    /// The values the compositor works with: everything within its range.
    pub fn clamped(&self) -> Self {
        let mut t = self.clone();
        t.outer_gap = t.outer_gap.clamp(0, Self::GAP_MAX);
        t.inner_gap = t.inner_gap.clamp(0, Self::GAP_MAX);
        t.edge_zone = t.edge_zone.clamp(8, 120);
        t
    }
}

impl Default for TilingSettings {
    fn default() -> Self {
        Self {
            outer_gap: 8,
            inner_gap: 8,
            snap_to_edges: true,
            snap_preview: true,
            snap_corners: true,
            edge_zone: 32,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DockSettings {
    /// Tile size in logical px.
    pub icon_size: f32,
    /// Size of the tile under the pointer. Equal to `icon_size` means
    /// magnification is off.
    pub magnified_size: f32,
    /// When the Dock hides itself.
    pub autohide: DockAutohide,
    /// Screen edge the Dock sits on.
    pub position: DockPosition,
    /// Show the Trash at the end of the Dock.
    pub show_trash: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DockAutohide {
    /// Always visible (reserves an exclusive zone).
    #[default]
    Never,
    /// Hide whenever a window overlaps the Dock; reveal on edge hover.
    AvoidWindows,
    /// Always hide until the pointer touches the screen edge.
    Always,
}

impl<'de> Deserialize<'de> for DockAutohide {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = DockAutohide;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a dock hide mode (\"never\", \"avoid_windows\", \"always\") or boolean")
            }

            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(if v { DockAutohide::Always } else { DockAutohide::Never })
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                match v {
                    "never" => Ok(DockAutohide::Never),
                    "avoid_windows" => Ok(DockAutohide::AvoidWindows),
                    "always" => Ok(DockAutohide::Always),
                    _ => Err(E::unknown_variant(v, &["never", "avoid_windows", "always"])),
                }
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DockPosition {
    Left,
    #[default]
    Bottom,
    Right,
}

impl Default for DockSettings {
    fn default() -> Self {
        Self {
            icon_size: 52.0,
            magnified_size: 84.0,
            autohide: DockAutohide::Never,
            position: DockPosition::Bottom,
            show_trash: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MenuBarSettings {
    /// None = follow the locale.
    pub clock_24h: Option<bool>,
    pub show_seconds: bool,
    pub show_date: bool,
    pub show_weekday: bool,
    pub battery_percentage: bool,
    pub show_network: bool,
    pub show_volume: bool,
    pub show_battery: bool,
}

impl Default for MenuBarSettings {
    fn default() -> Self {
        Self {
            clock_24h: None,
            show_seconds: false,
            show_date: true,
            show_weekday: true,
            battery_percentage: false,
            show_network: true,
            show_volume: true,
            show_battery: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowSettings {
    pub double_click_titlebar: TitlebarDoubleClick,
    /// Focus follows the pointer (no raise).
    pub focus_follows_mouse: bool,
    /// Show files and folders from ~/Desktop on the desktop background.
    pub show_desktop_icons: bool,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            double_click_titlebar: TitlebarDoubleClick::Zoom,
            focus_follows_mouse: false,
            show_desktop_icons: true,
        }
    }
}

/// Spotlight (⌘Space) behaviour and result sources.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchSettings {
    /// Include installed applications.
    pub show_apps: bool,
    /// Include System Settings panes.
    pub show_settings: bool,
    /// Include system actions (Lock Screen, Restart, …).
    pub show_actions: bool,
    /// Include files and folders from the home folder.
    pub show_files: bool,
    /// Offer a web search when nothing else fits.
    pub show_web: bool,
    /// Reopen Spotlight with the previous search selected.
    pub keep_query: bool,
    /// Rebuild the file index when it is older than this many minutes.
    pub index_minutes: u32,
}

impl SearchSettings {
    pub const INDEX_MINUTES: [u32; 5] = [5, 10, 30, 60, 180];

    /// The interval the shell uses, clamped to a sane range.
    pub fn reindex_after(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.index_minutes.clamp(1, 24 * 60) as u64 * 60)
    }
}

impl Default for SearchSettings {
    fn default() -> Self {
        Self {
            show_apps: true,
            show_settings: true,
            show_actions: true,
            show_files: true,
            show_web: true,
            keep_query: true,
            index_minutes: 10,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct KeyboardSettings {
    /// Empty = the system layout from systemd-localed (comma separated for several).
    pub layout: String,
    pub variant: String,
    pub options: String,
    /// ms; 0 = default (250)
    pub repeat_delay: i32,
    /// keys/s; 0 = default (30)
    pub repeat_rate: i32,
    /// Start with Num Lock on.
    pub numlock: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SecondaryClick {
    /// Two fingers anywhere (macOS)
    #[default]
    TwoFingers,
    /// Bottom-right corner of the pad
    BottomRight,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PointerSettings {
    /// libinput accel speed, -1.0..=1.0
    pub mouse_speed: f64,
    /// adaptive (true) or flat (false) acceleration
    pub mouse_acceleration: bool,
    pub mouse_natural_scroll: bool,
    pub mouse_left_handed: bool,
    pub touchpad_speed: f64,
    pub tap_to_click: bool,
    pub touchpad_natural_scroll: bool,
    pub disable_while_typing: bool,
    pub secondary_click: SecondaryClick,
    /// Scroll speed multiplier
    pub scroll_factor: f64,
    /// Shaking the mouse briefly enlarges the pointer.
    pub shake_to_locate: bool,
}

impl Default for PointerSettings {
    fn default() -> Self {
        Self {
            mouse_speed: 0.0,
            mouse_acceleration: true,
            mouse_natural_scroll: false,
            mouse_left_handed: false,
            touchpad_speed: 0.0,
            tap_to_click: true,
            touchpad_natural_scroll: true,
            disable_while_typing: true,
            secondary_click: SecondaryClick::TwoFingers,
            scroll_factor: 1.0,
            shake_to_locate: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CornerAction {
    #[default]
    None,
    MissionControl,
    AppExpose,
    Launcher,
    LockScreen,
    Launchpad,
}

impl CornerAction {
    pub const ALL: [CornerAction; 6] = [
        CornerAction::None,
        CornerAction::MissionControl,
        CornerAction::AppExpose,
        CornerAction::Launchpad,
        CornerAction::Launcher,
        CornerAction::LockScreen,
    ];
    pub fn label(self) -> &'static str {
        match self {
            CornerAction::None => "—",
            CornerAction::MissionControl => "Mission Control",
            CornerAction::AppExpose => "Application Windows",
            CornerAction::Launcher => "Launcher",
            CornerAction::LockScreen => "Lock Screen",
            CornerAction::Launchpad => "Launchpad",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct HotCorners {
    pub top_left: CornerAction,
    pub top_right: CornerAction,
    pub bottom_left: CornerAction,
    pub bottom_right: CornerAction,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DesktopSettings {
    pub hot_corners: HotCorners,
    /// Pressing ⌘↑ etc. switches into Mission Control; swipes too.
    pub gestures: bool,
    /// Switch to a Space with open windows for the application when
    /// activating it from the Dock.
    pub switch_space_on_activate: bool,
    /// Wallpaper image; None = Orchard's built-in gradient.
    pub wallpaper: Option<String>,
    /// User-added wallpaper image paths shown in Settings.
    pub wallpapers: Vec<String>,
}

impl Default for DesktopSettings {
    fn default() -> Self {
        Self {
            hot_corners: HotCorners::default(),
            gestures: true,
            switch_space_on_activate: true,
            wallpaper: None,
            wallpapers: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LockSettings {
    /// Lock after this many minutes without input; 0 = never.
    pub lock_after_minutes: u32,
    /// Command used for locking (⌃⌘Q, hot corner, idle). Empty: Orchard's own
    /// lock screen (`orchard-lock`).
    pub locker: String,
}

/// The locker settings files written before `orchard-lock` existed; treated
/// like an empty `locker`.
pub const LEGACY_LOCKER: &str = "swaylock -f -c 1e1e1e";

impl Default for LockSettings {
    fn default() -> Self {
        Self {
            lock_after_minutes: 0,
            locker: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DisplaySettings {
    /// output name -> scale factor
    pub scale: BTreeMap<String, f64>,
    /// output name -> refresh rate in mHz (at the display's preferred resolution)
    pub refresh: BTreeMap<String, i32>,
    /// Output that shows the menu bar and Dock. `None` = the first output.
    pub primary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// ⌘Return
    pub terminal: String,
    /// Run `~/.config/autostart` and `/etc/xdg/autostart` entries at login.
    pub run_autostart: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            terminal: String::new(),
            run_autostart: true,
        }
    }
}

/// True if the first token of `cmd` is an existing executable on `PATH` (or an absolute path).
pub fn command_in_path(cmd: &str) -> bool {
    let Some(bin) = cmd.split_whitespace().next() else {
        return false;
    };
    if bin.starts_with('/') {
        return std::path::Path::new(bin).is_file();
    }
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

/// First installed terminal emulator (no KDE dependency).
pub fn default_terminal() -> String {
    const CANDIDATES: &[&str] = &[
        "ghostty",
        "kitty",
        "alacritty",
        "foot",
        "wezterm",
        "ptyxis",
        "gnome-terminal",
        "xfce4-terminal",
        "kgx",
        "xterm",
    ];
    for &c in CANDIDATES {
        if command_in_path(c) {
            return c.to_string();
        }
    }
    for app in desktop::applications() {
        if app.categories().iter().any(|c| c == "TerminalEmulator") {
            if let Some(cmd) = app.command() {
                if let Some(bin) = cmd.split_whitespace().next() {
                    if command_in_path(bin) {
                        return bin.to_string();
                    }
                }
            }
        }
    }
    "xterm".to_string()
}

/// Resolve the terminal command: `ORCHARD_TERMINAL` env var, then `configured`
/// (if non-empty and installed), then [`default_terminal`].
pub fn resolve_terminal(configured: &str) -> String {
    if let Ok(env) = std::env::var("ORCHARD_TERMINAL") {
        let t = env.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    let t = configured.trim();
    if !t.is_empty() && command_in_path(t) {
        return t.to_string();
    }
    default_terminal()
}

/// Build a shell command line that executes `args` inside `terminal`.
pub fn terminal_exec(terminal: &str, args: &str) -> String {
    let bin = terminal.split_whitespace().next().unwrap_or(terminal);
    let base = bin.rsplit('/').next().unwrap_or(bin);
    match base {
        "gnome-terminal" | "ptyxis" | "kgx" => format!("{terminal} -- {args}"),
        "wezterm" => format!("{terminal} start -- {args}"),
        _ => format!("{terminal} -e {args}"),
    }
}

/// Best available system monitor command (GUI first, else a TUI monitor in the terminal).
pub fn system_monitor_command() -> String {
    const GUI: &[&str] = &[
        "missioncenter",
        "resources",
        "gnome-system-monitor",
        "xfce4-taskmanager",
        "mate-system-monitor",
        "lxtask",
    ];
    for &c in GUI {
        if command_in_path(c) {
            return c.to_string();
        }
    }
    let term = resolve_terminal(&Settings::load().apps.terminal);
    for &tui in &["btop", "htop", "top"] {
        if command_in_path(tui) {
            return terminal_exec(&term, tui);
        }
    }
    terminal_exec(&term, "top")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VrrMode {
    #[default]
    Off,
    /// Enable VRR automatically while a fullscreen window is active on the output.
    Fullscreen,
    /// Always enable VRR on outputs that support it.
    On,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PerformancePreset {
    #[default]
    Balanced,
    /// Lowest latency: VRR for fullscreen, direct scanout, tighter frame pacing.
    Performance,
    /// Battery-friendly: static wallpaper blur and reduced background polling.
    PowerSaver,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PerformanceSettings {
    pub preset: PerformancePreset,
    /// Variable Refresh Rate (Adaptive Sync / FreeSync / G-Sync) policy.
    pub vrr: VrrMode,
    /// Bypass composition for fullscreen windows on supported GPUs.
    pub direct_scanout: bool,
    /// Reduce compositor repaint delay for lower input-to-photon latency.
    pub low_latency: bool,
    /// Live frosted-glass blur behind panels (false = fast pre-blurred wallpaper).
    pub live_blur: bool,
    /// Automatically disable live blur and slow down shell polling while on battery.
    pub battery_saver: bool,
}

impl Default for PerformanceSettings {
    fn default() -> Self {
        Self {
            preset: PerformancePreset::Balanced,
            vrr: VrrMode::Off,
            direct_scanout: true,
            low_latency: false,
            live_blur: true,
            battery_saver: true,
        }
    }
}

impl PerformanceSettings {
    pub fn apply_preset(&mut self, preset: PerformancePreset) {
        self.preset = preset;
        match preset {
            PerformancePreset::Balanced => {
                self.vrr = VrrMode::Off;
                self.direct_scanout = true;
                self.low_latency = false;
                self.live_blur = true;
                self.battery_saver = true;
            }
            PerformancePreset::Performance => {
                self.vrr = VrrMode::Fullscreen;
                self.direct_scanout = true;
                self.low_latency = true;
                self.live_blur = true;
                self.battery_saver = false;
            }
            PerformancePreset::PowerSaver => {
                self.vrr = VrrMode::Off;
                self.direct_scanout = true;
                self.low_latency = false;
                self.live_blur = false;
                self.battery_saver = true;
            }
        }
    }
}

/// Game Mode: switched on automatically while a game runs full screen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameModeSettings {
    /// Turn Game Mode on whenever a game is full screen (Control Center switch).
    pub enabled: bool,
    /// Hold back notification banners (critical ones still show).
    pub silence_notifications: bool,
    /// Variable refresh rate on the game's display, even if VRR is off otherwise.
    pub vrr: bool,
}

impl Default for GameModeSettings {
    fn default() -> Self {
        Self { enabled: true, silence_notifications: true, vrr: true }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NightShiftSchedule {
    /// Only the manual switch (Control Center / Settings) turns it on.
    #[default]
    Off,
    /// Every day from `from_minute` to `to_minute` (local time).
    Custom,
}

/// A manual Night Shift switch that holds until `until` (Unix seconds), like
/// macOS's "Turn On Until Tomorrow" / switching it off during the schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NightShiftOverride {
    pub on: bool,
    pub until: i64,
}

/// Warmer display colours at night (macOS Night Shift).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NightShiftSettings {
    pub schedule: NightShiftSchedule,
    /// Schedule start, minutes after local midnight.
    pub from_minute: u32,
    /// Schedule end, minutes after local midnight (may be before `from_minute`: overnight).
    pub to_minute: u32,
    /// 0 = less warm … 1 = more warm.
    pub warmth: f32,
    /// Manual switch, ignored once it expires.
    pub manual: Option<NightShiftOverride>,
}

impl Default for NightShiftSettings {
    fn default() -> Self {
        Self {
            schedule: NightShiftSchedule::Off,
            from_minute: 22 * 60,
            to_minute: 7 * 60,
            warmth: 0.5,
            manual: None,
        }
    }
}

const DAY_MINUTES: u32 = 24 * 60;

impl NightShiftSettings {
    /// Colour temperature (Kelvin) at full strength.
    pub fn kelvin(&self) -> f32 {
        5500.0 - self.warmth.clamp(0.0, 1.0) * 2800.0
    }

    /// Whether the schedule alone wants Night Shift at `minute` (0..1440, local).
    pub fn scheduled(&self, minute: u32) -> bool {
        if self.schedule != NightShiftSchedule::Custom {
            return false;
        }
        let (from, to, m) = (self.from_minute % DAY_MINUTES, self.to_minute % DAY_MINUTES, minute % DAY_MINUTES);
        match from.cmp(&to) {
            std::cmp::Ordering::Equal => false,
            std::cmp::Ordering::Less => (from..to).contains(&m),
            // overnight, e.g. 22:00 → 07:00
            std::cmp::Ordering::Greater => m >= from || m < to,
        }
    }

    /// Whether Night Shift is on at `now` (Unix seconds) / `minute` (local minute of the day).
    pub fn active(&self, now: i64, minute: u32) -> bool {
        match self.manual {
            Some(o) if now < o.until => o.on,
            _ => self.scheduled(minute),
        }
    }

    /// Switch Night Shift on or off by hand. Holds until the schedule would next
    /// change state anyway: on → the next morning (`to_minute`, 07:00 without a
    /// schedule); off during the schedule → its next start.
    pub fn set_manual(&mut self, on: bool, now: i64, minute: u32) {
        if on == self.scheduled(minute) {
            // the schedule already does that: just drop the override
            self.manual = None;
            return;
        }
        let target = if on || self.schedule != NightShiftSchedule::Custom {
            self.to_minute
        } else {
            self.from_minute
        } % DAY_MINUTES;
        let minute = minute % DAY_MINUTES;
        let mut wait = (target + DAY_MINUTES - minute) % DAY_MINUTES;
        if wait == 0 {
            wait = DAY_MINUTES;
        }
        self.manual = Some(NightShiftOverride { on, until: now + wait as i64 * 60 });
    }

    /// "HH:MM" for a minute of the day.
    pub fn clock(minute: u32) -> String {
        let m = minute % DAY_MINUTES;
        format!("{:02}:{:02}", m / 60, m % 60)
    }
}

/// Current local time: (Unix seconds, minute of the day).
pub fn local_now() -> (i64, u32) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let t = now as libc::time_t;
    // SAFETY: localtime_r only writes into the zeroed `tm` we own.
    let minute = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            0
        } else {
            (tm.tm_hour * 60 + tm.tm_min) as u32
        }
    };
    (now, minute)
}

impl NightShiftSettings {
    /// Whether Night Shift is on right now.
    pub fn active_now(&self) -> bool {
        let (now, minute) = local_now();
        self.active(now, minute)
    }

    /// Manual switch at the current local time.
    pub fn set_manual_now(&mut self, on: bool) {
        let (now, minute) = local_now();
        self.set_manual(on, now, minute);
    }
}

/// Per-channel multipliers (sRGB, 0..1) for a black-body colour temperature;
/// 6500 K is neutral. Tanner Helland's fit of the black-body curve, normalised
/// so 6500 K gives white (the same curve redshift/gammastep follow).
pub fn kelvin_to_rgb(kelvin: f32) -> [f32; 3] {
    fn raw(k: f32) -> [f32; 3] {
        let t = k.clamp(1000.0, 40000.0) / 100.0;
        let r = if t <= 66.0 { 255.0 } else { 329.698_73 * (t - 60.0).powf(-0.133_204_76) };
        let g = if t <= 66.0 {
            99.470_8 * t.ln() - 161.119_57
        } else {
            288.122_17 * (t - 60.0).powf(-0.075_514_85)
        };
        let b = if t >= 66.0 {
            255.0
        } else if t <= 19.0 {
            0.0
        } else {
            138.517_73 * (t - 10.0).ln() - 305.044_8
        };
        [r, g, b].map(|c| (c / 255.0).clamp(0.0, 1.0))
    }
    let white = raw(6500.0);
    let c = raw(kelvin);
    [0, 1, 2].map(|i| (c[i] / white[i]).clamp(0.0, 1.0))
}

/// One control in the Control Center grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKind {
    /// Wi-Fi / Bluetooth / Ethernet group.
    Connectivity,
    /// Standalone Wi-Fi quick toggle.
    Wifi,
    /// Standalone Bluetooth quick toggle.
    Bluetooth,
    /// Now-playing media transport.
    NowPlaying,
    /// Do Not Disturb / Focus.
    Focus,
    /// Display colour temperature.
    NightShift,
    /// Automatic game mode.
    GameMode,
    /// Brightness slider.
    Brightness,
    /// Volume slider.
    Volume,
    /// Network details (Wi-Fi / Ethernet).
    Network,
    /// Sound output details.
    Sound,
    /// Per-display brightness details.
    Displays,
}

impl ControlKind {
    pub const ALL: [ControlKind; 12] = [
        ControlKind::Connectivity,
        ControlKind::Wifi,
        ControlKind::Bluetooth,
        ControlKind::NowPlaying,
        ControlKind::Focus,
        ControlKind::NightShift,
        ControlKind::GameMode,
        ControlKind::Brightness,
        ControlKind::Volume,
        ControlKind::Network,
        ControlKind::Sound,
        ControlKind::Displays,
    ];

    /// The size a control gets when it is added from the gallery.
    pub fn default_size(self) -> ControlSize {
        match self {
            ControlKind::Connectivity | ControlKind::NowPlaying => ControlSize::Large,
            ControlKind::Brightness | ControlKind::Volume => ControlSize::Tall,
            _ => ControlSize::Small,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ControlSize {
    /// 1 × 1 grid cells.
    #[default]
    Small,
    /// 2 × 1 grid cells.
    Wide,
    /// 1 × 2 grid cells.
    Tall,
    /// 2 × 2 grid cells.
    Large,
    /// 4 × 1 grid cells.
    Full,
}

impl ControlSize {
    /// `(columns, rows)` occupied in the four-column Control Center grid.
    pub fn span(self) -> (usize, usize) {
        match self {
            ControlSize::Small => (1, 1),
            ControlSize::Wide => (2, 1),
            ControlSize::Tall => (1, 2),
            ControlSize::Large => (2, 2),
            ControlSize::Full => (4, 1),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ControlItem {
    pub kind: ControlKind,
    pub size: ControlSize,
}

impl ControlItem {
    pub const fn new(kind: ControlKind, size: ControlSize) -> Self {
        Self { kind, size }
    }

    pub fn with_default_size(kind: ControlKind) -> Self {
        Self {
            kind,
            size: kind.default_size(),
        }
    }
}

impl Default for ControlItem {
    fn default() -> Self {
        Self {
            kind: ControlKind::Connectivity,
            size: ControlSize::Large,
        }
    }
}

/// Control Center layout: an ordered list of pages, each a list of controls.
/// The UI packs them into a four-column grid, preserving this order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ControlCenterSettings {
    pub pages: Vec<Vec<ControlItem>>,
}

impl Default for ControlCenterSettings {
    fn default() -> Self {
        Self {
            pages: vec![vec![
                ControlItem::new(ControlKind::Connectivity, ControlSize::Large),
                ControlItem::new(ControlKind::NowPlaying, ControlSize::Large),
                ControlItem::new(ControlKind::Focus, ControlSize::Small),
                ControlItem::new(ControlKind::NightShift, ControlSize::Small),
                ControlItem::new(ControlKind::Brightness, ControlSize::Tall),
                ControlItem::new(ControlKind::Volume, ControlSize::Tall),
                ControlItem::new(ControlKind::GameMode, ControlSize::Small),
                ControlItem::new(ControlKind::Displays, ControlSize::Small),
                ControlItem::new(ControlKind::Network, ControlSize::Small),
                ControlItem::new(ControlKind::Sound, ControlSize::Small),
                ControlItem::new(ControlKind::Wifi, ControlSize::Small),
                ControlItem::new(ControlKind::Bluetooth, ControlSize::Small),
            ]],
        }
    }
}

impl ControlCenterSettings {
    /// Every control currently placed on any page.
    pub fn contains(&self, kind: ControlKind) -> bool {
        self.pages.iter().flatten().any(|c| c.kind == kind)
    }

    /// Drop repeated controls, keeping pages (even empty ones) and the user's
    /// order, so a layout never has two identical controls after a bad edit.
    pub fn sanitized(&self) -> Self {
        let mut seen = std::collections::HashSet::new();
        let pages = self
            .pages
            .iter()
            .map(|page| {
                page.iter()
                    .copied()
                    .filter(|c| seen.insert(c.kind))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        Self { pages }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Settings {
    pub appearance: AppearanceSettings,
    pub dock: DockSettings,
    pub menu_bar: MenuBarSettings,
    pub windows: WindowSettings,
    pub tiling: TilingSettings,
    pub search: SearchSettings,
    pub keyboard: KeyboardSettings,
    pub pointer: PointerSettings,
    pub desktop: DesktopSettings,
    pub lock: LockSettings,
    pub displays: DisplaySettings,
    pub apps: AppSettings,
    pub performance: PerformanceSettings,
    pub night_shift: NightShiftSettings,
    pub game_mode: GameModeSettings,
    pub control_center: ControlCenterSettings,
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"))
        .join("orchard")
}

pub fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

impl Settings {
    /// Load the settings file; missing or broken files give the defaults.
    ///
    /// Loading is lenient: a field with a wrong type or an unknown value (say, from a
    /// newer Orchard) falls back to its default on its own, every other field is kept.
    /// If anything had to be dropped, the original file is copied to
    /// `settings.json.bad` so a later `save()` never destroys the user's data silently.
    pub fn load() -> Settings {
        let path = settings_path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Settings::default();
        };
        let (mut settings, problems) = Settings::from_json_lenient(&text);
        migrate_legacy(&text, &mut settings);
        if !problems.is_empty() {
            // Callers poll the file (the Settings app every couple of seconds,
            // the compositor once a second). Report and back up each broken
            // version only once instead of rewriting settings.json.bad and
            // spamming the log forever.
            let stamp = std::fs::metadata(&path).ok().and_then(|m| Some((m.modified().ok()?, m.len())));
            static LAST: std::sync::Mutex<Option<(PathBuf, std::time::SystemTime, u64)>> = std::sync::Mutex::new(None);
            let key = stamp.map(|(mtime, len)| (path.clone(), mtime, len));
            let already_reported = match (LAST.lock(), key) {
                (Ok(mut last), Some(key)) => {
                    let seen = last.as_ref() == Some(&key);
                    if !seen {
                        *last = Some(key);
                    }
                    seen
                }
                _ => false,
            };
            if !already_reported {
                for p in &problems {
                    eprintln!("orchard: {}: {p}; using the default there", path.display());
                }
                let _ = std::fs::copy(&path, path.with_extension("json.bad"));
            }
        }
        settings
    }

    /// Parse settings, keeping every valid field. Returns the settings plus a
    /// description of each part that was replaced by its default.
    pub fn from_json_lenient(text: &str) -> (Settings, Vec<String>) {
        let user: serde_json::Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => return (Settings::default(), vec![format!("not valid JSON ({e})")]),
        };
        // fast path: the whole file is fine
        if let Ok(s) = serde_json::from_value::<Settings>(user.clone()) {
            return (s, Vec::new());
        }
        let mut merged = serde_json::to_value(Settings::default()).expect("defaults serialize");
        let mut problems = Vec::new();
        overlay(&mut merged, &user, "", &mut problems);
        let settings = serde_json::from_value(merged).unwrap_or_else(|e| {
            problems.push(format!("unexpected: {e}"));
            Settings::default()
        });
        (settings, problems)
    }

    /// Atomically write the settings file.
    ///
    /// The temporary name is unique per process and save, so the Settings app
    /// and the shell (or a Control Center toggle) saving at the same time can
    /// never rename each other's half-written file. Durability is best effort:
    /// sync the data before the rename, and the directory afterwards.
    pub fn save(&self) -> std::io::Result<()> {
        use std::io::Write;
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = settings_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = path.with_extension(format!("json.{}.{seq}.tmp", std::process::id()));
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        let write = || -> std::io::Result<()> {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&json)?;
            f.sync_all()?;
            Ok(())
        };
        if let Err(e) = write().and_then(|_| std::fs::rename(&tmp, &path)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        if let Some(dir) = path.parent() {
            if let Ok(d) = std::fs::File::open(dir) {
                let _ = d.sync_all();
            }
        }
        Ok(())
    }
}

/// Three-way merge for a settings editor that saves a whole `Settings`:
/// apply only the fields that changed from `base` (what the editor loaded or
/// last saved) to `mine` (what it holds now) onto `disk` (the file as it is
/// now). Changes others made meanwhile (the Control Center, the menu bar, a
/// text editor) survive instead of being reverted by a stale copy.
pub fn merge_settings(base: &Settings, mine: &Settings, disk: &Settings) -> Settings {
    use serde_json::Value;
    fn walk(base: &Value, mine: &Value, out: &mut Value) {
        match (base, mine, &mut *out) {
            (Value::Object(b), Value::Object(m), Value::Object(o)) => {
                for (k, mv) in m {
                    match (b.get(k), o.get_mut(k)) {
                        (Some(bv), _) if bv == mv => {}
                        (Some(bv), Some(ov)) if bv.is_object() && mv.is_object() && ov.is_object() => walk(bv, mv, ov),
                        _ => {
                            o.insert(k.clone(), mv.clone());
                        }
                    }
                }
            }
            _ => *out = mine.clone(),
        }
    }
    let (Ok(b), Ok(m), Ok(mut d)) = (serde_json::to_value(base), serde_json::to_value(mine), serde_json::to_value(disk))
    else {
        return mine.clone();
    };
    walk(&b, &m, &mut d);
    serde_json::from_value(d).unwrap_or_else(|_| mine.clone())
}

/// Copy `user` onto `root` (which starts as the serialized defaults) one leaf at a time,
/// keeping each change only while the whole thing still deserializes as `Settings`.
fn overlay(root: &mut serde_json::Value, user: &serde_json::Value, ptr: &str, problems: &mut Vec<String>) {
    use serde_json::Value;
    let Value::Object(fields) = user else {
        problems.push(format!("{} must be an object", if ptr.is_empty() { "the file" } else { ptr }));
        return;
    };
    for (key, value) in fields {
        let child = format!("{ptr}/{}", key.replace('~', "~0").replace('/', "~1"));
        let existing = root.pointer(&child).cloned();
        match (&existing, value) {
            // nested section that exists in the defaults: merge field by field
            (Some(Value::Object(_)), Value::Object(_)) => overlay(root, value, &child, problems),
            _ => {
                let Some(Value::Object(parent)) = root.pointer_mut(ptr) else { continue };
                let previous = parent.insert(key.clone(), value.clone());
                if serde_json::from_value::<Settings>(root.clone()).is_err() {
                    let Some(Value::Object(parent)) = root.pointer_mut(ptr) else { continue };
                    match previous {
                        Some(prev) => parent.insert(key.clone(), prev),
                        None => parent.remove(key),
                    };
                    problems.push(format!("invalid value at {child}: {value}"));
                }
            }
        }
    }
}

/// Carry settings whose meaning changed across versions forward.
fn migrate_legacy(text: &str, settings: &mut Settings) {
    // Older versions had a separate `dock.magnification` switch; the size
    // slider is the switch now (equal sizes = off). Preserve a user who had
    // magnification turned off instead of turning it back on.
    if let Ok(raw) = serde_json::from_str::<serde_json::Value>(text) {
        if raw.pointer("/dock/magnification").and_then(|v| v.as_bool()) == Some(false) {
            settings.dock.magnified_size = settings.dock.icon_size;
        }
    }
}

/// Path of the compositor's IPC socket.
pub fn socket_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ORCHARD_SOCKET") {
        return Some(PathBuf::from(p));
    }
    let dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".into());
    Some(PathBuf::from(dir).join(format!("orchard-{display}.sock")))
}

/// Send one IPC request and return the first reply line.
pub fn ipc_request(request: &serde_json::Value) -> std::io::Result<serde_json::Value> {
    use std::io::{BufRead, BufReader, Write};
    let path = socket_path().ok_or_else(|| std::io::Error::other("no XDG_RUNTIME_DIR"))?;
    let mut stream = std::os::unix::net::UnixStream::connect(path)?;
    stream.set_read_timeout(Some(std::time::Duration::from_millis(1500)))?;
    let mut line = serde_json::to_vec(request).map_err(std::io::Error::other)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    serde_json::from_str(&reply).map_err(std::io::Error::other)
}

pub mod desktop;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip() {
        let json = serde_json::to_string(&Settings::default()).unwrap();
        let (s, problems) = Settings::from_json_lenient(&json);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn empty_and_partial_files_use_defaults() {
        let (s, p) = Settings::from_json_lenient("{}");
        assert!(p.is_empty());
        assert_eq!(s, Settings::default());
        let (s, p) = Settings::from_json_lenient(r#"{"appearance":{}}"#);
        assert!(p.is_empty());
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let (s, p) = Settings::from_json_lenient(r#"{"from_the_future":1,"dock":{"also_new":true}}"#);
        assert!(p.is_empty(), "{p:?}");
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn legacy_magnification_off_stays_off() {
        let json = r#"{"dock":{"icon_size":60.0,"magnified_size":90.0,"magnification":false}}"#;
        let (mut s, p) = Settings::from_json_lenient(json);
        assert!(p.is_empty(), "{p:?}");
        migrate_legacy(json, &mut s);
        assert_eq!(s.dock.magnified_size, 60.0);
        // and an explicit size from the new slider is left alone
        let json = r#"{"dock":{"icon_size":60.0,"magnified_size":90.0}}"#;
        let (mut s, _) = Settings::from_json_lenient(json);
        migrate_legacy(json, &mut s);
        assert_eq!(s.dock.magnified_size, 90.0);
    }

    #[test]
    fn one_bad_field_keeps_the_rest() {
        let mut want = Settings::default();
        want.appearance.accent = Accent::Green;
        want.appearance.mode = ThemeMode::Dark;
        let mut v = serde_json::to_value(&want).unwrap();
        // an accent this version doesn't know, plus a type error elsewhere
        v["appearance"]["accent"] = "ultraviolet".into();
        v["dock"] = serde_json::json!({ "icon_size": "huge" });
        let (s, p) = Settings::from_json_lenient(&v.to_string());
        assert_eq!(p.len(), 2, "{p:?}");
        assert_eq!(s.appearance.mode, ThemeMode::Dark, "valid sibling kept");
        assert_eq!(s.appearance.accent, Accent::default(), "bad value reset");
        assert_eq!(s.dock, DockSettings::default());
    }

    #[test]
    fn garbage_gives_defaults() {
        for text in ["", "not json", "[1,2]", "null", r#"{"appearance": 5}"#] {
            let (s, p) = Settings::from_json_lenient(text);
            assert!(!p.is_empty(), "{text:?} should report a problem");
            assert_eq!(s, Settings::default(), "{text:?}");
        }
    }

    #[test]
    fn wallpapers_list_round_trips() {
        let mut s = Settings::default();
        s.desktop.wallpaper = Some("/home/user/Pictures/a.jpg".into());
        s.desktop.wallpapers = vec![
            "/home/user/Pictures/a.jpg".into(),
            "/home/user/Downloads/b.webp".into(),
        ];
        let json = serde_json::to_string(&s).unwrap();
        let (loaded, problems) = Settings::from_json_lenient(&json);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(loaded.desktop.wallpaper, s.desktop.wallpaper);
        assert_eq!(loaded.desktop.wallpapers, s.desktop.wallpapers);
    }

    #[test]
    fn dock_autohide_round_trips_and_migrates_bools() {
        for mode in [DockAutohide::Never, DockAutohide::AvoidWindows, DockAutohide::Always] {
            let mut s = Settings::default();
            s.dock.autohide = mode;
            let json = serde_json::to_string(&s).unwrap();
            let (loaded, problems) = Settings::from_json_lenient(&json);
            assert!(problems.is_empty(), "{problems:?}");
            assert_eq!(loaded.dock.autohide, mode);
        }
        let (s_true, p1) = Settings::from_json_lenient(r#"{"dock":{"autohide":true}}"#);
        assert!(p1.is_empty(), "{p1:?}");
        assert_eq!(s_true.dock.autohide, DockAutohide::Always);
        let (s_false, p2) = Settings::from_json_lenient(r#"{"dock":{"autohide":false}}"#);
        assert!(p2.is_empty(), "{p2:?}");
        assert_eq!(s_false.dock.autohide, DockAutohide::Never);
    }

    #[test]
    fn performance_settings_and_presets_round_trip() {
        let mut s = Settings::default();
        s.performance.apply_preset(PerformancePreset::Performance);
        assert_eq!(s.performance.vrr, VrrMode::Fullscreen);
        assert!(s.performance.low_latency);
        assert!(!s.performance.battery_saver);

        let json = serde_json::to_string(&s).unwrap();
        let (loaded, problems) = Settings::from_json_lenient(&json);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(loaded.performance, s.performance);

        s.performance.apply_preset(PerformancePreset::PowerSaver);
        assert!(!s.performance.live_blur);
        assert!(s.performance.battery_saver);
    }

    #[test]
    fn wallpaper_accent_round_trips_and_resolves() {
        let mut s = Settings::default();
        assert_eq!(s.appearance.accent_rgb(), Accent::Blue.rgb());

        s.appearance.accent = Accent::Wallpaper;
        assert_eq!(s.appearance.accent_rgb(), Accent::Wallpaper.rgb());

        s.appearance.wallpaper_accent = Some([0xE8, 0x5A, 0x3C]);
        assert_eq!(s.appearance.accent_rgb(), [0xE8, 0x5A, 0x3C]);

        let json = serde_json::to_string(&s).unwrap();
        let (loaded, problems) = Settings::from_json_lenient(&json);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(loaded.appearance.accent, Accent::Wallpaper);
        assert_eq!(loaded.appearance.wallpaper_accent, Some([0xE8, 0x5A, 0x3C]));
        assert_eq!(loaded.appearance.accent_rgb(), [0xE8, 0x5A, 0x3C]);
    }

    #[test]
    fn removed_keys_still_load() {
        let json = r#"{
            "appearance": {"blur_style": "liquid_glass", "glass_transparency": 0.7},
            "menu_bar": {"show_tiling": false},
            "windows": {"tiling_mode": "tiling"},
            "tiling": {"smart_gaps": true, "split_ratio": 0.6, "split_direction": "vertical", "new_window": "last"},
            "stage_manager": {"enabled": true}
        }"#;
        let (_, problems) = Settings::from_json_lenient(json);
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn merge_keeps_changes_made_elsewhere() {
        let base = Settings::default();
        let mut mine = base.clone();
        mine.dock.icon_size = 64.0;
        let mut disk = base.clone();
        disk.night_shift.warmth = 0.9;
        disk.windows.focus_follows_mouse = true;
        let merged = merge_settings(&base, &mine, &disk);
        assert_eq!(merged.dock.icon_size, 64.0, "my edit applied");
        assert_eq!(merged.night_shift.warmth, 0.9, "other writer kept");
        assert!(merged.windows.focus_follows_mouse);
        // both changed the same field: the editor (latest user action) wins
        disk.dock.icon_size = 40.0;
        assert_eq!(merge_settings(&base, &mine, &disk).dock.icon_size, 64.0);
        // map entries merge per key
        let mut mine = base.clone();
        mine.displays.scale.insert("DP-1".into(), 1.5);
        let mut disk = base.clone();
        disk.displays.scale.insert("HDMI-A-1".into(), 2.0);
        let merged = merge_settings(&base, &mine, &disk);
        assert_eq!(merged.displays.scale.len(), 2);
    }

    #[test]
    fn night_shift_schedule_wraps_midnight() {
        let mut n = NightShiftSettings::default();
        assert!(!n.scheduled(23 * 60), "schedule off by default");
        n.schedule = NightShiftSchedule::Custom;
        assert!(n.scheduled(22 * 60));
        assert!(n.scheduled(23 * 60 + 59));
        assert!(n.scheduled(0));
        assert!(n.scheduled(6 * 60 + 59));
        assert!(!n.scheduled(7 * 60));
        assert!(!n.scheduled(12 * 60));
        // daytime range
        n.from_minute = 9 * 60;
        n.to_minute = 17 * 60;
        assert!(n.scheduled(12 * 60));
        assert!(!n.scheduled(20 * 60));
        n.to_minute = n.from_minute;
        assert!(!n.scheduled(9 * 60), "empty range never runs");
    }

    #[test]
    fn night_shift_manual_override_expires_at_the_next_boundary() {
        let mut n = NightShiftSettings { schedule: NightShiftSchedule::Custom, ..Default::default() };
        let noon = 12 * 60;
        let t0 = 1_000_000;
        // on at noon: holds until 07:00 tomorrow (19 h)
        n.set_manual(true, t0, noon);
        assert!(n.active(t0, noon));
        assert_eq!(n.manual.unwrap().until, t0 + 19 * 3600);
        assert!(!n.active(t0 + 19 * 3600, 7 * 60), "back to the schedule in the morning");

        // off at 23:00 inside the schedule: off until 22:00 the next day
        let late = 23 * 60;
        n.set_manual(false, t0, late);
        assert!(!n.active(t0, late));
        assert_eq!(n.manual.unwrap().until, t0 + 23 * 3600);

        // asking for what the schedule does anyway clears the override
        n.set_manual(true, t0, late);
        assert_eq!(n.manual, None);
        assert!(n.active(t0, late));

        // no schedule: "on until tomorrow" morning
        let mut n = NightShiftSettings::default();
        n.set_manual(true, t0, 20 * 60);
        assert_eq!(n.manual.unwrap().until, t0 + 11 * 3600);
        n.set_manual(false, t0, 20 * 60);
        assert_eq!(n.manual, None);
        assert!(!n.active(t0, 20 * 60));
    }

    #[test]
    fn control_center_layout_round_trips_and_sanitizes() {
        let mut s = Settings::default();
        s.control_center.pages = vec![
            vec![
                ControlItem::new(ControlKind::Connectivity, ControlSize::Large),
                ControlItem::new(ControlKind::Focus, ControlSize::Small),
                // duplicates are dropped
                ControlItem::new(ControlKind::Focus, ControlSize::Wide),
            ],
            Vec::new(),
        ];
        let round = serde_json::from_str::<Settings>(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(round.control_center, s.control_center);
        let clean = s.control_center.sanitized();
        assert_eq!(clean.pages.len(), 2, "an empty page is still a page");
        assert_eq!(clean.pages[0].len(), 2);
        assert!(clean.pages[0].iter().all(|c| c.kind != ControlKind::Focus || c.size == ControlSize::Small));
        // Older files without the field get the iOS-style default.
        let parsed: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed.control_center.pages.len(), 1);
        assert!(parsed.control_center.contains(ControlKind::Connectivity));
        assert!(parsed.control_center.contains(ControlKind::Volume));
    }

    #[test]
    fn control_sizes_span_the_grid() {
        assert_eq!(ControlSize::Small.span(), (1, 1));
        assert_eq!(ControlSize::Wide.span(), (2, 1));
        assert_eq!(ControlSize::Tall.span(), (1, 2));
        assert_eq!(ControlSize::Large.span(), (2, 2));
        assert_eq!(ControlSize::Full.span(), (4, 1));
    }

    #[test]
    fn kelvin_curve_is_neutral_at_6500_and_warm_below() {
        let w = kelvin_to_rgb(6500.0);
        assert!(w.iter().all(|c| (c - 1.0).abs() < 1e-4), "{w:?}");
        let [r, g, b] = kelvin_to_rgb(3400.0);
        assert!((r - 1.0).abs() < 1e-4);
        assert!(g < 0.85 && g > 0.65, "{g}");
        assert!(b < g && b > 0.35, "{b}");
        let n = NightShiftSettings::default();
        assert!((2700.0..=5500.0).contains(&n.kelvin()));
    }

    #[test]
    fn terminal_exec_formats_by_emulator() {
        assert_eq!(terminal_exec("kitty", "less +G /tmp/log"), "kitty -e less +G /tmp/log");
        assert_eq!(terminal_exec("ghostty", "htop"), "ghostty -e htop");
        assert_eq!(terminal_exec("gnome-terminal", "htop"), "gnome-terminal -- htop");
        assert_eq!(terminal_exec("ptyxis", "htop"), "ptyxis -- htop");
        assert_eq!(terminal_exec("wezterm", "htop"), "wezterm start -- htop");
    }
}
