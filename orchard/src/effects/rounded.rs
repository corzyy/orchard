//! Rounded window corners: wraps a surface render element and draws it with a
//! texture shader that masks the corners.

use smithay::{
    backend::renderer::{
        element::{surface::WaylandSurfaceRenderElement, Element, Id, Kind, RenderElement, UnderlyingStorage},
        gles::{GlesError, GlesFrame, GlesRenderer, GlesTexProgram, Uniform, UniformName, UniformType},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
    },
    utils::{Buffer, Physical, Point, Rectangle, Scale, Transform},
};

use std::cell::RefCell;

const SHADER: &str = include_str!("shaders/rounded.frag");

/// A compiled program cached per GL context, recompiled after `reload_shaders`.
struct ProgramSlot<M>(RefCell<(u64, GlesTexProgram)>, std::marker::PhantomData<M>);
struct RoundedMarker;

fn uniforms() -> Vec<UniformName<'static>> {
    vec![
        UniformName::new("size", UniformType::_2f),
        UniformName::new("radius", UniformType::_1f),
        UniformName::new("corners", UniformType::_4f),
        UniformName::new("src_off", UniformType::_2f),
        UniformName::new("src_scale", UniformType::_2f),
        UniformName::new("rim", UniformType::_4f),
        UniformName::new("rim_width", UniformType::_1f),
    ]
}

fn cached<M: 'static>(renderer: &mut GlesRenderer, file: &str, fallback: &'static str) -> Option<GlesTexProgram> {
    let gen = crate::effects::shader_generation();
    if let Some(cached) = renderer.egl_context().user_data().get::<ProgramSlot<M>>() {
        let (cached_gen, ref prog) = *cached.0.borrow();
        if cached_gen == gen {
            return Some(prog.clone());
        }
    }
    let src = crate::effects::load_shader(file, fallback);
    let compiled = renderer
        .compile_custom_texture_shader(&src, &uniforms())
        .map_err(|e| tracing::error!("failed to compile {file}: {e:?}"))
        .ok();
    let ud = renderer.egl_context().user_data();
    match (compiled, ud.get::<ProgramSlot<M>>()) {
        (Some(program), Some(cached)) => {
            *cached.0.borrow_mut() = (gen, program.clone());
            Some(program)
        }
        (Some(program), None) => {
            ud.insert_if_missing(|| ProgramSlot::<M>(RefCell::new((gen, program.clone())), std::marker::PhantomData));
            Some(program)
        }
        (None, Some(cached)) => {
            // Keep the last working program and advance gen so we don't retry every frame.
            let mut slot = cached.0.borrow_mut();
            slot.0 = gen;
            Some(slot.1.clone())
        }
        (None, None) => None,
    }
}

/// Compile (once per GL context, or again after `reload_shaders`) and return
/// the rounded-corner program.
pub fn program(renderer: &mut GlesRenderer) -> Option<GlesTexProgram> {
    cached::<RoundedMarker>(renderer, "rounded.frag", SHADER)
}

/// Which corners to round (top-left, top-right, bottom-right, bottom-left).
#[derive(Debug, Clone, Copy)]
pub struct Corners(pub [bool; 4]);

impl Corners {
    pub const ALL: Corners = Corners([true; 4]);
    pub const BOTTOM: Corners = Corners([false, false, true, true]);
}

#[derive(Debug)]
pub struct RoundedElement<E = WaylandSurfaceRenderElement<GlesRenderer>> {
    inner: E,
    program: GlesTexProgram,
    radius: f32,
    corners: Corners,
    /// size of the whole texture when the element only samples a crop of it
    tex_size: Option<(f64, f64)>,
    /// optional inner hairline rim: (r, g, b, alpha)
    rim: (f32, f32, f32, f32),
    /// rim thickness in physical pixels
    rim_width: f32,
}

impl<E> RoundedElement<E> {
    /// `radius` is in physical pixels.
    pub fn new(inner: E, program: GlesTexProgram, radius: f32, corners: Corners) -> Self {
        Self {
            inner,
            program,
            radius,
            corners,
            tex_size: None,
            rim: (0.0, 0.0, 0.0, 0.0),
            rim_width: 1.0,
        }
    }

    /// The inner element samples `src` out of a texture of this size (buffer px).
    pub fn with_texture_size(mut self, w: f64, h: f64) -> Self {
        self.tex_size = Some((w, h));
        self
    }

    /// Draw a crisp inner hairline rim highlight along the rounded contour.
    pub fn with_rim(mut self, rgba: [f32; 4], width_phys: f32) -> Self {
        self.rim = (rgba[0], rgba[1], rgba[2], rgba[3]);
        self.rim_width = width_phys.max(1.0);
        self
    }
}

impl<E: Element> Element for RoundedElement<E> {
    fn id(&self) -> &Id {
        self.inner.id()
    }
    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }
    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }
    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.inner.location(scale)
    }
    fn transform(&self) -> Transform {
        self.inner.transform()
    }
    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }
    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        // carve the rounded corners out of the opaque region so whatever is
        // behind them still gets drawn
        let regions = self.inner.opaque_regions(scale);
        let geo = self.inner.geometry(scale);
        let r = self.radius.ceil() as i32;
        let c = self.corners.0;
        let mut cuts = Vec::new();
        let (w, h) = (geo.size.w, geo.size.h);
        if c[0] {
            cuts.push(Rectangle::new((0, 0).into(), (r, r).into()));
        }
        if c[1] {
            cuts.push(Rectangle::new((w - r, 0).into(), (r, r).into()));
        }
        if c[2] {
            cuts.push(Rectangle::new((w - r, h - r).into(), (r, r).into()));
        }
        if c[3] {
            cuts.push(Rectangle::new((0, h - r).into(), (r, r).into()));
        }
        regions
            .into_iter()
            .flat_map(|rect| rect.subtract_rects(cuts.iter().copied()))
            .collect()
    }
    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }
    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl<E: RenderElement<GlesRenderer>> RenderElement<GlesRenderer> for RoundedElement<E> {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        let c = self.corners.0;
        let f = |b: bool| if b { 1.0f32 } else { 0.0 };
        // map texture coordinates of the sampled crop back to 0..1
        let (off, scale) = match self.tex_size {
            Some((tw, th)) => (
                ((src.loc.x / tw) as f32, (src.loc.y / th) as f32),
                ((src.size.w / tw) as f32, (src.size.h / th) as f32),
            ),
            None => ((0.0, 0.0), (1.0, 1.0)),
        };
        let uniforms = vec![
            Uniform::new("size", (dst.size.w as f32, dst.size.h as f32)),
            Uniform::new("radius", self.radius),
            Uniform::new("corners", (f(c[0]), f(c[1]), f(c[2]), f(c[3]))),
            Uniform::new("src_off", off),
            Uniform::new("src_scale", scale),
            Uniform::new("rim", self.rim),
            Uniform::new("rim_width", self.rim_width),
        ];
        frame.override_default_tex_program(self.program.clone(), uniforms);
        let res = RenderElement::<GlesRenderer>::draw(&self.inner, frame, src, dst, damage, opaque_regions);
        frame.clear_tex_program_override();
        res
    }

    fn underlying_storage(&self, renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        // never allow direct scan-out: the corners must be masked by our shader
        let _ = renderer;
        None
    }
}
