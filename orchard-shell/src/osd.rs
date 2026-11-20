//! Volume / brightness HUD ("bezel") and the system commands behind the media keys.
//!
//! Styled after macOS 27 Golden Gate: a compact, top-right frosted-glass capsule
//! tucked beneath the menu bar with a single prominent glyph on the left and a
//! 16-segment pill bar (supporting quarter-step fine adjustments via Shift+Alt),
//! paired with smooth spring-like enter/exit and level glide animations.

use std::time::{Duration, Instant};

use tiny_skia::{
    BlendMode, Color, FillRule, GradientStop, LinearGradient, Paint, PathBuilder, Pixmap, Point,
    SpreadMode, Stroke, Transform,
};

use crate::{
    draw::{fill_rounded, paint, rounded_rect},
    text::Rgba,
    theme::Theme,
};

/// Logical pill dimensions (macOS Golden Gate capsule).
pub const PILL_W: f32 = 248.0;
pub const PILL_H: f32 = 46.0;
pub const PILL_R: f32 = PILL_H * 0.5;

/// Transparent canvas padding around the resting pill:
/// provides the screen/menu-bar offset, vertical slide travel, and room for the soft drop shadow.
pub const PAD_LEFT: f32 = 14.0;
pub const PAD_RIGHT: f32 = 12.0;
pub const PAD_TOP: f32 = 10.0;
pub const PAD_BOTTOM: f32 = 16.0;

pub const SURF_W: u32 = (PILL_W + PAD_LEFT + PAD_RIGHT) as u32;
pub const SURF_H: u32 = (PILL_H + PAD_TOP + PAD_BOTTOM) as u32;

pub const SHOW_FOR: Duration = Duration::from_millis(1400);
const STEPS: usize = 16;

const ENTER_Y: f32 = -10.0;
const EXIT_Y: f32 = -6.0;
const EPS: f32 = 0.004;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Volume,
    Mic,
    Brightness,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Level {
    pub kind: Kind,
    /// 0.0 ..= 1.0
    pub value: f32,
    pub muted: bool,
}

pub struct Osd {
    pub level: Level,
    pub until: Instant,
    /// Smoothed displayed level (0.0..=1.0) for fluid bar transitions.
    display_value: f32,
    /// Current surface/backdrop opacity (0.0..=1.0).
    opacity: f32,
    /// Vertical offset in logical px relative to resting position.
    y_offset: f32,
    /// Timestamp of the last animation step.
    last_step: Instant,
    /// True once the hold window has elapsed and the pill is fading out.
    pub closing: bool,
    /// Last `(y, alpha)` sent to compositor backdrop IPC to avoid redundant requests.
    last_backdrop: Option<(f32, f32)>,
}

impl Osd {
    pub fn new(level: Level) -> Osd {
        let now = Instant::now();
        let start_val = level.value.clamp(0.0, 1.0);
        Osd {
            level,
            until: now + SHOW_FOR,
            display_value: start_val,
            opacity: 0.0,
            y_offset: ENTER_Y,
            last_step: now,
            closing: false,
            last_backdrop: None,
        }
    }

    /// Update the level while the pill is already visible without replaying the entrance.
    pub fn update(&mut self, level: Level) {
        let now = Instant::now();
        let was_idle = !self.is_animating();
        if self.level.kind != level.kind {
            self.display_value = level.value.clamp(0.0, 1.0);
        }
        self.level = level;
        self.until = now + SHOW_FOR;
        if self.closing || was_idle {
            self.closing = false;
            self.last_step = now;
        }
    }

    /// Start the exit fade-out animation.
    pub fn begin_close(&mut self) {
        if !self.closing {
            self.closing = true;
            self.last_step = Instant::now();
        }
    }

    /// Advance enter/exit and level smoothing animations.
    pub fn step(&mut self, now: Instant) {
        let dt = now.duration_since(self.last_step).as_secs_f32().clamp(0.001, 0.05);
        self.last_step = now;
        if now >= self.until {
            self.closing = true;
        }

        let (target_alpha, target_y, k_vis) = if self.closing {
            (0.0, EXIT_Y, 16.0)
        } else {
            (1.0, 0.0, 18.0)
        };
        let f_vis = 1.0 - (-k_vis * dt).exp();
        self.opacity += (target_alpha - self.opacity) * f_vis;
        self.y_offset += (target_y - self.y_offset) * f_vis;
        if (self.opacity - target_alpha).abs() < EPS {
            self.opacity = target_alpha;
        }
        if (self.y_offset - target_y).abs() < 0.08 {
            self.y_offset = target_y;
        }

        let target_val = self.level.value.clamp(0.0, 1.0);
        let f_val = 1.0 - (-26.0 * dt).exp();
        self.display_value += (target_val - self.display_value) * f_val;
        if (self.display_value - target_val).abs() < 0.002 {
            self.display_value = target_val;
        }
    }

    pub fn is_animating(&self) -> bool {
        let (target_alpha, target_y) = if self.closing { (0.0, EXIT_Y) } else { (1.0, 0.0) };
        let target_val = self.level.value.clamp(0.0, 1.0);
        (self.opacity - target_alpha).abs() >= EPS
            || (self.y_offset - target_y).abs() >= 0.08
            || (self.display_value - target_val).abs() >= 0.002
    }

    pub fn is_closed(&self) -> bool {
        self.closing && self.opacity <= EPS
    }

    /// Current frosted-glass backdrop geometry `(x, y, w, h, radius, alpha)` in logical surface coords.
    pub fn backdrop_rect(&self) -> (f32, f32, f32, f32, f32, f32) {
        (PAD_LEFT, PAD_TOP + self.y_offset, PILL_W, PILL_H, PILL_R, self.opacity.clamp(0.0, 1.0))
    }

    /// Returns a new backdrop rect only when its position or opacity changed meaningfully.
    pub fn take_backdrop_update(&mut self) -> Option<(f32, f32, f32, f32, f32, f32)> {
        let rect = self.backdrop_rect();
        let (_, y, _, _, _, a) = rect;
        let changed = match self.last_backdrop {
            None => true,
            Some((py, pa)) => (y - py).abs() > 0.1 || (a - pa).abs() > 0.005 || ((a == 1.0) != (pa == 1.0)),
        };
        if changed {
            self.last_backdrop = Some((y, a));
            Some(rect)
        } else {
            None
        }
    }

    pub fn paint(&self, pm: &mut Pixmap, scale: f32, theme: &Theme) {
        let op = self.opacity.clamp(0.0, 1.0);
        if op <= 0.001 {
            return;
        }
        let s = scale;
        let px = PAD_LEFT;
        let py = PAD_TOP + self.y_offset;
        let dark = theme.bar_text[0] > 128;
        let solid = theme.menu_bg[3] == 255;

        let alpha = |a: u8| -> u8 { (a as f32 * op).round().clamp(0.0, 255.0) as u8 };
        let rgba = |r: u8, g: u8, b: u8, a: u8| -> Rgba { [r, g, b, alpha(a)] };

        // 1. Soft outer drop shadow, then punch out the capsule interior so the translucent
        //    frosted-glass body stays crystal-clear without shadow darkening.
        let shadow_layers: [(f32, f32, u8); 2] = if dark {
            [(4.5, 4.0, 24), (2.0, 1.5, 34)]
        } else {
            [(4.5, 4.0, 16), (2.0, 1.5, 22)]
        };
        for (dy, spread, a) in shadow_layers {
            fill_rounded(
                pm,
                (px - spread) * s,
                (py + dy - spread * 0.5) * s,
                (PILL_W + spread * 2.0) * s,
                (PILL_H + spread * 1.5) * s,
                (PILL_R + spread) * s,
                rgba(0, 0, 0, a),
            );
        }

        let Some(pill_path) = rounded_rect(px * s, py * s, PILL_W * s, PILL_H * s, PILL_R * s) else {
            return;
        };
        let mut clear = Paint {
            blend_mode: BlendMode::Clear,
            anti_alias: true,
            ..Default::default()
        };
        clear.set_color(Color::TRANSPARENT);
        pm.fill_path(&pill_path, &clear, FillRule::Winding, Transform::identity(), None);

        // 2. Frosted-glass tint (adapts to Light/Dark and Reduce Transparency).
        let base_a = if solid {
            255
        } else {
            // Keep slightly more transparent than menus so the compositor's blur shines through.
            (theme.menu_bg[3] as f32 * 0.84).round().clamp(28.0, 225.0) as u8
        };
        let bg = if dark {
            rgba(32, 32, 36, base_a)
        } else {
            rgba(248, 248, 250, base_a)
        };
        pm.fill_path(&pill_path, &paint(bg), FillRule::Winding, Transform::identity(), None);

        // 3. Top-lit domed glass sheen inside the capsule.
        if !solid {
            let sheen_top = if dark { alpha(22) } else { alpha(52) };
            let sheen_mid = if dark { alpha(4) } else { alpha(10) };
            if let Some(shader) = LinearGradient::new(
                Point::from_xy(px * s, py * s),
                Point::from_xy(px * s, (py + PILL_H) * s),
                vec![
                    GradientStop::new(0.0, Color::from_rgba8(255, 255, 255, sheen_top)),
                    GradientStop::new(0.42, Color::from_rgba8(255, 255, 255, sheen_mid)),
                    GradientStop::new(1.0, Color::from_rgba8(255, 255, 255, 0)),
                ],
                SpreadMode::Pad,
                Transform::identity(),
            ) {
                let mut sp = Paint::default();
                sp.shader = shader;
                sp.anti_alias = true;
                pm.fill_path(&pill_path, &sp, FillRule::Winding, Transform::identity(), None);
            }
        }

        // 4. Specular glass bezel rim (subtle dark outer separation + luminous top-lit inner hairline).
        let outer_stroke = Stroke {
            width: 0.75 * s,
            ..Default::default()
        };
        let outer_col = if dark { rgba(0, 0, 0, 58) } else { rgba(0, 0, 0, 26) };
        pm.stroke_path(&pill_path, &paint(outer_col), &outer_stroke, Transform::identity(), None);

        let inset = 0.75;
        if let Some(inner_path) = rounded_rect(
            (px + inset) * s,
            (py + inset) * s,
            (PILL_W - inset * 2.0) * s,
            (PILL_H - inset * 2.0) * s,
            (PILL_R - inset) * s,
        ) {
            let (top_rim, bot_rim) = if dark {
                (alpha(92), alpha(34))
            } else {
                (alpha(185), alpha(70))
            };
            if let Some(shader) = LinearGradient::new(
                Point::from_xy(px * s, py * s),
                Point::from_xy(px * s, (py + PILL_H) * s),
                vec![
                    GradientStop::new(0.0, Color::from_rgba8(255, 255, 255, top_rim)),
                    GradientStop::new(0.55, Color::from_rgba8(255, 255, 255, bot_rim)),
                    GradientStop::new(1.0, Color::from_rgba8(255, 255, 255, top_rim / 2)),
                ],
                SpreadMode::Pad,
                Transform::identity(),
            ) {
                let mut rim_p = Paint::default();
                rim_p.shader = shader;
                rim_p.anti_alias = true;
                let inner_stroke = Stroke {
                    width: 1.0 * s,
                    ..Default::default()
                };
                pm.stroke_path(&inner_path, &rim_p, &inner_stroke, Transform::identity(), None);
            }
        }

        // 5. Left glyph (macOS 27.2 Golden Gate single prominent icon on the left).
        let l = &self.level;
        let fg = if dark {
            rgba(255, 255, 255, 244)
        } else {
            rgba(26, 26, 30, 236)
        };
        let muted_fg = if dark {
            rgba(255, 255, 255, 130)
        } else {
            rgba(26, 26, 30, 120)
        };
        let seg_fill = if dark {
            rgba(255, 255, 255, 238)
        } else {
            rgba(28, 28, 32, 224)
        };
        let seg_track = if dark {
            rgba(255, 255, 255, 52)
        } else {
            rgba(0, 0, 0, 36)
        };

        let is_muted = l.muted && l.kind != Kind::Brightness;
        let icon_color = if is_muted { muted_fg } else { fg };
        let (ix, iy) = ((px + 24.0) * s, (py + PILL_H * 0.5) * s);
        let val = self.display_value.clamp(0.0, 1.0);
        match l.kind {
            Kind::Volume => speaker(pm, ix, iy, 9.5 * s, if is_muted { 0.0 } else { val }, is_muted, icon_color),
            Kind::Mic => mic(pm, ix, iy, 9.5 * s, is_muted, icon_color),
            Kind::Brightness => sun(pm, ix, iy, 9.5 * s, val, icon_color),
        }

        // 6. 16-segment pill bar (macOS 27.2 Golden Gate segmented HUD).
        let bar_x = px + 44.0;
        let bar_w = PILL_W - 44.0 - 18.0;
        let bar_h = 7.0;
        let bar_y = py + (PILL_H - bar_h) * 0.5;
        let gap = 2.5;
        let cell_w = (bar_w - gap * (STEPS as f32 - 1.0)) / STEPS as f32;
        let seg_r = 2.5;
        let filled = if is_muted { 0.0 } else { val * STEPS as f32 };

        for i in 0..STEPS {
            let x = bar_x + i as f32 * (cell_w + gap);
            fill_rounded(pm, x * s, bar_y * s, cell_w * s, bar_h * s, seg_r * s, seg_track);
            let part = (filled - i as f32).clamp(0.0, 1.0);
            if part > 0.0 {
                let fw = cell_w * part;
                let fr = seg_r.min(fw * 0.5);
                fill_rounded(pm, x * s, bar_y * s, fw * s, bar_h * s, fr * s, seg_fill);
            }
        }
    }
}

fn stroke_width(w: f32) -> Stroke {
    Stroke {
        width: w,
        line_cap: tiny_skia::LineCap::Round,
        ..Default::default()
    }
}

/// Speaker cone plus 0–3 sound waves (or a cross when muted).
pub(crate) fn speaker(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, level: f32, muted: bool, c: Rgba) {
    let p = paint(c);
    let x0 = cx - r * 0.95;
    let mut pb = PathBuilder::new();
    pb.move_to(x0, cy - r * 0.28);
    pb.line_to(x0 + r * 0.38, cy - r * 0.28);
    pb.line_to(x0 + r * 0.85, cy - r * 0.68);
    pb.line_to(x0 + r * 0.85, cy + r * 0.68);
    pb.line_to(x0 + r * 0.38, cy + r * 0.28);
    pb.line_to(x0, cy + r * 0.28);
    pb.close();
    if let Some(path) = pb.finish() {
        pm.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
    }
    let wx = x0 + r * 0.95;
    if muted {
        let st = stroke_width(r * 0.13);
        let (a, b) = (wx + r * 0.18, wx + r * 0.74);
        for (y0, y1) in [(cy - r * 0.28, cy + r * 0.28), (cy + r * 0.28, cy - r * 0.28)] {
            let mut pb = PathBuilder::new();
            pb.move_to(a, y0);
            pb.line_to(b, y1);
            if let Some(path) = pb.finish() {
                pm.stroke_path(&path, &p, &st, Transform::identity(), None);
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
    let st = stroke_width(r * 0.11);
    for i in 0..3 {
        let rad = r * (0.30 + 0.28 * i as f32);
        let mut wave = c;
        if i >= waves {
            wave[3] = ((c[3] as u32 * 55) / 255) as u8;
        }
        // arc from -50° to 50° around (wx, cy)
        let mut pb = PathBuilder::new();
        let n = 12;
        for k in 0..=n {
            let a = (-50.0 + 100.0 * k as f32 / n as f32).to_radians();
            let (x, y) = (wx + rad * a.cos(), cy + rad * a.sin());
            if k == 0 {
                pb.move_to(x, y);
            } else {
                pb.line_to(x, y);
            }
        }
        if let Some(path) = pb.finish() {
            pm.stroke_path(&path, &paint(wave), &st, Transform::identity(), None);
        }
    }
}

fn mic(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, muted: bool, c: Rgba) {
    let p = paint(c);
    fill_rounded(pm, cx - r * 0.26, cy - r * 0.80, r * 0.52, r * 0.96, r * 0.26, c);
    let st = stroke_width(r * 0.12);
    let mut pb = PathBuilder::new();
    let n = 14;
    for k in 0..=n {
        let a = (180.0 * k as f32 / n as f32).to_radians();
        let (x, y) = (cx + r * 0.48 * a.cos(), cy - r * 0.04 + r * 0.48 * a.sin());
        if k == 0 {
            pb.move_to(x, y);
        } else {
            pb.line_to(x, y);
        }
    }
    pb.move_to(cx, cy + r * 0.44);
    pb.line_to(cx, cy + r * 0.76);
    pb.move_to(cx - r * 0.28, cy + r * 0.76);
    pb.line_to(cx + r * 0.28, cy + r * 0.76);
    if muted {
        pb.move_to(cx - r * 0.65, cy - r * 0.78);
        pb.line_to(cx + r * 0.65, cy + r * 0.74);
    }
    if let Some(path) = pb.finish() {
        pm.stroke_path(&path, &p, &st, Transform::identity(), None);
    }
}

pub(crate) fn sun(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, level: f32, c: Rgba) {
    let p = paint(c);
    if let Some(path) = PathBuilder::from_circle(cx, cy, r * 0.34) {
        pm.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
    }
    let st = stroke_width(r * 0.13);
    let len = r * (0.18 + 0.22 * level.clamp(0.0, 1.0));
    let mut pb = PathBuilder::new();
    for i in 0..8 {
        let a = (i as f32 * 45.0).to_radians();
        let (s, e) = (r * 0.54, r * 0.54 + len);
        pb.move_to(cx + s * a.cos(), cy + s * a.sin());
        pb.line_to(cx + e * a.cos(), cy + e * a.sin());
    }
    if let Some(path) = pb.finish() {
        pm.stroke_path(&path, &p, &st, Transform::identity(), None);
    }
}

// ---- system commands (run on a helper thread) ---------------------------

fn sh(cmd: &str) -> Option<String> {
    let out = std::process::Command::new("sh").arg("-c").arg(cmd).output().ok()?;
    if !out.status.success() {
        tracing::debug!("{cmd:?} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// "Volume: 0.40 [MUTED]" -> (0.40, true)
pub fn parse_wpctl(s: &str) -> Option<(f32, bool)> {
    let rest = s.trim().strip_prefix("Volume:")?.trim();
    let num = rest.split_whitespace().next()?.parse::<f32>().ok()?;
    Some((num, rest.contains("[MUTED]")))
}

fn wp_level(kind: Kind, node: &str) -> Option<Level> {
    let (value, muted) = parse_wpctl(&sh(&format!("wpctl get-volume {node}"))?)?;
    Some(Level { kind, value, muted })
}

fn has_backlight() -> bool {
    std::fs::read_dir("/sys/class/backlight").map(|mut d| d.next().is_some()).unwrap_or(false)
}

/// "intel_backlight,backlight,400,40%,1000" -> 0.40
pub fn parse_brightnessctl(s: &str) -> Option<f32> {
    let pct = s.trim().split(',').nth(3)?.trim_end_matches('%');
    Some(pct.parse::<f32>().ok()? / 100.0)
}

/// "VCP 10 C 50 100" -> 0.5
pub fn parse_ddcutil(s: &str) -> Option<f32> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 5 || parts[0] != "VCP" {
        return None;
    }
    let cur: f32 = parts[3].parse().ok()?;
    let max: f32 = parts[4].parse().ok()?;
    (max > 0.0).then(|| cur / max)
}

/// Run a media key. Returns the level to show, if any.
pub fn run_key(key: &str, fine: bool) -> Option<Level> {
    let step = if fine { 100.0 / 64.0 } else { 100.0 / STEPS as f32 };
    let sink = "@DEFAULT_AUDIO_SINK@";
    match key {
        "volume_up" | "volume_down" => {
            let sign = if key == "volume_up" { '+' } else { '-' };
            // like macOS, changing the volume unmutes
            sh(&format!("wpctl set-mute {sink} 0; wpctl set-volume -l 1.0 {sink} {step:.4}%{sign}"));
            wp_level(Kind::Volume, sink)
        }
        "volume_mute" => {
            sh(&format!("wpctl set-mute {sink} toggle"));
            wp_level(Kind::Volume, sink)
        }
        "mic_mute" => {
            let src = "@DEFAULT_AUDIO_SOURCE@";
            sh(&format!("wpctl set-mute {src} toggle"));
            wp_level(Kind::Mic, src)
        }
        "brightness_up" | "brightness_down" => {
            let sign = if key == "brightness_up" { '+' } else { '-' };
            let value = if has_backlight() {
                let dir = if sign == '+' { format!("{step:.2}%+") } else { format!("{step:.2}%-") };
                sh(&format!("brightnessctl -q set {dir}"));
                parse_brightnessctl(&sh("brightnessctl -m")?)?
            } else {
                // external monitors: DDC/CI across all detected displays
                let amount = step.round().max(1.0) as i32;
                crate::ddc::adjust_all_ddc(sign, amount)?
            };
            Some(Level {
                kind: Kind::Brightness,
                value,
                muted: false,
            })
        }
        "play_pause" => {
            sh("playerctl play-pause");
            None
        }
        "next" => {
            sh("playerctl next");
            None
        }
        "previous" => {
            sh("playerctl previous");
            None
        }
        "stop" => {
            sh("playerctl stop");
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Appearance;

    #[test]
    fn parses_wpctl() {
        assert_eq!(parse_wpctl("Volume: 0.40\n"), Some((0.40, false)));
        assert_eq!(parse_wpctl("Volume: 1.00 [MUTED]\n"), Some((1.0, true)));
        assert_eq!(parse_wpctl("garbage"), None);
    }

    #[test]
    fn parses_brightness() {
        assert_eq!(parse_brightnessctl("intel_backlight,backlight,400,40%,1000"), Some(0.40));
        assert_eq!(parse_ddcutil("VCP 10 C 50 100"), Some(0.5));
        assert_eq!(parse_ddcutil("VCP 10 ERR"), None);
    }

    #[test]
    fn golden_gate_osd_lifecycle() {
        let mut osd = Osd::new(Level {
            kind: Kind::Volume,
            value: 0.5,
            muted: false,
        });
        assert!(osd.is_animating());
        assert!(!osd.is_closed());

        // Step through entrance until settled
        let mut now = osd.last_step;
        for _ in 0..40 {
            now += Duration::from_millis(16);
            osd.step(now);
        }
        assert!(!osd.is_animating(), "should settle to idle after entrance");
        let (x, y, w, h, r, a) = osd.backdrop_rect();
        assert_eq!((x, y, w, h, r, a), (PAD_LEFT, PAD_TOP, PILL_W, PILL_H, PILL_R, 1.0));

        // Update level smoothly while open (does not restart entrance)
        osd.update(Level {
            kind: Kind::Volume,
            value: 0.75,
            muted: false,
        });
        assert!(osd.is_animating());
        assert_eq!(osd.opacity, 1.0);

        // Switching kind snaps display value immediately
        osd.update(Level {
            kind: Kind::Brightness,
            value: 0.25,
            muted: false,
        });
        assert_eq!(osd.display_value, 0.25);

        // Closing fades smoothly to closed
        osd.begin_close();
        now = osd.last_step;
        for _ in 0..40 {
            now += Duration::from_millis(16);
            osd.step(now);
        }
        assert!(osd.is_closed());
        assert!(!osd.is_animating());
    }

    #[test]
    fn paints_cleanly_in_light_and_dark_modes() {
        for app in [Appearance::Light, Appearance::Dark] {
            let theme = Theme::new(app);
            for kind in [Kind::Volume, Kind::Mic, Kind::Brightness] {
                for muted in [false, true] {
                    let mut osd = Osd::new(Level {
                        kind,
                        value: 0.625,
                        muted,
                    });
                    osd.opacity = 1.0;
                    osd.y_offset = 0.0;
                    let mut pm = Pixmap::new(SURF_W * 2, SURF_H * 2).unwrap();
                    osd.paint(&mut pm, 2.0, &theme);
                    assert!(pm.data().chunks_exact(4).any(|px| px[3] > 0));
                }
            }
        }
    }
}
