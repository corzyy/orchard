//! Minimal text rendering for compositor-drawn UI (window titles).
//!
//! Uses fontconfig (`fc-match`) once to find an SF Pro/Inter/sans font file and
//! rasterises glyphs with `fontdue`.

use std::{collections::HashMap, sync::Mutex, sync::OnceLock};

use fontdue::{Font, FontSettings, Metrics};
use tiny_skia::Pixmap;

use super::theme::Rgba;

pub struct TextRenderer {
    font: Font,
    cache: Mutex<HashMap<(char, u32), (Metrics, Vec<u8>)>>,
}

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
    load_matching(pattern, "")
}

/// Load `pattern`, rejecting fontconfig fallbacks whose file name lacks `needle`.
fn load_matching(pattern: &str, needle: &str) -> Option<Font> {
    let (file, index) = find_font(pattern)?;
    if !file.to_lowercase().contains(needle) {
        return None;
    }
    let data = std::fs::read(&file).ok()?;
    let font = Font::from_bytes(
        data,
        FontSettings {
            collection_index: index,
            scale: 40.0,
            ..Default::default()
        },
    )
    .map_err(|e| tracing::warn!("failed to load font {file}: {e}"))
    .ok()?;
    tracing::info!("chrome font: {file} (index {index})");
    Some(font)
}

/// The font used for window titles (SF Pro Text Semibold, then Inter Semibold, then sans-serif).
pub fn title_font() -> Option<&'static TextRenderer> {
    static FONT: OnceLock<Option<TextRenderer>> = OnceLock::new();
    FONT.get_or_init(|| {
        let font = load_matching("SF Pro Text:style=Semibold", "sf-pro")
            .or_else(|| load("Inter:weight=semibold"))
            .or_else(|| load("sans-serif:weight=medium"))
            .or_else(|| load("sans-serif"))?;
        Some(TextRenderer {
            font,
            cache: Mutex::new(HashMap::new()),
        })
    })
    .as_ref()
}

impl TextRenderer {
    fn glyph(&self, c: char, px: f32) -> (Metrics, Vec<u8>) {
        let key = (c, (px * 64.0) as u32);
        let mut cache = self.cache.lock().unwrap();
        cache
            .entry(key)
            .or_insert_with(|| self.font.rasterize(c, px))
            .clone()
    }

    fn advance(&self, prev: Option<char>, c: char, px: f32) -> f32 {
        let _ = prev; // fontdue mis-kerns Inter (legacy kern table only), so no kerning
        self.font.metrics(c, px).advance_width
    }

    pub fn measure(&self, text: &str, px: f32) -> f32 {
        let mut w = 0.0;
        let mut prev = None;
        for c in text.chars() {
            w += self.advance(prev, c, px);
            prev = Some(c);
        }
        w
    }

    /// Shorten `text` with an ellipsis so it fits into `max_width`.
    pub fn ellipsize(&self, text: &str, px: f32, max_width: f32) -> String {
        if self.measure(text, px) <= max_width {
            return text.to_string();
        }
        let ell = "…";
        let ell_w = self.measure(ell, px);
        let mut out = String::new();
        let mut w = 0.0;
        let mut prev = None;
        for c in text.chars() {
            let a = self.advance(prev, c, px);
            if w + a + ell_w > max_width {
                break;
            }
            w += a;
            out.push(c);
            prev = Some(c);
        }
        out.push_str(ell);
        out
    }

    /// Draw `text` with its baseline at `baseline_y`, starting at `x`.
    pub fn draw(&self, pixmap: &mut Pixmap, text: &str, x: f32, baseline_y: f32, px: f32, color: Rgba) {
        let pw = pixmap.width() as i32;
        let ph = pixmap.height() as i32;
        let data = pixmap.data_mut();
        let mut pen = x;
        for c in text.chars() {
            let (m, bitmap) = self.glyph(c, px);
            let gx = (pen + m.xmin as f32).round() as i32;
            let gy = (baseline_y - m.height as f32 - m.ymin as f32).round() as i32;
            for row in 0..m.height as i32 {
                let y = gy + row;
                if y < 0 || y >= ph {
                    continue;
                }
                for col in 0..m.width as i32 {
                    let x = gx + col;
                    if x < 0 || x >= pw {
                        continue;
                    }
                    let cov = bitmap[(row * m.width as i32 + col) as usize] as u32;
                    if cov == 0 {
                        continue;
                    }
                    let a = cov * color[3] as u32 / 255;
                    let i = ((y * pw + x) * 4) as usize;
                    // source-over in premultiplied space
                    for k in 0..3 {
                        let src = color[k] as u32 * a / 255;
                        data[i + k] = (src + data[i + k] as u32 * (255 - a) / 255) as u8;
                    }
                    data[i + 3] = (a + data[i + 3] as u32 * (255 - a) / 255) as u8;
                }
            }
            pen += m.advance_width;
        }
    }

    pub fn ascent(&self, px: f32) -> f32 {
        self.font
            .horizontal_line_metrics(px)
            .map(|m| m.ascent)
            .unwrap_or(px * 0.8)
    }

    pub fn descent(&self, px: f32) -> f32 {
        self.font
            .horizontal_line_metrics(px)
            .map(|m| m.descent)
            .unwrap_or(-px * 0.2)
    }
}
