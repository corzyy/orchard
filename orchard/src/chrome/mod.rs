//! Orchard's compositor-drawn window chrome: macOS-style title bars and shadows.

pub mod font;
pub mod shadow;
pub mod theme;
pub mod titlebar;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

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

/// Shadows are expensive to blur, so they are generated once per (scale, active).
fn nine_slice(scale: f64, active: bool) -> Arc<NineSlice> {
    static CACHE: OnceLock<Mutex<HashMap<(u32, bool, u32), Arc<NineSlice>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let mut cache = cache.lock().unwrap();
    cache
        .entry((scale_key(scale), active, theme::corner_radius().to_bits()))
        .or_insert_with(|| {
            let style = if active { shadow::ACTIVE } else { shadow::INACTIVE };
            Arc::new(shadow::build(style, scale))
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
    shadow: Option<((u32, bool, u32), ShadowBuffers)>,
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
            let nine = nine_slice(scale, active);
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
            self.shadow = Some((key, ShadowBuffers { nine, buffers }));
        }
        &self.shadow.as_ref().unwrap().1
    }
}
