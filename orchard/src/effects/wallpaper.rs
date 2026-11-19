//! Desktop wallpaper drawn by the compositor, plus a pre-blurred copy used for
//! translucent "frosted glass" surfaces (menu bar, Dock, sidebars).
//!
//! The image is `ORCHARD_WALLPAPER`, the one chosen in Orchard's settings, or a
//! neutral gradient.

use std::{collections::HashMap, path::PathBuf};

use image::{imageops::FilterType, DynamicImage, RgbaImage};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{texture::TextureBuffer, texture::TextureRenderElement, Id, Kind},
            gles::{GlesRenderer, GlesTexture},
        },
    },
    utils::{Logical, Physical, Point, Rectangle, Size, Transform},
};

/// Factor by which the blurred copy is downscaled before blurring.
const BLUR_DOWNSCALE: u32 = 8;
/// Gaussian sigma applied at the downscaled resolution.
const BLUR_SIGMA: f32 = 3.5;

pub fn configured_path() -> Option<PathBuf> {
    std::env::var_os("ORCHARD_WALLPAPER")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .or_else(|| {
            crate::settings::get()
                .desktop
                .wallpaper
                .as_deref()
                .map(|p| PathBuf::from(p.trim_start_matches("file://")))
                .filter(|p| p.is_file())
        })
}

fn fallback_gradient(w: u32, h: u32) -> RgbaImage {
    // soft blue/purple diagonal gradient
    RgbaImage::from_fn(w, h, |x, y| {
        let t = (x as f32 / w as f32 * 0.6 + y as f32 / h as f32 * 0.4).clamp(0.0, 1.0);
        let a = [64.0, 92.0, 160.0];
        let b = [168.0, 112.0, 176.0];
        let c = |i: usize| (a[i] + (b[i] - a[i]) * t) as u8;
        image::Rgba([c(0), c(1), c(2), 255])
    })
}

/// Scale-to-fill and center-crop like macOS "Fill Screen".
fn fill(img: &DynamicImage, w: u32, h: u32) -> RgbaImage {
    let (iw, ih) = (img.width() as f32, img.height() as f32);
    let scale = (w as f32 / iw).max(h as f32 / ih);
    let sw = (iw * scale).ceil() as u32;
    let sh = (ih * scale).ceil() as u32;
    let resized = img.resize_exact(sw, sh, FilterType::Triangle);
    let x = (sw.saturating_sub(w)) / 2;
    let y = (sh.saturating_sub(h)) / 2;
    resized.crop_imm(x, y, w, h).to_rgba8()
}

pub struct WallpaperTextures {
    pub sharp: TextureBuffer<GlesTexture>,
    pub sharp_texture: GlesTexture,
    pub blurred: GlesTexture,
    pub blurred_size: Size<i32, smithay::utils::Buffer>,
    pub size: Size<i32, Physical>,
}

#[derive(Default)]
pub struct Wallpaper {
    source: Option<Option<DynamicImage>>,
    path: Option<PathBuf>,
    cache: HashMap<(i32, i32), WallpaperTextures>,
}

impl Wallpaper {
    /// Reload the image on next use, if the configured path changed.
    pub fn reset(&mut self) {
        let path = configured_path();
        if path != self.path {
            self.path = path;
            self.source = None;
            self.cache.clear();
        }
    }

    fn source(&mut self) -> Option<&DynamicImage> {
        if self.source.is_none() {
            self.path = configured_path();
            let img = self.path.clone().and_then(|p| match image::open(&p) {
                Ok(img) => {
                    tracing::info!("wallpaper: {}", p.display());
                    Some(img)
                }
                Err(e) => {
                    tracing::warn!("failed to load wallpaper {}: {e}", p.display());
                    None
                }
            });
            self.source = Some(img);
        }
        self.source.as_ref().unwrap().as_ref()
    }

    pub fn textures(&mut self, renderer: &mut GlesRenderer, size: Size<i32, Physical>) -> Option<&WallpaperTextures> {
        let key = (size.w, size.h);
        if !self.cache.contains_key(&key) {
            let (w, h) = (size.w.max(1) as u32, size.h.max(1) as u32);
            let sharp_img = match self.source() {
                Some(src) => fill(src, w, h),
                None => fallback_gradient(w, h),
            };
            let bw = (w / BLUR_DOWNSCALE).max(1);
            let bh = (h / BLUR_DOWNSCALE).max(1);
            let small = image::imageops::resize(&sharp_img, bw, bh, FilterType::Triangle);
            let blurred_img = image::imageops::blur(&small, BLUR_SIGMA);

            use smithay::backend::renderer::ImportMem;
            let sharp_texture = renderer
                .import_memory(sharp_img.as_raw(), Fourcc::Abgr8888, (w as i32, h as i32).into(), false)
                .map_err(|e| tracing::warn!("wallpaper upload failed: {e:?}"))
                .ok()?;
            let sharp = TextureBuffer::from_texture(
                renderer,
                sharp_texture.clone(),
                1,
                Transform::Normal,
                Some(vec![Rectangle::from_size((w as i32, h as i32).into())]),
            );
            let blurred = renderer
                .import_memory(blurred_img.as_raw(), Fourcc::Abgr8888, (bw as i32, bh as i32).into(), false)
                .map_err(|e| tracing::warn!("blurred wallpaper upload failed: {e:?}"))
                .ok()?;
            self.cache.insert(
                key,
                WallpaperTextures {
                    sharp,
                    sharp_texture,
                    blurred,
                    blurred_size: (bw as i32, bh as i32).into(),
                    size,
                },
            );
        }
        self.cache.get(&key)
    }
}

impl WallpaperTextures {
    /// Full-screen wallpaper element.
    pub fn element(&self, scale: f64) -> TextureRenderElement<GlesTexture> {
        let logical: Size<i32, Logical> = self.size.to_f64().to_logical(scale).to_i32_round();
        TextureRenderElement::from_texture_buffer(
            (0.0, 0.0),
            &self.sharp,
            None,
            None,
            Some(logical),
            Kind::Unspecified,
        )
    }

    /// The blurred wallpaper cropped to `region` (output-relative, physical px).
    /// `id` must be stable per consumer so damage tracking works.
    pub fn blurred_region(
        &self,
        renderer: &GlesRenderer,
        id: &Id,
        region: Rectangle<i32, Physical>,
        scale: f64,
        alpha: f32,
    ) -> TextureRenderElement<GlesTexture> {
        use smithay::backend::renderer::Renderer;
        let fx = self.blurred_size.w as f64 / self.size.w as f64;
        let fy = self.blurred_size.h as f64 / self.size.h as f64;
        let src = Rectangle::<f64, Logical>::new(
            (region.loc.x as f64 * fx, region.loc.y as f64 * fy).into(),
            (region.size.w as f64 * fx, region.size.h as f64 * fy).into(),
        );
        let dst_size: Size<i32, Logical> = region.size.to_f64().to_logical(scale).to_i32_round();
        TextureRenderElement::from_static_texture(
            id.clone(),
            renderer.context_id(),
            Point::<f64, Physical>::from((region.loc.x as f64, region.loc.y as f64)),
            self.blurred.clone(),
            1,
            Transform::Normal,
            Some(alpha),
            Some(src),
            Some(dst_size),
            None,
            Kind::Unspecified,
        )
    }
}
