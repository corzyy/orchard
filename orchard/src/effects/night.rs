//! Night Shift: warmer colours at night.
//!
//! One full-output quad drawn last with multiplicative blending
//! (`dst = dst * colour`) scales each channel exactly like a redshift gamma
//! ramp would, but needs no DRM gamma ioctl racing the compositor's page flips,
//! works on every backend and never reaches screenshots or screen sharing
//! (only the on-screen render paths add it).

use std::time::Duration;

use smithay::{
    backend::renderer::{
        element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
        gles::{ffi, GlesError, GlesFrame, GlesRenderer},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
        Color32F,
    },
    utils::{Buffer, Physical, Rectangle, Scale, Size},
};

/// How long switching on or off takes.
const FADE: Duration = Duration::from_millis(1800);

pub struct NightShift {
    /// strength 0..1 at the start of the running fade
    from: f32,
    /// strength the fade heads for
    target: f32,
    fade_start: Option<Duration>,
    /// full-strength channel multipliers
    tint: [f32; 3],
    id: Id,
    commit: CommitCounter,
    /// colour drawn on the last frame (a change bumps `commit`)
    last: Option<[f32; 3]>,
}

impl Default for NightShift {
    fn default() -> Self {
        Self {
            from: 0.0,
            target: 0.0,
            fade_start: None,
            tint: [1.0; 3],
            id: Id::new(),
            commit: CommitCounter::default(),
            last: None,
        }
    }
}

impl NightShift {
    /// Re-read the settings and the clock; starts a fade when the wanted state
    /// changed. Cheap: call it from timers and after every settings reload.
    pub fn refresh(&mut self, now: Duration) {
        let s = crate::settings::get();
        let ns = &s.night_shift;
        let (unix, minute) = orchard_config::local_now();
        let on = ns.active(unix, minute) && std::env::var_os("ORCHARD_NO_NIGHT_SHIFT").is_none();
        self.tint = orchard_config::kelvin_to_rgb(ns.kelvin());
        let target = if on { 1.0 } else { 0.0 };
        if (target - self.target).abs() > f32::EPSILON {
            self.from = self.strength(now);
            self.target = target;
            self.fade_start = Some(now);
            tracing::info!(on, kelvin = ns.kelvin(), "night shift");
        }
    }

    fn strength(&self, now: Duration) -> f32 {
        let Some(start) = self.fade_start else { return self.target };
        let t = (now.saturating_sub(start).as_secs_f32() / FADE.as_secs_f32()).clamp(0.0, 1.0);
        // ease in-out: the change is barely noticeable at either end
        let e = t * t * (3.0 - 2.0 * t);
        self.from + (self.target - self.from) * e
    }

    pub fn is_animating(&self, now: Duration) -> bool {
        self.fade_start.map(|s| now.saturating_sub(s) < FADE).unwrap_or(false)
    }

    /// The overlay for one output, or None while Night Shift is off.
    pub fn element(&mut self, output_size: Size<i32, Physical>, now: Duration) -> Option<NightShiftElement> {
        let k = self.strength(now);
        if !self.is_animating(now) {
            self.fade_start = None;
            self.from = self.target;
        }
        if k <= 0.001 {
            self.last = None;
            return None;
        }
        let color = self.tint.map(|c| 1.0 + (c - 1.0) * k);
        if self.last != Some(color) {
            self.last = Some(color);
            self.commit.increment();
        }
        Some(NightShiftElement {
            id: self.id.clone(),
            commit: self.commit,
            size: output_size,
            color,
        })
    }
}

#[derive(Debug)]
pub struct NightShiftElement {
    id: Id,
    commit: CommitCounter,
    size: Size<i32, Physical>,
    color: [f32; 3],
}

impl Element for NightShiftElement {
    fn id(&self) -> &Id {
        &self.id
    }
    fn current_commit(&self) -> CommitCounter {
        self.commit
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size((self.size.w as f64, self.size.h as f64).into())
    }
    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        Rectangle::from_size(self.size)
    }
    fn damage_since(&self, _scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        if commit == Some(self.commit) {
            DamageSet::default()
        } else {
            DamageSet::from_slice(&[Rectangle::from_size(self.size)])
        }
    }
    /// Never opaque: everything below must still be drawn.
    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }
    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for NightShiftElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        if damage.is_empty() {
            return Ok(());
        }
        let [r, g, b] = self.color;
        frame.with_context(|gl| unsafe {
            gl.Enable(ffi::BLEND);
            // result = colour * destination (alpha kept)
            gl.BlendFuncSeparate(ffi::DST_COLOR, ffi::ZERO, ffi::ZERO, ffi::ONE);
        })?;
        // the inherent draw_solid leaves the blend state alone
        let res = GlesFrame::draw_solid(frame, dst, damage, Color32F::new(r, g, b, 1.0));
        frame.with_context(|gl| unsafe {
            gl.BlendFunc(ffi::ONE, ffi::ONE_MINUS_SRC_ALPHA);
        })?;
        res
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}
