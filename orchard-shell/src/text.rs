//! Text rendering with fontdue into tiny-skia pixmaps.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use fontdue::{Font, FontSettings, Metrics};
use tiny_skia::Pixmap;

pub type Rgba = [u8; 4];

fn find_font(pattern: &str) -> Option<(String, u32)> {
    let out = std::process::Command::new("fc-match")
        .args(["-f", "%{file}:%{index}", pattern])
        .output()
        .ok()?;
    let s = String::from_utf8(out.stdout).ok()?;
    let (file, idx) = s.rsplit_once(':')?;
    Some((file.to_string(), idx.trim().parse().unwrap_or(0)))
}

fn load(pattern: &str) -> Option<Font> {
    let (file, index) = find_font(pattern)?;
    load_file(file, index)
}

/// Like `load`, but only accepts the match if its file name contains `needle`
/// (fontconfig always falls back to *something*).
fn load_family(pattern: &str, needle: &str) -> Option<Font> {
    let (file, index) = find_font(pattern)?;
    if !file.to_lowercase().contains(needle) {
        return None;
    }
    load_file(file, index)
}

fn load_file(file: String, index: u32) -> Option<Font> {
    let data = std::fs::read(&file).ok()?;
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

impl Fonts {
    pub fn load() -> Fonts {
        // SF Pro (Apple's system font) when installed, else Inter, else sans-serif.
        let regular = load_family("SF Pro Text:style=Regular", "sf-pro")
            .or_else(|| load("Inter:weight=medium"))
            .or_else(|| load("sans-serif"))
            .expect("no usable font found (fc-match)");
        let bold = load_family("SF Pro Text:style=Semibold", "sf-pro")
            .or_else(|| load("Inter:weight=bold"))
            .or_else(|| load("sans-serif:bold"));
        Fonts {
            bold: bold.unwrap_or_else(|| regular.clone()),
            regular,
            cache: RefCell::new(HashMap::new()),
        }
    }

    fn font(&self, w: Weight) -> &Font {
        match w {
            Weight::Regular => &self.regular,
            Weight::Bold => &self.bold,
        }
    }

    fn glyph(&self, w: Weight, c: char, px: f32) -> Rc<(Metrics, Vec<u8>)> {
        let key = (w, c, (px * 64.0) as u32);
        self.cache
            .borrow_mut()
            .entry(key)
            .or_insert_with(|| Rc::new(self.font(w).rasterize(c, px)))
            .clone()
    }

    pub fn measure(&self, text: &str, px: f32, w: Weight) -> f32 {
        text.chars().map(|c| self.font(w).metrics(c, px).advance_width).sum()
    }

    /// Shorten `text` with "…" so it fits in `max_w`.
    pub fn elide(&self, text: &str, px: f32, w: Weight, max_w: f32) -> String {
        if self.measure(text, px, w) <= max_w {
            return text.to_string();
        }
        let ell = self.measure("…", px, w);
        let mut out = String::new();
        let mut width = 0.0;
        for c in text.chars() {
            let cw = self.measure(c.encode_utf8(&mut [0; 4]), px, w);
            if width + cw + ell > max_w {
                break;
            }
            width += cw;
            out.push(c);
        }
        out.truncate(out.trim_end().len());
        out.push('…');
        out
    }

    /// Word-wrap `text` into at most `max_lines` lines of `max_w`; if text is
    /// left over, the last line ends in "…". Explicit newlines are kept.
    pub fn wrap(&self, text: &str, px: f32, w: Weight, max_w: f32, max_lines: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let mut overflow = false;
        for para in text.split('\n') {
            if lines.len() >= max_lines {
                overflow = overflow || !para.trim().is_empty();
                continue;
            }
            let mut line = String::new();
            for word in para.split_whitespace() {
                if lines.len() >= max_lines {
                    overflow = true;
                    break;
                }
                let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
                if line.is_empty() || self.measure(&candidate, px, w) <= max_w {
                    line = candidate;
                } else {
                    lines.push(std::mem::replace(&mut line, word.to_string()));
                }
            }
            if !line.is_empty() {
                if lines.len() < max_lines {
                    lines.push(line);
                } else {
                    overflow = true;
                }
            }
        }
        if let Some(last) = lines.last_mut() {
            if overflow {
                last.push_str(" …");
            }
            *last = self.elide(last, px, w, max_w);
        }
        lines
    }

    /// Ascent+descent box height helper: returns the baseline y that vertically
    /// centers a line of text inside [top, top+height].
    pub fn baseline_centered(&self, top: f32, height: f32, px: f32, w: Weight) -> f32 {
        let lm = self.font(w).horizontal_line_metrics(px);
        let (asc, desc) = lm.map(|m| (m.ascent, m.descent)).unwrap_or((px * 0.8, -px * 0.2));
        (top + (height + asc + desc) / 2.0).round()
    }

    /// Draw text; returns the advance width.
    pub fn draw(&self, pm: &mut Pixmap, text: &str, x: f32, baseline: f32, px: f32, w: Weight, color: Rgba) -> f32 {
        let pw = pm.width() as i32;
        let ph = pm.height() as i32;
        let data = pm.data_mut();
        let mut pen = x;
        for c in text.chars() {
            let g = self.glyph(w, c, px);
            let (m, bitmap) = (&g.0, &g.1);
            let gx = (pen + m.xmin as f32).round() as i32;
            let gy = (baseline - m.height as f32 - m.ymin as f32).round() as i32;
            for row in 0..m.height as i32 {
                let y = gy + row;
                if y < 0 || y >= ph {
                    continue;
                }
                for col in 0..m.width as i32 {
                    let xx = gx + col;
                    if xx < 0 || xx >= pw {
                        continue;
                    }
                    let cov = bitmap[(row * m.width as i32 + col) as usize] as u32;
                    if cov == 0 {
                        continue;
                    }
                    let a = cov * color[3] as u32 / 255;
                    let i = ((y * pw + xx) * 4) as usize;
                    for k in 0..3 {
                        let src = color[k] as u32 * a / 255;
                        data[i + k] = (src + data[i + k] as u32 * (255 - a) / 255) as u8;
                    }
                    data[i + 3] = (a + data[i + 3] as u32 * (255 - a) / 255) as u8;
                }
            }
            pen += m.advance_width;
        }
        pen - x
    }
}
