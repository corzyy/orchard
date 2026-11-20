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
    /// Look of the glass behind the menu bar, Dock, menus and popups.
    pub blur_style: BlurStyle,
    /// Liquid Glass transparency, 0.0 (frosted) ..= 1.0 (clear).
    pub glass_transparency: f32,
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
    /// [`Self::glass_transparency`], clamped to 0..=1.
    pub fn glass_transparency(&self) -> f32 {
        if self.glass_transparency.is_finite() { self.glass_transparency.clamp(0.0, 1.0) } else { 0.5 }
    }
}

/// How shell panels render the desktop behind them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BlurStyle {
    /// Dual Kawase frosted glass.
    #[default]
    Kawase,
    /// Lighter blur with refracting edges and a specular rim (macOS Tahoe).
    LiquidGlass,
}

impl BlurStyle {
    pub fn name(self) -> &'static str {
        match self {
            BlurStyle::Kawase => "Kawase",
            BlurStyle::LiquidGlass => "Liquid Glass",
        }
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
            blur_style: BlurStyle::Kawase,
            glass_transparency: 0.5,
            corner_radius: 10.0,
            icon_theme: None,
            cursor_theme: None,
            cursor_size: 24,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MinimizeEffect {
    #[default]
    Genie,
    Scale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TitlebarDoubleClick {
    #[default]
    Zoom,
    Minimize,
    Nothing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TilingMode {
    #[default]
    Floating,
    Tiling,
}

/// Where a new window goes in Tiling mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NewWindowPlacement {
    /// Splits the focused window.
    #[default]
    Focused,
    /// Goes to the end of the layout (the tail of the spiral).
    Last,
}

/// Which way Tiling mode splits a pane for a new window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    /// Along the longer side of the pane (dwindle).
    #[default]
    Auto,
    /// Side by side.
    Horizontal,
    /// Stacked.
    Vertical,
}

/// Floating-mode snapping and Tiling-mode layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TilingSettings {
    // ---- shared
    /// Margin between tiles and the screen edges, logical px.
    pub outer_gap: i32,
    /// Space between two tiles, logical px.
    pub inner_gap: i32,
    // ---- floating
    /// Dragging a window against a screen edge tiles it.
    pub snap_to_edges: bool,
    /// Show the translucent drop preview while dragging.
    pub snap_preview: bool,
    /// Corners give quarter tiles.
    pub snap_corners: bool,
    /// How close to the edge (px) the pointer must get to snap.
    pub edge_zone: i32,
    // ---- tiling
    /// No gaps when a display has a single window.
    pub smart_gaps: bool,
    /// Share of a pane the existing window keeps when it splits, 0.3..=0.7.
    pub split_ratio: f32,
    pub split_direction: SplitDirection,
    pub new_window: NewWindowPlacement,
}

impl TilingSettings {
    pub const GAP_MAX: i32 = 48;

    /// The values the compositor works with: everything within its range.
    pub fn clamped(&self) -> Self {
        let mut t = self.clone();
        t.outer_gap = t.outer_gap.clamp(0, Self::GAP_MAX);
        t.inner_gap = t.inner_gap.clamp(0, Self::GAP_MAX);
        t.edge_zone = t.edge_zone.clamp(8, 120);
        t.split_ratio = if t.split_ratio.is_finite() { t.split_ratio.clamp(0.3, 0.7) } else { 0.5 };
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
            smart_gaps: false,
            split_ratio: 0.5,
            split_direction: SplitDirection::Auto,
            new_window: NewWindowPlacement::Focused,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DockSettings {
    /// Tile size in logical px.
    pub icon_size: f32,
    pub magnification: bool,
    /// Size of the tile under the pointer when magnification is on.
    pub magnified_size: f32,
    /// When the Dock hides itself.
    pub autohide: DockAutohide,
    pub show_indicators: bool,
    pub animate_launch: bool,
    pub minimize_effect: MinimizeEffect,
    /// Screen edge the Dock sits on.
    pub position: DockPosition,
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
            magnification: true,
            magnified_size: 84.0,
            autohide: DockAutohide::Never,
            show_indicators: true,
            animate_launch: true,
            minimize_effect: MinimizeEffect::Genie,
            position: DockPosition::Bottom,
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
    pub show_tiling: bool,
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
            show_tiling: true,
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
    /// Window layout mode: Floating (macOS-style) or Tiling (dwindle).
    pub tiling_mode: TilingMode,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            double_click_titlebar: TitlebarDoubleClick::Zoom,
            focus_follows_mouse: false,
            show_desktop_icons: true,
            tiling_mode: TilingMode::Floating,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Settings {
    pub appearance: AppearanceSettings,
    pub dock: DockSettings,
    pub menu_bar: MenuBarSettings,
    pub windows: WindowSettings,
    pub tiling: TilingSettings,
    pub keyboard: KeyboardSettings,
    pub pointer: PointerSettings,
    pub desktop: DesktopSettings,
    pub lock: LockSettings,
    pub displays: DisplaySettings,
    pub apps: AppSettings,
    pub performance: PerformanceSettings,
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
        let (settings, problems) = Settings::from_json_lenient(&text);
        if !problems.is_empty() {
            for p in &problems {
                eprintln!("orchard: {}: {p}; using the default there", path.display());
            }
            let _ = std::fs::copy(&path, path.with_extension("json.bad"));
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
    pub fn save(&self) -> std::io::Result<()> {
        let path = settings_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(tmp, path)
    }
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
    fn one_bad_field_keeps_the_rest() {
        let mut want = Settings::default();
        want.appearance.accent = Accent::Green;
        want.appearance.mode = ThemeMode::Dark;
        let mut v = serde_json::to_value(&want).unwrap();
        // an accent this version doesn't know, plus a type error elsewhere
        v["appearance"]["accent"] = "ultraviolet".into();
        v["dock"] = serde_json::json!({ "magnification": "very" });
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
    fn terminal_exec_formats_by_emulator() {
        assert_eq!(terminal_exec("kitty", "less +G /tmp/log"), "kitty -e less +G /tmp/log");
        assert_eq!(terminal_exec("ghostty", "htop"), "ghostty -e htop");
        assert_eq!(terminal_exec("gnome-terminal", "htop"), "gnome-terminal -- htop");
        assert_eq!(terminal_exec("ptyxis", "htop"), "ptyxis -- htop");
        assert_eq!(terminal_exec("wezterm", "htop"), "wezterm start -- htop");
    }
}
