//! Shared macOS-style spring physics, easing curves and colour helpers for
//! `orchard-shell` surfaces (Control Center, Notification Center, OSD, menus,
//! dialogs, app switcher).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::text::Rgba;

static REDUCE_MOTION: AtomicBool = AtomicBool::new(false);

/// Whether Accessibility → Reduce motion is enabled.
pub fn reduced() -> bool {
    REDUCE_MOTION.load(Ordering::Relaxed)
}

/// Updates the Reduce Motion setting for all shell surfaces.
pub fn set_reduced(on: bool) {
    REDUCE_MOTION.store(on, Ordering::Relaxed);
}

/// Damped spring parameters modelled after Apple's `CASpringAnimation`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpringParams {
    /// Approximate undamped period in seconds (smaller = snappier).
    pub response: f32,
    /// Damping ratio (`1.0` = critically damped, `< 1.0` = subtle settle).
    pub damping: f32,
}

impl SpringParams {
    /// Control Center / Notification Center toggle switches & circular toggles.
    pub const SWITCH: Self = Self { response: 0.21, damping: 0.86 };
    /// Pill slider click-jump / mute glide (never overshoots).
    pub const SLIDER: Self = Self { response: 0.13, damping: 1.0 };
    /// Control Center container height morph between views.
    pub const PANEL_HEIGHT: Self = Self { response: 0.22, damping: 0.94 };
    /// Disclosure chevron rotation.
    pub const CHEVRON: Self = Self { response: 0.18, damping: 0.92 };
    /// Cmd+Tab selection highlight glide.
    pub const SWITCHER: Self = Self { response: 0.14, damping: 0.94 };
}

/// Analytical damped spring for a single scalar value.
#[derive(Debug, Clone, Copy)]
pub struct SpringFloat {
    from: f32,
    to: f32,
    v0: f32,
    start: Instant,
    params: SpringParams,
    settled: bool,
}

impl SpringFloat {
    /// Creates a spring already settled at `initial`.
    pub fn new(initial: f32, params: SpringParams) -> Self {
        Self {
            from: initial,
            to: initial,
            v0: 0.0,
            start: Instant::now(),
            params,
            settled: true,
        }
    }

    /// Snaps immediately to `v` with zero velocity.
    pub fn snap(&mut self, v: f32) {
        self.from = v;
        self.to = v;
        self.v0 = 0.0;
        self.settled = true;
    }

    /// Smoothly retargets the spring, preserving current position and velocity.
    pub fn set_target(&mut self, target: f32, now: Instant) {
        if (self.to - target).abs() < 1e-4 {
            return;
        }
        if reduced() {
            self.snap(target);
            return;
        }
        let pos = self.value(now);
        let vel = if self.settled { 0.0 } else { self.velocity(now) };
        self.from = pos;
        self.to = target;
        self.v0 = vel;
        self.start = now;
        self.settled = (pos - target).abs() < 1e-4 && vel.abs() < 1e-3;
    }

    /// Current value at `now`.
    pub fn value(&self, now: Instant) -> f32 {
        if self.settled || self.is_done(now) {
            return self.to;
        }
        let t = now.saturating_duration_since(self.start).as_secs_f32();
        self.to + self.disp(t)
    }

    /// Returns `true` if still animating at `now`.
    pub fn is_animating(&self, now: Instant) -> bool {
        !self.settled && !self.is_done(now)
    }

    /// Marks the spring settled once it has converged, returning its value.
    pub fn sample(&mut self, now: Instant) -> f32 {
        if self.settled || self.is_done(now) {
            self.settled = true;
            self.from = self.to;
            self.v0 = 0.0;
            return self.to;
        }
        let t = now.saturating_duration_since(self.start).as_secs_f32();
        self.to + self.disp(t)
    }

    fn disp(&self, t: f32) -> f32 {
        let w = std::f32::consts::TAU / self.params.response.max(0.01);
        let z = self.params.damping;
        let x0 = self.from - self.to;
        let v0 = self.v0;
        if z < 1.0 {
            let wd = w * (1.0 - z * z).sqrt();
            let b = (v0 + z * w * x0) / wd;
            (-z * w * t).exp() * (x0 * (wd * t).cos() + b * (wd * t).sin())
        } else {
            (x0 + (v0 + w * x0) * t) * (-w * t).exp()
        }
    }

    fn velocity(&self, now: Instant) -> f32 {
        let t = now.saturating_duration_since(self.start).as_secs_f32();
        let h = 1e-4_f32;
        (self.disp(t + h) - self.disp(t)) / h
    }

    fn is_done(&self, now: Instant) -> bool {
        if reduced() {
            return true;
        }
        let t = now.saturating_duration_since(self.start).as_secs_f32();
        let w = std::f32::consts::TAU / self.params.response.max(0.01);
        let range = (self.from - self.to).abs().max(1.0);
        let env = (-self.params.damping.min(1.0) * w * t).exp() * (1.0 + w * t);
        (self.disp(t).abs() < 0.0015 * range && env < 0.003) || t > 2.0
    }
}

/// Standard macOS cubic ease-out (`1 - (1 - t)^3`).
pub fn ease_out_cubic(t: f32) -> f32 {
    let inv = 1.0 - t.clamp(0.0, 1.0);
    1.0 - inv * inv * inv
}

/// Smooth cubic ease-in (`t^3`), used for crisp exit fades.
pub fn ease_in_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * t
}

pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Linearly blends two `Rgba` colours.
pub fn lerp_rgba(a: Rgba, b: Rgba, t: f32) -> Rgba {
    let t = t.clamp(0.0, 1.0);
    [
        lerp(a[0] as f32, b[0] as f32, t).round().clamp(0.0, 255.0) as u8,
        lerp(a[1] as f32, b[1] as f32, t).round().clamp(0.0, 255.0) as u8,
        lerp(a[2] as f32, b[2] as f32, t).round().clamp(0.0, 255.0) as u8,
        lerp(a[3] as f32, b[3] as f32, t).round().clamp(0.0, 255.0) as u8,
    ]
}

/// Multiplies a colour's alpha channel by `factor` (`0..=1`).
pub fn scale_alpha(c: Rgba, factor: f32) -> Rgba {
    [c[0], c[1], c[2], (c[3] as f32 * factor.clamp(0.0, 1.0)).round().clamp(0.0, 255.0) as u8]
}

/// Subtle tactile macOS tap pulse (`0.91 -> 1.0` over 200 ms) for circular toggle buttons.
pub fn tap_scale(pressed_at: Option<Instant>, now: Instant) -> (f32, bool) {
    let Some(t0) = pressed_at else { return (1.0, false) };
    if reduced() {
        return (1.0, false);
    }
    let dt = now.saturating_duration_since(t0).as_secs_f32();
    const DUR: f32 = 0.20;
    if dt >= DUR {
        return (1.0, false);
    }
    // Quick dip in the first 55 ms, smooth spring-back over the remaining 145 ms
    let s = if dt < 0.055 {
        lerp(1.0, 0.91, ease_out_cubic(dt / 0.055))
    } else {
        lerp(0.91, 1.0, ease_out_cubic((dt - 0.055) / (DUR - 0.055)))
    };
    (s, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn spring_float_converges_without_overshoot_when_critical() {
        set_reduced(false);
        let t0 = Instant::now();
        let mut s = SpringFloat::new(0.0, SpringParams::SLIDER);
        s.set_target(1.0, t0);
        assert!(s.is_animating(t0 + Duration::from_millis(20)));
        let mid = s.value(t0 + Duration::from_millis(50));
        assert!(mid > 0.0 && mid <= 1.001);
        assert!(!s.is_animating(t0 + Duration::from_millis(600)));
        assert_eq!(s.value(t0 + Duration::from_millis(600)), 1.0);
    }

    #[test]
    fn lerp_rgba_interpolates_channels() {
        assert_eq!(lerp_rgba([0, 100, 200, 0], [100, 200, 0, 255], 0.5), [50, 150, 100, 128]);
    }
}
