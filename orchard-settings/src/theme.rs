//! Palette, typography (fontdue), and low-level tiny-skia drawing helpers.

use std::{cell::RefCell, collections::HashMap, path::Path, rc::Rc};

use fontdue::{Font, FontSettings, Metrics};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};

pub type Rgba = [u8; 4];

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    pub sidebar: Rgba,
    pub content: Rgba,
    pub group: Rgba,
    pub group_border: Rgba,
    pub separator: Rgba,
    pub text: Rgba,
    pub text2: Rgba,
    pub text3: Rgba,
    pub accent: Rgba,
    pub control: Rgba,
    pub control_border: Rgba,
    pub toggle_off: Rgba,
    pub knob: Rgba,
    pub hover: Rgba,
    pub field: Rgba,
    pub menu: Rgba,
    pub danger: Rgba,
}

impl Palette {
    /// macOS System Settings–style colours: grouped forms are a faint tint over
    /// the window background (darker in light mode, lighter in dark mode).
    pub fn new(dark: bool, accent_rgb: [u8; 3]) -> Self {
        let accent = [accent_rgb[0], accent_rgb[1], accent_rgb[2], 255];
        if dark {
            Self {
                dark,
                sidebar: [0x2C, 0x2B, 0x2E, 255],
                content: [0x1F, 0x1F, 0x21, 255],
                group: [255, 255, 255, 12],
                group_border: [255, 255, 255, 16],
                separator: [255, 255, 255, 20],
                text: [0xEC, 0xEC, 0xEE, 255],
                text2: [0x9C, 0x9C, 0xA2, 255],
                text3: [0x68, 0x68, 0x6E, 255],
                accent,
                control: [0x5A, 0x5A, 0x5E, 255],
                control_border: [255, 255, 255, 22],
                toggle_off: [255, 255, 255, 40],
                knob: [0xEE, 0xEE, 0xF0, 255],
                hover: [255, 255, 255, 18],
                field: [255, 255, 255, 14],
                menu: [0x2E, 0x2D, 0x31, 255],
                danger: [0xFF, 0x45, 0x3A, 255],
            }
        } else {
            Self {
                dark,
                sidebar: [0xE6, 0xE5, 0xE8, 255],
                content: [0xF1, 0xF1, 0xF2, 255],
                group: [0, 0, 0, 10],
                group_border: [0, 0, 0, 14],
                separator: [0, 0, 0, 20],
                text: [0x1D, 0x1D, 0x1F, 255],
                text2: [0x7A, 0x7A, 0x80, 255],
                text3: [0xAA, 0xAA, 0xB0, 255],
                accent,
                control: [0xFF, 0xFF, 0xFF, 255],
                control_border: [0, 0, 0, 34],
                toggle_off: [0, 0, 0, 30],
                knob: [0xFF, 0xFF, 0xFF, 255],
                hover: [0, 0, 0, 14],
                field: [0, 0, 0, 12],
                menu: [0xF6, 0xF6, 0xF7, 255],
                danger: [0xE0, 0x38, 0x3E, 255],
            }
        }
    }
}

pub fn resolve_dark(mode: orchard_config::ThemeMode) -> bool {
    match mode {
        orchard_config::ThemeMode::Dark => true,
        orchard_config::ThemeMode::Light => false,
        orchard_config::ThemeMode::Auto => crate::sys::output(
            "gsettings",
            &["get", "org.gnome.desktop.interface", "color-scheme"],
        )
        .map(|s| s.contains("dark"))
        .unwrap_or(false),
    }
}

pub fn rgb(c: [u8; 3]) -> Rgba {
    [c[0], c[1], c[2], 255]
}

pub fn with_alpha(c: Rgba, a: u8) -> Rgba {
    [c[0], c[1], c[2], a]
}

pub fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    [lerp(a[0], b[0]), lerp(a[1], b[1]), lerp(a[2], b[2]), lerp(a[3], b[3])]
}

pub fn skia_color(c: Rgba) -> Color {
    Color::from_rgba8(c[0], c[1], c[2], c[3])
}

// ---------------------------------------------------------------- Fonts

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Weight {
    Regular,
    Bold,
}

pub struct Fonts {
    regular: Font,
    bold: Font,
    cache: RefCell<HashMap<(Weight, char, u32), Rc<(Metrics, Vec<u8>)>>>,
}

fn load_font_file(path: &Path, index: u32) -> Option<Font> {
    let data = std::fs::read(path).ok()?;
    Font::from_bytes(
        data,
        FontSettings {
            collection_index: index,
            scale: 40.0,
            ..Default::default()
        },
    )
    .ok()
}

fn fc_match(pattern: &str) -> Option<(String, u32)> {
    let out = std::process::Command::new("fc-match")
        .args(["-f", "%{file}\n%{index}", pattern])
        .output()
        .ok()?;
    let s = String::from_utf8(out.stdout).ok()?;
    let mut lines = s.lines();
    let file = lines.next()?.trim().to_string();
    let idx = lines.next().and_then(|l| l.trim().parse().ok()).unwrap_or(0);
    Some((file, idx))
}

/// First matching file among `paths` (no subprocess).
fn first_existing(paths: &[String]) -> Option<Font> {
    paths
        .iter()
        .map(Path::new)
        .filter(|p| p.is_file())
        .find_map(|p| load_font_file(p, 0))
}

/// `fc-match`, accepted only when the resolved file name contains `needle`
/// (fontconfig always falls back to *something*, e.g. a variable Cantarell).
fn fc_family(pattern: &str, needle: &str) -> Option<Font> {
    let (f, i) = fc_match(pattern)?;
    if !f.to_lowercase().contains(needle) {
        return None;
    }
    load_font_file(Path::new(&f), i)
}

impl Fonts {
    /// UI fonts like the rest of Orchard: SF Pro Text when installed, else
    /// Inter, else the fontconfig sans-serif. Known paths are tried first so
    /// startup normally spawns no `fc-match` at all.
    pub fn load() -> Self {
        let home = std::env::var("HOME").unwrap_or_default();
        let candidates = |sf: &str| -> Vec<String> {
            vec![
                format!("{home}/.local/share/fonts/SF-Pro/SF-Pro-Text-{sf}.otf"),
                format!("{home}/.local/share/fonts/SF-Pro-Text-{sf}.otf"),
                format!("{home}/.fonts/SF-Pro-Text-{sf}.otf"),
                format!("/usr/share/fonts/SF-Pro/SF-Pro-Text-{sf}.otf"),
                format!("/usr/local/share/fonts/SF-Pro-Text-{sf}.otf"),
            ]
        };

        let regular = first_existing(&candidates("Regular"))
            .or_else(|| fc_family("SF Pro Text:style=Regular", "sf-pro"))
            .or_else(|| fc_family("Inter:style=Regular", "inter"))
            .or_else(|| {
                let (f, i) = fc_match("sans-serif:style=Regular")?;
                load_font_file(Path::new(&f), i)
            })
            .expect("no usable UI font found");

        let bold = first_existing(&candidates("Semibold"))
            .or_else(|| fc_family("SF Pro Text:style=Semibold", "sf-pro"))
            .or_else(|| fc_family("Inter:style=SemiBold", "inter"))
            .or_else(|| {
                let (f, i) = fc_match("sans-serif:style=Bold")?;
                load_font_file(Path::new(&f), i)
            })
            .unwrap_or_else(|| regular.clone());

        Self {
            regular,
            bold,
            cache: RefCell::new(HashMap::with_capacity(512)),
        }
    }

    fn font(&self, w: Weight) -> &Font {
        match w {
            Weight::Regular => &self.regular,
            Weight::Bold => &self.bold,
        }
    }

    fn glyph(&self, w: Weight, c: char, px: f32) -> Rc<(Metrics, Vec<u8>)> {
        let key = (w, c, (px * 16.0).round() as u32);
        self.cache
            .borrow_mut()
            .entry(key)
            .or_insert_with(|| Rc::new(self.font(w).rasterize(c, px)))
            .clone()
    }

    pub fn measure(&self, text: &str, px: f32, w: Weight) -> f32 {
        let f = self.font(w);
        let mut prev: Option<char> = None;
        let mut width = 0.0;
        for c in text.chars() {
            if let Some(p) = prev {
                width += f.horizontal_kern(p, c, px).unwrap_or(0.0);
            }
            width += f.metrics(c, px).advance_width;
            prev = Some(c);
        }
        width
    }

    /// Greedy word wrap into lines no wider than `max_w`.
    pub fn wrap(&self, text: &str, px: f32, w: Weight, max_w: f32) -> Vec<String> {
        let mut lines = Vec::new();
        let mut cur = String::new();
        for word in text.split_whitespace() {
            let cand = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
            if !cur.is_empty() && self.measure(&cand, px, w) > max_w {
                lines.push(std::mem::replace(&mut cur, word.to_string()));
            } else {
                cur = cand;
            }
        }
        if !cur.is_empty() {
            lines.push(cur);
        }
        lines
    }

    pub fn elide(&self, text: &str, px: f32, w: Weight, max_w: f32) -> String {
        if max_w <= 0.0 {
            return String::new();
        }
        if self.measure(text, px, w) <= max_w {
            return text.to_string();
        }
        let ell = self.measure("…", px, w);
        let f = self.font(w);
        let mut out = String::new();
        let mut width = 0.0;
        for c in text.chars() {
            let cw = f.metrics(c, px).advance_width;
            if width + cw + ell > max_w {
                break;
            }
            width += cw;
            out.push(c);
        }
        while out.ends_with(' ') {
            out.pop();
        }
        out.push('…');
        out
    }

    /// Draw `text` with baseline at `baseline_y` in physical pixels, clipped to `clip`.
    pub fn draw(
        &self,
        pm: &mut Pixmap,
        text: &str,
        x: f32,
        baseline_y: f32,
        px: f32,
        w: Weight,
        color: Rgba,
        clip: Option<[i32; 4]>,
    ) {
        if color[3] == 0 || text.is_empty() {
            return;
        }
        let pw = pm.width() as i32;
        let ph = pm.height() as i32;
        let (cx0, cy0, cx1, cy1) = match clip {
            Some([x0, y0, x1, y1]) => (x0.max(0), y0.max(0), x1.min(pw), y1.min(ph)),
            None => (0, 0, pw, ph),
        };
        if cx0 >= cx1 || cy0 >= cy1 {
            return;
        }
        let data = pm.data_mut();
        let mut pen_x = x;
        let mut prev: Option<char> = None;
        let font = self.font(w);
        let sr = color[0] as u32;
        let sg = color[1] as u32;
        let sb = color[2] as u32;
        let sa = color[3] as u32;

        for c in text.chars() {
            if let Some(p) = prev {
                pen_x += font.horizontal_kern(p, c, px).unwrap_or(0.0);
            }
            prev = Some(c);
            let g = self.glyph(w, c, px);
            let (m, ref cov) = *g;
            if m.width > 0 && m.height > 0 {
                let gx0 = pen_x.round() as i32 + m.xmin;
                let gy0 = baseline_y.round() as i32 - m.height as i32 - m.ymin;
                let x_start = (cx0 - gx0).max(0) as usize;
                let x_end = (cx1 - gx0).min(m.width as i32).max(0) as usize;
                let y_start = (cy0 - gy0).max(0) as usize;
                let y_end = (cy1 - gy0).min(m.height as i32).max(0) as usize;

                for row in y_start..y_end {
                    let py = (gy0 + row as i32) as usize;
                    let src_row = row * m.width;
                    let dst_row = py * pw as usize;
                    for col in x_start..x_end {
                        let a = cov[src_row + col] as u32;
                        if a == 0 {
                            continue;
                        }
                        let eff_a = (a * sa + 127) / 255;
                        if eff_a == 0 {
                            continue;
                        }
                        let idx = (dst_row + (gx0 + col as i32) as usize) * 4;
                        let inv = 255 - eff_a;
                        // Pixmap stores premultiplied RGBA
                        let pr = (sr * eff_a + 127) / 255;
                        let pg = (sg * eff_a + 127) / 255;
                        let pb = (sb * eff_a + 127) / 255;
                        data[idx] = (pr + (data[idx] as u32 * inv + 127) / 255).min(255) as u8;
                        data[idx + 1] = (pg + (data[idx + 1] as u32 * inv + 127) / 255).min(255) as u8;
                        data[idx + 2] = (pb + (data[idx + 2] as u32 * inv + 127) / 255).min(255) as u8;
                        data[idx + 3] = (eff_a + (data[idx + 3] as u32 * inv + 127) / 255).min(255) as u8;
                    }
                }
            }
            pen_x += m.advance_width;
        }
    }
}

// ---------------------------------------------------------------- Shapes

pub fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let r = r.min(w * 0.5).min(h * 0.5).max(0.0);
    let mut pb = PathBuilder::new();
    if r <= 0.01 {
        pb.push_rect(tiny_skia::Rect::from_xywh(x, y, w, h)?);
        return pb.finish();
    }
    let k = r * 0.5522848;
    let x1 = x + w;
    let y1 = y + h;
    pb.move_to(x + r, y);
    pb.line_to(x1 - r, y);
    pb.cubic_to(x1 - r + k, y, x1, y + r - k, x1, y + r);
    pb.line_to(x1, y1 - r);
    pb.cubic_to(x1, y1 - r + k, x1 - r + k, y1, x1 - r, y1);
    pb.line_to(x + r, y1);
    pb.cubic_to(x + r - k, y1, x, y1 - r + k, x, y1 - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

pub fn fill_rrect(pm: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32, color: Rgba) {
    if color[3] == 0 {
        return;
    }
    let Some(path) = rounded_rect_path(x, y, w, h, r) else { return };
    let mut paint = Paint::default();
    paint.set_color(skia_color(color));
    paint.anti_alias = true;
    pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
}

/// Rounded rect with a vertical linear gradient from `top` to `bottom`.
pub fn fill_rrect_vgrad(pm: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32, top: Rgba, bottom: Rgba) {
    let Some(path) = rounded_rect_path(x, y, w, h, r) else { return };
    let Some(shader) = tiny_skia::LinearGradient::new(
        tiny_skia::Point::from_xy(x, y),
        tiny_skia::Point::from_xy(x, y + h),
        vec![
            tiny_skia::GradientStop::new(0.0, skia_color(top)),
            tiny_skia::GradientStop::new(1.0, skia_color(bottom)),
        ],
        tiny_skia::SpreadMode::Pad,
        Transform::identity(),
    ) else {
        return fill_rrect(pm, x, y, w, h, r, top);
    };
    let mut paint = Paint::default();
    paint.shader = shader;
    paint.anti_alias = true;
    pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
}

pub fn stroke_rrect(pm: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32, width: f32, color: Rgba) {
    if color[3] == 0 {
        return;
    }
    let half = width * 0.5;
    let Some(path) = rounded_rect_path(x + half, y + half, w - width, h - width, (r - half).max(0.0)) else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_color(skia_color(color));
    paint.anti_alias = true;
    let stroke = Stroke {
        width,
        ..Default::default()
    };
    pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
}

pub fn fill_circle(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, color: Rgba) {
    if color[3] == 0 || r <= 0.0 {
        return;
    }
    let Some(path) = PathBuilder::from_circle(cx, cy, r) else { return };
    let mut paint = Paint::default();
    paint.set_color(skia_color(color));
    paint.anti_alias = true;
    pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
}

pub fn stroke_circle(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, width: f32, color: Rgba) {
    if color[3] == 0 || r <= 0.0 {
        return;
    }
    let Some(path) = PathBuilder::from_circle(cx, cy, r) else { return };
    let mut paint = Paint::default();
    paint.set_color(skia_color(color));
    paint.anti_alias = true;
    let stroke = Stroke {
        width,
        ..Default::default()
    };
    pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
}

pub fn line(pm: &mut Pixmap, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, color: Rgba) {
    if color[3] == 0 {
        return;
    }
    let mut pb = PathBuilder::new();
    pb.move_to(x0, y0);
    pb.line_to(x1, y1);
    let Some(path) = pb.finish() else { return };
    let mut paint = Paint::default();
    paint.set_color(skia_color(color));
    paint.anti_alias = true;
    let stroke = Stroke {
        width,
        line_cap: tiny_skia::LineCap::Round,
        line_join: tiny_skia::LineJoin::Round,
        ..Default::default()
    };
    pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
}

/// Blit an alpha-mask (coverage bytes, `w x h`) tinted with `color` at `(dx, dy)` in physical pixels.
pub fn blit_mask(
    pm: &mut Pixmap,
    mask: &[u8],
    mw: u32,
    mh: u32,
    dx: i32,
    dy: i32,
    color: Rgba,
    clip: Option<[i32; 4]>,
) {
    if color[3] == 0 {
        return;
    }
    let pw = pm.width() as i32;
    let ph = pm.height() as i32;
    let (cx0, cy0, cx1, cy1) = match clip {
        Some([x0, y0, x1, y1]) => (x0.max(0), y0.max(0), x1.min(pw), y1.min(ph)),
        None => (0, 0, pw, ph),
    };
    let x_start = (cx0 - dx).max(0) as u32;
    let x_end = (cx1 - dx).min(mw as i32).max(0) as u32;
    let y_start = (cy0 - dy).max(0) as u32;
    let y_end = (cy1 - dy).min(mh as i32).max(0) as u32;
    if x_start >= x_end || y_start >= y_end {
        return;
    }
    let data = pm.data_mut();
    let sr = color[0] as u32;
    let sg = color[1] as u32;
    let sb = color[2] as u32;
    let sa = color[3] as u32;

    for row in y_start..y_end {
        let py = (dy + row as i32) as usize;
        let src_row = (row * mw) as usize;
        let dst_row = py * pw as usize;
        for col in x_start..x_end {
            let a = mask[src_row + col as usize] as u32;
            if a == 0 {
                continue;
            }
            let eff_a = (a * sa + 127) / 255;
            if eff_a == 0 {
                continue;
            }
            let idx = (dst_row + (dx + col as i32) as usize) * 4;
            let inv = 255 - eff_a;
            let pr = (sr * eff_a + 127) / 255;
            let pg = (sg * eff_a + 127) / 255;
            let pb = (sb * eff_a + 127) / 255;
            data[idx] = (pr + (data[idx] as u32 * inv + 127) / 255).min(255) as u8;
            data[idx + 1] = (pg + (data[idx + 1] as u32 * inv + 127) / 255).min(255) as u8;
            data[idx + 2] = (pb + (data[idx + 2] as u32 * inv + 127) / 255).min(255) as u8;
            data[idx + 3] = (eff_a + (data[idx + 3] as u32 * inv + 127) / 255).min(255) as u8;
        }
    }
}

/// Blit an RGBA8 image into `pm` inside `(dx, dy, dw, dh)` with rounded corners `radius`, clipped to `clip`.
pub fn blit_rounded_rgba(
    pm: &mut Pixmap,
    src: &[u8],
    sw: u32,
    sh: u32,
    dx: i32,
    dy: i32,
    dw: i32,
    dh: i32,
    radius: f32,
    clip: Option<[i32; 4]>,
) {
    if dw <= 0 || dh <= 0 || sw == 0 || sh == 0 {
        return;
    }
    let pw = pm.width() as i32;
    let ph = pm.height() as i32;
    let (cx0, cy0, cx1, cy1) = match clip {
        Some([x0, y0, x1, y1]) => (x0.max(0), y0.max(0), x1.min(pw), y1.min(ph)),
        None => (0, 0, pw, ph),
    };
    let x0 = dx.max(cx0);
    let y0 = dy.max(cy0);
    let x1 = (dx + dw).min(cx1);
    let y1 = (dy + dh).min(cy1);
    if x0 >= x1 || y0 >= y1 {
        return;
    }

    let r = radius.min(dw as f32 * 0.5).min(dh as f32 * 0.5).max(0.0);
    let data = pm.data_mut();
    let fx_scale = sw as f32 / dw as f32;
    let fy_scale = sh as f32 / dh as f32;

    for py in y0..y1 {
        let ly = (py - dy) as f32 + 0.5;
        let sy = ((ly * fy_scale) as u32).min(sh - 1);
        let src_row = (sy * sw) as usize;
        let dst_row = py as usize * pw as usize;

        for px in x0..x1 {
            let lx = (px - dx) as f32 + 0.5;
            // Signed distance to rounded rect for anti-aliased corners
            let mut cov = 1.0f32;
            if r > 0.5 {
                let qx = if lx < r {
                    r - lx
                } else if lx > dw as f32 - r {
                    lx - (dw as f32 - r)
                } else {
                    0.0
                };
                let qy = if ly < r {
                    r - ly
                } else if ly > dh as f32 - r {
                    ly - (dh as f32 - r)
                } else {
                    0.0
                };
                if qx > 0.0 && qy > 0.0 {
                    let dist = (qx * qx + qy * qy).sqrt() - r;
                    if dist >= 0.5 {
                        continue;
                    }
                    if dist > -0.5 {
                        cov = 0.5 - dist;
                    }
                }
            }

            let sx = ((lx * fx_scale) as u32).min(sw - 1);
            let si = (src_row + sx as usize) * 4;
            let sr = src[si] as u32;
            let sg = src[si + 1] as u32;
            let sb = src[si + 2] as u32;
            let sa = ((src[si + 3] as f32 * cov).round() as u32).min(255);
            if sa == 0 {
                continue;
            }
            let di = (dst_row + px as usize) * 4;
            if sa == 255 {
                data[di] = sr as u8;
                data[di + 1] = sg as u8;
                data[di + 2] = sb as u8;
                data[di + 3] = 255;
            } else {
                let inv = 255 - sa;
                data[di] = ((sr * sa + data[di] as u32 * inv + 127) / 255).min(255) as u8;
                data[di + 1] = ((sg * sa + data[di + 1] as u32 * inv + 127) / 255).min(255) as u8;
                data[di + 2] = ((sb * sa + data[di + 2] as u32 * inv + 127) / 255).min(255) as u8;
                data[di + 3] = (sa + (data[di + 3] as u32 * inv + 127) / 255).min(255) as u8;
            }
        }
    }
}
