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
    /// large white heading (App Exposé's app name)
    Header,
    /// big glyph (unused by Mission Control now, kept for other overlays)
    Glyph,
    /// the "+" on the add-Space button
    Plus,
    /// round "×" button (remove a Space); the text is ignored
    Close,
    /// the same, under the pointer
    CloseHot,
}

#[derive(Default)]
pub struct Labels {
    /// style + scale bucket -> text -> buffer
    cache: HashMap<(LabelStyle, u32), HashMap<String, (MemoryRenderBuffer, (i32, i32))>>,
}

impl Labels {
    /// Returns the buffer and its logical size.
    pub fn get(&mut self, text: &str, style: LabelStyle, scale: f64) -> Option<(&MemoryRenderBuffer, (i32, i32))> {
        let bucket = (style, (scale * 100.0) as u32);
        let cached = self.cache.get(&bucket).is_some_and(|m| m.contains_key(text));
        if !cached {
            let (pm, logical) = paint(text, style, scale)?;
            let buf = MemoryRenderBuffer::from_slice(
                pm.data(),
                Fourcc::Abgr8888,
                (pm.width() as i32, pm.height() as i32),
                1,
                Transform::Normal,
                None,
            );
            let map = self.cache.entry(bucket).or_default();
            if map.len() > 64 {
                map.clear();
            }
            map.insert(text.to_string(), (buf, logical));
        }
        self.cache.get(&bucket).and_then(|m| m.get(text)).map(|(b, s)| (b, *s))
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
        LabelStyle::Header => (18.0, 6.0, 28.0),
        LabelStyle::Glyph => (34.0, 2.0, 40.0),
        LabelStyle::Plus => (22.0, 3.0, 28.0),
        LabelStyle::Close | LabelStyle::CloseHot => return None,
    };
    let max_w = match style {
        LabelStyle::Header => 640.0,
        _ => 420.0,
    };
    let text = font.ellipsize(text, px * s, max_w * s);
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
    if matches!(
        style,
        LabelStyle::Plain | LabelStyle::Header | LabelStyle::Plus | LabelStyle::Glyph
    ) {
        let shadow = if style == LabelStyle::Header { 170 } else { 140 };
        font.draw(&mut pm, &text, pad_x * s + 1.0, baseline + 1.0, px * s, [0, 0, 0, shadow]);
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
