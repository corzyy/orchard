//! Render a window (including title bar and shadow) into an offscreen texture.
//! Used for close / minimize animations, where the client surface goes away
//! (or is unmapped) before the animation ends.

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker,
            element::AsRenderElements,
            gles::{GlesRenderer, GlesTexture},
            Bind, Color32F, Frame, Offscreen, Renderer,
        },
    },
    desktop::space::SpaceElement,
    utils::{Logical, Physical, Point, Rectangle, Scale, Size, Transform},
};

use crate::shell::{WindowElement, WindowRenderElement};

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub texture: GlesTexture,
    /// size of the texture in physical px
    pub size: Size<i32, Physical>,
    /// area the texture covers, relative to the surface origin
    /// (i.e. `element_location - geometry().loc`), logical
    pub bbox: Rectangle<i32, Logical>,
    pub scale: f64,
}

pub fn snapshot_window(renderer: &mut GlesRenderer, window: &WindowElement, scale: f64) -> Option<Snapshot> {
    let bbox = SpaceElement::bbox(window);
    if bbox.is_empty() {
        return None;
    }
    let size: Size<i32, Physical> = bbox.size.to_physical_precise_ceil(scale);
    let buf_size = (size.w, size.h).into();
    let mut texture: GlesTexture = renderer
        .create_buffer(Fourcc::Abgr8888, buf_size)
        .map_err(|e| tracing::warn!("snapshot buffer: {e:?}"))
        .ok()?;
    {
        let mut target = renderer
            .bind(&mut texture)
            .map_err(|e| tracing::warn!("snapshot bind: {e:?}"))
            .ok()?;
        // Clear freshly allocated VRAM (prevents NVIDIA white-dot speckles).
        if let Ok(mut frame) = renderer.render(&mut target, size, Transform::Normal) {
            let _ = frame.clear(Color32F::TRANSPARENT, &[Rectangle::from_size(size)]);
            let _ = frame.finish();
        }
        // render origin is the surface origin; bbox is relative to it
        let origin: Point<i32, Physical> =
            (Point::<i32, Logical>::from((0, 0)) - bbox.loc).to_physical_precise_round(scale);
        let elements: Vec<WindowRenderElement> =
            AsRenderElements::<GlesRenderer>::render_elements(window, renderer, origin, Scale::from(scale), 1.0);
        let mut tracker = OutputDamageTracker::new(size, scale, Transform::Normal);
        let res = tracker.render_output(renderer, &mut target, 0, &elements, Color32F::TRANSPARENT);
        if let Err(e) = res {
            tracing::warn!("snapshot render failed: {e:?}");
            return None;
        }
    }
    Some(Snapshot {
        texture,
        size,
        bbox,
        scale,
    })
}

/// A small picture of a window (title bar included, no shadow) for its
/// minimized Dock tile: straight-alpha RGBA, at most `max_px` on its long side.
pub fn thumbnail_rgba(renderer: &mut GlesRenderer, window: &WindowElement, max_px: u32) -> Option<(Vec<u8>, u32, u32)> {
    use smithay::backend::renderer::ExportMem;
    let geo = SpaceElement::geometry(window);
    if geo.is_empty() {
        return None;
    }
    // render at twice the final size, then filter down: sharper than
    // letting GL minify the surfaces
    let long = geo.size.w.max(geo.size.h) as f64;
    let scale = (2.0 * max_px as f64 / long).min(2.0);
    let size: Size<i32, Physical> = geo.size.to_physical_precise_ceil(scale);
    if size.w <= 0 || size.h <= 0 {
        return None;
    }
    let buf_size: Size<i32, smithay::utils::Buffer> = (size.w, size.h).into();
    let mut texture: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, buf_size).ok()?;
    let mut data = {
        let mut target = renderer.bind(&mut texture).ok()?;
        if let Ok(mut frame) = renderer.render(&mut target, size, Transform::Normal) {
            let _ = frame.clear(Color32F::TRANSPARENT, &[Rectangle::from_size(size)]);
            let _ = frame.finish();
        }
        let origin: Point<i32, Physical> = (Point::<i32, Logical>::from((0, 0)) - geo.loc).to_physical_precise_round(scale);
        let elements: Vec<WindowRenderElement> =
            AsRenderElements::<GlesRenderer>::render_elements(window, renderer, origin, Scale::from(scale), 1.0);
        let mut tracker = OutputDamageTracker::new(size, scale, Transform::Normal);
        tracker
            .render_output(renderer, &mut target, 0, &elements, Color32F::TRANSPARENT)
            .map_err(|e| tracing::warn!("thumbnail render failed: {e:?}"))
            .ok()?;
        let mapping = renderer
            .copy_framebuffer(&target, Rectangle::from_size(buf_size), Fourcc::Abgr8888)
            .ok()?;
        renderer.map_texture(&mapping).ok()?.to_vec()
    };
    // GL gives premultiplied alpha; PNG wants straight alpha
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    let img = image::RgbaImage::from_raw(size.w as u32, size.h as u32, data)?;
    let k = max_px as f64 / long;
    let (w, h) = (((geo.size.w as f64 * k).round() as u32).max(1), ((geo.size.h as f64 * k).round() as u32).max(1));
    let small = image::imageops::resize(&img, w, h, image::imageops::FilterType::Triangle);
    Some((small.into_raw(), w, h))
}
