//! Layout and drawing of the lock screen. Everything is drawn onto a
//! transparent surface: the compositor puts the blurred wallpaper underneath.
//!
//! The screen is split into horizontal bands (clock, login, power buttons)
//! that are cleared and redrawn independently, so typing or the shake only
//! touches a thin strip of a 4K buffer.

use tiny_skia::{FillRule, LineCap, Paint, Path, PathBuilder, Pixmap, PixmapPaint, Stroke, Transform};

use crate::text::{Face, Shadow, TextCache};

pub const BAND_CLOCK: u8 = 1;
pub const BAND_LOGIN: u8 = 2;
pub const BAND_POWER: u8 = 4;
pub const BAND_ALL: u8 = 7;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    Submit,
    Sleep,
    Restart,
    ShutDown,
}

pub const POWER: [Hit; 3] = [Hit::Sleep, Hit::Restart, Hit::ShutDown];

/// Positions in logical px.
pub struct Layout {
    pub w: f32,
    pub date_px: f32,
    pub date_baseline: f32,
    pub clock_px: f32,
    pub clock_baseline: f32,
    pub clock_band: (f32, f32),
    pub avatar_d: f32,
    pub avatar_cy: f32,
    pub name_px: f32,
    pub name_baseline: f32,
    /// x, y, w, h
    pub field: (f32, f32, f32, f32),
    pub message_baseline: f32,
    pub login_band: (f32, f32),
    pub power_cy: f32,
    pub power_r: f32,
    pub power_gap: f32,
    pub power_band: (f32, f32),
}

impl Layout {
    pub fn new(w: f32, h: f32, text: &TextCache) -> Layout {
        let f = &text.fonts;
        let date_px = (h * 0.0165).clamp(17.0, 30.0);
        let clock_px = (h * 0.092).clamp(72.0, 190.0);
        let top = (h * 0.06).max(28.0);
        let date_baseline = top + f.cap_height(date_px, Face::Semibold);
        let clock_baseline = date_baseline + date_px * 0.45 + f.cap_height(clock_px, Face::Clock);
        let clock_band = (0.0, clock_baseline + clock_px * 0.3 + 24.0);

        let (fw, fh) = (196.0, 30.0);
        let field_cy = (h * 0.86).min(h - 150.0).max(clock_band.1 + 150.0);
        let field = ((w - fw) / 2.0, field_cy - fh / 2.0, fw, fh);
        let name_px = 15.0;
        let name_baseline = field.1 - 13.0;
        let avatar_d = 72.0;
        let avatar_cy = name_baseline - f.cap_height(name_px, Face::Semibold) - 12.0 - avatar_d / 2.0;
        let message_baseline = field.1 + fh + 24.0;
        let login_band = (avatar_cy - avatar_d / 2.0 - 16.0, message_baseline + 14.0);

        let power_r = 17.0;
        let power_cy = h - 70.0;
        let power_band = ((power_cy - power_r - 10.0).max(login_band.1), h);
        Layout {
            w,
            date_px,
            date_baseline,
            clock_px,
            clock_baseline,
            clock_band,
            avatar_d,
            avatar_cy,
            name_px,
            name_baseline,
            field,
            message_baseline,
            login_band,
            power_cy,
            power_r,
            power_gap: 96.0,
            power_band,
        }
    }

    pub fn submit_center(&self) -> (f32, f32) {
        let (x, y, w, h) = self.field;
        (x + w - h / 2.0, y + h / 2.0)
    }

    pub fn power_center(&self, i: usize) -> (f32, f32) {
        (self.w / 2.0 + (i as f32 - 1.0) * self.power_gap, self.power_cy)
    }

    pub fn hit(&self, x: f32, y: f32, primary: bool, can_submit: bool) -> Option<Hit> {
        if !primary {
            return None;
        }
        let (sx, sy) = self.submit_center();
        if can_submit && (x - sx).hypot(y - sy) <= 13.0 {
            return Some(Hit::Submit);
        }
        for (i, hit) in POWER.iter().enumerate() {
            let (cx, cy) = self.power_center(i);
            // the circle and its label
            if (x - cx).abs() <= 40.0 && y >= cy - self.power_r - 4.0 && y <= cy + self.power_r + 26.0 {
                return Some(*hit);
            }
        }
        None
    }

    /// Bands in logical px (top, bottom) for a set of band bits.
    pub fn bands(&self, bits: u8, primary: bool) -> Vec<(u8, (f32, f32))> {
        let mut v = Vec::new();
        if bits & BAND_CLOCK != 0 {
            v.push((BAND_CLOCK, self.clock_band));
        }
        if primary && bits & BAND_LOGIN != 0 {
            v.push((BAND_LOGIN, self.login_band));
        }
        if primary && bits & BAND_POWER != 0 {
            v.push((BAND_POWER, self.power_band));
        }
        v
    }
}

/// Everything that changes what the lock screen shows.
pub struct View<'a> {
    /// overall opacity (fade in at start, fade out on unlock)
    pub alpha: f32,
    pub date: &'a str,
    pub time: &'a str,
    pub name: &'a str,
    pub avatar: Option<&'a Pixmap>,
    pub placeholder: &'a str,
    pub power_labels: [&'a str; 3],
    pub chars: usize,
    pub caps: bool,
    pub busy: bool,
    /// seconds since the spinner started
    pub spin: f32,
    /// horizontal offset of the shaking field (logical px)
    pub shake: f32,
    pub hover: Option<Hit>,
    pub pressed: Option<Hit>,
    pub message: Option<&'a str>,
}

pub fn paint(c: [u8; 4], alpha: f32) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(c[0], c[1], c[2], (c[3] as f32 * alpha.clamp(0.0, 1.0)).round() as u8);
    p.anti_alias = true;
    p
}

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let k = 0.5523 * r;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

fn fill(pm: &mut Pixmap, path: Option<Path>, p: &Paint) {
    if let Some(path) = path {
        pm.fill_path(&path, p, FillRule::Winding, Transform::identity(), None);
    }
}

fn stroke(pm: &mut Pixmap, path: Option<Path>, p: &Paint, width: f32) {
    if let Some(path) = path {
        let st = Stroke { width, line_cap: LineCap::Round, ..Default::default() };
        pm.stroke_path(&path, p, &st, Transform::identity(), None);
    }
}

fn arc(cx: f32, cy: f32, r: f32, from_deg: f32, to_deg: f32) -> Option<Path> {
    let mut pb = PathBuilder::new();
    let n = 40;
    for i in 0..=n {
        let a = (from_deg + (to_deg - from_deg) * i as f32 / n as f32).to_radians();
        let (x, y) = (cx + r * a.cos(), cy + r * a.sin());
        if i == 0 {
            pb.move_to(x, y);
        } else {
            pb.line_to(x, y);
        }
    }
    pb.finish()
}

fn lines(points: &[(f32, f32)]) -> Option<Path> {
    let mut pb = PathBuilder::new();
    for (i, (x, y)) in points.iter().enumerate() {
        if i == 0 {
            pb.move_to(*x, *y);
        } else {
            pb.line_to(*x, *y);
        }
    }
    pb.finish()
}

const WHITE: [u8; 4] = [255, 255, 255, 255];

/// Clear one band of the canvas and draw what lies in it.
pub fn draw_band(pm: &mut Pixmap, text: &mut TextCache, l: &Layout, v: &View, band: u8, s: f32, rows: (usize, usize)) {
    let stride = pm.width() as usize * 4;
    pm.data_mut()[rows.0 * stride..rows.1 * stride].fill(0);
    match band {
        BAND_CLOCK => draw_clock(pm, text, l, v, s),
        BAND_LOGIN => draw_login(pm, text, l, v, s),
        BAND_POWER => draw_power(pm, text, l, v, s),
        _ => {}
    }
}

fn draw_clock(pm: &mut Pixmap, text: &mut TextCache, l: &Layout, v: &View, s: f32) {
    let date_px = l.date_px * s;
    let dw = text.fonts.measure(v.date, date_px, Face::Semibold);
    let shadow = Some(Shadow { radius: 5.0 * s, dy: 1.0 * s, alpha: 70 });
    text.draw(pm, v.date, (l.w * s - dw) / 2.0, l.date_baseline * s, date_px, Face::Semibold, [255, 255, 255, 235], shadow, v.alpha);
    let clock_px = l.clock_px * s;
    let tw = text.fonts.measure(v.time, clock_px, Face::Clock);
    let shadow = Some(Shadow { radius: 10.0 * s, dy: 2.0 * s, alpha: 55 });
    text.draw(pm, v.time, (l.w * s - tw) / 2.0, l.clock_baseline * s, clock_px, Face::Clock, [255, 255, 255, 240], shadow, v.alpha);
}

fn draw_login(pm: &mut Pixmap, text: &mut TextCache, l: &Layout, v: &View, s: f32) {
    let a = v.alpha;
    // avatar with a faint ring
    let (acx, acy, ar) = (l.w / 2.0 * s, l.avatar_cy * s, l.avatar_d / 2.0 * s);
    fill(pm, PathBuilder::from_circle(acx, acy + 1.5 * s, ar + 1.0 * s), &paint([0, 0, 0, 40], a));
    if let Some(av) = v.avatar {
        let paint = PixmapPaint { opacity: a, ..Default::default() };
        pm.draw_pixmap(
            (acx - av.width() as f32 / 2.0).round() as i32,
            (acy - av.height() as f32 / 2.0).round() as i32,
            av.as_ref(),
            &paint,
            Transform::identity(),
            None,
        );
    }
    stroke(pm, PathBuilder::from_circle(acx, acy, ar - 0.5 * s), &paint([255, 255, 255, 50], a), 1.0 * s);

    // name
    let name_px = l.name_px * s;
    let nw = text.fonts.measure(v.name, name_px, Face::Semibold);
    let shadow = Some(Shadow { radius: 4.0 * s, dy: 1.0 * s, alpha: 90 });
    text.draw(pm, v.name, (l.w * s - nw) / 2.0, l.name_baseline * s, name_px, Face::Semibold, WHITE, shadow, a);

    // password field (shakes on a wrong password)
    let (fx, fy, fw, fh) = l.field;
    let (x, y, w, h) = ((fx + v.shake) * s, fy * s, fw * s, fh * s);
    fill(pm, rounded_rect(x, y, w, h, h / 2.0), &paint([255, 255, 255, 52], a));
    stroke(pm, rounded_rect(x + 0.5 * s, y + 0.5 * s, w - s, h - s, h / 2.0), &paint([255, 255, 255, 48], a), 1.0 * s);
    let cy = y + h / 2.0;
    let right = x + w - h / 2.0;
    if v.chars == 0 {
        let px = 13.0 * s;
        let pw = text.fonts.measure(v.placeholder, px, Face::Regular);
        let (asc, desc) = text.fonts.line(px, Face::Regular);
        let base = (cy + (asc + desc) / 2.0).round();
        text.draw(pm, v.placeholder, x + (w - pw) / 2.0, base, px, Face::Regular, [255, 255, 255, 165], None, a);
    } else {
        let gap = 10.5 * s;
        let max = (((w - 14.0 * s - h) / gap).floor() as usize).max(1);
        let n = v.chars.min(max);
        for i in 0..n {
            fill(pm, PathBuilder::from_circle(x + 15.0 * s + i as f32 * gap, cy, 3.3 * s), &paint(WHITE, a));
        }
    }
    if v.busy {
        // spinner: 8 spokes, the bright one turning
        let step = (v.spin * 12.0) as i32;
        for k in 0..8 {
            let ang = (k as f32 * 45.0 - 90.0).to_radians();
            let fade = ((k - step).rem_euclid(8)) as f32 / 8.0;
            let (c, sn) = (ang.cos(), ang.sin());
            let p = lines(&[(right + c * 3.6 * s, cy + sn * 3.6 * s), (right + c * 7.6 * s, cy + sn * 7.6 * s)]);
            stroke(pm, p, &paint([255, 255, 255, (70.0 + 185.0 * (1.0 - fade)) as u8], a), 1.8 * s);
        }
    } else if v.chars > 0 {
        let hot = v.hover == Some(Hit::Submit);
        let r = 10.5 * s;
        if hot {
            fill(pm, PathBuilder::from_circle(right, cy, r), &paint([255, 255, 255, if v.pressed == Some(Hit::Submit) { 110 } else { 70 }], a));
        }
        stroke(pm, PathBuilder::from_circle(right, cy, r - 0.6 * s), &paint([255, 255, 255, 170], a), 1.3 * s);
        let p = lines(&[(right - 4.5 * s, cy), (right + 4.5 * s, cy)]);
        stroke(pm, p, &paint(WHITE, a), 1.6 * s);
        let p = lines(&[(right + 1.0 * s, cy - 3.8 * s), (right + 4.8 * s, cy), (right + 1.0 * s, cy + 3.8 * s)]);
        stroke(pm, p, &paint(WHITE, a), 1.6 * s);
    }
    if v.caps {
        let cx = if v.chars > 0 || v.busy { right - 24.0 * s } else { right };
        let u = s;
        let p = lines(&[
            (cx, cy - 6.5 * u),
            (cx + 5.5 * u, cy - 0.8 * u),
            (cx + 2.6 * u, cy - 0.8 * u),
            (cx + 2.6 * u, cy + 2.4 * u),
            (cx - 2.6 * u, cy + 2.4 * u),
            (cx - 2.6 * u, cy - 0.8 * u),
            (cx - 5.5 * u, cy - 0.8 * u),
            (cx, cy - 6.5 * u),
        ]);
        fill(pm, p, &paint([255, 255, 255, 200], a));
        fill(pm, rounded_rect(cx - 2.6 * u, cy + 3.8 * u, 5.2 * u, 1.9 * u, 0.5 * u), &paint([255, 255, 255, 200], a));
    }

    if let Some(msg) = v.message {
        let px = 12.0 * s;
        let mw = text.fonts.measure(msg, px, Face::Regular);
        let shadow = Some(Shadow { radius: 3.0 * s, dy: 1.0 * s, alpha: 90 });
        text.draw(pm, msg, (l.w * s - mw) / 2.0, l.message_baseline * s, px, Face::Regular, [255, 255, 255, 220], shadow, a);
    }
}

fn draw_power(pm: &mut Pixmap, text: &mut TextCache, l: &Layout, v: &View, s: f32) {
    let a = v.alpha;
    for (i, hit) in POWER.iter().enumerate() {
        let (cx, cy) = l.power_center(i);
        let (cx, cy, r) = (cx * s, cy * s, l.power_r * s);
        let hot = v.hover == Some(*hit);
        let bg = match (hot, v.pressed == Some(*hit)) {
            (_, true) => 120,
            (true, _) => 85,
            _ => 48,
        };
        fill(pm, PathBuilder::from_circle(cx, cy, r), &paint([255, 255, 255, bg], a));
        let ink = paint([255, 255, 255, 235], a);
        let w = 1.7 * s;
        match hit {
            Hit::Sleep => {
                // crescent moon: a disc with an offset disc cut out of it
                let d = (16.0 * s).ceil() as u32 + 2;
                if let Some(mut icon) = Pixmap::new(d, d) {
                    let c = d as f32 / 2.0;
                    fill(&mut icon, PathBuilder::from_circle(c, c, 7.0 * s), &paint(WHITE, 1.0));
                    let mut cut = paint(WHITE, 1.0);
                    cut.blend_mode = tiny_skia::BlendMode::Clear;
                    fill(&mut icon, PathBuilder::from_circle(c + 4.3 * s, c - 3.6 * s, 6.2 * s), &cut);
                    let pp = PixmapPaint { opacity: a * 0.92, ..Default::default() };
                    pm.draw_pixmap((cx - c).round() as i32, (cy - c).round() as i32, icon.as_ref(), &pp, Transform::identity(), None);
                }
            }
            Hit::Restart => {
                // clockwise arrow, open at the top, arrowhead at its end
                let r = 6.4 * s;
                let (from, to) = (300.0f32, 300.0 + 285.0);
                stroke(pm, arc(cx, cy, r, from, to), &ink, w);
                let t = to.to_radians();
                let (ex, ey) = (cx + r * t.cos(), cy + r * t.sin());
                let (tx, ty) = (-t.sin(), t.cos()); // direction of travel
                let (nx, ny) = (t.cos(), t.sin());
                let head = 3.4 * s;
                let p = lines(&[
                    (ex + tx * head * 1.1, ey + ty * head * 1.1),
                    (ex + nx * head - tx * head * 0.3, ey + ny * head - ty * head * 0.3),
                    (ex - nx * head - tx * head * 0.3, ey - ny * head - ty * head * 0.3),
                    (ex + tx * head * 1.1, ey + ty * head * 1.1),
                ]);
                fill(pm, p, &ink);
            }
            Hit::ShutDown => {
                stroke(pm, arc(cx, cy + 0.6 * s, 6.8 * s, -55.0, 235.0), &ink, w);
                stroke(pm, lines(&[(cx, cy - 8.2 * s), (cx, cy - 1.2 * s)]), &ink, w);
            }
            Hit::Submit => {}
        }
        let px = 12.0 * s;
        let label = v.power_labels[i];
        let lw = text.fonts.measure(label, px, Face::Regular);
        let shadow = Some(Shadow { radius: 3.0 * s, dy: 1.0 * s, alpha: 90 });
        text.draw(pm, label, cx - lw / 2.0, cy + r + 18.0 * s, px, Face::Regular, [255, 255, 255, if hot { 255 } else { 215 }], shadow, a);
    }
}
