//! Fonts (SF Pro when installed, else Inter, else sans-serif) and cached text
//! layers with a soft shadow. Glyph outlines come straight from ttf-parser and
//! are filled by tiny-skia: nothing is parsed up front, so the lock screen can
//! draw its first frame a few ms after it starts (a full SF Pro parse takes
//! half a second).

use std::{collections::HashMap, rc::Rc, thread};

use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, PixmapPaint, Transform};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Face {
    /// the big clock (SF Pro Rounded)
    Clock,
    /// date, user name
    Semibold,
    /// placeholder, button labels
    Regular,
}

type TtfFace = ttf_parser::Face<'static>;

fn find_font(pattern: &str) -> Option<(String, u32)> {
    let out = std::process::Command::new("fc-match").args(["-f", "%{file}:%{index}", pattern]).output().ok()?;
    let s = String::from_utf8(out.stdout).ok()?;
    let (file, idx) = s.rsplit_once(':')?;
    Some((file.to_string(), idx.trim().parse().unwrap_or(0)))
}

/// First candidate whose fontconfig match is really that family (fontconfig
/// always falls back to *something*); `None` needle accepts any match.
fn load_first(candidates: &[(&str, Option<&str>)]) -> Option<TtfFace> {
    for (pattern, needle) in candidates {
        let Some((file, index)) = find_font(pattern) else { continue };
        let name = file.to_lowercase().replace(['-', '_', ' '], "");
        if needle.is_some_and(|n| !name.contains(n)) {
            continue;
        }
        let Ok(data) = std::fs::read(&file) else { continue };
        // the process lives as long as the lock: keep the bytes for good
        let data: &'static [u8] = Box::leak(data.into_boxed_slice());
        if let Ok(f) = ttf_parser::Face::parse(data, index) {
            return Some(f);
        }
    }
    None
}

pub struct Fonts {
    clock: TtfFace,
    semibold: TtfFace,
    regular: TtfFace,
}

/// Glyph outline → tiny-skia path, scaled to px and flipped (font y points up).
struct Outline<'a> {
    pb: &'a mut PathBuilder,
    x: f32,
    y: f32,
    s: f32,
}

impl ttf_parser::OutlineBuilder for Outline<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.pb.move_to(self.x + x * self.s, self.y - y * self.s);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.pb.line_to(self.x + x * self.s, self.y - y * self.s);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.pb.quad_to(self.x + x1 * self.s, self.y - y1 * self.s, self.x + x * self.s, self.y - y * self.s);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.pb.cubic_to(
            self.x + x1 * self.s,
            self.y - y1 * self.s,
            self.x + x2 * self.s,
            self.y - y2 * self.s,
            self.x + x * self.s,
            self.y - y * self.s,
        );
    }
    fn close(&mut self) {
        self.pb.close();
    }
}

impl Fonts {
    /// Finds and maps the fonts on background threads (fc-match is a process
    /// per font); `join` waits for them.
    pub fn spawn() -> thread::JoinHandle<Fonts> {
        thread::spawn(|| {
            let clock = thread::spawn(|| {
                load_first(&[
                    ("SF Pro Rounded:style=Semibold", Some("sfprorounded")),
                    ("SF Pro Display:style=Semibold", Some("sfpro")),
                    ("Inter:weight=semibold", Some("inter")),
                    ("sans-serif:bold", None),
                ])
            });
            let semibold = thread::spawn(|| {
                load_first(&[
                    ("SF Pro Display:style=Semibold", Some("sfpro")),
                    ("Inter:weight=semibold", Some("inter")),
                    ("sans-serif:bold", None),
                ])
            });
            let regular = load_first(&[
                ("SF Pro Text:style=Regular", Some("sfpro")),
                ("Inter", Some("inter")),
                ("sans-serif", None),
            ])
            .expect("no usable font (fc-match)");
            let semibold = semibold.join().ok().flatten().unwrap_or_else(|| regular.clone());
            let clock = clock.join().ok().flatten().unwrap_or_else(|| semibold.clone());
            Fonts { clock, semibold, regular }
        })
    }

    fn face(&self, f: Face) -> &TtfFace {
        match f {
            Face::Clock => &self.clock,
            Face::Semibold => &self.semibold,
            Face::Regular => &self.regular,
        }
    }

    fn scale(&self, px: f32, f: Face) -> f32 {
        px / self.face(f).units_per_em() as f32
    }

    fn advance(&self, c: char, px: f32, f: Face) -> f32 {
        let face = self.face(f);
        face.glyph_index(c)
            .and_then(|g| face.glyph_hor_advance(g))
            .map(|a| a as f32 * self.scale(px, f))
            .unwrap_or(px * 0.5)
    }

    pub fn measure(&self, text: &str, px: f32, face: Face) -> f32 {
        text.chars().map(|c| self.advance(c, px, face)).sum()
    }

    /// (ascent, descent) in px; descent is negative.
    pub fn line(&self, px: f32, f: Face) -> (f32, f32) {
        let face = self.face(f);
        let s = self.scale(px, f);
        (face.ascender() as f32 * s, face.descender() as f32 * s)
    }

    /// Height of capital letters / digits (for optical centering).
    pub fn cap_height(&self, px: f32, f: Face) -> f32 {
        let face = self.face(f);
        let units = face
            .capital_height()
            .map(|h| h as f32)
            .or_else(|| face.glyph_index('H').and_then(|g| face.glyph_bounding_box(g)).map(|b| b.y_max as f32))
            .unwrap_or(face.units_per_em() as f32 * 0.7);
        units * self.scale(px, f)
    }

    /// Draw `text` into a fresh pixmap that also has room for the shadow.
    /// The layer's origin sits at (`ox`, `oy`) relative to (text x, baseline).
    fn render(&self, text: &str, px: f32, f: Face, color: [u8; 4], shadow: Option<Shadow>) -> Option<Layer> {
        let face = self.face(f);
        let (asc, desc) = self.line(px, f);
        let pad = shadow.map(|s| (s.radius * 2.0 + s.dy.abs()).ceil() as i32 + 2).unwrap_or(2);
        let w = (self.measure(text, px, f).ceil() as i32 + pad * 2 + 4).max(1) as u32;
        let h = ((asc - desc).ceil() as i32 + pad * 2 + 4).max(1) as u32;
        let base = pad as f32 + asc.ceil() + 2.0;
        let left = pad as f32 + 2.0;

        let mut pb = PathBuilder::new();
        let mut pen = left;
        let s = self.scale(px, f);
        for c in text.chars() {
            if let Some(g) = face.glyph_index(c) {
                face.outline_glyph(g, &mut Outline { pb: &mut pb, x: pen, y: base, s });
            }
            pen += self.advance(c, px, f);
        }
        let mut glyphs = Pixmap::new(w, h)?;
        if let Some(path) = pb.finish() {
            let mut paint = Paint::default();
            paint.set_color_rgba8(color[0], color[1], color[2], color[3]);
            paint.anti_alias = true;
            glyphs.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
        }
        let Some(sh) = shadow else {
            return Some(Layer { pm: glyphs, ox: -left, oy: -base });
        };
        // soft shadow from the glyph coverage, text on top
        let (wu, hu) = (w as usize, h as usize);
        let mut cov: Vec<u8> = glyphs.data().chunks_exact(4).map(|p| p[3]).collect();
        blur(&mut cov, wu, hu, sh.radius.round() as usize);
        let mut out = Pixmap::new(w, h)?;
        let dy = sh.dy.round() as i32;
        let data = out.data_mut();
        for y in 0..hu as i32 {
            let sy = y - dy;
            if sy < 0 || sy >= hu as i32 {
                continue;
            }
            for x in 0..wu {
                data[(y as usize * wu + x) * 4 + 3] = (cov[sy as usize * wu + x] as u32 * sh.alpha as u32 / 255) as u8;
            }
        }
        out.draw_pixmap(0, 0, glyphs.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
        Some(Layer { pm: out, ox: -left, oy: -base })
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Shadow {
    pub radius: f32,
    pub dy: f32,
    pub alpha: u8,
}

pub struct Layer {
    pm: Pixmap,
    ox: f32,
    oy: f32,
}

/// Three box-blur passes ≈ a gaussian.
fn blur(buf: &mut [u8], w: usize, h: usize, r: usize) {
    if r == 0 {
        return;
    }
    let mut tmp = vec![0u8; buf.len()];
    for _ in 0..3 {
        // horizontal
        for y in 0..h {
            let row = &buf[y * w..y * w + w];
            let mut acc: u32 = 0;
            for x in 0..(r + 1).min(w) {
                acc += row[x] as u32;
            }
            acc += row[0] as u32 * r as u32;
            for x in 0..w {
                tmp[y * w + x] = (acc / (2 * r as u32 + 1)) as u8;
                let add = row[(x + r + 1).min(w - 1)] as u32;
                let sub = row[x.saturating_sub(r)] as u32;
                acc = (acc + add).saturating_sub(sub);
            }
        }
        // vertical
        for x in 0..w {
            let mut acc: u32 = 0;
            for y in 0..(r + 1).min(h) {
                acc += tmp[y * w + x] as u32;
            }
            acc += tmp[x] as u32 * r as u32;
            for y in 0..h {
                buf[y * w + x] = (acc / (2 * r as u32 + 1)) as u8;
                let add = tmp[(y + r + 1).min(h - 1) * w + x] as u32;
                let sub = tmp[y.saturating_sub(r) * w + x] as u32;
                acc = (acc + add).saturating_sub(sub);
            }
        }
    }
}

#[derive(PartialEq, Eq, Hash)]
struct Key {
    text: String,
    px: u32,
    face: Face,
    color: [u8; 4],
    shadow: Option<(u32, i32, u8)>,
}

/// Rendered text layers, so animation frames only composite pixmaps.
pub struct TextCache {
    pub fonts: Fonts,
    layers: HashMap<Key, Rc<Layer>>,
}

impl TextCache {
    pub fn new(fonts: Fonts) -> Self {
        TextCache { fonts, layers: HashMap::new() }
    }

    /// Draw `text` at (x, baseline) with `opacity`; returns its width.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        pm: &mut Pixmap,
        text: &str,
        x: f32,
        baseline: f32,
        px: f32,
        face: Face,
        color: [u8; 4],
        shadow: Option<Shadow>,
        opacity: f32,
    ) -> f32 {
        let key = Key {
            text: text.to_string(),
            px: (px * 16.0) as u32,
            face,
            color,
            shadow: shadow.map(|s| ((s.radius * 4.0) as u32, (s.dy * 4.0) as i32, s.alpha)),
        };
        if self.layers.len() > 96 {
            self.layers.clear();
        }
        let layer = match self.layers.get(&key) {
            Some(l) => l.clone(),
            None => match self.fonts.render(text, px, face, color, shadow) {
                Some(l) => {
                    let l = Rc::new(l);
                    self.layers.insert(key, l.clone());
                    l
                }
                None => return 0.0,
            },
        };
        if opacity > 0.004 {
            let paint = PixmapPaint { opacity: opacity.clamp(0.0, 1.0), ..Default::default() };
            pm.draw_pixmap(
                (x + layer.ox).round() as i32,
                (baseline + layer.oy).round() as i32,
                layer.pm.as_ref(),
                &paint,
                Transform::identity(),
                None,
            );
        }
        self.fonts.measure(text, px, face)
    }
}
