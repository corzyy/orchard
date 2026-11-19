//! Drawing helpers on top of tiny-skia.

use tiny_skia::{Color, FillRule, Paint, Path, PathBuilder, Pixmap, Stroke, Transform};

use crate::text::Rgba;

pub fn paint(c: Rgba) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(Color::from_rgba8(c[0], c[1], c[2], c[3]));
    p.anti_alias = true;
    p
}

pub fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let k = 0.5523 * r; // cubic circle approximation
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

pub fn fill_rounded(pm: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32, c: Rgba) {
    if let Some(p) = rounded_rect(x, y, w, h, r) {
        pm.fill_path(&p, &paint(c), FillRule::Winding, Transform::identity(), None);
    }
}

pub fn stroke_rounded(pm: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32, width: f32, c: Rgba) {
    if let Some(p) = rounded_rect(x, y, w, h, r) {
        let st = Stroke {
            width,
            ..Default::default()
        };
        pm.stroke_path(&p, &paint(c), &st, Transform::identity(), None);
    }
}

pub fn fill_circle(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    if let Some(p) = PathBuilder::from_circle(cx, cy, r) {
        pm.fill_path(&p, &paint(c), FillRule::Winding, Transform::identity(), None);
    }
}

/// Copy a premultiplied RGBA pixmap into a wl_shm ARGB8888 canvas (BGRA bytes).
pub fn blit_to_shm(pm: &Pixmap, canvas: &mut [u8]) {
    for (dst, src) in canvas.chunks_exact_mut(4).zip(pm.data().chunks_exact(4)) {
        dst[0] = src[2];
        dst[1] = src[1];
        dst[2] = src[0];
        dst[3] = src[3];
    }
}

/// `v.clamp(lo, hi)` that never panics: when the range is empty (content wider than
/// the space it has), the result is `lo`, so text starts at the left padding.
pub fn clamp_span(v: f32, lo: f32, hi: f32) -> f32 {
    if hi < lo || v.is_nan() {
        lo
    } else {
        v.clamp(lo, hi)
    }
}

#[cfg(test)]
mod clamp_tests {
    use super::clamp_span;

    #[test]
    fn empty_range_does_not_panic() {
        assert_eq!(clamp_span(10.0, 4.0, 2.0), 4.0);
        assert_eq!(clamp_span(f32::NAN, 4.0, 8.0), 4.0);
        assert_eq!(clamp_span(1.0, 4.0, 8.0), 4.0);
        assert_eq!(clamp_span(9.0, 4.0, 8.0), 8.0);
    }
}
