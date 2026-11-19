//! The "genie" minimize effect: a window texture drawn on a deformed mesh that
//! funnels into its Dock target. Also used (with a flat mesh) for scaled /
//! faded transient windows such as the close animation.

use std::ffi::CString;

use smithay::{
    backend::renderer::{
        element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
        gles::{ffi, GlesError, GlesFrame, GlesRenderer, GlesTexture},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
    },
    utils::{Buffer, Physical, Rectangle, Scale, Transform},
};

const VERT: &str = r#"#version 100
uniform mat3 proj;
attribute vec2 a_pos;
attribute vec2 a_uv;
varying vec2 v_uv;
void main() {
    v_uv = a_uv;
    gl_Position = vec4((proj * vec3(a_pos, 1.0)).xy, 0.0, 1.0);
}
"#;

const FRAG: &str = r#"#version 100
precision mediump float;
uniform sampler2D tex;
uniform float alpha;
varying vec2 v_uv;
void main() {
    gl_FragColor = texture2D(tex, v_uv) * alpha;
}
"#;

const ROWS: usize = 48;

#[derive(Clone, Copy)]
struct MeshProgram {
    program: ffi::types::GLuint,
    u_proj: ffi::types::GLint,
    u_tex: ffi::types::GLint,
    u_alpha: ffi::types::GLint,
    a_pos: ffi::types::GLuint,
    a_uv: ffi::types::GLuint,
}

unsafe fn compile(gl: &ffi::Gles2, kind: ffi::types::GLenum, src: &str) -> Option<ffi::types::GLuint> {
    let shader = gl.CreateShader(kind);
    let c = CString::new(src).ok()?;
    gl.ShaderSource(shader, 1, &c.as_ptr(), std::ptr::null());
    gl.CompileShader(shader);
    let mut ok = 0;
    gl.GetShaderiv(shader, ffi::COMPILE_STATUS, &mut ok);
    if ok == 0 {
        let mut buf = vec![0u8; 1024];
        let mut len = 0;
        gl.GetShaderInfoLog(shader, 1024, &mut len, buf.as_mut_ptr() as *mut _);
        tracing::error!("genie shader: {}", String::from_utf8_lossy(&buf[..len as usize]));
        gl.DeleteShader(shader);
        return None;
    }
    Some(shader)
}

unsafe fn build_program(gl: &ffi::Gles2) -> Option<MeshProgram> {
    let vs = compile(gl, ffi::VERTEX_SHADER, VERT)?;
    let fs = compile(gl, ffi::FRAGMENT_SHADER, FRAG)?;
    let program = gl.CreateProgram();
    gl.AttachShader(program, vs);
    gl.AttachShader(program, fs);
    gl.LinkProgram(program);
    gl.DeleteShader(vs);
    gl.DeleteShader(fs);
    let mut ok = 0;
    gl.GetProgramiv(program, ffi::LINK_STATUS, &mut ok);
    if ok == 0 {
        tracing::error!("genie program failed to link");
        return None;
    }
    let name = |s: &str| CString::new(s).unwrap();
    Some(MeshProgram {
        program,
        u_proj: gl.GetUniformLocation(program, name("proj").as_ptr()),
        u_tex: gl.GetUniformLocation(program, name("tex").as_ptr()),
        u_alpha: gl.GetUniformLocation(program, name("alpha").as_ptr()),
        a_pos: gl.GetAttribLocation(program, name("a_pos").as_ptr()) as u32,
        a_uv: gl.GetAttribLocation(program, name("a_uv").as_ptr()) as u32,
    })
}

struct MeshProgramSlot(std::cell::Cell<Option<MeshProgram>>);

/// Axis aligned rectangle in physical px (f32 for smooth animation).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl RectF {
    pub fn lerp(a: RectF, b: RectF, t: f32) -> RectF {
        RectF {
            x: a.x + (b.x - a.x) * t,
            y: a.y + (b.y - a.y) * t,
            w: a.w + (b.w - a.w) * t,
            h: a.h + (b.h - a.h) * t,
        }
    }

    fn bounds(a: RectF, b: RectF) -> Rectangle<i32, Physical> {
        let x0 = a.x.min(b.x).floor() as i32;
        let y0 = a.y.min(b.y).floor() as i32;
        let x1 = (a.x + a.w).max(b.x + b.w).ceil() as i32;
        let y1 = (a.y + a.h).max(b.y + b.h).ceil() as i32;
        Rectangle::new((x0, y0).into(), (x1 - x0, y1 - y0).into())
    }
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    if (e1 - e0).abs() < f32::EPSILON {
        return if x >= e1 { 1.0 } else { 0.0 };
    }
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Position of mesh point (u, v) in [0,1]² at genie progress `t` in [0,1].
pub fn genie_point(u: f32, v: f32, t: f32, src: RectF, dst: RectF) -> (f32, f32) {
    // phase 1 (0..0.45): the sides bend toward the target
    let bend = smoothstep(0.0, 0.45, t);
    // phase 2 (0.2..1): the window slides down the funnel
    let slide = smoothstep(0.2, 1.0, t);
    let y_src = src.y + v * src.h;
    let y_dst = dst.y + v * dst.h;
    let y = y_src + (y_dst - y_src) * slide;
    // funnel: how far along the vertical path from window top to target top
    let (top, bottom) = if dst.y >= src.y {
        (src.y, dst.y)
    } else {
        (src.y + src.h, dst.y + dst.h)
    };
    let s = smoothstep(top, bottom, y);
    let l_curve = src.x + (dst.x - src.x) * s;
    let r_curve = (src.x + src.w) + ((dst.x + dst.w) - (src.x + src.w)) * s;
    let l = src.x + (l_curve - src.x) * bend;
    let r = (src.x + src.w) + (r_curve - (src.x + src.w)) * bend;
    (l + (r - l) * u, y)
}

/// The genie towards a Dock on the left or right: the same funnel with the
/// axes swapped (the window's left/right edge leads, top and bottom bend).
pub fn genie_point_h(u: f32, v: f32, t: f32, src: RectF, dst: RectF) -> (f32, f32) {
    let tr = |r: RectF| RectF { x: r.y, y: r.x, w: r.h, h: r.w };
    let (a, b) = genie_point(v, u, t, tr(src), tr(dst));
    (b, a)
}

/// Transient window drawn from a snapshot texture.
#[derive(Debug)]
pub struct MeshElement {
    id: Id,
    commit: CommitCounter,
    texture: GlesTexture,
    tex_size: (i32, i32),
    /// where the window was (physical, output-relative)
    pub src: RectF,
    /// where it goes (physical, output-relative)
    pub dst: RectF,
    /// 0 = at src, 1 = at dst
    pub progress: f32,
    pub alpha: f32,
    /// true: genie funnel, false: straight scale between rects
    pub genie: bool,
    /// texture rows must be flipped when sampling
    pub flip_y: bool,
    /// funnel sideways (Dock on the left or right)
    pub horizontal: bool,
}

impl MeshElement {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: Id,
        commit: CommitCounter,
        texture: GlesTexture,
        tex_size: (i32, i32),
        src: RectF,
        dst: RectF,
        progress: f32,
        alpha: f32,
        genie: bool,
    ) -> Self {
        Self {
            id,
            commit,
            texture,
            tex_size,
            src,
            dst,
            progress,
            alpha,
            genie,
            flip_y: false,
            horizontal: false,
        }
    }

    fn vertices(&self) -> Vec<f32> {
        // triangle strip pairs per row: (left, right) for each row boundary
        let mut out = Vec::with_capacity((ROWS + 1) * 2 * 4);
        let t = self.progress.clamp(0.0, 1.0);
        let point = |u: f32, v: f32| -> (f32, f32) {
            if !self.genie {
                let r = RectF::lerp(self.src, self.dst, t);
                (r.x + u * r.w, r.y + v * r.h)
            } else if self.horizontal {
                genie_point_h(u, v, t, self.src, self.dst)
            } else {
                genie_point(u, v, t, self.src, self.dst)
            }
        };
        for i in 0..=ROWS {
            let k = i as f32 / ROWS as f32;
            // a strip of rows (vertical funnel) or of columns (sideways)
            let pair = if self.horizontal { [(k, 0.0f32), (k, 1.0)] } else { [(0.0f32, k), (1.0, k)] };
            for (u, v) in pair {
                let (x, y) = point(u, v);
                let tv = if self.flip_y { 1.0 - v } else { v };
                out.extend_from_slice(&[x, y, u, tv]);
            }
        }
        out
    }
}

impl Element for MeshElement {
    fn id(&self) -> &Id {
        &self.id
    }
    fn current_commit(&self) -> CommitCounter {
        self.commit
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size((self.tex_size.0 as f64, self.tex_size.1 as f64).into())
    }
    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        if self.genie {
            RectF::bounds(self.src, self.dst)
        } else {
            let r = RectF::lerp(self.src, self.dst, self.progress.clamp(0.0, 1.0));
            RectF::bounds(r, r)
        }
    }
    fn transform(&self) -> Transform {
        Transform::Normal
    }
    fn damage_since(&self, scale: Scale<f64>, _commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        let geo = self.geometry(scale);
        DamageSet::from_slice(&[Rectangle::from_size(geo.size)])
    }
    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }
    fn alpha(&self) -> f32 {
        self.alpha
    }
    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for MeshElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        _dst: Rectangle<i32, Physical>,
        _damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        let proj = *frame.projection();
        let slot_program = {
            let ud = frame.egl_context().user_data();
            ud.insert_if_missing(|| MeshProgramSlot(std::cell::Cell::new(None)));
            ud.get::<MeshProgramSlot>().unwrap().0.get()
        };
        let verts = self.vertices();
        let tex_id = self.texture.tex_id();
        let alpha = self.alpha;
        let built = frame.with_context(|gl| unsafe {
            let program = match slot_program {
                Some(p) => p,
                None => build_program(gl)?,
            };
            gl.UseProgram(program.program);
            gl.UniformMatrix3fv(program.u_proj, 1, ffi::FALSE, proj.as_ptr());
            gl.Uniform1f(program.u_alpha, alpha);
            gl.ActiveTexture(ffi::TEXTURE0);
            gl.BindTexture(ffi::TEXTURE_2D, tex_id);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
            gl.Uniform1i(program.u_tex, 0);
            gl.Enable(ffi::BLEND);
            gl.BlendFunc(ffi::ONE, ffi::ONE_MINUS_SRC_ALPHA);
            gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
            let stride = (4 * std::mem::size_of::<f32>()) as i32;
            gl.EnableVertexAttribArray(program.a_pos);
            gl.EnableVertexAttribArray(program.a_uv);
            if gl.VertexAttribDivisor.is_loaded() {
                gl.VertexAttribDivisor(program.a_pos, 0);
                gl.VertexAttribDivisor(program.a_uv, 0);
            }
            gl.VertexAttribPointer(program.a_pos, 2, ffi::FLOAT, ffi::FALSE, stride, verts.as_ptr() as *const _);
            gl.VertexAttribPointer(
                program.a_uv,
                2,
                ffi::FLOAT,
                ffi::FALSE,
                stride,
                verts.as_ptr().add(2) as *const _,
            );
            gl.DrawArrays(ffi::TRIANGLE_STRIP, 0, (verts.len() / 4) as i32);
            gl.DisableVertexAttribArray(program.a_pos);
            gl.DisableVertexAttribArray(program.a_uv);
            gl.BindTexture(ffi::TEXTURE_2D, 0);
            Some(program)
        })?;
        if slot_program.is_none() {
            if let Some(p) = built {
                frame
                    .egl_context()
                    .user_data()
                    .get::<MeshProgramSlot>()
                    .unwrap()
                    .0
                    .set(Some(p));
            }
        }
        Ok(())
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: RectF = RectF {
        x: 100.0,
        y: 100.0,
        w: 800.0,
        h: 600.0,
    };
    const DST: RectF = RectF {
        x: 600.0,
        y: 1000.0,
        w: 60.0,
        h: 40.0,
    };

    #[test]
    fn genie_endpoints() {
        for (u, v) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0), (0.5, 0.5)] {
            let (x, y) = genie_point(u, v, 0.0, SRC, DST);
            assert!((x - (SRC.x + u * SRC.w)).abs() < 0.01 && (y - (SRC.y + v * SRC.h)).abs() < 0.01);
            let (x, y) = genie_point(u, v, 1.0, SRC, DST);
            assert!(
                (x - (DST.x + u * DST.w)).abs() < 0.5 && (y - (DST.y + v * DST.h)).abs() < 0.5,
                "({u},{v}) -> ({x},{y})"
            );
        }
    }

    #[test]
    fn sideways_genie_ends_in_the_tile_and_narrows_its_leading_edge() {
        let src = RectF { x: 400.0, y: 200.0, w: 800.0, h: 600.0 };
        let dst = RectF { x: 10.0, y: 500.0, w: 60.0, h: 40.0 };
        for (u, v) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
            let (x, y) = genie_point_h(u, v, 1.0, src, dst);
            assert!((x - (dst.x + u * dst.w)).abs() < 0.5 && (y - (dst.y + v * dst.h)).abs() < 0.5);
            let (x, y) = genie_point_h(u, v, 0.0, src, dst);
            assert!((x - (src.x + u * src.w)).abs() < 0.01 && (y - (src.y + v * src.h)).abs() < 0.01);
        }
        // midway the left edge (towards a left Dock) is shorter than the right one
        let h_at = |u| {
            let (_, a) = genie_point_h(u, 0.0, 0.4, src, dst);
            let (_, b) = genie_point_h(u, 1.0, 0.4, src, dst);
            b - a
        };
        assert!(h_at(0.0) < h_at(1.0));
    }

    #[test]
    fn genie_bottom_narrows_first() {
        // midway, the bottom edge is narrower than the top edge
        let (l_top, _) = genie_point(0.0, 0.0, 0.4, SRC, DST);
        let (r_top, _) = genie_point(1.0, 0.0, 0.4, SRC, DST);
        let (l_bot, _) = genie_point(0.0, 1.0, 0.4, SRC, DST);
        let (r_bot, _) = genie_point(1.0, 1.0, 0.4, SRC, DST);
        assert!(r_bot - l_bot < r_top - l_top);
    }
}
