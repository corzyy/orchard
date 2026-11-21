//! Damped spring animations, modelled after the springs macOS uses for
//! window and Dock animations (parameterised by response + damping ratio).

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpringParams {
    /// Approximate period of the undamped oscillation, in seconds.
    pub response: f64,
    /// 1.0 = critically damped, < 1.0 bounces.
    pub damping: f64,
}

impl SpringParams {
    /// Snappy, no overshoot: window open/close, zoom.
    pub const SMOOTH: SpringParams = SpringParams {
        response: 0.32,
        damping: 1.0,
    };
    /// Slight overshoot: Dock magnification, Mission Control.
    pub const BOUNCY: SpringParams = SpringParams {
        response: 0.4,
        damping: 0.82,
    };
    /// Mission Control's entrance: a little quicker than [`SMOOTH`], settling
    /// without overshoot so windows never cross their grid cells.
    pub const OVERVIEW: SpringParams = SpringParams {
        response: 0.3,
        damping: 1.0,
    };
    /// Small pointer-driven growth (highlighted window, Space thumbnails):
    /// quick, with just a hint of bounce.
    pub const HOVER: SpringParams = SpringParams {
        response: 0.24,
        damping: 0.85,
    };
    /// Window frame changes (tile, zoom, restore): quick, settles without bounce.
    pub const FRAME: SpringParams = SpringParams {
        response: 0.3,
        damping: 1.0,
    };
    /// Tile drop preview growing out of the dragged window.
    pub const PREVIEW: SpringParams = SpringParams {
        response: 0.22,
        damping: 0.9,
    };
    /// Slower, used for the genie effect.
    pub const GENIE: SpringParams = SpringParams {
        response: 0.5,
        damping: 1.0,
    };
}

#[derive(Debug, Clone, Copy)]
pub struct Spring {
    pub from: f64,
    pub to: f64,
    pub initial_velocity: f64,
    pub params: SpringParams,
    /// time the spring started, monotonic
    pub start: Duration,
}

impl Spring {
    pub fn new(from: f64, to: f64, params: SpringParams, now: Duration) -> Self {
        Spring {
            from,
            to,
            initial_velocity: 0.0,
            params,
            start: now,
        }
    }

    fn omega(&self) -> f64 {
        2.0 * std::f64::consts::PI / self.params.response.max(0.01)
    }

    /// Displacement from the target at time `t` seconds after start.
    fn displacement(&self, t: f64) -> f64 {
        let w = self.omega();
        let z = self.params.damping;
        let x0 = self.from - self.to;
        let v0 = self.initial_velocity;
        if z < 1.0 {
            let wd = w * (1.0 - z * z).sqrt();
            let a = x0;
            let b = (v0 + z * w * x0) / wd;
            (-z * w * t).exp() * (a * (wd * t).cos() + b * (wd * t).sin())
        } else {
            // critically damped (treat over-damped as critical)
            (x0 + (v0 + w * x0) * t) * (-w * t).exp()
        }
    }

    pub fn value_at(&self, now: Duration) -> f64 {
        let t = now.saturating_sub(self.start).as_secs_f64();
        if self.is_done_at(now) {
            return self.to;
        }
        self.to + self.displacement(t)
    }

    pub fn velocity_at(&self, now: Duration) -> f64 {
        let t = now.saturating_sub(self.start).as_secs_f64();
        let h = 1e-4;
        (self.displacement(t + h) - self.displacement(t)) / h
    }

    pub fn is_done_at(&self, now: Duration) -> bool {
        // A spring that never had anywhere to go (e.g. an idle hover spring)
        // is settled from the start.
        if (self.from - self.to).abs() < 1e-12 && self.initial_velocity.abs() < 1e-12 {
            return true;
        }
        let t = now.saturating_sub(self.start).as_secs_f64();
        let range = (self.from - self.to).abs().max(1e-6);
        // envelope based settle check: displacement and velocity both tiny
        let d = self.displacement(t).abs() / range;
        let envelope = (-self.params.damping.min(1.0) * self.omega() * t).exp() * (1.0 + self.omega() * t);
        (d < 0.001 && envelope < 0.002) || t > 3.0
    }

    /// Retarget a running spring, keeping its current position and velocity.
    pub fn retarget(&mut self, to: f64, now: Duration) {
        let pos = self.value_at(now);
        let vel = if self.is_done_at(now) {
            0.0
        } else {
            self.velocity_at(now)
        };
        self.from = pos;
        self.to = to;
        self.initial_velocity = vel;
        self.start = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    #[test]
    fn starts_and_ends_at_endpoints() {
        let s = Spring::new(0.0, 1.0, SpringParams::SMOOTH, ms(1000));
        assert!((s.value_at(ms(1000)) - 0.0).abs() < 1e-9);
        assert!(s.is_done_at(ms(3000)));
        assert_eq!(s.value_at(ms(3000)), 1.0);
    }

    #[test]
    fn critically_damped_never_overshoots() {
        let s = Spring::new(0.0, 1.0, SpringParams::SMOOTH, ms(0));
        for t in 0..1000 {
            assert!(s.value_at(ms(t)) <= 1.0 + 1e-9);
        }
    }

    #[test]
    fn bouncy_overshoots() {
        let s = Spring::new(0.0, 1.0, SpringParams::BOUNCY, ms(0));
        let max = (0..1000).map(|t| s.value_at(ms(t))).fold(0.0, f64::max);
        assert!(max > 1.0);
    }

    #[test]
    fn settles_in_reasonable_time() {
        let s = Spring::new(0.0, 1.0, SpringParams::SMOOTH, ms(0));
        assert!(!s.is_done_at(ms(100)));
        assert!(s.is_done_at(ms(800)), "value at 800ms {}", s.value_at(ms(800)));
    }

    #[test]
    fn zero_length_spring_is_done() {
        let s = Spring::new(0.0, 0.0, SpringParams::HOVER, ms(0));
        assert!(s.is_done_at(ms(0)));
        assert_eq!(s.value_at(ms(0)), 0.0);
    }

    #[test]
    fn retarget_is_continuous() {
        let mut s = Spring::new(0.0, 1.0, SpringParams::SMOOTH, ms(0));
        let before = s.value_at(ms(100));
        s.retarget(0.0, ms(100));
        assert!((s.value_at(ms(100)) - before).abs() < 1e-9);
    }
}
