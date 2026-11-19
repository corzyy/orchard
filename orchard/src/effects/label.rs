//! Small text labels drawn by the compositor (Mission Control titles, Space names).

use std::collections::HashMap;

use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    utils::Transform,
};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Transform as SkTransform};

use crate::chrome::font::title_font;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LabelStyle {
    /// dark translucent pill with white text (window titles)
    Pill,
    /// white text with a soft shadow (Space names)
    Plain,
    /// big glyph (the "+" button)
    Glyph,
    /// round "×" button (remove a Space); the text is ignored
    Close,
    /// the same, under the pointer
    CloseHot,
}

#[derive(Default)]
pub struct Labels {
    cache: HashMap<(String, LabelStyle, u32), (MemoryRenderBuffer, (i32, i32))>,
}

impl Labels {
    /// Returns the buffer and its logical size.
    pub fn get(&mut self, text: &str, style: LabelStyle, scale: f64) -> Option<(&MemoryRenderBuffer, (i32, i32))> {
        let key = (text.to_string(), style, (scale * 100.0) as u32);
        if !self.cache.contains_key(&key) {
            let (pm, logical) = paint(text, style, scale)?;
            let buf = MemoryRenderBuffer::from_slice(
                pm.data(),
                Fourcc::Abgr8888,
                (pm.width() as i32, pm.height() as i32),
                1,
                Transform::Normal,
                None,
            );
            if self.cache.len() > 256 {
                self.cache.clear();
            }
            self.cache.insert(key.clone(), (buf, logical));
        }
        self.cache.get(&key).map(|(b, s)| (b, *s))
    }
}

fn paint(text: &str, style: LabelStyle, scale: f64) -> Option<(Pixmap, (i32, i32))> {
    if matches!(style, LabelStyle::Close | LabelStyle::CloseHot) {
        return paint_close(style == LabelStyle::CloseHot, scale);
    }
    let font = title_font()?;
    let s = scale as f32;
    let (px, pad_x, h) = match style {
        LabelStyle::Pill => (13.0, 10.0, 24.0),
        LabelStyle::Plain => (12.0, 4.0, 18.0),
        LabelStyle::Glyph => (34.0, 2.0, 40.0),
        LabelStyle::Close | LabelStyle::CloseHot => return None,
    };
    let text = font.ellipsize(text, px * s, 420.0 * s);
    let tw = font.measure(&text, px * s) / s;
    let w = (tw + 2.0 * pad_x).ceil();
    let mut pm = Pixmap::new((w * s).ceil() as u32, (h * s).ceil() as u32)?;
    if style == LabelStyle::Pill {
        let mut pb = PathBuilder::new();
        let r = h * s / 2.0;
        let (pw, ph) = (pm.width() as f32, pm.height() as f32);
        pb.move_to(r, 0.0);
        pb.line_to(pw - r, 0.0);
        pb.quad_to(pw, 0.0, pw, r);
        pb.quad_to(pw, ph, pw - r, ph);
        pb.line_to(r, ph);
        pb.quad_to(0.0, ph, 0.0, r);
        pb.quad_to(0.0, 0.0, r, 0.0);
        pb.close();
        if let Some(path) = pb.finish() {
            let mut paint = Paint::default();
            paint.set_color_rgba8(30, 30, 30, 200);
            paint.anti_alias = true;
            pm.fill_path(&path, &paint, FillRule::Winding, SkTransform::identity(), None);
        }
    }
    let asc = font.ascent(px * s);
    let desc = font.descent(px * s);
    let baseline = ((h * s) + asc + desc) / 2.0;
    if style == LabelStyle::Plain {
        font.draw(&mut pm, &text, pad_x * s + 1.0, baseline + 1.0, px * s, [0, 0, 0, 140]);
    }
    font.draw(&mut pm, &text, pad_x * s, baseline, px * s, [255, 255, 255, 255]);
    Some((pm, (w as i32, h as i32)))
}

/// A 20 px circle with an "×", like the close badge on macOS Space thumbnails.
fn paint_close(hot: bool, scale: f64) -> Option<(Pixmap, (i32, i32))> {
    use tiny_skia::{LineCap, Stroke};
    let d = 20.0f32;
    let s = scale as f32;
    let mut pm = Pixmap::new((d * s).ceil() as u32, (d * s).ceil() as u32)?;
    let c = d * s / 2.0;
    let mut paint = Paint::default();
    paint.anti_alias = true;
    if let Some(circle) = PathBuilder::from_circle(c, c, c - 0.5) {
        let (r, g, b) = if hot { (235, 235, 235) } else { (70, 70, 70) };
        paint.set_color_rgba8(r, g, b, 240);
        pm.fill_path(&circle, &paint, FillRule::Winding, SkTransform::identity(), None);
    }
    let a = 5.0 * s;
    let mut pb = PathBuilder::new();
    pb.move_to(c - a, c - a);
    pb.line_to(c + a, c + a);
    pb.move_to(c + a, c - a);
    pb.line_to(c - a, c + a);
    if let Some(x) = pb.finish() {
        let v = if hot { 30 } else { 240 };
        paint.set_color_rgba8(v, v, v, 255);
        let stroke = Stroke {
            width: 1.8 * s,
            line_cap: LineCap::Round,
            ..Default::default()
        };
        pm.stroke_path(&x, &paint, &stroke, SkTransform::identity(), None);
    }
    Some((pm, (d as i32, d as i32)))
}
