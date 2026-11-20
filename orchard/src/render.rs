use std::time::Duration;

use smithay::{
    backend::renderer::{
        damage::{Error as OutputDamageTrackerError, OutputDamageTracker, RenderOutputResult},
        element::{
            memory::MemoryRenderBufferRenderElement,
            solid::{SolidColorBuffer, SolidColorRenderElement},
            surface::WaylandSurfaceRenderElement,
            texture::TextureRenderElement,
            utils::{Relocate, RelocateRenderElement, RescaleRenderElement},
            AsRenderElements, Element, Id, Kind,
        },
        gles::{GlesError, GlesRenderer, GlesTarget, GlesTexture},
        Color32F, ImportAll, ImportMem, Renderer,
    },
    desktop::{layer_map_for_output, space::SpaceElement, Space},
    output::Output,
    utils::{Logical, Physical, Point, Rectangle, Scale, Transform},
    wayland::shell::wlr_layer::Layer,
};

#[cfg(feature = "debug")]
use crate::drawing::FpsElement;
use crate::{
    drawing::{PointerRenderElement, CLEAR_COLOR, CLEAR_COLOR_FULLSCREEN},
    effects::{
        genie::{MeshElement, RectF},
        rounded::{Corners, RoundedElement},
        label::LabelStyle,
        wallpaper::WallpaperTextures,
        Effects,
    },
    mission_control::MissionControl,
    shell::{FullscreenSurface, WindowElement, WindowRenderElement},
};

smithay::backend::renderer::element::render_elements! {
    pub CustomRenderElements<R> where
        R: ImportAll + ImportMem;
    Pointer=PointerRenderElement<R>,
    Surface=WaylandSurfaceRenderElement<R>,
    #[cfg(feature = "debug")]
    // Note: We would like to borrow this element instead, but that would introduce
    // a feature-dependent lifetime, which introduces a lot more feature bounds
    // as the whole type changes and we can't have an unused lifetime (for when "debug" is disabled)
    // in the declaration.
    Fps=FpsElement<R::TextureId>,
}

impl<R: Renderer> std::fmt::Debug for CustomRenderElements<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pointer(arg0) => f.debug_tuple("Pointer").field(arg0).finish(),
            Self::Surface(arg0) => f.debug_tuple("Surface").field(arg0).finish(),
            #[cfg(feature = "debug")]
            Self::Fps(arg0) => f.debug_tuple("Fps").field(arg0).finish(),
            Self::_GenericCatcher(arg0) => f.debug_tuple("_GenericCatcher").field(arg0).finish(),
        }
    }
}

smithay::backend::renderer::element::render_elements! {
    pub OutputRenderElements<=GlesRenderer>;
    Custom=CustomRenderElements<GlesRenderer>,
    Layer=WaylandSurfaceRenderElement<GlesRenderer>,
    Window=RescaleRenderElement<WindowRenderElement>,
    Texture=TextureRenderElement<GlesTexture>,
    Mesh=MeshElement,
    Backdrop=RoundedElement<TextureRenderElement<GlesTexture>>,
    McWindow=RelocateRenderElement<RescaleRenderElement<WindowRenderElement>>,
    Solid=SolidColorRenderElement,
    Label=MemoryRenderBufferRenderElement<GlesRenderer>,
    Night=crate::effects::night::NightShiftElement,
}

/// Layer surfaces of one layer, topmost first. The Dock stays above the
/// shell's other surfaces in its layer (Launchpad is drawn below it).
pub fn layers_top_first(map: &smithay::desktop::LayerMap, layer: Layer) -> Vec<smithay::desktop::LayerSurface> {
    let mut v: Vec<_> = map.layers_on(layer).rev().cloned().collect();
    v.sort_by_key(|l| l.namespace() != "orchard-dock");
    v
}

/// The layer surface (in `layer`) whose input region contains `pos`
/// (output-relative), topmost first. Unlike `LayerMap::layer_under`, a
/// surface whose input region does not cover the point lets it through.
pub fn layer_input_under(
    map: &smithay::desktop::LayerMap,
    layer: Layer,
    pos: Point<f64, Logical>,
) -> Option<(
    smithay::desktop::LayerSurface,
    smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    Point<i32, Logical>,
)> {
    layers_top_first(map, layer).into_iter().find_map(|l| {
        let loc = map.layer_geometry(&l)?.loc;
        let (s, sl) = l.surface_under(pos - loc.to_f64(), smithay::desktop::WindowSurfaceType::ALL)?;
        Some((l.clone(), s, sl + loc))
    })
}

/// Stable ids for the frosted-glass backdrops of a layer surface.
#[derive(Default)]
struct BackdropIds(std::cell::RefCell<Vec<Id>>);

pub type Backdrops = std::collections::HashMap<String, Vec<crate::ipc::Rect>>;

/// A frosted-glass backdrop waiting for everything below it to be known.
struct PendingBackdrop {
    /// position in the element list (everything after it is below)
    index: usize,
    id: Id,
    region: Rectangle<i32, Physical>,
    radius: f32,
    alpha: f32,
    /// a shell surface (menu bar, Dock, panel…); client window blur stays on
    /// even when Reduce transparency makes panels solid
    panel: bool,
}

#[allow(clippy::too_many_arguments)]
fn layer_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    layer: Layer,
    scale: f64,
    blur: bool,
    backdrops: &Backdrops,
    pending: &mut Vec<PendingBackdrop>,
    out: &mut Vec<OutputRenderElements>,
) {
    layer_elements_filtered(renderer, output, layer, scale, blur, backdrops, pending, out, |_, _| true)
}

#[allow(clippy::too_many_arguments)]
fn layer_elements_where(
    renderer: &mut GlesRenderer,
    output: &Output,
    layer: Layer,
    scale: f64,
    blur: bool,
    backdrops: &Backdrops,
    pending: &mut Vec<PendingBackdrop>,
    out: &mut Vec<OutputRenderElements>,
    keep: impl Fn(Rectangle<i32, Logical>) -> bool,
) {
    layer_elements_filtered(renderer, output, layer, scale, blur, backdrops, pending, out, |_, geo| keep(geo))
}

#[allow(clippy::too_many_arguments)]
fn layer_elements_filtered(
    renderer: &mut GlesRenderer,
    output: &Output,
    layer: Layer,
    scale: f64,
    blur: bool,
    backdrops: &Backdrops,
    pending: &mut Vec<PendingBackdrop>,
    out: &mut Vec<OutputRenderElements>,
    keep: impl Fn(&str, Rectangle<i32, Logical>) -> bool,
) {
    let map = layer_map_for_output(output);
    for surface in layers_top_first(&map, layer) {
        let surface = &surface;
        let Some(geo) = map.layer_geometry(surface) else { continue };
        if !keep(surface.namespace(), geo) {
            continue;
        }
        let loc = geo.loc.to_physical_precise_round(scale);
        out.extend(
            AsRenderElements::<GlesRenderer>::render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
                surface,
                renderer,
                loc,
                Scale::from(scale),
                1.0,
            )
            .into_iter()
            .map(OutputRenderElements::Layer),
        );
        if let (true, Some(rects)) = (blur, backdrops.get(surface.namespace())) {
            surface.user_data().insert_if_missing(BackdropIds::default);
            let ids = surface.user_data().get::<BackdropIds>().unwrap();
            let mut ids = ids.0.borrow_mut();
            while ids.len() < rects.len() {
                ids.push(Id::new());
            }
            for (rect, id) in rects.iter().zip(ids.iter()) {
                let region = Rectangle::<f64, Logical>::new(
                    (geo.loc.x as f64 + rect.x, geo.loc.y as f64 + rect.y).into(),
                    (rect.w, rect.h).into(),
                )
                .to_physical_precise_round(scale);
                pending.push(PendingBackdrop {
                    index: out.len(),
                    id: id.clone(),
                    region,
                    radius: (rect.radius * scale) as f32,
                    alpha: rect.alpha.clamp(0.0, 1.0) as f32,
                    panel: true,
                });
            }
        }
    }
}

/// Insert the backdrops, bottom-most first, each blurring what is below it
/// (live), or the pre-blurred wallpaper when live blur is unavailable.
#[allow(clippy::too_many_arguments)]
fn resolve_backdrops(
    renderer: &mut GlesRenderer,
    fx: &mut Effects,
    pending: Vec<PendingBackdrop>,
    mode_size: smithay::utils::Size<i32, Physical>,
    scale: f64,
    clear: Color32F,
    now: Duration,
    out: &mut Vec<OutputRenderElements>,
) {
    if pending.is_empty() {
        return;
    }
    fx.blur.evict(now);
    let program = crate::effects::rounded::program(renderer);
    let live = crate::settings::effective_live_blur();
    // "Reduce transparency" paints panels solid: no blur behind them at all.
    let solid_panels = crate::settings::get().appearance.reduce_transparency;
    for p in pending.into_iter().rev() {
        if p.panel && solid_panels {
            continue;
        }
        let iterations = crate::effects::blur::ITERATIONS;
        let blurred = if live {
            fx.blur
                .backdrop(renderer, &p.id, p.region, mode_size, &out[p.index..], scale, p.alpha, clear, now, iterations)
                .map(|(tex, size)| (tex, (size.w as f64, size.h as f64)))
        } else {
            None
        };
        let blurred = blurred.or_else(|| {
            let wp = fx.wallpaper.textures(renderer, mode_size)?;
            let tex = wp.blurred_region(renderer, &p.id, p.region, scale, p.alpha);
            Some((tex, (wp.blurred_size.w as f64, wp.blurred_size.h as f64)))
        });
        let Some((tex, (tw, th))) = blurred else { continue };
        let el = match &program {
            Some(prog) => OutputRenderElements::Backdrop(
                RoundedElement::new(tex, prog.clone(), p.radius, Corners::ALL).with_texture_size(tw, th),
            ),
            None => OutputRenderElements::Texture(tex),
        };
        out.insert(p.index, el);
    }
}

#[allow(clippy::too_many_arguments)]
fn window_elements(
    renderer: &mut GlesRenderer,
    output_geo: Rectangle<i32, Logical>,
    window: &WindowElement,
    location: Point<i32, Logical>,
    offset_x: f64,
    scale: f64,
    now: Duration,
    out: &mut Vec<OutputRenderElements>,
) {
    let location = location + Point::from((offset_x.round() as i32, 0));
    if frame_anim_elements(renderer, output_geo, window, location, scale, now, out) {
        return;
    }
    if open_anim_elements(renderer, output_geo, window, location, scale, now, out) {
        return;
    }
    let (anim_scale, alpha) = window.anim().transform(now);
    let render_loc = (location - window.geometry().loc - output_geo.loc).to_physical_precise_round(scale);
    let geo = window.geometry();
    let center: Point<i32, Physical> = (location - output_geo.loc + Point::from((geo.size.w / 2, geo.size.h / 2)))
        .to_physical_precise_round(scale);
    let elems: Vec<WindowRenderElement> =
        AsRenderElements::<GlesRenderer>::render_elements(window, renderer, render_loc, Scale::from(scale), alpha);
    out.extend(elems.into_iter().map(|e| {
        OutputRenderElements::Window(RescaleRenderElement::from_element(e, center, anim_scale))
    }));
}

/// A window mid open-animation: render from a cached offscreen snapshot on a
/// floating-point mesh quad so scaling is sub-pixel smooth and titlebar,
/// client, and shadow never jitter relative to each other.
fn open_anim_elements(
    renderer: &mut GlesRenderer,
    output_geo: Rectangle<i32, Logical>,
    window: &WindowElement,
    location: Point<i32, Logical>,
    scale: f64,
    now: Duration,
    out: &mut Vec<OutputRenderElements>,
) -> bool {
    let (anim_scale, alpha) = {
        let mut anim = window.anim();
        let Some(oa) = anim.open_at_mut(now) else {
            return false;
        };
        oa.transform(now)
    };
    let bbox = SpaceElement::bbox(window);
    let geo = window.geometry();
    if bbox.is_empty() || geo.size.w <= 0 || geo.size.h <= 0 {
        return false;
    }
    let origin: Point<i32, Physical> =
        (Point::<i32, Logical>::from((0, 0)) - bbox.loc).to_physical_precise_round(scale);
    let elems: Vec<WindowRenderElement> =
        AsRenderElements::<GlesRenderer>::render_elements(window, renderer, origin, Scale::from(scale), 1.0);
    let key: Vec<(Id, smithay::backend::renderer::utils::CommitCounter)> =
        elems.iter().map(|e| (e.id().clone(), e.current_commit())).collect();

    let mut anim = window.anim();
    let Some(oa) = anim.open.as_mut() else {
        return false;
    };
    let cache_hit = oa
        .snapshot
        .as_ref()
        .is_some_and(|s| s.bbox == bbox && (s.scale - scale).abs() < 1e-6)
        && oa.snapshot_key == key;
    if !cache_hit {
        let reuse = oa.snapshot.take().map(|s| s.texture);
        oa.snapshot = crate::effects::snapshot::snapshot_elements_into(renderer, &elems, bbox, scale, reuse);
        oa.snapshot_key = key;
    }
    let Some(snap) = oa.snapshot.as_ref() else {
        return false;
    };
    let cx = ((location.x - output_geo.loc.x) as f64 + geo.size.w as f64 * 0.5) * scale;
    let cy = ((location.y - output_geo.loc.y) as f64 + geo.size.h as f64 * 0.5) * scale;
    let base: Point<i32, Physical> =
        (location - geo.loc - output_geo.loc + snap.bbox.loc).to_physical_precise_round(scale);
    let x0 = base.x as f64;
    let y0 = base.y as f64;
    let w0 = snap.size.w as f64;
    let h0 = snap.size.h as f64;
    let r = RectF {
        x: (cx + (x0 - cx) * anim_scale) as f32,
        y: (cy + (y0 - cy) * anim_scale) as f32,
        w: (w0 * anim_scale) as f32,
        h: (h0 * anim_scale) as f32,
    };
    out.push(OutputRenderElements::Mesh(MeshElement::new(
        oa.id.clone(),
        oa.next_commit(),
        snap.texture.clone(),
        (snap.size.w, snap.size.h),
        r,
        r,
        0.0,
        alpha,
        false,
    )));
    true
}

/// Full-screen reveal (fs_reveal.rs): the menu bar and a title bar with
/// traffic lights drawn over the top of the full-screen window.
#[allow(clippy::too_many_arguments)]
fn fs_reveal_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geo: Rectangle<i32, Logical>,
    window: &WindowElement,
    scale: f64,
    backdrops: &Backdrops,
    pending: &mut Vec<PendingBackdrop>,
    out: &mut Vec<OutputRenderElements>,
) {
    use crate::chrome::{theme, titlebar::{TitlebarParams, TrafficLight}};
    use crate::shell::ssd::HEADER_BAR_HEIGHT;
    let Some((menu_h, buf)) = crate::fs_reveal::with_reveal(output, |r| {
        let params = TitlebarParams {
            width: output_geo.size.w,
            scale,
            appearance: theme::appearance(),
            radius: 0.0,
            active: true,
            group_hovered: r.hover.map(|h| TrafficLight::in_group(h.x, h.y)).unwrap_or(false),
            pressed: r.pressed,
            title: window.title(),
            fullscreen: true,
            alt_held: false,
        };
        (r.menu_h, r.chrome.titlebar(params).cloned())
    }) else {
        return;
    };
    // the menu bar (top-anchored Top-layer surfaces, not the Dock)
    layer_elements_where(renderer, output, Layer::Top, scale, true, backdrops, pending, out, |g| {
        g.loc.y == 0 && g.size.h <= menu_h.max(1)
    });
    let Some(buf) = buf else { return };
    let loc: Point<f64, Physical> = Point::<i32, Logical>::from((0, menu_h)).to_f64().to_physical(scale);
    match MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        loc,
        &buf,
        None,
        None,
        Some((output_geo.size.w, HEADER_BAR_HEIGHT).into()),
        Kind::Unspecified,
    ) {
        Ok(e) => out.push(OutputRenderElements::Window(RescaleRenderElement::from_element(
            WindowRenderElement::Chrome(e),
            Point::default(),
            1.0,
        ))),
        Err(err) => tracing::warn!("full-screen title bar: {err:?}"),
    }
}

/// A window mid frame-animation (tile / zoom / restore): the live surface
/// stretched onto the animated frame, under a fading snapshot of its old
/// contents. Returns false when the window is not animating its frame.
fn frame_anim_elements(
    renderer: &mut GlesRenderer,
    output_geo: Rectangle<i32, Logical>,
    window: &WindowElement,
    location: Point<i32, Logical>,
    scale: f64,
    now: Duration,
    out: &mut Vec<OutputRenderElements>,
) -> bool {
    let geo = window.geometry();
    let mut anim = window.anim();
    let Some(fa) = anim.frame_at(now) else { return false };
    if geo.size.w <= 0 || geo.size.h <= 0 {
        return false;
    }
    if fa.resized_at.get().is_none()
        && (geo.size.w - fa.to_size.w).abs() <= 2
        && (geo.size.h - fa.to_size.h).abs() <= 2
    {
        fa.resized_at.set(Some(now));
    }
    let cur = fa.current(location, now);
    let local = |p: Point<f64, Logical>| p - output_geo.loc.to_f64();

    // old contents on top, fading out once the client has caught up
    let alpha = fa.snapshot_alpha(now);
    if let Some(snap) = fa.snapshot.as_ref().filter(|_| alpha > 0.0) {
        let sx = cur.size.w / fa.snap_geo.size.w.max(1) as f64;
        let sy = cur.size.h / fa.snap_geo.size.h.max(1) as f64;
        // snapshot bbox is relative to the surface origin; geometry starts at snap_geo.loc
        let off = snap.bbox.loc - fa.snap_geo.loc;
        let x = local(cur.loc).x + off.x as f64 * sx;
        let y = local(cur.loc).y + off.y as f64 * sy;
        let r = RectF {
            x: (x * scale) as f32,
            y: (y * scale) as f32,
            w: (snap.bbox.size.w as f64 * sx * scale) as f32,
            h: (snap.bbox.size.h as f64 * sy * scale) as f32,
        };
        out.push(OutputRenderElements::Mesh(MeshElement::new(
            fa.id.clone(),
            fa.next_commit(),
            snap.texture.clone(),
            (snap.size.w, snap.size.h),
            r,
            r,
            0.0,
            alpha,
            false,
        )));
    }

    // live surface: geometry origin pinned to the frame origin, stretched to its size
    let origin: Point<i32, Physical> = local(cur.loc).to_physical(scale).to_i32_round();
    let render_loc = origin - geo.loc.to_f64().to_physical(scale).to_i32_round();
    let s = Scale {
        x: cur.size.w / geo.size.w as f64,
        y: cur.size.h / geo.size.h as f64,
    };
    drop(anim);
    let elems: Vec<WindowRenderElement> =
        AsRenderElements::<GlesRenderer>::render_elements(window, renderer, render_loc, Scale::from(scale), 1.0);
    out.extend(
        elems
            .into_iter()
            .map(|e| OutputRenderElements::Window(RescaleRenderElement::from_element(e, origin, s))),
    );
    true
}

/// Everything besides the space that influences what an output shows.
pub struct RenderCtx<'a> {
    pub fx: &'a mut Effects,
    pub backdrops: &'a Backdrops,
    pub mc: Option<&'a MissionControl>,
    /// (active Space, number of Spaces) of the output being rendered
    pub space_info: (usize, usize),
    /// session locked: show only the lock surface (or black)
    pub locked: bool,
    pub lock_surface: Option<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>,
}

fn solid(fx: &mut Effects, idx: usize, rect: Rectangle<i32, Physical>, color: [f32; 4], scale: f64) -> SolidColorRenderElement {
    while fx.mc_solids.len() <= idx {
        fx.mc_solids.push(SolidColorBuffer::default());
    }
    let buf = &mut fx.mc_solids[idx];
    let logical = rect.size.to_f64().to_logical(scale).to_i32_round();
    buf.update(logical, color);
    SolidColorRenderElement::from_buffer(buf, rect.loc, scale, 1.0, Kind::Unspecified)
}

/// ⌘⇧4: grey area with a thin white frame, or a blue tint over the window.
fn shot_overlay_elements(
    fx: &mut Effects,
    overlay: crate::screenshot::Overlay,
    output_geo: Rectangle<i32, Logical>,
    scale: f64,
    out: &mut Vec<OutputRenderElements>,
) {
    use crate::screenshot::Overlay;
    let (r, fill, border) = match overlay {
        Overlay::Area(r) => (r, [0.12, 0.12, 0.12, 0.25], true),
        Overlay::Window(r) => (r, [0.06, 0.16, 0.32, 0.35], false),
    };
    let Some(r) = r.intersection(output_geo) else { return };
    let local = Rectangle::new(r.loc - output_geo.loc, r.size);
    let mut push = |i: usize, rect: Rectangle<i32, Logical>, color: [f32; 4], out: &mut Vec<OutputRenderElements>| {
        if rect.size.w <= 0 || rect.size.h <= 0 {
            return;
        }
        let loc = rect.loc.to_f64().to_physical(scale).to_i32_round();
        fx.shot_bufs[i].update(rect.size, color);
        out.push(OutputRenderElements::Solid(SolidColorRenderElement::from_buffer(
            &fx.shot_bufs[i],
            loc,
            scale,
            1.0,
            Kind::Unspecified,
        )));
    };
    if border {
        let w = [0.9, 0.9, 0.9, 0.9];
        let (x, y, rw, rh) = (local.loc.x, local.loc.y, local.size.w, local.size.h);
        push(1, Rectangle::new((x, y).into(), (rw, 1).into()), w, out);
        push(2, Rectangle::new((x, y + rh - 1).into(), (rw, 1).into()), w, out);
        push(3, Rectangle::new((x, y).into(), (1, rh).into()), w, out);
        push(4, Rectangle::new((x + rw - 1, y).into(), (1, rh).into()), w, out);
    }
    push(0, local, fill, out);
}

/// Translucent drop target for edge tiling (inset a little, like macOS):
/// a light fill with a brighter hairline border.
fn tile_preview_elements(
    fx: &mut Effects,
    r: Rectangle<f64, Logical>,
    opacity: f32,
    output_geo: Rectangle<i32, Logical>,
    scale: f64,
    out: &mut Vec<OutputRenderElements>,
) {
    let inset = 8.0;
    let x0 = ((r.loc.x - output_geo.loc.x as f64 + inset) * scale).round() as i32;
    let y0 = ((r.loc.y - output_geo.loc.y as f64 + inset) * scale).round() as i32;
    let x1 = ((r.loc.x + r.size.w - output_geo.loc.x as f64 - inset) * scale).round() as i32;
    let y1 = ((r.loc.y + r.size.h - output_geo.loc.y as f64 - inset) * scale).round() as i32;
    if x1 - x0 < 2 || y1 - y0 < 2 {
        return;
    }
    let b = (1.5 * scale).round().max(1.0) as i32;
    let border = 0.55 * opacity;
    let fill = 0.16 * opacity;
    let rects = [
        (Rectangle::<i32, Physical>::new((x0, y0).into(), (x1 - x0, b).into()), border),
        (Rectangle::new((x0, y1 - b).into(), (x1 - x0, b).into()), border),
        (Rectangle::new((x0, y0 + b).into(), (b, y1 - y0 - 2 * b).into()), border),
        (Rectangle::new((x1 - b, y0 + b).into(), (b, y1 - y0 - 2 * b).into()), border),
        (Rectangle::new((x0 + b, y0 + b).into(), (x1 - x0 - 2 * b, y1 - y0 - 2 * b).into()), fill),
    ];
    for (i, (rect, a)) in rects.into_iter().enumerate() {
        if rect.size.w <= 0 || rect.size.h <= 0 {
            continue;
        }
        // buffers are sized in logical px; keep physical rounding exact
        let size = rect.size.to_f64().to_logical(scale);
        fx.tile_preview_bufs[i].update((size.w.ceil() as i32, size.h.ceil() as i32), [a, a, a, a]);
        out.push(OutputRenderElements::Solid(SolidColorRenderElement::from_buffer(
            &fx.tile_preview_bufs[i],
            rect.loc,
            scale,
            1.0,
            Kind::Unspecified,
        )));
    }
}

fn mc_id(fx: &mut Effects, idx: usize) -> Id {
    while fx.mc_ids.len() <= idx {
        fx.mc_ids.push(Id::new());
    }
    fx.mc_ids[idx].clone()
}

/// German UI strings for the compositor-drawn labels.
fn german() -> bool {
    static GERMAN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *GERMAN.get_or_init(|| std::env::var("LANG").map(|l| l.starts_with("de")).unwrap_or(false))
}

fn label_element(
    renderer: &mut GlesRenderer,
    fx: &mut Effects,
    text: &str,
    style: LabelStyle,
    center_x: f64,
    top: f64,
    scale: f64,
    alpha: f32,
) -> Option<OutputRenderElements> {
    let (buf, (w, h)) = fx.labels.get(text, style, scale)?;
    let loc = Point::<f64, Logical>::from((center_x - w as f64 / 2.0, top)).to_physical(scale);
    MemoryRenderBufferRenderElement::from_buffer(renderer, loc, buf, Some(alpha), None, Some((w, h).into()), Kind::Unspecified)
        .ok()
        .map(OutputRenderElements::Label)
}

/// A plain colour as a tiny texture, so it can be drawn with rounded corners.
fn fill_texture(renderer: &mut GlesRenderer, fx: &mut Effects, rgb: [u8; 3]) -> Option<GlesTexture> {
    use smithay::backend::{allocator::Fourcc, renderer::ImportMem};
    if let Some(t) = fx.fill_textures.get(&rgb) {
        return Some(t.clone());
    }
    let px: Vec<u8> = std::iter::repeat([rgb[0], rgb[1], rgb[2], 255]).take(16).flatten().collect();
    let tex = renderer.import_memory(&px, Fourcc::Abgr8888, (4, 4).into(), false).ok()?;
    fx.fill_textures.insert(rgb, tex.clone());
    Some(tex)
}

/// A filled rounded rectangle (output-local logical coordinates).
#[allow(clippy::too_many_arguments)]
fn rounded_fill(
    renderer: &mut GlesRenderer,
    fx: &mut Effects,
    id: usize,
    rect: Rectangle<f64, Logical>,
    radius: f64,
    rgb: [u8; 3],
    alpha: f32,
    scale: f64,
) -> Option<OutputRenderElements> {
    if rect.size.w < 1.0 || rect.size.h < 1.0 || alpha <= 0.0 {
        return None;
    }
    let tex = fill_texture(renderer, fx, rgb)?;
    let prog = crate::effects::rounded::program(renderer)?;
    let phys: Rectangle<i32, Physical> = rect.to_physical_precise_round(scale);
    let el = TextureRenderElement::from_static_texture(
        mc_id(fx, id),
        renderer.context_id(),
        phys.loc.to_f64(),
        tex,
        1,
        Transform::Normal,
        Some(alpha.min(1.0)),
        // sample the whole 4×4 texture (the default would be the element size)
        Some(Rectangle::from_size((4.0, 4.0).into())),
        Some(rect.size.to_i32_round()),
        None,
        Kind::Unspecified,
    );
    let r = (radius * scale).min(phys.size.w.min(phys.size.h) as f64 / 2.0);
    Some(OutputRenderElements::Backdrop(RoundedElement::new(el, prog, r as f32, Corners::ALL)))
}

fn grow(r: Rectangle<f64, Logical>, d: f64) -> Rectangle<f64, Logical> {
    Rectangle::new((r.loc.x - d, r.loc.y - d).into(), (r.size.w + 2.0 * d, r.size.h + 2.0 * d).into())
}

/// A window drawn scaled from its real rect `real` into `r` (global logical).
fn mc_window(
    renderer: &mut GlesRenderer,
    window: &WindowElement,
    real: Rectangle<f64, Logical>,
    r: Rectangle<f64, Logical>,
    origin: Point<f64, Logical>,
    scale: f64,
    out: &mut Vec<OutputRenderElements>,
) {
    let k = if real.size.w > 0.0 { r.size.w / real.size.w } else { 1.0 };
    let render_loc = (real.loc - window.geometry().loc.to_f64() - origin).to_physical_precise_round(scale);
    let scale_origin: Point<i32, Physical> = (real.loc - origin).to_physical_precise_round(scale);
    let offset: Point<i32, Physical> = (r.loc - real.loc).to_physical_precise_round(scale);
    let elems: Vec<WindowRenderElement> =
        AsRenderElements::<GlesRenderer>::render_elements(window, renderer, render_loc, Scale::from(scale), 1.0);
    for e in elems {
        let e = RescaleRenderElement::from_element(e, scale_origin, k);
        out.push(OutputRenderElements::McWindow(RelocateRenderElement::from_element(
            e,
            offset,
            Relocate::Relative,
        )));
    }
}

/// macOS's selection blue
const ACCENT: [u8; 3] = [10, 132, 255];

/// Mission Control on one output: windows springing into the grid, the
/// dragged window, the Spaces strip, dimming.
#[allow(clippy::too_many_arguments)]
fn mission_control_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geo: Rectangle<i32, Logical>,
    mc: &MissionControl,
    ctx: &mut RenderCtx<'_>,
    wallpaper_tex: Option<(GlesTexture, (i32, i32))>,
    leave_off: f64,
    scale: f64,
    now: Duration,
    out: &mut Vec<OutputRenderElements>,
) {
    let Some(si) = mc.screens.iter().position(|s| s.output == *output) else { return };
    let screen = &mc.screens[si];
    let p = mc.progress(now).clamp(0.0, 1.1);
    let fade = p.clamp(0.0, 1.0) as f32;
    let origin = output_geo.loc.to_f64();
    let fx = &mut *ctx.fx;
    let local = |r: Rectangle<f64, Logical>| Rectangle::<f64, Logical>::new(r.loc - origin, r.size);
    let highlighted = mc.highlighted().cloned();
    let dragging = mc.drag.as_ref().filter(|d| d.active);
    let corner = crate::chrome::theme::corner_radius() as f64;

    // the dragged window floats above everything, on whichever output it is
    if let Some(drag) = dragging {
        let real = mc
            .screens
            .iter()
            .flat_map(|s| s.wins.iter())
            .find(|w| w.window == drag.window)
            .map(|w| w.real);
        let r = drag.rect_at(now);
        if let Some(real) = real.filter(|_| r.to_i32_round::<i32>().overlaps(output_geo)) {
            mc_window(renderer, &drag.window, real, r, origin, scale, out);
        }
    }

    // title of the highlighted window
    if let (Some(h), false) = (&highlighted, mc.closing) {
        if let Some(w) = screen.wins.iter().find(|w| w.window == *h) {
            let t = w.grid_at(now);
            let title = h.title();
            if !title.is_empty() && p > 0.8 {
                if let Some(e) = label_element(
                    renderer,
                    fx,
                    &title,
                    LabelStyle::Pill,
                    t.loc.x - origin.x + t.size.w / 2.0,
                    t.loc.y - origin.y + t.size.h + 10.0,
                    scale,
                    fade,
                ) {
                    out.push(e);
                }
            }
        }
    }

    // windows, topmost (last) first; the highlighted one gets a blue ring
    for w in screen.wins.iter().rev() {
        if dragging.map(|d| d.window == w.window).unwrap_or(false) {
            continue;
        }
        let mut r = w.rect_at(p, mc.closing, now);
        // switched Space while closing: the old Space's windows slide out too
        let leaving = fx
            .space_transition
            .as_ref()
            .map(|t| t.from.iter().any(|(x, _)| *x == w.window))
            .unwrap_or(false);
        if leaving {
            r.loc.x += leave_off;
        }
        mc_window(renderer, &w.window, w.real, r, origin, scale, out);
        if !mc.closing && highlighted.as_ref() == Some(&w.window) {
            let k = if w.real.size.w > 0.0 { r.size.w / w.real.size.w } else { 1.0 };
            let ring = grow(local(r), 4.0);
            if let Some(e) = rounded_fill(renderer, fx, 90, ring, corner * k + 4.0, ACCENT, 0.95 * fade, scale) {
                out.push(e);
            }
        }
    }

    // Spaces strip
    let (active, count) = ctx.space_info;
    let count = count.min(screen.space_count());
    for (i, thumb) in screen.thumbs.iter().enumerate() {
        // slide in from above
        let l = local(*thumb);
        let l = Rectangle::new((l.loc.x, l.loc.y - (1.0 - fade as f64) * 140.0).into(), l.size);
        let hovered = mc.hovered_thumb == Some((si, i));
        if i == count {
            if let Some(e) = label_element(
                renderer,
                fx,
                "+",
                LabelStyle::Glyph,
                l.loc.x + l.size.w / 2.0,
                l.loc.y + (l.size.h - 40.0) / 2.0,
                scale,
                fade,
            ) {
                out.push(e);
            }
            let (rgb, a) = match (hovered, dragging.is_some()) {
                (true, true) => (ACCENT, 0.6),
                (true, false) => ([255, 255, 255], 0.26),
                _ => ([255, 255, 255], 0.14),
            };
            if let Some(e) = rounded_fill(renderer, fx, 91, l, THUMB_RADIUS, rgb, a * fade, scale) {
                out.push(e);
            }
            continue;
        }
        // remove button on the hovered Space
        if hovered && count > 1 && dragging.is_none() && !mc.closing {
            let a = if mc.hovered_remove { 1.0 } else { 0.85 } * fade;
            let style = if mc.hovered_remove { LabelStyle::CloseHot } else { LabelStyle::Close };
            if let Some(e) = label_element(
                renderer,
                fx,
                "",
                style,
                l.loc.x,
                l.loc.y - crate::mission_control::REMOVE_R,
                scale,
                a,
            ) {
                out.push(e);
            }
        }
        // name below the thumbnail
        let name = if german() {
            format!("Schreibtisch {}", i + 1)
        } else {
            format!("Desktop {}", i + 1)
        };
        if let Some(e) = label_element(
            renderer,
            fx,
            &name,
            LabelStyle::Plain,
            l.loc.x + l.size.w / 2.0,
            l.loc.y + l.size.h + 7.0,
            scale,
            fade,
        ) {
            out.push(e);
        }
        // the Space's miniature, or just the wallpaper
        let phys: Rectangle<i32, Physical> = l.to_physical_precise_round(scale);
        let thumb_tex = screen
            .thumb_textures
            .get(i)
            .and_then(|t| t.as_ref())
            .map(|t| (t.texture.clone(), (t.size.w, t.size.h), Some(t.id.clone())));
        let src = thumb_tex.or_else(|| wallpaper_tex.clone().map(|(t, s)| (t, s, None)));
        if let Some((tex, (tw, th), own_id)) = &src {
            let id = own_id.clone().unwrap_or_else(|| mc_id(fx, i));
            let tex_el = TextureRenderElement::from_static_texture(
                id,
                renderer.context_id(),
                phys.loc.to_f64(),
                tex.clone(),
                1,
                Transform::Normal,
                Some(fade),
                None,
                Some(l.size.to_i32_round()),
                None,
                Kind::Unspecified,
            );
            match crate::effects::rounded::program(renderer) {
                Some(prog) => out.push(OutputRenderElements::Backdrop(
                    RoundedElement::new(tex_el, prog, (THUMB_RADIUS * scale) as f32, Corners::ALL)
                        .with_texture_size(*tw as f64, *th as f64),
                )),
                None => out.push(OutputRenderElements::Texture(tex_el)),
            }
        }
        // ring: current Space white, hovered dimmer, drop target blue
        let ring = match (hovered, dragging.is_some(), i == active) {
            (true, true, _) => Some((ACCENT, 1.0)),
            (_, _, true) => Some(([255, 255, 255], 0.9)),
            (true, false, false) => Some(([255, 255, 255], 0.5)),
            _ => None,
        };
        if let Some((rgb, a)) = ring {
            if let Some(e) = rounded_fill(renderer, fx, 100 + i, grow(l, 3.0), THUMB_RADIUS + 3.0, rgb, a * fade, scale) {
                out.push(e);
            }
        }
        // soft shadow under the miniature
        soft_shadow(renderer, fx, 400 + i * 3, l, THUMB_RADIUS, 0.9 * fade, scale, out);
    }

    // the frosted shelf the Spaces sit on
    if let (Some(first), Some(last)) = (screen.thumbs.first(), screen.thumbs.iter().take(count.max(1)).last()) {
        let first = local(*first);
        let last = local(*last);
        let plus = screen.thumbs.last().map(|r| local(*r)).unwrap_or(last);
        let x0 = first.loc.x.min(plus.loc.x) - 18.0;
        let x1 = (last.loc.x + last.size.w).max(plus.loc.x + plus.size.w) + 18.0;
        let y0 = first.loc.y - 10.0;
        let y1 = first.loc.y + first.size.h + 28.0;
        let shelf = Rectangle::<f64, Logical>::new((x0, y0 - (1.0 - fade as f64) * 140.0).into(), (x1 - x0, y1 - y0).into());
        if let Some(e) = rounded_fill(renderer, fx, 300, grow(shelf, 0.5), 18.5, [255, 255, 255], 0.10 * fade, scale) {
            // (a hairline brighter edge, drawn below the fill)
            if let Some(f) = rounded_fill(renderer, fx, 301, shelf, 18.0, [0, 0, 0], 0.30 * fade, scale) {
                out.push(f);
            }
            out.push(e);
        }
    }

    // the desktop behind: dimmed and blurred, so the windows stand out
    let full = Rectangle::<i32, Physical>::from_size(output_geo.size.to_physical_precise_round(scale));
    out.push(OutputRenderElements::Solid(solid(fx, 0, full, [0.0, 0.0, 0.0, 0.30 * fade], scale)));
    let mode_size = output
        .current_mode()
        .map(|m| output.current_transform().transform_size(m.size))
        .unwrap_or_default();
    let wp_id = mc_id(fx, 310);
    if let Some(wp) = fx.wallpaper.textures(renderer, mode_size) {
        out.push(OutputRenderElements::Texture(wp.blurred_region(renderer, &wp_id, full, scale, fade)));
    }
}

/// Space miniatures' corner radius (logical px).
const THUMB_RADIUS: f64 = 8.0;

/// A cheap soft shadow below `r`: a few widening, fading rounded layers,
/// shifted down a little. Uses ids `id..id + 3`.
#[allow(clippy::too_many_arguments)]
fn soft_shadow(
    renderer: &mut GlesRenderer,
    fx: &mut Effects,
    id: usize,
    r: Rectangle<f64, Logical>,
    radius: f64,
    alpha: f32,
    scale: f64,
    out: &mut Vec<OutputRenderElements>,
) {
    for (k, (d, a)) in [(2.0, 0.16f32), (5.0, 0.09), (9.0, 0.05)].into_iter().enumerate() {
        let s = grow(Rectangle::new((r.loc.x, r.loc.y + d * 0.5).into(), r.size), d);
        if let Some(e) = rounded_fill(renderer, fx, id + k, s, radius + d, [0, 0, 0], a * alpha, scale) {
            out.push(e);
        }
    }
}

/// Lock screen backdrop: the blurred wallpaper under a light dim, at `alpha`.
/// Shown while locked (under the locker's translucent surface, and before the
/// locker has drawn anything) and faded out over the desktop after unlocking.
fn lock_backdrop_elements(
    renderer: &mut GlesRenderer,
    fx: &mut Effects,
    mode_size: smithay::utils::Size<i32, Physical>,
    scale: f64,
    alpha: f32,
    out: &mut Vec<OutputRenderElements>,
) {
    let full = Rectangle::<i32, Physical>::from_size(mode_size);
    let logical = mode_size.to_f64().to_logical(scale).to_i32_round();
    fx.lock_dim.update(logical, [0.0, 0.0, 0.0, 0.16 * alpha]);
    out.push(OutputRenderElements::Solid(SolidColorRenderElement::from_buffer(
        &fx.lock_dim,
        (0, 0),
        scale,
        1.0,
        Kind::Unspecified,
    )));
    let id = fx.lock_bg_id.0.clone();
    let wp = fx.wallpaper.textures(renderer, mode_size).map(|wp| wp.blurred_region(renderer, &id, full, scale, alpha));
    match wp {
        Some(el) => out.push(OutputRenderElements::Texture(el)),
        // no wallpaper (yet): a dark slate, like the default gradient's darkest tone
        None => {
            fx.lock_fallback.update(logical, [0.09, 0.10, 0.13, alpha]);
            out.push(OutputRenderElements::Solid(SolidColorRenderElement::from_buffer(
                &fx.lock_fallback,
                (0, 0),
                scale,
                1.0,
                Kind::Unspecified,
            )));
        }
    }
}

#[profiling::function]
pub fn output_elements(
    output: &Output,
    space: &Space<WindowElement>,
    ctx: &mut RenderCtx<'_>,
    custom_elements: impl IntoIterator<Item = CustomRenderElements<GlesRenderer>>,
    renderer: &mut GlesRenderer,
    now: Duration,
) -> (Vec<OutputRenderElements>, Color32F) {
    let custom: Vec<_> = custom_elements.into_iter().collect();
    let n_custom = custom.len();
    let (mut out, clear) = output_elements_inner(output, space, ctx, custom, renderer, now);
    // just unlocked: the lock backdrop dissolves over the (already running) desktop
    if !ctx.locked {
        if let Some(alpha) = ctx.fx.unlock_fade_alpha(now) {
            let scale = output.current_scale().fractional_scale();
            let mode_size = output
                .current_mode()
                .map(|m| output.current_transform().transform_size(m.size))
                .unwrap_or_default();
            let mut overlay = Vec::new();
            lock_backdrop_elements(renderer, ctx.fx, mode_size, scale, alpha, &mut overlay);
            let at = n_custom.min(out.len());
            out.splice(at..at, overlay);
        }
    }
    (out, clear)
}

fn output_elements_inner(
    output: &Output,
    space: &Space<WindowElement>,
    ctx: &mut RenderCtx<'_>,
    custom_elements: Vec<CustomRenderElements<GlesRenderer>>,
    renderer: &mut GlesRenderer,
    now: Duration,
) -> (Vec<OutputRenderElements>, Color32F) {
    let scale = output.current_scale().fractional_scale();
    let output_geo = space.output_geometry(output).unwrap_or_default();
    let mut out: Vec<OutputRenderElements> = custom_elements
        .into_iter()
        .map(OutputRenderElements::Custom)
        .collect();
    let backdrops = ctx.backdrops;

    if let Some(overlay) = ctx.fx.shot_overlay {
        shot_overlay_elements(ctx.fx, overlay, output_geo, scale, &mut out);
    }

    if ctx.locked {
        if let Some(surface) = ctx.lock_surface.as_ref() {
            out.extend(
                smithay::backend::renderer::element::surface::render_elements_from_surface_tree::<
                    _,
                    WaylandSurfaceRenderElement<GlesRenderer>,
                >(renderer, surface, (0, 0), Scale::from(scale), 1.0, Kind::Unspecified)
                .into_iter()
                .map(OutputRenderElements::Layer),
            );
        }
        // windows are never drawn while locked; the backdrop is wallpaper only
        let mode_size = output
            .current_mode()
            .map(|m| output.current_transform().transform_size(m.size))
            .unwrap_or_default();
        lock_backdrop_elements(renderer, ctx.fx, mode_size, scale, 1.0, &mut out);
        return (out, Color32F::new(0.0, 0.0, 0.0, 1.0));
    }

    // (Mission Control shows the full-screen window in its grid instead)
    let mc_here = ctx.mc.map(|m| m.screen_of(output).is_some()).unwrap_or(false);
    if let Some(window) = output
        .user_data()
        .get::<FullscreenSurface>()
        .and_then(|f| f.get())
        .filter(|_| !mc_here)
    {
        let hide_shot = ctx.fx.hide_screenshot_ui;
        let mut pending = Vec::new();
        layer_elements_filtered(renderer, output, Layer::Overlay, scale, true, backdrops, &mut pending, &mut out, |ns, _| {
            !(hide_shot && ns == "orchard-screenshot")
        });
        fs_reveal_elements(renderer, output, output_geo, &window, scale, backdrops, &mut pending, &mut out);
        window_elements(renderer, output_geo, &window, output_geo.loc, 0.0, scale, now, &mut out);
        if !pending.is_empty() {
            let mode_size = output
                .current_mode()
                .map(|m| output.current_transform().transform_size(m.size))
                .unwrap_or_default();
            resolve_backdrops(renderer, ctx.fx, pending, mode_size, scale, CLEAR_COLOR_FULLSCREEN, now, &mut out);
        }
        return (out, CLEAR_COLOR_FULLSCREEN);
    }

    let mode_size = output
        .current_mode()
        .map(|m| output.current_transform().transform_size(m.size))
        .unwrap_or_default();
    // render the wallpaper-derived layers first, keep a texture handle for thumbnails
    let mut wallpaper_tex = None;
    let mut pending = Vec::new();
    {
        let wallpaper: Option<&WallpaperTextures> = ctx.fx.wallpaper.textures(renderer, mode_size);
        if let Some(wp) = wallpaper {
            wallpaper_tex = Some((wp.sharp_texture.clone(), (wp.size.w, wp.size.h)));
        }
    }
    let hide_shot = ctx.fx.hide_screenshot_ui;
    layer_elements_filtered(renderer, output, Layer::Overlay, scale, true, backdrops, &mut pending, &mut out, |ns, _| {
        !(hide_shot && ns == "orchard-screenshot")
    });
    // animating snapshots float above regular windows; the genie also passes
    // over the Dock (and the menu bar), like on macOS
    let transient_on = |t: &crate::effects::Transient| {
        t.rect.to_i32_round::<i32>().overlaps(output_geo) || t.target.to_i32_round::<i32>().overlaps(output_geo)
    };
    let is_genie = |t: &crate::effects::Transient| !matches!(t.kind, crate::effects::TransientKind::Close);
    for t in ctx.fx.transients.iter().rev().filter(|t| is_genie(t) && transient_on(t)) {
        out.push(OutputRenderElements::Mesh(t.element(output_geo.loc, scale, now)));
    }
    layer_elements(renderer, output, Layer::Top, scale, true, backdrops, &mut pending, &mut out);
    for t in ctx.fx.transients.iter().rev().filter(|t| !is_genie(t) && transient_on(t)) {
        out.push(OutputRenderElements::Mesh(t.element(output_geo.loc, scale, now)));
    }

    // Space slide: entering windows come in from one side, leaving ones exit the other
    let (enter_off, leave_off) = ctx
        .fx
        .space_transition
        .as_ref()
        .filter(|t| t.output == output.name())
        .map(|t| crate::spaces::transition_offsets(t, output_geo.size.w as f64, now))
        .unwrap_or((0.0, 0.0));

    let mc = ctx.mc.filter(|m| m.screen_of(output).is_some());
    if let Some(mc) = mc {
        mission_control_elements(renderer, output, output_geo, mc, ctx, wallpaper_tex.clone(), leave_off, scale, now, &mut out);
    }

    // Green traffic-light Move & Resize popover (drawn above windows, frosted glass)
    if let Some(menu) = ctx.fx.tile_menu.as_mut() {
        let id = menu.backdrop_id.clone();
        if let Some((el, phys, radius)) = menu.render_element(renderer, output_geo, scale) {
            out.push(OutputRenderElements::Label(el));
            pending.push(PendingBackdrop {
                index: out.len(),
                id,
                region: phys,
                radius,
                alpha: 1.0,
                panel: true,
            });
        }
    }

    // tile drop preview: right below the dragged (topmost) window
    let mut preview = ctx
        .fx
        .tile_preview_at(now)
        .filter(|(r, _)| r.to_i32_round::<i32>().overlaps(output_geo) && mc.is_none());
    for window in space.elements_for_output(output).rev() {
        if let Some(mc) = mc {
            let in_mc = mc.screens.iter().any(|s| s.wins.iter().any(|w| w.window == *window));
            // App Exposé hides other apps' windows
            if in_mc || (mc.app_only.is_some() && mc.progress(now) > 0.02) {
                continue;
            }
        }
        if let Some(loc) = space.element_location(window) {
            window_elements(renderer, output_geo, window, loc, enter_off, scale, now, &mut out);
            // Client-requested blur (org_kde_kwin_blur): insert a live Kawase
            // frosted-glass backdrop right behind this window's client area.
            if mc.is_none() && enter_off.abs() < 0.5 && !window.is_fullscreen() {
                if let Some(blur_id) = crate::kde_blur::window_blur_id(window) {
                    let is_ssd = window.is_ssd();
                    let client_geo = window.client_geometry();
                    let top_off = if is_ssd { crate::shell::ssd::HEADER_BAR_HEIGHT } else { 0 };
                    let (anim_scale, alpha) = window.anim().transform(now);
                    let win_geo = window.geometry();
                    let cx = (loc.x - output_geo.loc.x) as f64 + win_geo.size.w as f64 * 0.5;
                    let cy = (loc.y - output_geo.loc.y) as f64 + win_geo.size.h as f64 * 0.5;
                    let x0 = (loc.x - output_geo.loc.x) as f64;
                    let y0 = (loc.y - output_geo.loc.y + top_off) as f64;
                    let rect = Rectangle::<f64, Logical>::new(
                        (cx + (x0 - cx) * anim_scale, cy + (y0 - cy) * anim_scale).into(),
                        (client_geo.size.w as f64 * anim_scale, client_geo.size.h as f64 * anim_scale).into(),
                    )
                    .to_physical_precise_round(scale);
                    let radius = (crate::chrome::theme::corner_radius() as f64 * anim_scale * scale) as f32;
                    pending.push(PendingBackdrop {
                        index: out.len(),
                        id: blur_id,
                        region: rect,
                        radius,
                        alpha,
                        panel: false,
                    });
                }
            }
        }
        if let Some((r, a)) = preview.take() {
            tile_preview_elements(ctx.fx, r, a, output_geo, scale, &mut out);
        }
    }
    if let Some((r, a)) = preview {
        tile_preview_elements(ctx.fx, r, a, output_geo, scale, &mut out);
    }
    if let Some(t) = ctx.fx.space_transition.as_ref().filter(|t| t.output == output.name()) {
        // windows still in the closing Mission Control grid are drawn there
        let in_mc = |w: &WindowElement| {
            mc.and_then(|m| m.screen_of(output)).map(|s| s.wins.iter().any(|x| x.window == *w)).unwrap_or(false)
        };
        for (window, loc) in t.from.iter().rev().filter(|(w, _)| !in_mc(w)) {
            window_elements(renderer, output_geo, window, *loc, leave_off, scale, now, &mut out);
        }
    }

    let wallpaper_el = ctx.fx.wallpaper.textures(renderer, mode_size).map(|wp| wp.element(scale));
    layer_elements(renderer, output, Layer::Bottom, scale, true, backdrops, &mut pending, &mut out);
    layer_elements(renderer, output, Layer::Background, scale, true, backdrops, &mut pending, &mut out);
    out.extend(wallpaper_el.map(OutputRenderElements::Texture));
    resolve_backdrops(renderer, ctx.fx, pending, mode_size, scale, CLEAR_COLOR, now, &mut out);

    (out, CLEAR_COLOR)
}

#[allow(clippy::too_many_arguments)]
pub fn render_output<'d>(
    output: &Output,
    space: &Space<WindowElement>,
    ctx: &mut RenderCtx<'_>,
    custom_elements: impl IntoIterator<Item = CustomRenderElements<GlesRenderer>>,
    renderer: &mut GlesRenderer,
    framebuffer: &mut GlesTarget<'_>,
    damage_tracker: &'d mut OutputDamageTracker,
    age: usize,
    now: Duration,
) -> Result<RenderOutputResult<'d>, OutputDamageTrackerError<GlesError>> {
    let (elements, clear_color) = output_elements(output, space, ctx, custom_elements, renderer, now);
    damage_tracker.render_output(renderer, framebuffer, age, &elements, clear_color)
}
