//! 3D perspective-tilted textured quad for macOS-style Stage Manager
//! thumbnails and stage-to-strip glide transitions.
//!
//! Uses projective texture coordinates (`uv * w`, `w`) in GLES 2.0 so texture
//! interpolation across the two triangles has zero affine-seam distortion,
//! plus subtle depth shading from the near edge to the receding edge.

use std::ffi::CString;

use smithay::{
    backend::renderer::{
        element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
        gles::{ffi, GlesError, GlesFrame, GlesRenderer, GlesTexture},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
    },
    utils::{Buffer, Physical, Rectangle, Scale, Transform},
};

use crate::effects::genie::RectF;

const VERT: &str = r#"#version 100
uniform mat3 proj;
attribute vec2 a_pos;
attribute vec3 a_uvq;
attribute float a_shade;
varying vec3 v_uvq;
varying float v_shade;
void main() {
    v_uvq = a_uvq;
    v_shade = a_shade;
    gl_Position = vec4((proj * vec3(a_pos, 1.0)).xy, 0.0, 1.0);
}
"#;

const FRAG: &str = r#"#version 100
precision mediump float;
uniform sampler2D tex;
uniform float alpha;
varying vec3 v_uvq;
varying float v_shade;
void main() {
    vec2 uv = v_uvq.xy / v_uvq.z;
    vec4 c = texture2D(tex, uv);
    gl_FragColor = vec4(c.rgb * v_shade, c.a) * alpha;
}
"#;

#[derive(Clone, Copy)]
struct PerspProgram {
    program: ffi::types::GLuint,
    u_proj: ffi::types::GLint,
    u_tex: ffi::types::GLint,
    u_alpha: ffi::types::GLint,
    a_pos: ffi::types::GLuint,
    a_uvq: ffi::types::GLuint,
    a_shade: ffi::types::GLuint,
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
        tracing::error!("perspective shader: {}", String::from_utf8_lossy(&buf[..len as usize]));
        gl.DeleteShader(shader);
        return None;
    }
    Some(shader)
}

unsafe fn build_program(gl: &ffi::Gles2) -> Option<PerspProgram> {
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
        tracing::error!("perspective program failed to link");
        return None;
    }
    let name = |s: &str| CString::new(s).unwrap();
    Some(PerspProgram {
        program,
        u_proj: gl.GetUniformLocation(program, name("proj").as_ptr()),
        u_tex: gl.GetUniformLocation(program, name("tex").as_ptr()),
        u_alpha: gl.GetUniformLocation(program, name("alpha").as_ptr()),
        a_pos: gl.GetAttribLocation(program, name("a_pos").as_ptr()) as u32,
        a_uvq: gl.GetAttribLocation(program, name("a_uvq").as_ptr()) as u32,
        a_shade: gl.GetAttribLocation(program, name("a_shade").as_ptr()) as u32,
    })
}

struct PerspProgramSlot(std::cell::Cell<Option<PerspProgram>>);

/// A window snapshot drawn with 3D Y-axis perspective rotation (Stage Manager).
#[derive(Debug)]
pub struct PerspectiveElement {
    id: Id,
    commit: CommitCounter,
    texture: GlesTexture,
    tex_size: (i32, i32),
    /// Flat bounding rect in output-relative physical pixels.
    pub rect: RectF,
    /// Rotation around the vertical Y axis in radians.
    /// Positive = left edge closer/taller, right edge receding (left strip).
    /// Negative = right edge closer/taller, left edge receding (right strip).
    pub yaw: f32,
    /// Virtual camera focal distance in physical pixels.
    pub focal_len: f32,
    /// Opacity 0..=1.
    pub alpha: f32,
    /// Overall brightness multiplier (e.g. dimmed back cards in a stack).
    pub brightness: f32,
    /// Subtle far-edge shading strength (0.0 = flat, ~0.09 = macOS tilted card).
    pub depth_shade: f32,
}

/// Project one normalized corner `(u, v)` in `[0, 1]^2` of `rect` with Y-axis `yaw`.
/// Returns `(screen_x, screen_y, inv_w, shade)`.
pub fn project_corner(
    rect: RectF,
    u: f32,
    v: f32,
    yaw: f32,
    focal_len: f32,
    brightness: f32,
    depth_shade: f32,
) -> (f32, f32, f32, f32) {
    let cx = rect.x + rect.w * 0.5;
    let cy = rect.y + rect.h * 0.5;
    let lx = (u - 0.5) * rect.w;
    let ly = (v - 0.5) * rect.h;
    let (sy, cy_ang) = yaw.sin_cos();
    let x3 = lx * cy_ang;
    // yaw > 0: left edge (lx < 0) moves closer (z3 > 0 -> larger persp), right recedes
    let z3 = -lx * sy;
    let f = focal_len.max(rect.w.max(rect.h) * 1.25).max(1.0);
    let persp = f / (f - z3).max(f * 0.25);
    let px = cx + x3 * persp;
    let py = cy + ly * persp;
    // Far edge is subtly darkened proportional to yaw angle
    let far_t = if yaw >= 0.0 { u } else { 1.0 - u };
    let tilt_factor = (yaw.abs() / 0.40).clamp(0.0, 1.0);
    let shade = brightness * (1.0 - depth_shade * tilt_factor * far_t);
    (px, py, persp, shade)
}

impl PerspectiveElement {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: Id,
        commit: CommitCounter,
        texture: GlesTexture,
        tex_size: (i32, i32),
        rect: RectF,
        yaw: f32,
        focal_len: f32,
        alpha: f32,
        brightness: f32,
    ) -> Self {
        Self {
            id,
            commit,
            texture,
            tex_size,
            rect,
            yaw,
            focal_len,
            alpha,
            brightness,
            depth_shade: 0.09,
        }
    }

    fn projected_corners(&self) -> [(f32, f32, f32, f32, f32, f32); 4] {
        // Triangle strip order: TL, TR, BL, BR
        let coords = [(0.0f32, 0.0f32), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)];
        coords.map(|(u, v)| {
            let (px, py, q, shade) =
                project_corner(self.rect, u, v, self.yaw, self.focal_len, self.brightness, self.depth_shade);
            (px, py, u * q, v * q, q, shade)
        })
    }

    fn bounds_physical(&self) -> Rectangle<i32, Physical> {
        let cs = self.projected_corners();
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for (px, py, _, _, _, _) in cs {
            min_x = min_x.min(px);
            min_y = min_y.min(py);
            max_x = max_x.max(px);
            max_y = max_y.max(py);
        }
        let x0 = min_x.floor() as i32 - 1;
        let y0 = min_y.floor() as i32 - 1;
        let x1 = max_x.ceil() as i32 + 1;
        let y1 = max_y.ceil() as i32 + 1;
        Rectangle::new((x0, y0).into(), ((x1 - x0).max(0), (y1 - y0).max(0)).into())
    }
}

impl Element for PerspectiveElement {
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
        self.bounds_physical()
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

impl RenderElement<GlesRenderer> for PerspectiveElement {
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
            ud.insert_if_missing(|| PerspProgramSlot(std::cell::Cell::new(None)));
            ud.get::<PerspProgramSlot>().unwrap().0.get()
        };
        let corners = self.projected_corners();
        let mut verts = [0.0f32; 24];
        for (i, (px, py, uq, vq, q, shade)) in corners.into_iter().enumerate() {
            let base = i * 6;
            verts[base] = px;
            verts[base + 1] = py;
            verts[base + 2] = uq;
            verts[base + 3] = vq;
            verts[base + 4] = q;
            verts[base + 5] = shade;
        }
        let tex_id = self.texture.tex_id();
        let alpha = self.alpha;
        let built = frame.with_context(|gl| unsafe {
            let program = match slot_program {
                Some(p) => p,
                None => build_program(gl)?,
            };
            let prev_scissor = gl.IsEnabled(ffi::SCISSOR_TEST) != 0;
            if prev_scissor {
                gl.Disable(ffi::SCISSOR_TEST);
            }
            gl.UseProgram(program.program);
            gl.UniformMatrix3fv(program.u_proj, 1, ffi::FALSE, proj.as_ptr());
            gl.Uniform1f(program.u_alpha, alpha);
            gl.ActiveTexture(ffi::TEXTURE0);
            gl.BindTexture(ffi::TEXTURE_2D, tex_id);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_WRAP_S, ffi::CLAMP_TO_EDGE as i32);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_WRAP_T, ffi::CLAMP_TO_EDGE as i32);
            gl.Uniform1i(program.u_tex, 0);
            gl.Enable(ffi::BLEND);
            gl.BlendFunc(ffi::ONE, ffi::ONE_MINUS_SRC_ALPHA);
            gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
            let stride = (6 * std::mem::size_of::<f32>()) as i32;
            gl.EnableVertexAttribArray(program.a_pos);
            gl.EnableVertexAttribArray(program.a_uvq);
            gl.EnableVertexAttribArray(program.a_shade);
            if gl.VertexAttribDivisor.is_loaded() {
                gl.VertexAttribDivisor(program.a_pos, 0);
                gl.VertexAttribDivisor(program.a_uvq, 0);
                gl.VertexAttribDivisor(program.a_shade, 0);
            }
            gl.VertexAttribPointer(program.a_pos, 2, ffi::FLOAT, ffi::FALSE, stride, verts.as_ptr() as *const _);
            gl.VertexAttribPointer(
                program.a_uvq,
                3,
                ffi::FLOAT,
                ffi::FALSE,
                stride,
                verts.as_ptr().add(2) as *const _,
            );
            gl.VertexAttribPointer(
                program.a_shade,
                1,
                ffi::FLOAT,
                ffi::FALSE,
                stride,
                verts.as_ptr().add(5) as *const _,
            );
            gl.DrawArrays(ffi::TRIANGLE_STRIP, 0, 4);
            gl.DisableVertexAttribArray(program.a_pos);
            gl.DisableVertexAttribArray(program.a_uvq);
            gl.DisableVertexAttribArray(program.a_shade);
            gl.BindTexture(ffi::TEXTURE_2D, 0);
            gl.UseProgram(0);
            if prev_scissor {
                gl.Enable(ffi::SCISSOR_TEST);
            }
            Some(program)
        })?;
        if slot_program.is_none() {
            if let Some(p) = built {
                frame
                    .egl_context()
                    .user_data()
                    .get::<PerspProgramSlot>()
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

    #[test]
    fn left_strip_tilt_makes_left_edge_taller_than_right() {
        let r = RectF { x: 20.0, y: 100.0, w: 160.0, h: 100.0 };
        let (_, tl_y, _, _) = project_corner(r, 0.0, 0.0, 0.36, 900.0, 1.0, 0.09);
        let (_, bl_y, _, _) = project_corner(r, 0.0, 1.0, 0.36, 900.0, 1.0, 0.09);
        let (_, tr_y, _, _) = project_corner(r, 1.0, 0.0, 0.36, 900.0, 1.0, 0.09);
        let (_, br_y, _, _) = project_corner(r, 1.0, 1.0, 0.36, 900.0, 1.0, 0.09);
        let left_h = bl_y - tl_y;
        let right_h = br_y - tr_y;
        assert!(left_h > right_h, "left edge ({left_h}) should be taller than right edge ({right_h})");
    }

    #[test]
    fn zero_yaw_is_identity() {
        let r = RectF { x: 50.0, y: 80.0, w: 400.0, h: 300.0 };
        for (u, v) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
            let (px, py, q, shade) = project_corner(r, u, v, 0.0, 900.0, 1.0, 0.09);
            assert!((px - (r.x + u * r.w)).abs() < 1e-4);
            assert!((py - (r.y + v * r.h)).abs() < 1e-4);
            assert!((q - 1.0).abs() < 1e-4);
            assert!((shade - 1.0).abs() < 1e-4);
        }
    }
}
