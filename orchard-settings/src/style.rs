//! macOS System Settings look: palette, fonts, egui visuals.

use std::{cell::Cell, sync::Arc};

use eframe::egui::{
    self, style::WidgetVisuals, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Shadow,
    Stroke, TextStyle, Visuals,
};

#[derive(Clone, Copy, Debug)]
pub struct Pal {
    pub dark: bool,
    pub sidebar: Color32,
    pub content: Color32,
    pub group: Color32,
    pub group_border: Color32,
    pub separator: Color32,
    pub text: Color32,
    pub text2: Color32,
    pub text3: Color32,
    pub accent: Color32,
    pub control: Color32,
    pub control_border: Color32,
    pub toggle_off: Color32,
    pub knob: Color32,
    pub hover: Color32,
    pub field: Color32,
    pub menu: Color32,
}

thread_local! {
    static PAL: Cell<Option<Pal>> = const { Cell::new(None) };
}

pub fn pal() -> Pal {
    PAL.with(|p| p.get()).unwrap_or_else(|| Pal::new(false, [0, 122, 255]))
}

pub const BOLD: &str = "semibold";

impl Pal {
    pub fn new(dark: bool, accent: [u8; 3]) -> Pal {
        let accent = Color32::from_rgb(accent[0], accent[1], accent[2]);
        if dark {
            Pal {
                dark,
                sidebar: Color32::from_rgb(0x2A, 0x29, 0x2D),
                content: Color32::from_rgb(0x1F, 0x1E, 0x21),
                group: Color32::from_rgb(0x29, 0x28, 0x2B),
                group_border: Color32::from_white_alpha(14),
                separator: Color32::from_white_alpha(18),
                text: Color32::from_rgb(0xF2, 0xF2, 0xF4),
                text2: Color32::from_rgb(0x9C, 0x9C, 0xA1),
                text3: Color32::from_rgb(0x6C, 0x6C, 0x70),
                accent,
                control: Color32::from_rgb(0x48, 0x47, 0x4B),
                control_border: Color32::from_white_alpha(20),
                toggle_off: Color32::from_white_alpha(36),
                knob: Color32::from_rgb(0xE8, 0xE8, 0xEA),
                hover: Color32::from_white_alpha(14),
                field: Color32::from_rgb(0x34, 0x33, 0x37),
                menu: Color32::from_rgb(0x2E, 0x2D, 0x31),
            }
        } else {
            Pal {
                dark,
                sidebar: Color32::from_rgb(0xE9, 0xE7, 0xEB),
                content: Color32::from_rgb(0xF6, 0xF5, 0xF7),
                group: Color32::WHITE,
                group_border: Color32::from_black_alpha(16),
                separator: Color32::from_black_alpha(20),
                text: Color32::from_rgb(0x1D, 0x1D, 0x1F),
                text2: Color32::from_rgb(0x80, 0x80, 0x85),
                text3: Color32::from_rgb(0xB0, 0xB0, 0xB5),
                accent,
                control: Color32::WHITE,
                control_border: Color32::from_black_alpha(34),
                toggle_off: Color32::from_black_alpha(26),
                knob: Color32::WHITE,
                hover: Color32::from_black_alpha(10),
                field: Color32::WHITE,
                menu: Color32::from_rgb(0xF4, 0xF3, 0xF5),
            }
        }
    }
}

/// Look up a font file via fontconfig: (path, face index).
fn fc_match(pattern: &str) -> Option<(String, u32)> {
    let out = std::process::Command::new("fc-match")
        .args(["-f", "%{file}\n%{index}", pattern])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let mut l = s.lines();
    let file = l.next()?.to_string();
    let index = l.next().and_then(|i| i.trim().parse().ok()).unwrap_or(0);
    Some((file, index))
}

fn load_font(pattern: &str, want_family: &str) -> Option<FontData> {
    let (file, index) = fc_match(pattern)?;
    // fontconfig falls back to anything; only take what we asked for
    if !file.to_lowercase().contains(&want_family.to_lowercase()) {
        return None;
    }
    let bytes = std::fs::read(&file).ok()?;
    let mut fd = FontData::from_owned(bytes);
    fd.index = index;
    Some(fd)
}

pub fn install_fonts(ctx: &egui::Context) {
    let mut defs = FontDefinitions::default();
    let mut bold_family = Vec::new();
    // SF Pro when installed; Inter is the closest freely available match.
    let regular = load_font("SF Pro Text:style=Regular", "sf-pro")
        .or_else(|| load_font("Inter:style=Regular", "inter"))
        .or_else(|| load_font("Cantarell:style=Regular", "cantarell"))
        .or_else(|| load_font("Noto Sans:style=Regular", "noto"));
    let semibold = load_font("SF Pro Text:style=Semibold", "sf-pro")
        .or_else(|| load_font("Inter:style=SemiBold", "inter"))
        .or_else(|| load_font("Inter:style=Bold", "inter"))
        .or_else(|| load_font("Noto Sans:style=Bold", "noto"));
    if let Some(r) = regular {
        defs.font_data.insert("ui".into(), Arc::new(r));
        defs.families.get_mut(&FontFamily::Proportional).unwrap().insert(0, "ui".into());
    }
    if let Some(b) = semibold {
        defs.font_data.insert("ui-bold".into(), Arc::new(b));
        bold_family.push("ui-bold".to_string());
    }
    bold_family.extend(defs.families[&FontFamily::Proportional].iter().cloned());
    defs.families.insert(FontFamily::Name(BOLD.into()), bold_family);
    ctx.set_fonts(defs);
}

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(BOLD.into()))
}

pub fn apply(ctx: &egui::Context, dark: bool, accent: [u8; 3]) {
    let p = Pal::new(dark, accent);
    PAL.with(|c| c.set(Some(p)));

    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    v.panel_fill = p.content;
    v.window_fill = p.menu;
    v.extreme_bg_color = p.field;
    v.faint_bg_color = p.hover;
    v.override_text_color = Some(p.text);
    v.hyperlink_color = p.accent;
    v.selection.bg_fill = p.accent.gamma_multiply(0.45);
    v.selection.stroke = Stroke::new(1.0_f32, p.accent);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_stroke = Stroke::new(1.0_f32, p.control_border);
    v.window_shadow = Shadow {
        offset: [0, 6],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 110 } else { 50 }),
    };
    v.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 110 } else { 45 }),
    };
    v.text_cursor.stroke = Stroke::new(1.5_f32, p.accent);
    let w = |bg: Color32, stroke: Color32| WidgetVisuals {
        bg_fill: bg,
        weak_bg_fill: bg,
        bg_stroke: Stroke::new(1.0_f32, stroke),
        corner_radius: CornerRadius::same(6),
        fg_stroke: Stroke::new(1.0_f32, p.text),
        expansion: 0.0,
    };
    v.widgets.noninteractive = w(p.group, p.separator);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.separator);
    v.widgets.inactive = w(p.control, p.control_border);
    v.widgets.hovered = w(p.control, p.control_border);
    v.widgets.active = w(p.hover, p.control_border);
    v.widgets.open = w(p.control, p.control_border);
    ctx.set_visuals(v);

    ctx.style_mut(|s| {
        s.text_styles = [
            (TextStyle::Small, FontId::proportional(11.0)),
            (TextStyle::Body, FontId::proportional(13.0)),
            (TextStyle::Button, FontId::proportional(13.0)),
            (TextStyle::Monospace, FontId::monospace(12.0)),
            (TextStyle::Heading, bold(15.0)),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 3.0);
        s.spacing.interact_size.y = 22.0;
        s.spacing.menu_margin = egui::Margin::same(5);
        s.spacing.scroll = egui::style::ScrollStyle::floating();
        s.interaction.selectable_labels = false;
    });
}

/// Should the UI be dark for this appearance setting?
pub fn resolve_dark(mode: orchard_config::ThemeMode) -> bool {
    match mode {
        orchard_config::ThemeMode::Dark => true,
        orchard_config::ThemeMode::Light => false,
        orchard_config::ThemeMode::Auto => crate::sys::output("gsettings", &["get", "org.gnome.desktop.interface", "color-scheme"])
            .map(|s| s.contains("dark"))
            .unwrap_or(false),
    }
}
