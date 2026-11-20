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

use std::{cell::RefCell, collections::HashMap, time::Duration};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker,
            element::{
                texture::TextureRenderElement,
                utils::{Relocate, RelocateRenderElement, RescaleRenderElement},
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
pub const ITERATIONS: u32 = 4;
/// Fewest passes that still produce a result (one down, one up).
pub const MIN_ITERATIONS: u32 = 2;
/// Sample offset of each pass, in texels of the level being read.
const OFFSET: f32 = 2.5;
/// Extra area rendered around a backdrop, so it can grow or move a little
/// (Dock magnification) without re-rendering.
const SLACK: i32 = 160;

struct KawaseProgram(RefCell<(u64, GlesTexProgram)>);

fn program(renderer: &mut GlesRenderer) -> Option<GlesTexProgram> {
    let gen = crate::effects::shader_generation();
    if let Some(cached) = renderer.egl_context().user_data().get::<KawaseProgram>() {
        let (cached_gen, ref prog) = *cached.0.borrow();
        if cached_gen == gen {
            return Some(prog.clone());
        }
    }
    let src = crate::effects::load_shader("kawase.frag", SHADER);
    let compiled = renderer
        .compile_custom_texture_shader(
            &src,
            &[
                UniformName::new("halfpixel", UniformType::_2f),
                UniformName::new("up", UniformType::_1f),
                UniformName::new("final_pass", UniformType::_1f),
                UniformName::new("clampbox", UniformType::_4f),
            ],
        )
        .map_err(|e| tracing::error!("failed to compile blur shader: {e:?}"))
        .ok();
    let ud = renderer.egl_context().user_data();
    match (compiled, ud.get::<KawaseProgram>()) {
        (Some(program), Some(cached)) => {
            *cached.0.borrow_mut() = (gen, program.clone());
            Some(program)
        }
        (Some(program), None) => {
            ud.insert_if_missing(|| KawaseProgram(RefCell::new((gen, program.clone()))));
            Some(program)
        }
        (None, Some(cached)) => {
            let mut slot = cached.0.borrow_mut();
            slot.0 = gen;
            Some(slot.1.clone())
        }
        (None, None) => None,
    }
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

/// A new offscreen texture, cleared: freshly allocated GPU memory is
/// undefined (on NVIDIA it shows up as bright speckles), and bilinear
/// sampling at the edge of the used area reads one texel beyond it.
fn texture(renderer: &mut GlesRenderer, size: Size<i32, Buffer>) -> Result<GlesTexture, GlesError> {
    let mut tex: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, size)?;
    {
        let mut target = renderer.bind(&mut tex)?;
        let phys: Size<i32, Physical> = (size.w, size.h).into();
        let mut frame = renderer.render(&mut target, phys, Transform::Normal)?;
        frame.clear(Color32F::TRANSPARENT, &[Rectangle::from_size(phys)])?;
        let _ = frame.finish()?;
    }
    Ok(tex)
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
    final_pass: bool,
) -> Result<(), GlesError> {
    let mut dst = dst.clone();
    let mut target = renderer.bind(&mut dst)?;
    let size: Size<i32, Physical> = (dst_size.w, dst_size.h).into();
    let area = Rectangle::<i32, Physical>::from_size((dst_used.w, dst_used.h).into());
    let mut frame = renderer.render(&mut target, size, Transform::Normal)?;
    // Clear the target before drawing: render_texture_from_to uses alpha blending,
    // so stale pixels from previous frames or larger backdrop sizes would otherwise
    // accumulate or bleed into bilinear edge taps.
    frame.clear(Color32F::TRANSPARENT, &[Rectangle::from_size(size)])?;
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
            Uniform::new("final_pass", if final_pass { 1.0f32 } else { 0.0 }),
            Uniform::new("clampbox", clampbox),
        ],
    )?;
    // same GL context: later passes are ordered after this one
    let _ = frame.finish()?;
    Ok(())
}

impl LiveBlur {
    /// Drop all cached blurred backdrops (e.g. after a live shader reload).
    pub fn invalidate(&mut self) {
        self.cache.clear();
    }

    /// Forget backdrops that have not been drawn for a while.
    pub fn evict(&mut self, now: Duration) {
        self.cache
            .retain(|_, c| now.saturating_sub(c.last_used) < Duration::from_secs(5));
    }

    /// A blurred picture of `below` (output-local physical elements, topmost
    /// first) for the backdrop `region`. `output_size` bounds the padding.
    /// `iterations` is the blur strength at output scale 1 (see [`ITERATIONS`]).
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
        iterations: u32,
    ) -> Option<(TextureRenderElement<GlesTexture>, Size<i32, Buffer>)> {
        let output = Rectangle::from_size(output_size);
        // (the menu bar asks for a very wide rect; only the visible part matters)
        let region = region.intersection(output)?;
        if region.size.w <= 0 || region.size.h <= 0 {
            return None;
        }
        let prog = program(renderer)?;
        let iterations = iterations.clamp(MIN_ITERATIONS, ITERATIONS + 1);
        let iterations = if scale >= 1.75 { iterations + 1 } else { iterations };
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

        let size_ok = self
            .cache
            .get(id)
            .map(|c| c.padded.size == padded.size && c.levels.len() == iterations as usize)
            .unwrap_or(false);
        if !size_ok {
            // (re)allocate for the new size: capture `scene` at 0.5x physical scale
            // (cutting scene VRAM by 75% and GPU fillrate by 4x).
            let full_size: Size<i32, Buffer> = (padded.size.w, padded.size.h).into();
            let scene_size = half(full_size);
            let scene = texture(renderer, scene_size).map_err(|e| tracing::warn!("blur buffer: {e:?}")).ok()?;
            let mut levels = Vec::new();
            let mut sz = full_size;
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

        // Zero-allocation in-place fingerprint comparison against cached state.
        let fp_matches = cached.is_some() && {
            let mut cur = below.iter().filter_map(|e| {
                let g = e.geometry(s);
                g.overlaps(padded)
                    .then(|| (e.id(), e.current_commit(), g, e.alpha().to_bits()))
            });
            let mut old = cache.fingerprint.iter();
            loop {
                match (cur.next(), old.next()) {
                    (Some((id1, c1, g1, a1)), Some((id2, c2, g2, a2))) => {
                        if id1 != id2 || c1 != *c2 || g1 != *g2 || a1 != *a2 {
                            break false;
                        }
                    }
                    (None, None) => break true,
                    _ => break false,
                }
            }
        };

        let rescene = !fp_matches;
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
            if rescene {
                cache.fingerprint.clear();
                cache.fingerprint.extend(below.iter().filter_map(|e| {
                    let g = e.geometry(s);
                    g.overlaps(padded)
                        .then(|| (e.id().clone(), e.current_commit(), g, e.alpha().to_bits()))
                }));
            }
            cache.region = region;
            let used = cache.used;
            cache.damage.add([Rectangle::from_size(used)]);
        }

        let (tex, l0) = cache.levels[0].clone();
        let used = cache.used;
        // Inset by 0.5 texel on edges smaller than the backing texture so bilinear
        // filtering never samples into the cleared border outside `used`.
        let src_w = if used.w < l0.w { (used.w as f64 - 0.5).max(1.0) } else { used.w as f64 };
        let src_h = if used.h < l0.h { (used.h as f64 - 0.5).max(1.0) } else { used.h as f64 };
        let src = Rectangle::<f64, Logical>::from_size((src_w, src_h).into());
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
    let scene_size = half((padded.size.w, padded.size.h).into());
    // 1. what is below, cropped to the padded area and rendered at 0.5x physical scale
    if rescene {
        let offset = Point::<i32, Physical>::from((-padded.loc.x, -padded.loc.y));
        let elements: Vec<RescaleRenderElement<RelocateRenderElement<&E>>> = below
            .iter()
            .filter(|e| e.geometry(s).overlaps(padded))
            .map(|e| {
                let rel = RelocateRenderElement::from_element(e, offset, Relocate::Relative);
                RescaleRenderElement::from_element(rel, (0, 0).into(), 0.5)
            })
            .collect();
        let mut scene = cache.scene.clone();
        let mut target = renderer.bind(&mut scene)?;
        let half_phys: Size<i32, Physical> = (scene_size.w, scene_size.h).into();
        let mut tracker = OutputDamageTracker::new(half_phys, scale, Transform::Normal);
        tracker
            .render_output(renderer, &mut target, 0, &elements, clear)
            .map_err(|e| match e {
                smithay::backend::renderer::damage::Error::Rendering(e) => e,
                smithay::backend::renderer::damage::Error::OutputNoMode(_) => GlesError::UnknownSize,
            })?;
    }
    // 2. down from half-res scene directly to levels[1..], 3. back up to level 0
    let mut used = Vec::with_capacity(cache.levels.len());
    let mut u: Size<i32, Buffer> = (region.size.w, region.size.h).into();
    for _ in &cache.levels {
        u = half(u);
        used.push(u);
    }
    let rx = ((region.loc.x - padded.loc.x) / 2).clamp(0, scene_size.w.saturating_sub(1));
    let ry = ((region.loc.y - padded.loc.y) / 2).clamp(0, scene_size.h.saturating_sub(1));
    let rw = used[0].w.min(scene_size.w - rx).max(1);
    let rh = used[0].h.min(scene_size.h - ry).max(1);
    let mut src = (
        cache.scene.clone(),
        scene_size,
        Rectangle::<i32, Buffer>::new((rx, ry).into(), (rw, rh).into()),
    );
    for i in 1..cache.levels.len() {
        let (tex, size) = cache.levels[i].clone();
        let u = used[i];
        pass(renderer, prog, &src.0, src.1, src.2, &tex, size, u, false, false)?;
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
            i == 0,
        )?;
    }
    cache.used = used[0];
    Ok(())
}
