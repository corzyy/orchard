//! Shell colours (light / dark), approximating macOS Sonoma.

use crate::text::Rgba;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    Light,
    Dark,
}

pub fn appearance(mode: orchard_config::ThemeMode) -> Appearance {
    match std::env::var("ORCHARD_APPEARANCE").as_deref() {
        Ok("dark") => return Appearance::Dark,
        Ok("light") => return Appearance::Light,
        _ => {}
    }
    match mode {
        orchard_config::ThemeMode::Light => return Appearance::Light,
        orchard_config::ThemeMode::Dark => return Appearance::Dark,
        orchard_config::ThemeMode::Auto => {}
    }
    let out = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .output();
    match out {
        Ok(o) if String::from_utf8_lossy(&o.stdout).contains("dark") => Appearance::Dark,
        _ => Appearance::Light,
    }
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub bar_tint: Rgba,
    pub bar_text: Rgba,
    pub bar_highlight: Rgba,
    pub dock_tint: Rgba,
    pub dock_border: Rgba,
    pub dock_dot: Rgba,
    pub dock_separator: Rgba,
    pub tooltip_bg: Rgba,
    pub tooltip_text: Rgba,
    pub menu_bg: Rgba,
    pub menu_border: Rgba,
    pub menu_text: Rgba,
    pub menu_text_disabled: Rgba,
    pub menu_separator: Rgba,
    pub menu_selection: Rgba,
    pub menu_selection_text: Rgba,
}

impl Theme {
    pub fn new(appearance: Appearance) -> Theme {
        match appearance {
            Appearance::Light => Theme {
                bar_tint: [246, 246, 246, 150],
                bar_text: [20, 20, 20, 255],
                bar_highlight: [0, 0, 0, 40],
                dock_tint: [255, 255, 255, 90],
                dock_border: [255, 255, 255, 110],
                dock_dot: [40, 40, 40, 220],
                dock_separator: [0, 0, 0, 45],
                tooltip_bg: [236, 236, 236, 235],
                tooltip_text: [30, 30, 30, 255],
                menu_bg: [244, 244, 246, 175],
                menu_border: [0, 0, 0, 40],
                menu_text: [20, 20, 20, 255],
                menu_text_disabled: [20, 20, 20, 90],
                menu_separator: [0, 0, 0, 30],
                menu_selection: [10, 110, 245, 255],
                menu_selection_text: [255, 255, 255, 255],
            },
            Appearance::Dark => Theme {
                bar_tint: [30, 30, 30, 140],
                bar_text: [240, 240, 240, 255],
                bar_highlight: [255, 255, 255, 50],
                dock_tint: [40, 40, 40, 110],
                dock_border: [255, 255, 255, 45],
                dock_dot: [230, 230, 230, 220],
                dock_separator: [255, 255, 255, 50],
                tooltip_bg: [40, 40, 40, 235],
                tooltip_text: [235, 235, 235, 255],
                menu_bg: [38, 38, 40, 175],
                menu_border: [255, 255, 255, 40],
                menu_text: [235, 235, 235, 255],
                menu_text_disabled: [235, 235, 235, 90],
                menu_separator: [255, 255, 255, 35],
                menu_selection: [10, 100, 225, 255],
                menu_selection_text: [255, 255, 255, 255],
            },
        }
    }

    /// Theme for the user's settings: light/dark, accent colour, and
    /// solid backgrounds when "Reduce transparency" is on.
    pub fn from_settings(s: &orchard_config::Settings) -> Theme {
        let mut t = Theme::new(appearance(s.appearance.mode));
        let [r, g, b] = s.appearance.accent.rgb();
        t.menu_selection = [r, g, b, 255];
        if s.appearance.reduce_transparency {
            let dark = t.bar_text[0] > 128;
            let (bar, dock, menu) = if dark {
                ([36, 36, 38, 255], [44, 44, 46, 255], [44, 44, 46, 255])
            } else {
                ([236, 236, 236, 255], [228, 228, 230, 255], [242, 242, 242, 255])
            };
            t.bar_tint = bar;
            t.dock_tint = dock;
            t.menu_bg = menu;
            t.tooltip_bg[3] = 255;
        }
        t
    }
}
