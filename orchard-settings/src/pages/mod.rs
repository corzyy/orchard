//! Settings pages registry and CLI argument mapping.

pub mod appearance;
pub mod connectivity;
pub mod desktop;
pub mod input;
pub mod system;

use orchard_config::Settings;

use crate::{
    sys::Wake,
    theme::{rgb, Rgba},
    ui::Ui,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Page {
    Wifi,
    Bluetooth,
    Sound,
    Appearance,
    Wallpaper,
    DesktopDock,
    MenuBar,
    Displays,
    Keyboard,
    Pointer,
    LockPower,
    System,
}

use Page::*;

pub const SIDEBAR: &[&[Page]] = &[
    &[Wifi, Bluetooth, Sound],
    &[Appearance, Wallpaper, DesktopDock, MenuBar, Displays],
    &[Keyboard, Pointer],
    &[LockPower, System],
];

pub const ALL_PAGES: &[Page] = &[
    Wifi,
    Bluetooth,
    Sound,
    Appearance,
    Wallpaper,
    DesktopDock,
    MenuBar,
    Displays,
    Keyboard,
    Pointer,
    LockPower,
    System,
];

impl Page {
    pub fn title(self) -> &'static str {
        match self {
            Wifi => "Wi‑Fi & Network",
            Bluetooth => "Bluetooth",
            Sound => "Sound",
            Appearance => "Appearance",
            Wallpaper => "Wallpaper",
            DesktopDock => "Desktop & Dock",
            MenuBar => "Menu Bar",
            Displays => "Displays",
            Keyboard => "Keyboard",
            Pointer => "Mouse & Trackpad",
            LockPower => "Lock & Power",
            System => "System",
        }
    }

    pub fn icon(self) -> (&'static str, Rgba) {
        let blue = rgb([0x0A, 0x84, 0xFF]);
        let gray = rgb([0x8E, 0x8E, 0x93]);
        let dark = rgb([0x3A, 0x3A, 0x3E]);
        match self {
            Wifi => ("network-wireless", blue),
            Bluetooth => ("bluetooth", blue),
            Sound => ("audio-volume-high", rgb([0xFF, 0x37, 0x5F])),
            Appearance => ("preferences-desktop-appearance", dark),
            Wallpaper => ("preferences-desktop-wallpaper", rgb([0x32, 0xAD, 0xE6])),
            DesktopDock => ("view-app-grid", dark),
            MenuBar => ("open-menu", gray),
            Displays => ("video-display", blue),
            Keyboard => ("input-keyboard", gray),
            Pointer => ("input-mouse", gray),
            LockPower => ("battery-full", rgb([0x30, 0xD1, 0x58])),
            System => ("emblem-system", gray),
        }
    }

    pub fn keywords(self) -> &'static str {
        match self {
            Wifi => "wifi wireless wlan internet network ssid ethernet vpn",
            Bluetooth => "bluetooth devices headphones pair",
            Sound => "sound volume audio output input microphone speakers mute",
            Appearance => "appearance dark light mode accent color theme corner radius transparency motion icon cursor pointer",
            Wallpaper => "wallpaper background desktop picture",
            DesktopDock => "desktop dock magnification minimize hot corners mission control windows title bar focus",
            MenuBar => "menu bar clock control center battery percentage date seconds",
            Displays => "displays screen monitor resolution scale refresh hidpi",
            Keyboard => "keyboard layout repeat modifier caps numlock",
            Pointer => "mouse trackpad touchpad pointer speed scroll natural acceleration tap click gestures",
            LockPower => "lock screen idle password battery power energy profile performance",
            System => "system general about hardware cpu memory hostname date time timezone ntp default apps browser terminal login items autostart",
        }
    }

    /// Parse a CLI argument (supports both new and all legacy Orchard page names).
    pub fn from_arg(arg: &str) -> Option<Page> {
        let k: String = arg
            .to_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        match k.as_str() {
            "wifi" | "wifinetwork" | "network" | "wlan" => Some(Wifi),
            "bluetooth" | "bt" => Some(Bluetooth),
            "sound" | "audio" | "volume" => Some(Sound),
            "appearance" | "accessibility" | "theme" => Some(Appearance),
            "wallpaper" | "background" => Some(Wallpaper),
            "desktopdock" | "dock" | "desktop" | "windows" => Some(DesktopDock),
            "menubar" | "panel" | "clock" => Some(MenuBar),
            "displays" | "display" | "monitor" | "screen" => Some(Displays),
            "keyboard" | "keys" => Some(Keyboard),
            "mouse" | "trackpad" | "touchpad" | "pointer" | "mousetrackpad" => Some(Pointer),
            "lockscreen" | "lock" | "battery" | "power" | "lockpower" => Some(LockPower),
            "system" | "general" | "about" | "datetime" | "language" | "languageregion"
            | "loginitems" | "autostart" | "defaultapps" | "storage" | "users"
            | "usersgroups" => Some(System),
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
}

pub fn show(ui: &mut Ui, page: Page, s: &mut Settings, st: &mut PageState, wake: &Wake) {
    match page {
        Wifi => connectivity::wifi(ui, &mut st.conn, wake),
        Bluetooth => connectivity::bluetooth(ui, &mut st.conn, wake),
        Sound => connectivity::sound(ui, &mut st.conn, wake),
        Appearance => appearance::appearance(ui, s, &mut st.appearance, wake),
        Wallpaper => appearance::wallpaper(ui, s, &mut st.appearance, wake),
        DesktopDock => desktop::desktop_dock(ui, s),
        MenuBar => desktop::menu_bar(ui, s),
        Displays => desktop::displays(ui, s, &mut st.desktop, wake),
        Keyboard => input::keyboard(ui, s, &mut st.input, wake),
        Pointer => input::pointer(ui, s),
        LockPower => system::lock_power(ui, s, &mut st.system, wake),
        System => system::system(ui, s, &mut st.system, wake),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_args_map_legacy_and_current_pages() {
        assert_eq!(Page::from_arg("Wi‑Fi"), Some(Page::Wifi));
        assert_eq!(Page::from_arg("wifi"), Some(Page::Wifi));
        assert_eq!(Page::from_arg("Network"), Some(Page::Wifi));
        assert_eq!(Page::from_arg("Accessibility"), Some(Page::Appearance));
        assert_eq!(Page::from_arg("Desktop & Dock"), Some(Page::DesktopDock));
        assert_eq!(Page::from_arg("Trackpad"), Some(Page::Pointer));
        assert_eq!(Page::from_arg("Battery"), Some(Page::LockPower));
        assert_eq!(Page::from_arg("Login Items"), Some(Page::System));
        assert_eq!(Page::from_arg("Date & Time"), Some(Page::System));
        assert_eq!(Page::from_arg("unknown-xyz"), None);
    }
}
