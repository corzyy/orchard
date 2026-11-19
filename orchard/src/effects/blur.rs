//! Live frosted glass: whatever is drawn below a backdrop (windows, other
//! panels, the wallpaper) is rendered offscreen and blurred on the GPU with a
//! dual Kawase blur.
//!
//! Only the pixels inside the backdrop are blurred (samples are clamped to
//! its rect, edges extended), so a window next to a panel never tints it.
//!
//! Each backdrop keeps its textures and a fingerprint of the elements that
//! were below it (id, commit, geometry, alpha). The scene is only re-rendered
//! when that fingerprint changes, and the (cheap) blur passes only when it or
//! the backdrop rect changes, so a static desktop costs nothing per frame.

use std::{collections::HashMap, time::Duration};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker,
            element::{
                texture::TextureRenderElement,
                utils::{Relocate, RelocateRenderElement},
                Element, Id, Kind, RenderElement,
            },
            gles::{GlesError, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName, UniformType},
            utils::{CommitCounter, DamageBag},
            Bind, Color32F, Frame, Offscreen, Renderer,
        },
    },
    utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform},
};

const SHADER: &str = include_str!("shaders/kawase.frag");

/// Downsample/upsample passes at output scale 1 (each halves the resolution).
const ITERATIONS: u32 = 4;
/// Sample offset of each pass, in texels of the level being read.
const OFFSET: f32 = 2.5;
/// Extra area rendered around a backdrop, so it can grow or move a little
/// (Dock magnification) without re-rendering.
const SLACK: i32 = 160;

struct KawaseProgram(GlesTexProgram);

fn program(renderer: &mut GlesRenderer) -> Option<GlesTexProgram> {
    if let Some(p) = renderer.egl_context().user_data().get::<KawaseProgram>() {
        return Some(p.0.clone());
    }
    let program = renderer
        .compile_custom_texture_shader(
            SHADER,
            &[
                UniformName::new("halfpixel", UniformType::_2f),
                UniformName::new("up", UniformType::_1f),
                UniformName::new("clampbox", UniformType::_4f),
            ],
        )
        .map_err(|e| tracing::error!("failed to compile blur shader: {e:?}"))
        .ok()?;
    renderer
        .egl_context()
        .user_data()
        .insert_if_missing(|| KawaseProgram(program.clone()));
    Some(program)
}

/// Live blur can be switched off with ORCHARD_NO_LIVE_BLUR=1 (falls back to
/// the pre-blurred wallpaper).
pub fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("ORCHARD_NO_LIVE_BLUR").is_none())
}

type Fingerprint = Vec<(Id, CommitCounter, Rectangle<i32, Physical>, u32)>;

struct Cache {
    /// output-local physical area that was rendered (backdrop + slack)
    padded: Rectangle<i32, Physical>,
    scene: GlesTexture,
    /// level 0 is half the padded size and holds the result; only the
    /// top-left part matching the backdrop (`used`) is drawn
    levels: Vec<(GlesTexture, Size<i32, Buffer>)>,
    /// the backdrop the levels were blurred for, and level 0's used size
    region: Rectangle<i32, Physical>,
    used: Size<i32, Buffer>,
    fingerprint: Fingerprint,
    damage: DamageBag<i32, Buffer>,
    last_used: Duration,
}

#[derive(Default)]
pub struct LiveBlur {
    cache: HashMap<Id, Cache>,
}

impl std::fmt::Debug for LiveBlur {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveBlur").field("backdrops", &self.cache.len()).finish()
    }
}

fn texture(renderer: &mut GlesRenderer, size: Size<i32, Buffer>) -> Result<GlesTexture, GlesError> {
    renderer.create_buffer(Fourcc::Abgr8888, size)
}

fn half(s: Size<i32, Buffer>) -> Size<i32, Buffer> {
    (((s.w + 1) / 2).max(1), ((s.h + 1) / 2).max(1)).into()
}

/// One Kawase pass: the `src_rect` part of `src` → the top-left `dst_used`
/// part of `dst`. Samples never leave `src_rect`.
#[allow(clippy::too_many_arguments)]
fn pass(
    renderer: &mut GlesRenderer,
    prog: &GlesTexProgram,
    src: &GlesTexture,
    src_size: Size<i32, Buffer>,
    src_rect: Rectangle<i32, Buffer>,
    dst: &GlesTexture,
    dst_size: Size<i32, Buffer>,
    dst_used: Size<i32, Buffer>,
    up: bool,
) -> Result<(), GlesError> {
    let mut dst = dst.clone();
    let mut target = renderer.bind(&mut dst)?;
    let size: Size<i32, Physical> = (dst_size.w, dst_size.h).into();
    let area = Rectangle::<i32, Physical>::from_size((dst_used.w, dst_used.h).into());
    let mut frame = renderer.render(&mut target, size, Transform::Normal)?;
    let (sw, sh) = (src_size.w as f32, src_size.h as f32);
    let hp = (0.5 / sw * OFFSET, 0.5 / sh * OFFSET);
    // texel centres of the first and last pixel of `src_rect`
    let clampbox = (
        (src_rect.loc.x as f32 + 0.5) / sw,
        (src_rect.loc.y as f32 + 0.5) / sh,
        (src_rect.loc.x + src_rect.size.w) as f32 - 0.5,
        (src_rect.loc.y + src_rect.size.h) as f32 - 0.5,
    );
    let clampbox = (clampbox.0, clampbox.1, clampbox.2 / sw, clampbox.3 / sh);
    frame.render_texture_from_to(
        src,
        src_rect.to_f64(),
        area,
        &[area],
        &[],
        Transform::Normal,
        1.0,
        Some(prog),
        &[
            Uniform::new("halfpixel", hp),
            Uniform::new("up", if up { 1.0f32 } else { 0.0 }),
            Uniform::new("clampbox", clampbox),
        ],
    )?;
    // same GL context: later passes are ordered after this one
    let _ = frame.finish()?;
    Ok(())
}

impl LiveBlur {
    /// Forget backdrops that have not been drawn for a while.
    pub fn evict(&mut self, now: Duration) {
        self.cache
            .retain(|_, c| now.saturating_sub(c.last_used) < Duration::from_secs(5));
    }

    /// A blurred picture of `below` (output-local physical elements, topmost
    /// first) for the backdrop `region`. `output_size` bounds the padding.
    #[allow(clippy::too_many_arguments)]
    pub fn backdrop<E: RenderElement<GlesRenderer>>(
        &mut self,
        renderer: &mut GlesRenderer,
        id: &Id,
        region: Rectangle<i32, Physical>,
        output_size: Size<i32, Physical>,
        below: &[E],
        scale: f64,
        alpha: f32,
        clear: Color32F,
        now: Duration,
    ) -> Option<(TextureRenderElement<GlesTexture>, Size<i32, Buffer>)> {
        let output = Rectangle::from_size(output_size);
        // (the menu bar asks for a very wide rect; only the visible part matters)
        let region = region.intersection(output)?;
        if region.size.w <= 0 || region.size.h <= 0 {
            return None;
        }
        let prog = program(renderer)?;
        let iterations = if scale >= 1.75 { ITERATIONS + 1 } else { ITERATIONS };
        let grow = |r: Rectangle<i32, Physical>, d: i32| {
            Rectangle::<i32, Physical>::new((r.loc.x - d, r.loc.y - d).into(), (r.size.w + 2 * d, r.size.h + 2 * d).into())
                .intersection(output)
        };
        // the blur only sees what is below the backdrop itself
        let needed = region;

        // Keep the area rendered last time while the backdrop still fits in
        // it (the Dock changes size every frame while magnifying or
        // animating); otherwise render a larger area so small changes fit.
        let cached = self
            .cache
            .get(id)
            .filter(|c| c.levels.len() == iterations as usize && c.padded.contains_rect(needed))
            .map(|c| c.padded);
        let padded = match cached {
            Some(p) => p,
            None => grow(region, SLACK)?,
        };

        let s = Scale::from(scale);
        let fingerprint: Fingerprint = below
            .iter()
            .filter_map(|e| {
                let g = e.geometry(s);
                g.overlaps(padded)
                    .then(|| (e.id().clone(), e.current_commit(), g, e.alpha().to_bits()))
            })
            .collect();

        let size_ok = self
            .cache
            .get(id)
            .map(|c| c.padded.size == padded.size && c.levels.len() == iterations as usize)
            .unwrap_or(false);
        if !size_ok {
            // (re)allocate for the new size
            let scene_size: Size<i32, Buffer> = (padded.size.w, padded.size.h).into();
            let scene = texture(renderer, scene_size).map_err(|e| tracing::warn!("blur buffer: {e:?}")).ok()?;
            let mut levels = Vec::new();
            let mut sz = scene_size;
            for _ in 0..iterations {
                sz = half(sz);
                levels.push((texture(renderer, sz).ok()?, sz));
            }
            let damage = self.cache.remove(id).map(|c| c.damage).unwrap_or_else(|| DamageBag::new(4));
            self.cache.insert(
                id.clone(),
                Cache {
                    padded,
                    scene,
                    levels,
                    region: Rectangle::default(),
                    used: Size::default(),
                    fingerprint: Vec::new(),
                    damage,
                    last_used: now,
                },
            );
        }
        let cache = self.cache.get_mut(id)?;
        cache.last_used = now;

        let rescene = cached.is_none() || cache.fingerprint != fingerprint;
        if rescene || cache.region != region {
            tracing::debug!(
                "live blur: re-render {:?} {}x{} (scene: {}, realloc: {})",
                id,
                region.size.w,
                region.size.h,
                rescene,
                !size_ok
            );
            cache.padded = padded;
            if let Err(e) = render_blur(renderer, &prog, cache, rescene, region, below, scale, clear) {
                tracing::warn!("live blur failed: {e:?}");
                self.cache.remove(id);
                return None;
            }
            cache.fingerprint = fingerprint;
            cache.region = region;
            let used = cache.used;
            cache.damage.add([Rectangle::from_size(used)]);
        }

        let (tex, l0) = cache.levels[0].clone();
        let used = cache.used;
        let src = Rectangle::<f64, Logical>::from_size((used.w as f64, used.h as f64).into());
        let dst_size: Size<i32, Logical> = region.size.to_f64().to_logical(scale).to_i32_round();
        let el = TextureRenderElement::from_texture_with_damage(
            id.clone(),
            renderer.context_id(),
            region.loc.to_f64(),
            tex,
            1,
            Transform::Normal,
            Some(alpha),
            Some(src),
            Some(dst_size),
            None,
            cache.damage.snapshot(),
            Kind::Unspecified,
        );
        Some((el, l0))
    }
}

#[allow(clippy::too_many_arguments)]
fn render_blur<E: RenderElement<GlesRenderer>>(
    renderer: &mut GlesRenderer,
    prog: &GlesTexProgram,
    cache: &mut Cache,
    rescene: bool,
    region: Rectangle<i32, Physical>,
    below: &[E],
    scale: f64,
    clear: Color32F,
) -> Result<(), GlesError> {
    let padded = cache.padded;
    let s = Scale::from(scale);
    // 1. what is below, cropped to the padded area
    if rescene {
        let offset = Point::<i32, Physical>::from((-padded.loc.x, -padded.loc.y));
        let elements: Vec<RelocateRenderElement<&E>> = below
            .iter()
            .filter(|e| e.geometry(s).overlaps(padded))
            .map(|e| RelocateRenderElement::from_element(e, offset, Relocate::Relative))
            .collect();
        let mut scene = cache.scene.clone();
        let mut target = renderer.bind(&mut scene)?;
        let mut tracker = OutputDamageTracker::new(padded.size, scale, Transform::Normal);
        tracker
            .render_output(renderer, &mut target, 0, &elements, clear)
            .map_err(|e| match e {
                smithay::backend::renderer::damage::Error::Rendering(e) => e,
                smithay::backend::renderer::damage::Error::OutputNoMode(_) => GlesError::UnknownSize,
            })?;
    }
    // 2. down to the smallest level, 3. back up to level 0 — only the part
    // of each level that corresponds to `region` (top-left aligned)
    let scene_size: Size<i32, Buffer> = (padded.size.w, padded.size.h).into();
    let mut used = Vec::with_capacity(cache.levels.len());
    let mut u: Size<i32, Buffer> = (region.size.w, region.size.h).into();
    for _ in &cache.levels {
        u = half(u);
        used.push(u);
    }
    let mut src = (
        cache.scene.clone(),
        scene_size,
        Rectangle::<i32, Buffer>::new(
            (region.loc.x - padded.loc.x, region.loc.y - padded.loc.y).into(),
            (region.size.w, region.size.h).into(),
        ),
    );
    for ((tex, size), u) in cache.levels.clone().into_iter().zip(used.iter().copied()) {
        pass(renderer, prog, &src.0, src.1, src.2, &tex, size, u, false)?;
        src = (tex, size, Rectangle::from_size(u));
    }
    for i in (0..cache.levels.len().saturating_sub(1)).rev() {
        let (from, from_size) = cache.levels[i + 1].clone();
        let (to, to_size) = cache.levels[i].clone();
        pass(
            renderer,
            prog,
            &from,
            from_size,
            Rectangle::from_size(used[i + 1]),
            &to,
            to_size,
            used[i],
            true,
        )?;
    }
    cache.used = used[0];
    Ok(())
}
