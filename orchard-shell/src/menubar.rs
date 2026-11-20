//! The menu bar: logo menu, app menu, window menu, status items, clock.

use tiny_skia::{FillRule, PathBuilder, Pixmap, Stroke, Transform};

use crate::{
    draw::{fill_rounded, paint},
    status::{self, Battery, Network},
    text::{Fonts, Weight},
    theme::Theme,
};

pub const HEIGHT: f32 = 24.0;
const FONT: f32 = 13.0;
const ITEM_PAD: f32 = 10.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarItem {
    Logo,
    App,
    Window,
    Help,
    Clock,
    /// Control Center (the switches icon)
    Control,
    /// Sound / volume icon (left of Control Center)
    Volume,
    /// Ethernet / Wi-Fi icon (left of Volume)
    Network,
    /// one of the focused app's own menus (global menu), by index
    AppMenu(u8),
    /// "Rebuild Desktop" (only when running from a source checkout)
    Dev,
    /// System tray icon (index into `visible_tray_indices`)
    Tray(usize),
}

pub struct MenuBar {
    pub app_name: String,
    pub app_id: Option<String>,
    pub clock: String,
    pub battery: Option<Battery>,
    /// Settings ▸ Menu Bar
    pub cfg: orchard_config::MenuBarSettings,
    pub network: Network,
    /// Default output sink volume `(0.0..=1.0+, muted)`
    pub volume: Option<(f32, bool)>,
    /// item whose menu is open
    pub open: Option<BarItem>,
    /// Do Not Disturb is on: show a moon
    pub dnd: bool,
    /// Game Mode is on: a controller next to the clock
    pub game: bool,
    /// titles of the focused app's global menu (empty: Window + Help)
    pub app_menus: Vec<String>,
    /// source checkout status for the Rebuild item (`None`: hidden)
    pub dev: Option<crate::devbuild::DevBuild>,
    /// advances once a second; turns the Rebuild icon while a build runs
    pub dev_spin: u8,
    /// system tray items (StatusNotifierItem)
    pub tray_items: Vec<crate::tray::TrayItem>,
    pub german: bool,
}

impl MenuBar {
    pub fn new() -> MenuBar {
        MenuBar {
            app_name: "Finder".into(),
            app_id: None,
            clock: status::clock_text(&Default::default()),
            battery: status::battery(),
            cfg: Default::default(),
            network: status::network(),
            volume: None,
            open: None,
            dnd: false,
            game: false,
            app_menus: Vec::new(),
            dev: crate::devbuild::DevBuild::detect(),
            dev_spin: 0,
            tray_items: Vec::new(),
            german: std::env::var("LANG").map(|l| l.starts_with("de")).unwrap_or(false),
        }
    }

    /// Refresh only the clock text. Returns true if it changed.
    pub fn refresh_clock(&mut self) -> bool {
        let clock = status::clock_text(&self.cfg);
        if clock != self.clock {
            self.clock = clock;
            true
        } else {
            false
        }
    }

    /// Refresh clock / battery / network. Returns true if anything changed.
    pub fn refresh_status(&mut self) -> bool {
        self.refresh_status_with(false)
    }

    /// Refresh clock, plus battery/network only when shown (or needed for battery-saver).
    pub fn refresh_status_with(&mut self, need_battery: bool) -> bool {
        let clock = status::clock_text(&self.cfg);
        let battery = if self.cfg.show_battery || need_battery {
            status::battery()
        } else {
            self.battery.clone()
        };
        let network = if self.cfg.show_network {
            status::network()
        } else {
            self.network
        };
        let changed = clock != self.clock || battery != self.battery || network != self.network;
        self.clock = clock;
        self.battery = battery;
        self.network = network;
        changed
    }

    pub fn label(&self, item: BarItem) -> String {
        match item {
            BarItem::Logo => String::new(),
            BarItem::App => self.app_name.clone(),
            BarItem::Window => if self.german { "Fenster" } else { "Window" }.into(),
            BarItem::Help => if self.german { "Hilfe" } else { "Help" }.into(),
            BarItem::Clock => self.clock.clone(),
            BarItem::Control | BarItem::Volume | BarItem::Network | BarItem::Dev | BarItem::Tray(_) => {
                String::new()
            }
            BarItem::AppMenu(i) => self.app_menus.get(i as usize).cloned().unwrap_or_default(),
        }
    }

    /// Left-side items with their x ranges (logical).
    pub fn left_items(&self, fonts: &Fonts) -> Vec<(BarItem, f32, f32)> {
        let mut out = Vec::new();
        let mut x = 8.0;
        let logo_w = 34.0;
        out.push((BarItem::Logo, x, x + logo_w));
        x += logo_w;
        let items: Vec<BarItem> = if self.app_menus.is_empty() {
            vec![BarItem::App, BarItem::Window, BarItem::Help]
        } else {
            std::iter::once(BarItem::App)
                .chain((0..self.app_menus.len().min(30)).map(|i| BarItem::AppMenu(i as u8)))
                .collect()
        };
        for item in items {
            let weight = if item == BarItem::App { Weight::Bold } else { Weight::Regular };
            let w = fonts.measure(&self.label(item), FONT, weight) + 2.0 * ITEM_PAD;
            out.push((item, x, x + w));
            x += w;
        }
        out
    }

    pub fn clock_range(&self, fonts: &Fonts, width: f32) -> (f32, f32) {
        let w = fonts.measure(&self.clock, FONT, Weight::Regular) + 2.0 * ITEM_PAD;
        (width - 8.0 - w, width - 8.0)
    }

    /// The Control Center icon, left of the clock.
    pub fn control_range(&self, fonts: &Fonts, width: f32) -> (f32, f32) {
        let (ca, _) = self.clock_range(fonts, width);
        (ca - 32.0, ca - 2.0)
    }

    /// The Volume icon, left of Control Center (if enabled in Settings).
    pub fn volume_range(&self, fonts: &Fonts, width: f32) -> Option<(f32, f32)> {
        if !self.cfg.show_volume {
            return None;
        }
        let (cca, _) = self.control_range(fonts, width);
        Some((cca - 30.0, cca - 2.0))
    }

    /// The Ethernet / Wi-Fi icon, left of Volume or Control Center (if enabled in Settings).
    pub fn network_range(&self, fonts: &Fonts, width: f32) -> Option<(f32, f32)> {
        if !self.cfg.show_network {
            return None;
        }
        let right = self
            .volume_range(fonts, width)
            .map(|(va, _)| va)
            .unwrap_or_else(|| self.control_range(fonts, width).0);
        Some((right - 30.0, right - 2.0))
    }

    /// The Rebuild item, left of the other status icons (source checkouts only).
    pub fn dev_range(&self, fonts: &Fonts, width: f32) -> Option<(f32, f32)> {
        self.dev.as_ref()?;
        let right = self
            .network_range(fonts, width)
            .or_else(|| self.volume_range(fonts, width))
            .map(|(a, _)| a)
            .unwrap_or_else(|| self.control_range(fonts, width).0);
        Some((right - 30.0, right - 2.0))
    }

    /// Left edge of the fixed right-side items (DND moon and battery go further left).
    fn status_left_edge(&self, fonts: &Fonts, width: f32) -> f32 {
        self.dev_range(fonts, width)
            .or_else(|| self.network_range(fonts, width))
            .or_else(|| self.volume_range(fonts, width))
            .map(|(a, _)| a)
            .unwrap_or_else(|| self.control_range(fonts, width).0)
    }

    /// Left edge after DND moon and battery (system tray icons sit to the left of this).
    fn tray_right_edge(&self, fonts: &Fonts, width: f32) -> f32 {
        let mut x = self.status_left_edge(fonts, width) - 4.0;
        if self.game {
            x -= 24.0;
        }
        if self.dnd {
            x -= 22.0;
        }
        if self.cfg.show_battery {
            if let Some(b) = &self.battery {
                x -= 30.0;
                if self.cfg.battery_percentage {
                    let label = format!("{}%", b.percent);
                    let lw = fonts.measure(&label, 12.0, Weight::Regular);
                    x -= lw + 6.0;
                }
            }
        }
        x - 4.0
    }

    /// `(tray_items_index, x0, x1)` for each visible system tray item, right-to-left.
    pub fn tray_ranges(&self, fonts: &Fonts, width: f32) -> Vec<(usize, f32, f32)> {
        let mut x = self.tray_right_edge(fonts, width);
        let min_x = self.left_items(fonts).last().map(|(_, _, b)| *b + 12.0).unwrap_or(120.0);
        let mut out = Vec::new();
        for (i, item) in self.tray_items.iter().enumerate() {
            if !item.visible() {
                continue;
            }
            let x0 = x - 24.0;
            if x0 < min_x {
                break;
            }
            out.push((i, x0, x - 2.0));
            x = x0;
        }
        out
    }

    pub fn hit(&self, fonts: &Fonts, x: f32, width: f32) -> Option<BarItem> {
        if let Some((item, _, _)) = self.left_items(fonts).into_iter().find(|(_, a, b)| x >= *a && x < *b) {
            return Some(item);
        }
        let (a, b) = self.clock_range(fonts, width);
        if x >= a && x < b {
            return Some(BarItem::Clock);
        }
        let (a, b) = self.control_range(fonts, width);
        if x >= a && x < b {
            return Some(BarItem::Control);
        }
        if let Some((a, b)) = self.volume_range(fonts, width) {
            if x >= a && x < b {
                return Some(BarItem::Volume);
            }
        }
        if let Some((a, b)) = self.network_range(fonts, width) {
            if x >= a && x < b {
                return Some(BarItem::Network);
            }
        }
        if let Some((a, b)) = self.dev_range(fonts, width) {
            if x >= a && x < b {
                return Some(BarItem::Dev);
            }
        }
        for (idx, a, b) in self.tray_ranges(fonts, width) {
            if x >= a && x < b {
                return Some(BarItem::Tray(idx));
            }
        }
        None
    }

    pub fn paint(&self, pm: &mut Pixmap, scale: f32, fonts: &Fonts, apps: &crate::apps::Apps, theme: &Theme) {
        let s = scale;
        let w = pm.width() as f32 / s;
        // tint over the compositor's frosted backdrop
        fill_rounded(pm, 0.0, 0.0, pm.width() as f32, pm.height() as f32, 0.0, theme.bar_tint);
        let px = FONT * s;
        let baseline = fonts.baseline_centered(0.0, HEIGHT * s, px, Weight::Regular);

        for (item, a, b) in self.left_items(fonts) {
            if self.open == Some(item) {
                fill_rounded(pm, a * s, 3.0 * s, (b - a) * s, (HEIGHT - 6.0) * s, 4.0 * s, theme.bar_highlight);
            }
            match item {
                BarItem::Logo => draw_leaf(pm, (a + (b - a) / 2.0) * s, HEIGHT / 2.0 * s, s, theme.bar_text),
                _ => {
                    let weight = if item == BarItem::App { Weight::Bold } else { Weight::Regular };
                    fonts.draw(pm, &self.label(item), (a + ITEM_PAD) * s, baseline, px, weight, theme.bar_text);
                }
            }
        }

        // right side: clock, then status glyphs to its left
        let (ca, _) = self.clock_range(fonts, w);
        if self.open == Some(BarItem::Clock) {
            let (a, b) = self.clock_range(fonts, w);
            fill_rounded(pm, a * s, 3.0 * s, (b - a) * s, (HEIGHT - 6.0) * s, 4.0 * s, theme.bar_highlight);
        }
        fonts.draw(pm, &self.clock, (ca + ITEM_PAD) * s, baseline, px, Weight::Regular, theme.bar_text);
        let (cca, ccb) = self.control_range(fonts, w);
        if self.open == Some(BarItem::Control) {
            fill_rounded(pm, cca * s, 3.0 * s, (ccb - cca) * s, (HEIGHT - 6.0) * s, 4.0 * s, theme.bar_highlight);
        }
        draw_control_icon(pm, (cca + ccb) / 2.0 * s, HEIGHT / 2.0 * s, s, theme.bar_text);

        if let Some((va, vb)) = self.volume_range(fonts, w) {
            if self.open == Some(BarItem::Volume) {
                fill_rounded(pm, va * s, 3.0 * s, (vb - va) * s, (HEIGHT - 6.0) * s, 4.0 * s, theme.bar_highlight);
            }
            draw_volume(pm, (va + vb) / 2.0 * s, HEIGHT / 2.0 * s, s, self.volume, theme.bar_text);
        }
        if let Some((na, nb)) = self.network_range(fonts, w) {
            if self.open == Some(BarItem::Network) {
                fill_rounded(pm, na * s, 3.0 * s, (nb - na) * s, (HEIGHT - 6.0) * s, 4.0 * s, theme.bar_highlight);
            }
            draw_network(pm, (na + nb) / 2.0 * s, HEIGHT / 2.0 * s, s, self.network, theme.bar_text);
        }
        if let (Some((da, db)), Some(dev)) = (self.dev_range(fonts, w), self.dev.as_ref()) {
            if self.open == Some(BarItem::Dev) {
                fill_rounded(pm, da * s, 3.0 * s, (db - da) * s, (HEIGHT - 6.0) * s, 4.0 * s, theme.bar_highlight);
            }
            let running = dev.build == crate::devbuild::Build::Running;
            let mut c = theme.bar_text;
            if !running && dev.badge().is_none() {
                c[3] = (c[3] as u16 * 3 / 5) as u8; // nothing to do: recede
            }
            let turn = if running { self.dev_spin as f32 * 45f32.to_radians() } else { 0.0 };
            let (cx, cy) = ((da + db) / 2.0 * s, HEIGHT / 2.0 * s);
            draw_rebuild(pm, cx, cy, s, turn, c);
            if let Some(badge) = dev.badge() {
                crate::draw::fill_circle(pm, cx + 5.5 * s, cy - 5.0 * s, 2.6 * s, badge);
            }
        }

        let mut x = self.status_left_edge(fonts, w) - 4.0;
        if self.game {
            x -= 24.0;
            crate::panel::controller(pm, (x + 12.0) * s, HEIGHT / 2.0 * s, 7.0 * s, theme.bar_text);
        }
        if self.dnd {
            x -= 22.0;
            crate::panel::moon(pm, (x + 11.0) * s, HEIGHT / 2.0 * s, 6.0 * s, theme.bar_text);
        }
        if self.cfg.show_battery {
            if let Some(b) = &self.battery {
                x -= 30.0;
                draw_battery(pm, x * s, HEIGHT / 2.0 * s, s, b, theme.bar_text);
                if self.cfg.battery_percentage {
                    let label = format!("{}%", b.percent);
                    let lw = fonts.measure(&label, 12.0 * s, Weight::Regular) / s;
                    x -= lw + 6.0;
                    fonts.draw(pm, &label, x * s, baseline, 12.0 * s, Weight::Regular, theme.bar_text);
                }
            }
        }

        // System tray icons (StatusNotifierItem)
        let icon_px = (16.0 * s).round().max(16.0);
        for (idx, a, b) in self.tray_ranges(fonts, w) {
            if self.open == Some(BarItem::Tray(idx)) {
                fill_rounded(pm, a * s, 3.0 * s, (b - a) * s, (HEIGHT - 6.0) * s, 4.0 * s, theme.bar_highlight);
            }
            let Some(item) = self.tray_items.get(idx) else { continue };
            let cx = ((a + b) * 0.5) * s;
            let cy = (HEIGHT * 0.5) * s;
            let drew = draw_tray_icon(pm, item, apps, cx, cy, icon_px);
            if !drew {
                // Fallback dot if no icon could be loaded
                crate::draw::fill_circle(pm, cx, cy, 4.5 * s, theme.bar_text);
            }
        }
    }
}

fn draw_tray_icon(
    pm: &mut Pixmap,
    item: &crate::tray::TrayItem,
    apps: &crate::apps::Apps,
    cx: f32,
    cy: f32,
    target_px: f32,
) -> bool {
    let draw_src = |dst: &mut Pixmap, src: tiny_skia::PixmapRef<'_>| {
        let iw = src.width() as f32;
        let ih = src.height() as f32;
        if iw <= 0.0 || ih <= 0.0 {
            return false;
        }
        let sc = target_px / iw.max(ih);
        let dx = cx - iw * sc * 0.5;
        let dy = cy - ih * sc * 0.5;
        let p = tiny_skia::PixmapPaint {
            opacity: 1.0,
            quality: tiny_skia::FilterQuality::Bilinear,
            ..Default::default()
        };
        dst.draw_pixmap(0, 0, src, &p, Transform::from_scale(sc, sc).post_translate(dx, dy), None);
        true
    };
    if let Some((w, h, ref rgba)) = item.icon_pixmap {
        if let Some(src) = tiny_skia::PixmapRef::from_bytes(rgba, w, h) {
            return draw_src(pm, src);
        }
    }
    if let Some(name) = item.icon_name.as_deref() {
        if let Some(ico) = apps.icon_any(name, target_px.round() as u32) {
            return draw_src(pm, ico.as_ref().as_ref());
        }
    }
    false
}

/// Rebuild: a circular arrow (rotated by `turn` radians while building).
fn draw_rebuild(pm: &mut Pixmap, cx: f32, cy: f32, s: f32, turn: f32, color: [u8; 4]) {
    let r = 5.6 * s;
    let st = Stroke {
        width: 1.5 * s,
        line_cap: tiny_skia::LineCap::Round,
        ..Default::default()
    };
    let (a0, a1) = (turn + 30f32.to_radians(), turn + 320f32.to_radians());
    let mut pb = PathBuilder::new();
    let n = 24;
    for k in 0..=n {
        let a = a0 + (a1 - a0) * k as f32 / n as f32;
        let (px, py) = (cx + r * a.cos(), cy + r * a.sin());
        if k == 0 {
            pb.move_to(px, py);
        } else {
            pb.line_to(px, py);
        }
    }
    if let Some(p) = pb.finish() {
        pm.stroke_path(&p, &paint(color), &st, Transform::identity(), None);
    }
    // arrowhead at the end of the arc, pointing along it
    let (ex, ey) = (cx + r * a1.cos(), cy + r * a1.sin());
    let (tx, ty) = (-a1.sin(), a1.cos()); // tangent (direction of travel)
    let (nx, ny) = (a1.cos(), a1.sin()); // outward normal
    let (len, half) = (3.6 * s, 2.9 * s);
    let mut pb = PathBuilder::new();
    pb.move_to(ex + tx * len, ey + ty * len);
    pb.line_to(ex + nx * half, ey + ny * half);
    pb.line_to(ex - nx * half, ey - ny * half);
    pb.close();
    if let Some(p) = pb.finish() {
        pm.fill_path(&p, &paint(color), FillRule::Winding, Transform::identity(), None);
    }
}

/// Control Center: two stacked toggle pills.
fn draw_control_icon(pm: &mut Pixmap, cx: f32, cy: f32, s: f32, color: [u8; 4]) {
    let (w, h) = (16.0 * s, 6.0 * s);
    let st = Stroke {
        width: 1.3 * s,
        ..Default::default()
    };
    for (dy, knob_right) in [(-4.0f32, false), (4.0, true)] {
        let y = cy + dy * s - h / 2.0;
        if let Some(p) = crate::draw::rounded_rect(cx - w / 2.0, y, w, h, h / 2.0) {
            pm.stroke_path(&p, &paint(color), &st, Transform::identity(), None);
        }
        let kx = if knob_right { cx + w / 2.0 - h / 2.0 } else { cx - w / 2.0 + h / 2.0 };
        crate::draw::fill_circle(pm, kx, cy + dy * s, h / 2.0, color);
    }
}

/// Orchard's logo: a simple leaf (deliberately not an apple).
fn draw_leaf(pm: &mut Pixmap, cx: f32, cy: f32, s: f32, color: [u8; 4]) {
    let mut pb = PathBuilder::new();
    let h = 7.0 * s;
    let w = 5.0 * s;
    pb.move_to(cx - w * 0.2, cy + h);
    pb.cubic_to(cx - w * 1.6, cy + h * 0.2, cx - w * 0.6, cy - h * 0.9, cx + w * 0.9, cy - h);
    pb.cubic_to(cx + w * 1.2, cy - h * 0.1, cx + w * 0.6, cy + h * 0.8, cx - w * 0.2, cy + h);
    pb.close();
    if let Some(p) = pb.finish() {
        pm.fill_path(&p, &paint(color), FillRule::Winding, Transform::identity(), None);
    }
    // vein
    let mut v = PathBuilder::new();
    v.move_to(cx - w * 0.2, cy + h);
    v.quad_to(cx + w * 0.1, cy, cx + w * 0.7, cy - h * 0.8);
    if let Some(p) = v.finish() {
        // cut the vein out of the leaf
        let mut bg = paint([0, 0, 0, 0]);
        bg.blend_mode = tiny_skia::BlendMode::Clear;
        let st = Stroke {
            width: 0.9 * s,
            ..Default::default()
        };
        pm.stroke_path(&p, &bg, &st, Transform::identity(), None);
    }
}

fn draw_battery(pm: &mut Pixmap, x: f32, cy: f32, s: f32, b: &Battery, color: [u8; 4]) {
    let w = 22.0 * s;
    let h = 11.0 * s;
    let y = cy - h / 2.0;
    let mut dim = color;
    dim[3] = 110;
    crate::draw::stroke_rounded(pm, x, y, w, h, 3.0 * s, 1.0 * s, dim);
    fill_rounded(pm, x + w + 1.0 * s, cy - 2.0 * s, 1.5 * s, 4.0 * s, 1.0 * s, dim);
    let fill_w = (w - 4.0 * s) * (b.percent as f32 / 100.0).clamp(0.05, 1.0);
    let c = if b.percent <= 15 && !b.charging { [255, 69, 58, 255] } else { color };
    fill_rounded(pm, x + 2.0 * s, y + 2.0 * s, fill_w, h - 4.0 * s, 1.5 * s, c);
}

/// macOS-style menu bar speaker icon: cone + 0–3 sound waves, or an `×` when muted.
fn draw_volume(pm: &mut Pixmap, cx: f32, cy: f32, s: f32, vol: Option<(f32, bool)>, color: [u8; 4]) {
    let (level, muted) = vol.unwrap_or((0.5, false));
    let x0 = cx - 7.2 * s;
    let mut pb = PathBuilder::new();
    pb.move_to(x0, cy - 2.1 * s);
    pb.line_to(x0 + 2.8 * s, cy - 2.1 * s);
    pb.line_to(x0 + 6.3 * s, cy - 5.2 * s);
    pb.line_to(x0 + 6.3 * s, cy + 5.2 * s);
    pb.line_to(x0 + 2.8 * s, cy + 2.1 * s);
    pb.line_to(x0, cy + 2.1 * s);
    pb.close();
    if let Some(p) = pb.finish() {
        pm.fill_path(&p, &paint(color), FillRule::Winding, Transform::identity(), None);
    }

    let st = Stroke {
        width: 1.35 * s,
        line_cap: tiny_skia::LineCap::Round,
        ..Default::default()
    };

    if muted {
        let (a, b) = (cx + 1.2 * s, cx + 5.8 * s);
        let (y0, y1) = (cy - 2.3 * s, cy + 2.3 * s);
        for (ya, yb) in [(y0, y1), (y1, y0)] {
            let mut pb = PathBuilder::new();
            pb.move_to(a, ya);
            pb.line_to(b, yb);
            if let Some(p) = pb.finish() {
                pm.stroke_path(&p, &paint(color), &st, Transform::identity(), None);
            }
        }
        return;
    }

    let waves = if level <= 0.001 {
        0
    } else if level < 0.34 {
        1
    } else if level < 0.67 {
        2
    } else {
        3
    };
    let wx = x0 + 6.8 * s;
    for (i, rad) in [2.6f32, 5.0, 7.4].iter().enumerate() {
        let mut c = color;
        if i >= waves {
            c[3] = 65;
        }
        let mut pb = PathBuilder::new();
        let n = 10;
        for k in 0..=n {
            let a = (-48.0 + 96.0 * k as f32 / n as f32).to_radians();
            let (px, py) = (wx + rad * s * a.cos(), cy + rad * s * a.sin());
            if k == 0 {
                pb.move_to(px, py);
            } else {
                pb.line_to(px, py);
            }
        }
        if let Some(p) = pb.finish() {
            pm.stroke_path(&p, &paint(c), &st, Transform::identity(), None);
        }
    }
}

pub(crate) fn draw_network(pm: &mut Pixmap, cx: f32, cy: f32, s: f32, n: Network, color: [u8; 4]) {
    match n {
        Network::Wifi(level) => {
            let base_y = cy + 5.0 * s;
            crate::draw::fill_circle(pm, cx, base_y, 1.6 * s, color);
            for (i, r) in [5.0f32, 9.0, 13.0].iter().enumerate() {
                let mut c = color;
                if (i as u8) >= level {
                    c[3] = 70;
                }
                let mut pb = PathBuilder::new();
                let a0 = -std::f32::consts::FRAC_PI_4 * 3.0;
                let steps = 12;
                for k in 0..=steps {
                    let a = a0 + (std::f32::consts::FRAC_PI_2) * k as f32 / steps as f32;
                    let (px, py) = (cx + r * s * a.cos(), base_y + r * s * a.sin());
                    if k == 0 {
                        pb.move_to(px, py);
                    } else {
                        pb.line_to(px, py);
                    }
                }
                if let Some(p) = pb.finish() {
                    let st = Stroke {
                        width: 1.8 * s,
                        line_cap: tiny_skia::LineCap::Round,
                        ..Default::default()
                    };
                    pm.stroke_path(&p, &paint(c), &st, Transform::identity(), None);
                }
            }
        }
        Network::Wired => {
            // RJ45 Ethernet port outline with top contact pins and bottom clip notch
            let st = Stroke {
                width: 1.35 * s,
                line_cap: tiny_skia::LineCap::Round,
                line_join: tiny_skia::LineJoin::Round,
                ..Default::default()
            };
            let mut pb = PathBuilder::new();
            pb.move_to(cx - 6.5 * s, cy - 5.0 * s);
            pb.line_to(cx + 6.5 * s, cy - 5.0 * s);
            pb.line_to(cx + 6.5 * s, cy + 3.0 * s);
            pb.line_to(cx + 3.0 * s, cy + 3.0 * s);
            pb.line_to(cx + 3.0 * s, cy + 5.3 * s);
            pb.line_to(cx - 3.0 * s, cy + 5.3 * s);
            pb.line_to(cx - 3.0 * s, cy + 3.0 * s);
            pb.line_to(cx - 6.5 * s, cy + 3.0 * s);
            pb.close();
            if let Some(p) = pb.finish() {
                pm.stroke_path(&p, &paint(color), &st, Transform::identity(), None);
            }
            // contact pins at the top of the jack
            let pin_st = Stroke {
                width: 1.15 * s,
                line_cap: tiny_skia::LineCap::Round,
                ..Default::default()
            };
            for dx in [-3.0f32, 0.0, 3.0] {
                let mut pb = PathBuilder::new();
                pb.move_to(cx + dx * s, cy - 4.8 * s);
                pb.line_to(cx + dx * s, cy - 2.0 * s);
                if let Some(p) = pb.finish() {
                    pm.stroke_path(&p, &paint(color), &pin_st, Transform::identity(), None);
                }
            }
        }
        Network::Offline => {
            // Dimmed Wi-Fi arcs with a diagonal slash (macOS disconnected style)
            let mut dim = color;
            dim[3] = 80;
            draw_network(pm, cx, cy, s, Network::Wifi(3), dim);
            let mut pb = PathBuilder::new();
            pb.move_to(cx - 6.5 * s, cy - 5.5 * s);
            pb.line_to(cx + 6.5 * s, cy + 5.5 * s);
            if let Some(p) = pb.finish() {
                let st = Stroke {
                    width: 1.45 * s,
                    line_cap: tiny_skia::LineCap::Round,
                    ..Default::default()
                };
                pm.stroke_path(&p, &paint(color), &st, Transform::identity(), None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn right_side_hit_order_matches_macos() {
        let fonts = Fonts::load();
        let mut bar = MenuBar::new();
        let width = 1440.0;
        let (ca, cb) = bar.clock_range(&fonts, width);
        let (cca, ccb) = bar.control_range(&fonts, width);
        let (va, vb) = bar.volume_range(&fonts, width).unwrap();
        let (na, nb) = bar.network_range(&fonts, width).unwrap();

        // Network | Volume | Control Center | Clock (left to right)
        assert!(nb < va && vb < cca && ccb < ca && ca < cb);
        assert_eq!(bar.hit(&fonts, (na + nb) / 2.0, width), Some(BarItem::Network));
        assert_eq!(bar.hit(&fonts, (va + vb) / 2.0, width), Some(BarItem::Volume));
        assert_eq!(bar.hit(&fonts, (cca + ccb) / 2.0, width), Some(BarItem::Control));
        assert_eq!(bar.hit(&fonts, (ca + cb) / 2.0, width), Some(BarItem::Clock));

        // Disabling Volume shifts Network right next to Control Center
        bar.cfg.show_volume = false;
        assert!(bar.volume_range(&fonts, width).is_none());
        let (na2, nb2) = bar.network_range(&fonts, width).unwrap();
        assert_eq!((na2, nb2), (va, vb));
        assert_eq!(bar.hit(&fonts, (na2 + nb2) / 2.0, width), Some(BarItem::Network));

        // Disabling Network leaves only Control Center and Clock
        bar.cfg.show_network = false;
        assert!(bar.network_range(&fonts, width).is_none());
        assert_eq!(bar.hit(&fonts, (na2 + nb2) / 2.0, width), None);
    }

    #[test]
    fn draws_volume_and_network_states_without_panic() {
        let mut pm = Pixmap::new(64, 48).unwrap();
        for vol in [None, Some((0.0, false)), Some((0.2, false)), Some((0.5, false)), Some((0.9, false)), Some((0.5, true))] {
            draw_volume(&mut pm, 32.0, 24.0, 2.0, vol, [255, 255, 255, 255]);
        }
        for net in [Network::Wired, Network::Wifi(1), Network::Wifi(2), Network::Wifi(3), Network::Offline] {
            draw_network(&mut pm, 32.0, 24.0, 2.0, net, [255, 255, 255, 255]);
        }
    }
}
