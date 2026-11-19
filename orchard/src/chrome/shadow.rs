//! macOS-style soft window shadows as a nine-slice texture.
//!
//! A blurred, vertically offset rounded rectangle is rendered once per
//! (scale, active) on the CPU. The window interior is cut out so translucent
//! or rounded window corners never show a dark core. The texture is sliced
//! into four corners and four 1px edge strips that get stretched to any window
//! size.

use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Transform};

use super::theme::corner_radius;

#[derive(Debug, Clone, Copy)]
pub struct ShadowStyle {
    /// gaussian sigma (logical px)
    pub sigma: f32,
    /// vertical offset (logical px)
    pub dy: f32,
    /// opacity of the soft shadow
    pub alpha: f32,
    /// opacity of a tight 1px-ish contact shadow that outlines the window
    pub contact_alpha: f32,
}

pub const ACTIVE: ShadowStyle = ShadowStyle {
    sigma: 20.0,
    dy: 14.0,
    alpha: 0.75,
    contact_alpha: 0.35,
};

pub const INACTIVE: ShadowStyle = ShadowStyle {
    sigma: 11.0,
    dy: 7.0,
    alpha: 0.45,
    contact_alpha: 0.25,
};

/// Rects are (x, y, w, h) in physical px.
#[derive(Debug, Clone)]
pub struct Slice {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct NineSlice {
    /// distance the shadow extends past the window on every side (physical px)
    pub pad: i32,
    /// size of a corner piece (physical px)
    pub border: i32,
    /// [tl, tr, bl, br, top, bottom, left, right]
    pub slices: [Slice; 8],
}

fn gaussian_blur_alpha(a: &mut [f32], w: usize, h: usize, sigma: f32) {
    if sigma < 0.5 {
        return;
    }
    // three box blurs approximate a gaussian
    let n = 3;
    let w_ideal = (12.0 * sigma * sigma / n as f32 + 1.0).sqrt();
    let mut wl = w_ideal.floor() as i32;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wu = wl + 2;
    let m_ideal = (12.0 * sigma * sigma - (n * wl * wl) as f32 - (4 * n * wl) as f32 - 3.0 * n as f32)
        / (-4 * wl - 4) as f32;
    let m = m_ideal.round() as i32;
    let mut tmp = vec![0.0f32; a.len()];
    for i in 0..n {
        let r = (if i < m { wl } else { wu } - 1) / 2;
        box_h(a, &mut tmp, w, h, r as usize);
        box_v(&tmp, a, w, h, r as usize);
    }
}

fn box_h(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize) {
    let norm = 1.0 / (2 * r + 1) as f32;
    for y in 0..h {
        let row = &src[y * w..(y + 1) * w];
        let mut acc = 0.0;
        for x in 0..=r.min(w - 1) {
            acc += row[x];
        }
        for x in 0..w {
            dst[y * w + x] = acc * norm;
            let add = x + r + 1;
            if add < w {
                acc += row[add];
            }
            if x >= r {
                acc -= row[x - r];
            }
        }
    }
}

fn box_v(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize) {
    let norm = 1.0 / (2 * r + 1) as f32;
    for x in 0..w {
        let mut acc = 0.0;
        for y in 0..=r.min(h - 1) {
            acc += src[y * w + x];
        }
        for y in 0..h {
            dst[y * w + x] = acc * norm;
            let add = y + r + 1;
            if add < h {
                acc += src[add * w + x];
            }
            if y >= r {
                acc -= src[(y - r) * w + x];
            }
        }
    }
}

fn rounded_rect_mask(size: u32, x: f32, y: f32, wh: f32, r: f32) -> Vec<f32> {
    let mut pm = Pixmap::new(size, size).unwrap();
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + wh - r, y);
    pb.quad_to(x + wh, y, x + wh, y + r);
    pb.line_to(x + wh, y + wh - r);
    pb.quad_to(x + wh, y + wh, x + wh - r, y + wh);
    pb.line_to(x + r, y + wh);
    pb.quad_to(x, y + wh, x, y + wh - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    let path = pb.finish().unwrap();
    let mut paint = Paint::default();
    paint.set_color_rgba8(0, 0, 0, 255);
    paint.anti_alias = true;
    pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    pm.pixels().iter().map(|p| p.alpha() as f32 / 255.0).collect()
}

pub fn build(style: ShadowStyle, scale: f64) -> NineSlice {
    let s = scale as f32;
    let sigma = style.sigma * s;
    let dy = (style.dy * s).round();
    let r = corner_radius() * s;
    let extent = (3.0 * sigma).ceil();
    // shadow reaches `extent` past the (offset) window on each side
    let pad = (extent + dy).ceil() as i32;
    // the inner square must be large enough that its middle row/column are
    // unaffected by the corners in both the offset shadow and the cut-out
    let inner = (2.0 * (extent + dy + r)).ceil() as i32 + 1;
    let size = (2 * pad + inner) as u32;
    let border = (size / 2) as i32; // middle pixel is the stretchable strip

    let px = pad as f32;
    let mut shadow = rounded_rect_mask(size, px, px + dy, inner as f32, r);
    for v in shadow.iter_mut() {
        *v *= style.alpha;
    }
    gaussian_blur_alpha(&mut shadow, size as usize, size as usize, sigma);

    let mut contact = rounded_rect_mask(size, px - 0.5 * s, px - 0.5 * s, inner as f32 + s, r + 0.5 * s);
    gaussian_blur_alpha(&mut contact, size as usize, size as usize, 0.8 * s);

    let cutout = rounded_rect_mask(size, px, px, inner as f32, r);

    let n = size as usize;
    let mut rgba = vec![0u8; n * n * 4];
    for i in 0..n * n {
        let a_soft = shadow[i];
        let a_contact = contact[i] * style.contact_alpha;
        let a = 1.0 - (1.0 - a_soft) * (1.0 - a_contact);
        let a = a * (1.0 - cutout[i]);
        // black, premultiplied => only alpha is non-zero
        rgba[i * 4 + 3] = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
    }

    let b = border as u32;
    let crop = |x: u32, y: u32, w: u32, h: u32| -> Slice {
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in y..y + h {
            let start = ((row * size + x) * 4) as usize;
            out.extend_from_slice(&rgba[start..start + (w * 4) as usize]);
        }
        Slice { w, h, rgba: out }
    };
    let far = b + 1;
    let slices = [
        crop(0, 0, b, b),
        crop(far, 0, size - far, b),
        crop(0, far, b, size - far),
        crop(far, far, size - far, size - far),
        crop(b, 0, 1, b),
        crop(b, far, 1, size - far),
        crop(0, b, b, 1),
        crop(far, b, size - far, 1),
    ];

    NineSlice { pad, border, slices }
}

/// Destination rectangles (x, y, w, h) relative to the window's top-left, in
/// physical px, for each slice, given the window size in physical px.
pub fn layout(ns: &NineSlice, win_w: i32, win_h: i32) -> [(i32, i32, i32, i32); 8] {
    let p = ns.pad;
    let b = ns.border;
    let total_w = win_w + 2 * p;
    let total_h = win_h + 2 * p;
    // for tiny windows the corners would overlap; shrink the stretch to zero
    let mid_w = (total_w - 2 * b).max(0);
    let mid_h = (total_h - 2 * b).max(0);
    let x0 = -p;
    let y0 = -p;
    let xr = x0 + b + mid_w;
    let yb = y0 + b + mid_h;
    [
        (x0, y0, b, b),
        (xr, y0, b, b),
        (x0, yb, b, b),
        (xr, yb, b, b),
        (x0 + b, y0, mid_w, b),
        (x0 + b, yb, mid_w, b),
        (x0, y0 + b, b, mid_h),
        (xr, y0 + b, b, mid_h),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nine_slice_is_consistent() {
        let ns = build(ACTIVE, 1.0);
        let b = ns.border as u32;
        for s in &ns.slices[0..4] {
            assert_eq!((s.w, s.h), (b, b));
        }
        // interior near the window centre is cut out entirely
        let tl = &ns.slices[0];
        let last = ((tl.h - 1) * tl.w + (tl.w - 1)) as usize * 4 + 3;
        assert_eq!(tl.rgba[last], 0);
        // the shadow is darker below the window than above it
        let top = &ns.slices[4];
        let bottom = &ns.slices[5];
        let top_max = top.rgba.chunks(4).map(|p| p[3]).max().unwrap();
        let bottom_max = bottom.rgba.chunks(4).map(|p| p[3]).max().unwrap();
        assert!(bottom_max > top_max, "{bottom_max} > {top_max}");
    }

    #[test]
    fn layout_covers_window() {
        let ns = build(INACTIVE, 1.0);
        let l = layout(&ns, 800, 600);
        let right = l[1].0 + l[1].2;
        let bottom = l[2].1 + l[2].3;
        assert_eq!(right, 800 + ns.pad);
        assert_eq!(bottom, 600 + ns.pad);
    }
}
