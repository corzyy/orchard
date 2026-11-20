//! Drawing helpers on top of tiny-skia.

use tiny_skia::{Color, FillRule, Paint, Path, PathBuilder, Pixmap, Stroke, Transform};

use crate::text::Rgba;

pub fn paint(c: Rgba) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(Color::from_rgba8(c[0], c[1], c[2], c[3]));
    p.anti_alias = true;
    p
}

/// Superellipse exponent matching `SQUIRCLE_N` in `orchard/src/chrome/mod.rs`
/// and `N` in `orchard/src/effects/shaders/rounded.frag` so frosted-glass blur
/// backdrops and shell-painted chrome share the exact same corner contour.
const SQUIRCLE_N: f32 = 2.6;

/// Number of line segments per squircle corner (chord error < 0.03px at r = 24).
const SQUIRCLE_SEGMENTS: usize = 16;

pub fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
    use std::f32::consts::FRAC_PI_2;
    let x0 = x;
    let y0 = y;
    let x1 = x + w.max(0.0);
    let y1 = y + h.max(0.0);
    let r = r.clamp(0.0, (w * 0.5).min(h * 0.5).max(0.0));
    let mut pb = PathBuilder::new();
    if r <= 0.5 {
        pb.move_to(x0, y0);
        pb.line_to(x1, y0);
        pb.line_to(x1, y1);
        pb.line_to(x0, y1);
        pb.close();
        return pb.finish();
    }
    let exp = 2.0 / SQUIRCLE_N;
    pb.move_to(x0 + r, y0);
    pb.line_to(x1 - r, y0);
    // Top-right corner
    for i in 1..=SQUIRCLE_SEGMENTS {
        let t = FRAC_PI_2 * (i as f32 / SQUIRCLE_SEGMENTS as f32);
        let qx = r * t.sin().max(0.0).powf(exp);
        let qy = r * t.cos().max(0.0).powf(exp);
        pb.line_to(x1 - r + qx, y0 + r - qy);
    }
    pb.line_to(x1, y1 - r);
    // Bottom-right corner
    for i in 1..=SQUIRCLE_SEGMENTS {
        let t = FRAC_PI_2 * (i as f32 / SQUIRCLE_SEGMENTS as f32);
        let qx = r * t.cos().max(0.0).powf(exp);
        let qy = r * t.sin().max(0.0).powf(exp);
        pb.line_to(x1 - r + qx, y1 - r + qy);
    }
    pb.line_to(x0 + r, y1);
    // Bottom-left corner
    for i in 1..=SQUIRCLE_SEGMENTS {
        let t = FRAC_PI_2 * (i as f32 / SQUIRCLE_SEGMENTS as f32);
        let qx = r * t.sin().max(0.0).powf(exp);
        let qy = r * t.cos().max(0.0).powf(exp);
        pb.line_to(x0 + r - qx, y1 - r + qy);
    }
    pb.line_to(x0, y0 + r);
    // Top-left corner
    for i in 1..=SQUIRCLE_SEGMENTS {
        let t = FRAC_PI_2 * (i as f32 / SQUIRCLE_SEGMENTS as f32);
        let qx = r * t.cos().max(0.0).powf(exp);
        let qy = r * t.sin().max(0.0).powf(exp);
        pb.line_to(x0 + r - qx, y0 + r - qy);
    }
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
    // RGBA -> BGRA as one u32 swap of the R and B bytes (vectorizes well;
    // full-screen surfaces are tens of MB per frame on HiDPI)
    for (dst, src) in canvas.chunks_exact_mut(4).zip(pm.data().chunks_exact(4)) {
        let v = u32::from_le_bytes([src[0], src[1], src[2], src[3]]);
        let v = (v & 0xFF00_FF00) | ((v & 0xFF) << 16) | ((v >> 16) & 0xFF);
        dst.copy_from_slice(&v.to_le_bytes());
    }
}

#[cfg(test)]
mod blit_tests {
    #[test]
    fn swaps_red_and_blue() {
        let mut pm = tiny_skia::Pixmap::new(2, 1).unwrap();
        pm.data_mut().copy_from_slice(&[1, 2, 3, 4, 10, 20, 30, 40]);
        let mut out = [0u8; 8];
        super::blit_to_shm(&pm, &mut out);
        assert_eq!(out, [3, 2, 1, 4, 30, 20, 10, 40]);
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

/// Source-over `src` onto `dst` (both premultiplied RGBA), scaled by `k`
/// with its top-left corner at (`x`, `y`), faded by `opacity`. Much cheaper
/// than tiny-skia's generic pipeline for big layers: an unscaled, pixel
/// aligned layer is a straight blend, a scaled one is sampled bilinearly
/// in fixed point, and fully transparent source pixels are skipped.
pub fn composite(dst: &mut Pixmap, src: &Pixmap, x: f32, y: f32, k: f32, opacity: f32) {
    let op = (opacity.clamp(0.0, 1.0) * 256.0).round() as u32;
    if op == 0 || k <= 0.0 || !x.is_finite() || !y.is_finite() {
        return;
    }
    let (dw, dh) = (dst.width() as i32, dst.height() as i32);
    let (sw, sh) = (src.width() as i32, src.height() as i32);
    let x0 = (x.floor() as i32).max(0);
    let y0 = (y.floor() as i32).max(0);
    let x1 = ((x + sw as f32 * k).ceil() as i32).min(dw);
    let y1 = ((y + sh as f32 * k).ceil() as i32).min(dh);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let s = src.data();
    let d = dst.data_mut();
    let aligned = (k - 1.0).abs() < 1e-4 && x.fract() == 0.0 && y.fract() == 0.0;

    #[inline(always)]
    fn over(d: &mut [u8], c: [u32; 4]) {
        let ia = 255 - c[3];
        for i in 0..4 {
            let t = d[i] as u32 * ia + 128;
            d[i] = (c[i] + ((t + (t >> 8)) >> 8)).min(255) as u8;
        }
    }

    if aligned {
        let (ox, oy) = (x as i32, y as i32);
        for dy in y0..y1 {
            let sy = dy - oy;
            let srow = &s[(sy * sw) as usize * 4..][..sw as usize * 4];
            let drow = &mut d[(dy * dw) as usize * 4..][..dw as usize * 4];
            for dx in x0..x1 {
                let si = (dx - ox) as usize * 4;
                let a = srow[si + 3] as u32;
                if a == 0 {
                    continue;
                }
                let di = dx as usize * 4;
                let px = &srow[si..si + 4];
                if a == 255 && op == 256 {
                    drow[di..di + 4].copy_from_slice(px);
                    continue;
                }
                let c = [
                    px[0] as u32 * op >> 8,
                    px[1] as u32 * op >> 8,
                    px[2] as u32 * op >> 8,
                    a * op >> 8,
                ];
                over(&mut drow[di..di + 4], c);
            }
        }
        return;
    }

    // bilinear: taps outside `src` are transparent, so the edges stay smooth
    let inv = 1.0 / k;
    // per-column source index and weight, the same for every row
    let cols: Vec<(i32, u32)> = (x0..x1)
        .map(|dx| {
            let fx = (dx as f32 + 0.5 - x) * inv - 0.5;
            let ix = fx.floor();
            (ix as i32, ((fx - ix) * 256.0) as u32)
        })
        .collect();
    let row_of = |iy: i32| -> &[u8] {
        if iy < 0 || iy >= sh {
            &[]
        } else {
            &s[(iy * sw) as usize * 4..][..sw as usize * 4]
        }
    };
    #[inline(always)]
    fn px(row: &[u8], ix: i32) -> u32 {
        if ix < 0 || (ix as usize + 1) * 4 > row.len() {
            0
        } else {
            let i = ix as usize * 4;
            u32::from_le_bytes([row[i], row[i + 1], row[i + 2], row[i + 3]])
        }
    }
    // weighted sum of two RGBA pixels, two channels at a time (8.8 fixed point)
    #[inline(always)]
    fn lerp(a: u32, b: u32, w: u32) -> u32 {
        let (iw, mask) = (256 - w, 0x00FF_00FF);
        let rb = ((a & mask) * iw + (b & mask) * w) >> 8 & mask;
        let ga = (((a >> 8) & mask) * iw + ((b >> 8) & mask) * w) & !mask;
        rb | ga
    }
    for dy in y0..y1 {
        let fy = (dy as f32 + 0.5 - y) * inv - 0.5;
        let iy = fy.floor();
        let wy = ((fy - iy) * 256.0) as u32;
        let (r0, r1) = (row_of(iy as i32), row_of(iy as i32 + 1));
        if r0.is_empty() && r1.is_empty() {
            continue;
        }
        let drow = &mut d[(dy * dw) as usize * 4..][..dw as usize * 4];
        for (dx, &(ix, wx)) in (x0..x1).zip(cols.iter()) {
            let top = lerp(px(r0, ix), px(r0, ix + 1), wx);
            let bot = lerp(px(r1, ix), px(r1, ix + 1), wx);
            let mut v = lerp(top, bot, wy);
            if v >> 24 == 0 {
                continue;
            }
            if op < 256 {
                v = lerp(0, v, op);
            }
            let b = v.to_le_bytes();
            let di = dx as usize * 4;
            over(&mut drow[di..di + 4], [b[0] as u32, b[1] as u32, b[2] as u32, b[3] as u32]);
        }
    }
}

#[cfg(test)]
mod composite_tests {
    use tiny_skia::Pixmap;

    fn solid(w: u32, h: u32, px: [u8; 4]) -> Pixmap {
        let mut pm = Pixmap::new(w, h).unwrap();
        for c in pm.data_mut().chunks_exact_mut(4) {
            c.copy_from_slice(&px);
        }
        pm
    }

    #[test]
    fn aligned_copy_and_blend() {
        let mut dst = solid(4, 4, [0, 0, 255, 255]);
        super::composite(&mut dst, &solid(2, 2, [255, 0, 0, 255]), 1.0, 1.0, 1.0, 1.0);
        assert_eq!(&dst.data()[(4 + 1) * 4..][..4], &[255, 0, 0, 255]);
        assert_eq!(&dst.data()[..4], &[0, 0, 255, 255], "outside untouched");
        // half-transparent premultiplied red over blue
        let mut dst = solid(1, 1, [0, 0, 255, 255]);
        super::composite(&mut dst, &solid(1, 1, [128, 0, 0, 128]), 0.0, 0.0, 1.0, 1.0);
        let p = &dst.data()[..4];
        assert!(p[0] == 128 && (126..=128).contains(&p[2]) && p[3] == 255, "{p:?}");
    }

    #[test]
    fn scaled_and_faded_stays_in_bounds() {
        let mut dst = Pixmap::new(10, 10).unwrap();
        super::composite(&mut dst, &solid(8, 8, [255, 255, 255, 255]), -3.0, 5.5, 0.9, 0.5);
        let centre = &dst.data()[(7 * 10 + 1) * 4..][..4];
        assert!((120..=130).contains(&centre[3]), "{centre:?}");
        assert!(dst.data().chunks_exact(4).all(|p| p[0] <= p[3]), "stays premultiplied");
    }
}
