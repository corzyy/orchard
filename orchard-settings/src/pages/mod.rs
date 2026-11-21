//! Settings pages registry, CLI argument mapping (macOS System Settings
//! layout), and the shared plumbing pages build their views with.

pub mod appearance;
pub mod art;
pub mod connectivity;
pub mod desktop;
pub mod input;
pub mod system;
pub mod tiling;
pub mod updates;

use std::sync::Arc;

use iced::widget::text_input;
use iced::{Color, Element};
use orchard_config::Settings;
use orchard_ui::{form, icons, palette::from_rgb};

use crate::{sys::Wake, Message};

/// Pages build owned (`'static`) elements: their state lives behind a
/// `RefCell` that is only borrowed while the view is built.
pub type El = Element<'static, Message>;

/// A deferred mutation run by `update`: edits the settings and/or page state.
#[derive(Clone)]
pub struct Action(pub Arc<dyn Fn(&mut Settings, &mut PageState, &Wake) + Send + Sync>);

impl std::fmt::Debug for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Action")
    }
}

/// Message that edits the settings file.
pub fn edit(f: impl Fn(&mut Settings) + Send + Sync + 'static) -> Message {
    Message::Do(Action(Arc::new(move |s, _, _| f(s))))
}

/// Message with full access to settings, page state and the wake handle.
pub fn act(f: impl Fn(&mut Settings, &mut PageState, &Wake) + Send + Sync + 'static) -> Message {
    Message::Do(Action(Arc::new(f)))
}

/// A white system icon in a coloured squircle.
pub fn tile(icon: &str, color: Color, size: f32) -> El {
    icons::system_tile(icon, color, size)
}

/// Placeholder group while data loads.
pub fn loading(label: &'static str) -> El {
    form::group([form::value_row(label, "Loading…")])
}

pub fn some(s: impl Into<String>) -> Option<String> {
    Some(s.into())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Page {
    // Connectivity
    Wifi,
    Bluetooth,
    Network,
    // Audio
    Sound,
    // General & subpages
    General,
    About,
    SoftwareUpdate,
    DateTime,
    LoginItems,
    DefaultApps,
    // Appearance & Desktop
    Accessibility,
    Appearance,
    MenuBar,
    DesktopDock,
    Tiling,
    Displays,
    Wallpaper,
    // Security & Power
    LockScreen,
    Battery,
    // Input Devices
    Keyboard,
    Mouse,
    Trackpad,
}

use Page::*;

/// Top-level sidebar groups. macOS uses broad, labelled groups rather than
/// one long list; keeping them small makes the app feel calmer.
pub const SIDEBAR: &[(&str, &[Page])] = &[
    ("Network", &[Wifi, Bluetooth, Network]),
    ("Devices", &[Sound, Displays, Keyboard, Mouse, Trackpad]),
    ("System", &[General, Accessibility, LockScreen, Battery]),
    ("Appearance & Desktop", &[Appearance, Wallpaper, MenuBar, DesktopDock, Tiling]),
];

/// Every searchable page (including subpages under General).
pub const ALL_PAGES: &[Page] = &[
    Wifi,
    Bluetooth,
    Network,
    Sound,
    General,
    About,
    SoftwareUpdate,
    DateTime,
    LoginItems,
    DefaultApps,
    Accessibility,
    Appearance,
    MenuBar,
    DesktopDock,
    Tiling,
    Displays,
    Wallpaper,
    LockScreen,
    Battery,
    Keyboard,
    Mouse,
    Trackpad,
];

impl Page {
    /// Sidebar item that should appear selected when `self` is open.
    pub fn sidebar_item(self) -> Page {
        match self {
            About | SoftwareUpdate | DateTime | LoginItems | DefaultApps => General,
            other => other,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Wifi => "Wi‑Fi",
            Bluetooth => "Bluetooth",
            Network => "Network",
            Sound => "Sound",
            General => "General",
            About => "About",
            SoftwareUpdate => "Software Update",
            DateTime => "Date & Time",
            LoginItems => "Login Items",
            DefaultApps => "Default Apps",
            Accessibility => "Accessibility",
            Appearance => "Appearance",
            MenuBar => "Menu Bar",
            DesktopDock => "Desktop & Dock",
            Tiling => "Windows",
            Displays => "Displays",
            Wallpaper => "Wallpaper",
            LockScreen => "Lock Screen",
            Battery => "Battery",
            Keyboard => "Keyboard",
            Mouse => "Mouse",
            Trackpad => "Trackpad",
        }
    }

    pub fn icon(self) -> (&'static str, Color) {
        let rgb = from_rgb;
        let blue = rgb([0x0A, 0x84, 0xFF]);
        let gray = rgb([0x8E, 0x8E, 0x93]);
        let dark = rgb([0x3A, 0x3A, 0x3E]);
        match self {
            Wifi => ("network-wireless", blue),
            Bluetooth => ("bluetooth", blue),
            Network => ("network-workgroup", blue),
            Sound => ("audio-volume-high", rgb([0xFF, 0x37, 0x5F])),
            General => ("emblem-system", gray),
            About => ("computer", gray),
            SoftwareUpdate => ("software-update-available", gray),
            DateTime => ("preferences-system-time", blue),
            LoginItems => ("system-run", gray),
            DefaultApps => ("view-app-grid", rgb([0xAF, 0x52, 0xDE])),
            Accessibility => ("preferences-desktop-accessibility", blue),
            Appearance => ("preferences-desktop-appearance", dark),
            MenuBar => ("open-menu", gray),
            DesktopDock => ("dock", dark),
            Tiling => ("windows", rgb([0x5E, 0x5C, 0xE6])),
            Displays => ("video-display", blue),
            Wallpaper => ("preferences-desktop-wallpaper", rgb([0x32, 0xAD, 0xE6])),
            LockScreen => ("system-lock-screen", dark),
            Battery => ("battery-full", rgb([0x30, 0xD1, 0x58])),
            Keyboard => ("input-keyboard", gray),
            Mouse => ("input-mouse", gray),
            Trackpad => ("input-touchpad", gray),
        }
    }

    /// Blurb shown in the header card for hub panes (e.g. General).
    pub fn description(self) -> &'static str {
        match self {
            General => "Manage your overall setup and preferences for your computer, such as hardware info, software updates, date and time, login items and default apps.",
            _ => "",
        }
    }

    pub fn keywords(self) -> &'static str {
        match self {
            Wifi => "wifi wireless wlan internet ssid",
            Bluetooth => "bluetooth devices headphones pair",
            Network => "network ethernet vpn wireguard wired lan ip",
            Sound => "sound volume audio output input microphone speakers mute",
            General => "general system about software update date time login items default apps",
            About => "about hardware cpu processor graphics memory os kernel hostname computer",
            SoftwareUpdate => "software update updates upgrade dnf flatpak packages install",
            DateTime => "date time timezone ntp clock",
            LoginItems => "login items autostart startup",
            DefaultApps => "default apps browser terminal file manager text editor",
            Accessibility => "accessibility reduce motion transparency pointer cursor size theme shake locate find mouse",
            Appearance => "appearance dark light mode accent color colour theme corner radius icon",
            MenuBar => "menu bar control center clock battery percentage date seconds",
            DesktopDock => "desktop dock magnification minimize hot corners mission control windows title bar focus",
            Tiling => "windows tiling tile floating snap snapping layout gaps margin edge corner preview",
            Displays => "displays screen monitor resolution scale refresh hidpi vrr freesync gsync adaptive sync night shift blue light warm colour color temperature redshift",
            Wallpaper => "wallpaper background desktop picture",
            LockScreen => "lock screen idle password locker shortcut",
            Battery => "battery power energy profile performance vrr freesync gsync adaptive sync latency scanout blur gaming game mode gaming games fullscreen",
            Keyboard => "keyboard layout repeat modifier caps numlock",
            Mouse => "mouse pointer speed scroll natural acceleration primary button",
            Trackpad => "trackpad touchpad pointer speed tap click natural scrolling gestures swipe",
        }
    }

    /// Parse a CLI argument (supports macOS pane names and legacy Orchard names).
    pub fn from_arg(arg: &str) -> Option<Page> {
        let k: String = arg
            .to_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        match k.as_str() {
            "wifi" | "wifinetwork" | "wlan" => Some(Wifi),
            "bluetooth" | "bt" => Some(Bluetooth),
            "network" | "ethernet" | "vpn" => Some(Network),
            "sound" | "audio" | "volume" => Some(Sound),
            "general" | "system" | "users" | "usersgroups" | "language" | "languageregion" => Some(General),
            "about" | "storage" => Some(About),
            "softwareupdate" | "update" | "updates" | "upgrade" => Some(SoftwareUpdate),
            "datetime" | "time" | "timezone" => Some(DateTime),
            "loginitems" | "autostart" | "startup" => Some(LoginItems),
            "defaultapps" | "apps" => Some(DefaultApps),
            "accessibility" | "pointer" | "cursor" => Some(Accessibility),
            "appearance" | "theme" => Some(Appearance),
            "menubar" | "controlcenter" | "controlcentre" | "panel" | "clock" => Some(MenuBar),
            "desktopdock" | "dock" | "desktop" => Some(DesktopDock),
            "windows" | "tiling" | "windowstiling" | "snap" | "snapping" | "floating" | "layout" => Some(Tiling),
            "displays" | "display" | "monitor" | "screen" => Some(Displays),
            "wallpaper" | "background" => Some(Wallpaper),
            "lockscreen" | "lock" | "lockpower" => Some(LockScreen),
            "battery" | "power" | "energy" | "performance" => Some(Battery),
            "keyboard" | "keys" => Some(Keyboard),
            "mouse" | "mousetrackpad" => Some(Mouse),
            "trackpad" | "touchpad" => Some(Trackpad),
            _ => None,
        }
    }
}

#[derive(Default)]
pub struct PageState {
    pub appearance: appearance::AppearanceState,
    pub desktop: desktop::DesktopState,
    pub input: input::InputState,
    pub conn: connectivity::ConnState,
    pub system: system::SystemState,
    pub updates: updates::UpdatesState,
    /// Text field an action wants focused (taken by `update`).
    pub focus: Option<text_input::Id>,
}

/// Builds the content of `page`. May start background loads.
pub fn show(page: Page, s: &Settings, st: &mut PageState, wake: &Wake) -> El {
    match page {
        Wifi => connectivity::wifi(&mut st.conn, wake),
        Bluetooth => connectivity::bluetooth(&mut st.conn, wake),
        Network => connectivity::network(&mut st.conn, wake),
        Sound => connectivity::sound(&mut st.conn, wake),
        General => system::general(),
        About => system::about(&mut st.system, wake),
        SoftwareUpdate => updates::software_update(&mut st.updates, wake),
        DateTime => system::date_time(&mut st.system, wake),
        LoginItems => system::login_items(s, &mut st.system, wake),
        DefaultApps => system::default_apps(s, &mut st.system, wake),
        Accessibility => appearance::accessibility(s, &mut st.appearance, wake),
        Appearance => appearance::appearance(s, &mut st.appearance, wake),
        MenuBar => desktop::menu_bar(s),
        DesktopDock => desktop::desktop_dock(s),
        Tiling => tiling::tiling(s),
        Displays => desktop::displays(s, &mut st.desktop, wake),
        Wallpaper => appearance::wallpaper(s, &mut st.appearance, wake),
        LockScreen => system::lock_screen(s, &st.system),
        Battery => system::battery(s, &mut st.system, wake),
        Keyboard => input::keyboard(s, &mut st.input, wake),
        Mouse => input::mouse(s),
        Trackpad => input::trackpad(s),
    }
}

/// Folds finished background work into the settings (runs in `update`, so it
/// may change what gets saved).
pub fn poll(page: Page, s: &mut Settings, st: &mut PageState, wake: &Wake) {
    appearance::poll(s, &mut st.appearance, wake);
    if page == LockScreen && s.lock.locker.trim() == orchard_config::LEGACY_LOCKER {
        s.lock.locker.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_args_map_macos_and_legacy_pages() {
        assert_eq!(Page::from_arg("Wi‑Fi"), Some(Page::Wifi));
        assert_eq!(Page::from_arg("wifi"), Some(Page::Wifi));
        assert_eq!(Page::from_arg("Network"), Some(Page::Network));
        assert_eq!(Page::from_arg("General"), Some(Page::General));
        assert_eq!(Page::from_arg("About"), Some(Page::About));
        assert_eq!(Page::from_arg("Accessibility"), Some(Page::Accessibility));
        assert_eq!(Page::from_arg("Desktop & Dock"), Some(Page::DesktopDock));
        assert_eq!(Page::from_arg("Windows"), Some(Page::Tiling));
        assert_eq!(Page::from_arg("Windows & Tiling"), Some(Page::Tiling));
        assert_eq!(Page::from_arg("Mouse"), Some(Page::Mouse));
        assert_eq!(Page::from_arg("Trackpad"), Some(Page::Trackpad));
        assert_eq!(Page::from_arg("Lock Screen"), Some(Page::LockScreen));
        assert_eq!(Page::from_arg("Battery"), Some(Page::Battery));
        assert_eq!(Page::from_arg("Software Update"), Some(Page::SoftwareUpdate));
        assert_eq!(Page::from_arg("Login Items"), Some(Page::LoginItems));
        assert_eq!(Page::from_arg("Date & Time"), Some(Page::DateTime));
        assert_eq!(Page::from_arg("Default Apps"), Some(Page::DefaultApps));
        assert_eq!(Page::from_arg("unknown-xyz"), None);
    }
}
