//! Colours and metrics for the macOS-style window chrome.
//!
//! Values are eyeballed from macOS Sonoma/Sequoia screenshots; they are our own
//! approximations, not Apple assets.

use std::sync::{
    atomic::{AtomicU32, AtomicU8, Ordering},
    OnceLock,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Appearance {
    Light,
    Dark,
}

/// RGBA, straight (non-premultiplied) alpha, 0..=255.
pub type Rgba = [u8; 4];

#[derive(Debug, Clone)]
pub struct Palette {
    pub titlebar_active_top: Rgba,
    pub titlebar_active_bottom: Rgba,
    pub titlebar_inactive: Rgba,
    pub separator_active: Rgba,
    pub separator_inactive: Rgba,
    pub title_active: Rgba,
    pub title_inactive: Rgba,
    pub button_inactive_fill: Rgba,
    pub button_inactive_stroke: Rgba,
    pub close: (Rgba, Rgba),
    pub minimize: (Rgba, Rgba),
    pub zoom: (Rgba, Rgba),
    pub close_glyph: Rgba,
    pub minimize_glyph: Rgba,
    pub zoom_glyph: Rgba,
    pub rim_active: Rgba,
    pub rim_inactive: Rgba,
    pub top_highlight: Rgba,
}

/// Title bar height in logical pixels (standard macOS title bar).
pub const TITLEBAR_HEIGHT: i32 = 28;
/// Traffic-light diameter in logical pixels.
pub const BUTTON_DIAMETER: f32 = 12.0;
/// Center of the first traffic light from the left edge.
pub const BUTTON_FIRST_CENTER_X: f32 = 14.0;
/// Distance between traffic light centers.
pub const BUTTON_SPACING: f32 = 20.0;

static CORNER_RADIUS: AtomicU32 = AtomicU32::new(u32::MAX);

/// Window corner radius in logical pixels (Settings ▸ Appearance).
/// Cached in an atomic so the render hot path never acquires the settings RwLock.
pub fn corner_radius() -> f32 {
    let bits = CORNER_RADIUS.load(Ordering::Relaxed);
    if bits == u32::MAX {
        let r = crate::settings::get().appearance.corner_radius.clamp(0.0, 24.0);
        CORNER_RADIUS.store(r.to_bits(), Ordering::Relaxed);
        r
    } else {
        f32::from_bits(bits)
    }
}

static APPEARANCE: AtomicU8 = AtomicU8::new(0);

fn resolve_appearance() -> Appearance {
    use orchard_config::ThemeMode;
    match std::env::var("ORCHARD_APPEARANCE").as_deref() {
        Ok("dark") => return Appearance::Dark,
        Ok("light") => return Appearance::Light,
        _ => {}
    }
    match crate::settings::get().appearance.mode {
        ThemeMode::Light => Appearance::Light,
        ThemeMode::Dark => Appearance::Dark,
        ThemeMode::Auto => detect_system_appearance(),
    }
}

/// Re-evaluate light/dark and cached appearance metrics (after a settings reload).
pub fn refresh_appearance() {
    let r = crate::settings::get().appearance.corner_radius.clamp(0.0, 24.0);
    CORNER_RADIUS.store(r.to_bits(), Ordering::Relaxed);
    let a = resolve_appearance();
    APPEARANCE.store(if a == Appearance::Dark { 2 } else { 1 }, Ordering::Relaxed);
}

pub fn appearance() -> Appearance {
    match APPEARANCE.load(Ordering::Relaxed) {
        0 => {
            refresh_appearance();
            appearance()
        }
        2 => Appearance::Dark,
        _ => Appearance::Light,
    }
}

/// Follow the freedesktop colour-scheme preference if gsettings knows one.
/// Spawning `gsettings` is slow (a few ms), and settings reloads can happen in
/// quick succession while dragging a slider, so cache the answer briefly.
fn detect_system_appearance() -> Appearance {
    static CACHE: OnceLock<std::sync::Mutex<Option<(std::time::Instant, Appearance)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(None));
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, appearance)) = *guard {
        if at.elapsed() < std::time::Duration::from_secs(5) {
            return appearance;
        }
    }
    let out = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .output();
    let appearance = match out {
        Ok(o) if String::from_utf8_lossy(&o.stdout).contains("dark") => Appearance::Dark,
        _ => Appearance::Light,
    };
    *guard = Some((std::time::Instant::now(), appearance));
    appearance
}

pub fn palette(appearance: Appearance) -> &'static Palette {
    static LIGHT: OnceLock<Palette> = OnceLock::new();
    static DARK: OnceLock<Palette> = OnceLock::new();
    match appearance {
        Appearance::Light => LIGHT.get_or_init(|| Palette {
            titlebar_active_top: [0xF0, 0xF0, 0xF0, 0xFF],
            titlebar_active_bottom: [0xE5, 0xE5, 0xE5, 0xFF],
            titlebar_inactive: [0xF6, 0xF6, 0xF6, 0xFF],
            separator_active: [0x00, 0x00, 0x00, 0x18],
            separator_inactive: [0x00, 0x00, 0x00, 0x10],
            title_active: [0x26, 0x26, 0x26, 0xFF],
            title_inactive: [0x9E, 0x9E, 0x9E, 0xFF],
            button_inactive_fill: [0xDC, 0xDC, 0xDC, 0xFF],
            button_inactive_stroke: [0xC8, 0xC8, 0xC8, 0xFF],
            close: ([0xFF, 0x5F, 0x57, 0xFF], [0xE2, 0x46, 0x3F, 0xFF]),
            minimize: ([0xFE, 0xBC, 0x2E, 0xFF], [0xE1, 0xA1, 0x16, 0xFF]),
            zoom: ([0x28, 0xC8, 0x40, 0xFF], [0x14, 0xAE, 0x2A, 0xFF]),
            close_glyph: [0x4D, 0x00, 0x00, 0xD0],
            minimize_glyph: [0x99, 0x57, 0x00, 0xE0],
            zoom_glyph: [0x00, 0x65, 0x00, 0xE0],
            rim_active: [0x00, 0x00, 0x00, 0x1A],
            rim_inactive: [0x00, 0x00, 0x00, 0x0F],
            top_highlight: [0xFF, 0xFF, 0xFF, 0x80],
        }),
        Appearance::Dark => DARK.get_or_init(|| Palette {
            titlebar_active_top: [0x3D, 0x3D, 0x3F, 0xFF],
            titlebar_active_bottom: [0x34, 0x34, 0x36, 0xFF],
            titlebar_inactive: [0x2C, 0x2C, 0x2E, 0xFF],
            separator_active: [0x00, 0x00, 0x00, 0x42],
            separator_inactive: [0x00, 0x00, 0x00, 0x2E],
            title_active: [0xE6, 0xE6, 0xE6, 0xFF],
            title_inactive: [0x80, 0x80, 0x80, 0xFF],
            button_inactive_fill: [0x4A, 0x4A, 0x4C, 0xFF],
            button_inactive_stroke: [0x40, 0x40, 0x42, 0xFF],
            close: ([0xFF, 0x5F, 0x57, 0xFF], [0xE0, 0x44, 0x3E, 0xFF]),
            minimize: ([0xFE, 0xBC, 0x2E, 0xFF], [0xDE, 0x9F, 0x18, 0xFF]),
            zoom: ([0x28, 0xC8, 0x40, 0xFF], [0x16, 0xA8, 0x2C, 0xFF]),
            close_glyph: [0x4D, 0x00, 0x00, 0xD0],
            minimize_glyph: [0x99, 0x57, 0x00, 0xE0],
            zoom_glyph: [0x00, 0x65, 0x00, 0xE0],
            rim_active: [0xFF, 0xFF, 0xFF, 0x24],
            rim_inactive: [0xFF, 0xFF, 0xFF, 0x12],
            top_highlight: [0xFF, 0xFF, 0xFF, 0x00],
        }),
    }
}
