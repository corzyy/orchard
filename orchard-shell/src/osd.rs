//! Volume / brightness HUD ("bezel") and the system commands behind the media keys.
//!
//! The compositor forwards XF86 keys as `media_key` IPC events; we run the
//! command on a helper thread (wpctl / brightnessctl / ddcutil can take a while),
//! read the new level back and show the classic rounded-square HUD with 16 steps.

use std::time::{Duration, Instant};

use tiny_skia::{FillRule, PathBuilder, Pixmap, Stroke, Transform};

use crate::{
    draw::{fill_rounded, paint},
    text::Rgba,
    theme::Theme,
};

pub const SIZE: u32 = 200;
/// distance of the HUD's bottom edge from the bottom of the screen
pub const BOTTOM_MARGIN: i32 = 140;
pub const SHOW_FOR: Duration = Duration::from_millis(1500);
const STEPS: usize = 16;

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
}

impl Osd {
    pub fn paint(&self, pm: &mut Pixmap, scale: f32, theme: &Theme) {
        let s = scale;
        let size = SIZE as f32;
        let dark = theme.bar_text[0] > 128;
        let bg: Rgba = if dark { [40, 40, 42, 200] } else { [236, 236, 236, 200] };
        let fg: Rgba = if dark { [235, 235, 235, 255] } else { [40, 40, 40, 255] };
        let dim: Rgba = [fg[0], fg[1], fg[2], 70];
        fill_rounded(pm, 0.0, 0.0, size * s, size * s, 22.0 * s, bg);

        let l = &self.level;
        let (cx, cy) = (size / 2.0 * s, 82.0 * s);
        let icon_color = if l.muted { dim } else { fg };
        match l.kind {
            Kind::Volume => speaker(pm, cx, cy, 34.0 * s, if l.muted { 0.0 } else { l.value }, l.muted, icon_color),
            Kind::Mic => mic(pm, cx, cy, 34.0 * s, l.muted, icon_color),
            Kind::Brightness => sun(pm, cx, cy, 34.0 * s, l.value, icon_color),
        }

        // 16 steps; partial steps are drawn partially (Shift+Option fine control)
        let (bx, by, bw, bh) = (20.0 * s, 160.0 * s, (size - 40.0) * s, 8.0 * s);
        let gap = 2.0 * s;
        let cell = (bw - gap * (STEPS as f32 - 1.0)) / STEPS as f32;
        let filled = if l.muted && l.kind != Kind::Brightness { 0.0 } else { l.value.clamp(0.0, 1.0) * STEPS as f32 };
        for i in 0..STEPS {
            let x = bx + i as f32 * (cell + gap);
            fill_rounded(pm, x, by, cell, bh, 1.0 * s, dim);
            let part = (filled - i as f32).clamp(0.0, 1.0);
            if part > 0.0 {
                fill_rounded(pm, x, by, cell * part, bh, 1.0 * s, fg);
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
        let st = stroke_width(r * 0.1);
        let (a, b) = (wx + r * 0.2, wx + r * 0.75);
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
    let st = stroke_width(r * 0.09);
    for i in 0..3 {
        let rad = r * (0.32 + 0.3 * i as f32);
        let mut wave = c;
        if i >= waves {
            wave[3] = 60;
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
    fill_rounded(pm, cx - r * 0.25, cy - r * 0.8, r * 0.5, r * 1.0, r * 0.25, c);
    let st = stroke_width(r * 0.09);
    let mut pb = PathBuilder::new();
    let n = 14;
    for k in 0..=n {
        let a = (180.0 * k as f32 / n as f32).to_radians();
        let (x, y) = (cx + r * 0.45 * a.cos(), cy - r * 0.05 + r * 0.45 * a.sin());
        if k == 0 {
            pb.move_to(x, y);
        } else {
            pb.line_to(x, y);
        }
    }
    pb.move_to(cx, cy + r * 0.4);
    pb.line_to(cx, cy + r * 0.7);
    if muted {
        pb.move_to(cx - r * 0.6, cy - r * 0.8);
        pb.line_to(cx + r * 0.6, cy + r * 0.7);
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
    let st = stroke_width(r * 0.1);
    let len = r * (0.18 + 0.2 * level.clamp(0.0, 1.0));
    let mut pb = PathBuilder::new();
    for i in 0..8 {
        let a = (i as f32 * 45.0).to_radians();
        let (s, e) = (r * 0.52, r * 0.52 + len);
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
                // external monitors: DDC/CI (needs the i2c-dev module and access to /dev/i2c-*)
                let amount = step.round().max(1.0) as i32;
                sh(&format!("ddcutil --noverify setvcp 10 {sign} {amount}"))?;
                parse_ddcutil(&sh("ddcutil getvcp 10 --brief")?)?
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
}
