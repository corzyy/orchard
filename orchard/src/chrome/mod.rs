//! Orchard's compositor-drawn window chrome: macOS-style title bars and shadows.

pub mod font;
pub mod shadow;
pub mod theme;
pub mod tile_menu;
pub mod titlebar;

/// Superellipse exponent shared between CPU-painted chrome (titlebar, shadow)
/// and the GPU `rounded.frag` shader so top and bottom corners match identically.
pub const SQUIRCLE_N: f32 = 2.6;

/// Number of line segments per squircle corner (chord error < 0.03px at r = 24).
const SQUIRCLE_SEGMENTS: usize = 16;

/// Build a path with continuous superellipse top corners and straight bottom
/// edge from `(x0, y0)` to `(x1, y1)`. When `close` is false, the path stays
/// open at the bottom (left -> top-left -> top -> top-right -> right) so
/// stroking it never draws a line across the bottom seam.
pub fn squircle_top_path(
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    r: f32,
    close: bool,
) -> Option<tiny_skia::Path> {
    use std::f32::consts::FRAC_PI_2;
    let w = (x1 - x0).max(0.0);
    let h = (y1 - y0).max(0.0);
    let r = r.clamp(0.0, (w * 0.5).min(h));
    let mut pb = tiny_skia::PathBuilder::new();
    pb.move_to(x0, y1);
    if r <= 0.5 {
        pb.line_to(x0, y0);
        pb.line_to(x1, y0);
        pb.line_to(x1, y1);
    } else {
        let exp = 2.0 / SQUIRCLE_N;
        pb.line_to(x0, y0 + r);
        for i in 1..=SQUIRCLE_SEGMENTS {
            let t = FRAC_PI_2 * (i as f32 / SQUIRCLE_SEGMENTS as f32);
            let qx = r * t.cos().max(0.0).powf(exp);
            let qy = r * t.sin().max(0.0).powf(exp);
            pb.line_to(x0 + r - qx, y0 + r - qy);
        }
        pb.line_to(x1 - r, y0);
        for i in 1..=SQUIRCLE_SEGMENTS {
            let t = FRAC_PI_2 * (i as f32 / SQUIRCLE_SEGMENTS as f32);
            let qx = r * t.sin().max(0.0).powf(exp);
            let qy = r * t.cos().max(0.0).powf(exp);
            pb.line_to(x1 - r + qx, y0 + r - qy);
        }
        pb.line_to(x1, y1);
    }
    if close {
        pb.close();
    }
    pb.finish()
}

/// Build a closed path for a rectangle with all four continuous superellipse
/// corners matching `rounded.frag`.
pub fn squircle_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    use std::f32::consts::FRAC_PI_2;
    let x0 = x;
    let y0 = y;
    let x1 = x + w.max(0.0);
    let y1 = y + h.max(0.0);
    let r = r.clamp(0.0, (w * 0.5).min(h * 0.5).max(0.0));
    let mut pb = tiny_skia::PathBuilder::new();
    if r <= 0.5 {
        pb.move_to(x0, y0);
        pb.line_to(x1, y0);
        pb.line_to(x1, y1);
        pb.line_to(x0, y1);
        pb.close();
        return pb.finish();
    }
    let exp = 2.0 / SQUIRCLE_N;
    // Start at top-left after corner, go clockwise
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

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
};

static ALT_HELD: AtomicBool = AtomicBool::new(false);

pub fn set_alt_held(held: bool) {
    ALT_HELD.store(held, Ordering::Relaxed);
}

pub fn alt_held() -> bool {
    ALT_HELD.load(Ordering::Relaxed)
}

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::element::memory::MemoryRenderBuffer,
    },
    utils::Transform,
};

use self::{
    shadow::NineSlice,
    titlebar::{paint_titlebar, TitlebarParams},
};

fn scale_key(scale: f64) -> u32 {
    (scale * 1000.0).round() as u32
}

/// Shadows are expensive to blur and upload, so both the NineSlice and its 8
/// `MemoryRenderBuffer`s are shared globally across all windows per
/// `(scale, active, corner_radius)`. This eliminates N× shadow RAM/VRAM
/// duplication and avoids re-uploading 8 textures on every focus change.
fn shared_shadow(scale: f64, active: bool) -> Arc<ShadowBuffers> {
    static CACHE: OnceLock<Mutex<HashMap<(u32, bool, u32), Arc<ShadowBuffers>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let mut cache = cache.lock().unwrap();
    cache
        .entry((scale_key(scale), active, theme::corner_radius().to_bits()))
        .or_insert_with(|| {
            let style = if active { shadow::ACTIVE } else { shadow::INACTIVE };
            let nine = Arc::new(shadow::build(style, scale));
            let buffers = nine
                .slices
                .iter()
                .map(|s| {
                    MemoryRenderBuffer::from_slice(
                        &s.rgba,
                        Fourcc::Abgr8888,
                        (s.w as i32, s.h as i32),
                        1,
                        Transform::Normal,
                        None,
                    )
                })
                .collect();
            Arc::new(ShadowBuffers { nine, buffers })
        })
        .clone()
}

/// Largest distance (logical px) a shadow extends past a window, used for bbox/damage.
pub fn shadow_extent_logical() -> i32 {
    let s = shadow::ACTIVE;
    (3.0 * s.sigma + s.dy).ceil() as i32 + 1
}

pub struct ShadowBuffers {
    pub nine: Arc<NineSlice>,
    pub buffers: Vec<MemoryRenderBuffer>,
}

/// Per-window cached render buffers.
#[derive(Default)]
pub struct WindowChrome {
    titlebar: Option<(TitlebarParams, MemoryRenderBuffer)>,
    shadow: Option<((u32, bool, u32), Arc<ShadowBuffers>)>,
}

impl std::fmt::Debug for WindowChrome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowChrome").finish_non_exhaustive()
    }
}

impl WindowChrome {
    pub fn titlebar(&mut self, params: TitlebarParams) -> Option<&MemoryRenderBuffer> {
        let fresh = !matches!(&self.titlebar, Some((p, _)) if *p == params);
        if fresh {
            let pm = paint_titlebar(&params)?;
            let buf = MemoryRenderBuffer::from_slice(
                pm.data(),
                Fourcc::Abgr8888,
                (pm.width() as i32, pm.height() as i32),
                1,
                Transform::Normal,
                None,
            );
            self.titlebar = Some((params, buf));
        }
        self.titlebar.as_ref().map(|(_, b)| b)
    }

    pub fn shadow(&mut self, scale: f64, active: bool) -> &ShadowBuffers {
        let key = (scale_key(scale), active, theme::corner_radius().to_bits());
        let fresh = !matches!(&self.shadow, Some((k, _)) if *k == key);
        if fresh {
            self.shadow = Some((key, shared_shadow(scale, active)));
        }
        &self.shadow.as_ref().unwrap().1
    }
}
