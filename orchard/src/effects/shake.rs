//! Shake to locate: wiggling the mouse quickly makes the pointer grow for a
//! moment so it is easy to find (macOS Accessibility → Pointer).
//!
//! A shake is several quick horizontal direction changes, each after a
//! decent run, within a short window. Slow back-and-forth (text selection,
//! precise work) never qualifies.

use std::{collections::VecDeque, time::Duration};

use smithay::utils::{Logical, Point};

/// Reversals that must fall inside `WINDOW`.
const REVERSALS: usize = 4;
const WINDOW: Duration = Duration::from_millis(700);
/// Minimum horizontal travel (logical px) between two reversals.
const MIN_RUN: f64 = 70.0;
/// A pause this long ends the current run.
const IDLE: Duration = Duration::from_millis(120);
/// Full size relative to normal.
pub const MAX_ZOOM: f64 = 3.0;
const GROW: Duration = Duration::from_millis(180);
/// Stays big this long after the last wiggle, then shrinks back.
const HOLD: Duration = Duration::from_millis(350);
const SHRINK: Duration = Duration::from_millis(450);

#[derive(Default)]
pub struct Shake {
    last: Option<(Point<f64, Logical>, Duration)>,
    /// direction of the current run (-1, 0, 1) and its length
    dir: i8,
    run: f64,
    reversals: VecDeque<Duration>,
    /// when the pointer started growing / when it was last shaken
    grow_start: Option<Duration>,
    last_shake: Duration,
}

fn ease(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

impl Shake {
    /// Feed one pointer position (global logical) at `now`.
    pub fn motion(&mut self, pos: Point<f64, Logical>, now: Duration) {
        let Some((prev, at)) = self.last.replace((pos, now)) else { return };
        if now.saturating_sub(at) > IDLE {
            self.dir = 0;
            self.run = 0.0;
            self.reversals.clear();
        }
        let dx = pos.x - prev.x;
        if dx.abs() < 0.5 {
            return;
        }
        let dir = if dx > 0.0 { 1 } else { -1 };
        if dir == self.dir {
            self.run += dx.abs();
        } else {
            if self.dir != 0 && self.run >= MIN_RUN {
                self.reversals.push_back(now);
            }
            self.dir = dir;
            self.run = dx.abs();
        }
        while self.reversals.front().is_some_and(|t| now.saturating_sub(*t) > WINDOW) {
            self.reversals.pop_front();
        }
        let growing = self.grow_start.is_some();
        // once big, any continued wiggle keeps it big
        if self.reversals.len() >= REVERSALS || (growing && !self.reversals.is_empty()) {
            if !growing || self.zoom(now) <= 1.0 + 1e-3 {
                self.grow_start = Some(now);
            }
            self.last_shake = now;
        }
    }

    /// Pointer size factor at `now` (1.0 when not shaken).
    pub fn zoom(&self, now: Duration) -> f64 {
        let Some(start) = self.grow_start else { return 1.0 };
        let up = ease(now.saturating_sub(start).as_secs_f64() / GROW.as_secs_f64());
        let down = ease(now.saturating_sub(self.last_shake + HOLD).as_secs_f64() / SHRINK.as_secs_f64());
        1.0 + (MAX_ZOOM - 1.0) * up * (1.0 - down)
    }

    pub fn is_animating(&self, now: Duration) -> bool {
        self.grow_start.is_some() && now < self.last_shake + HOLD + SHRINK
    }

    /// Drop finished state (call once per frame).
    pub fn settle(&mut self, now: Duration) {
        if self.grow_start.is_some() && !self.is_animating(now) {
            self.grow_start = None;
            self.reversals.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Wiggle left/right `swings` times, `amp` px each way, `step_ms` per sample.
    fn wiggle(s: &mut Shake, start: u64, swings: usize, amp: f64, step_ms: u64) -> u64 {
        let mut t = start;
        let mut x = 500.0;
        for i in 0..swings {
            let dir = if i % 2 == 0 { 1.0 } else { -1.0 };
            for _ in 0..5 {
                x += dir * amp / 5.0;
                t += step_ms;
                s.motion((x, 300.0).into(), ms(t));
            }
        }
        t
    }

    #[test]
    fn fast_wide_shake_grows_then_shrinks() {
        let mut s = Shake::default();
        s.motion((500.0, 300.0).into(), ms(0));
        let t = wiggle(&mut s, 0, 6, 200.0, 8);
        assert!(s.is_animating(ms(t)));
        assert!(s.zoom(ms(t + 200)) > MAX_ZOOM - 0.01, "fully grown while held");
        let end = t + (HOLD + SHRINK).as_millis() as u64 + 10;
        assert!((s.zoom(ms(end)) - 1.0).abs() < 1e-6);
        s.settle(ms(end));
        assert!(!s.is_animating(ms(end)));
    }

    #[test]
    fn slow_or_small_motion_is_not_a_shake() {
        let mut s = Shake::default();
        s.motion((500.0, 300.0).into(), ms(0));
        // small wiggles (text selection)
        let t = wiggle(&mut s, 0, 10, 30.0, 8);
        assert_eq!(s.zoom(ms(t)), 1.0);
        // wide but slow: pauses between samples reset the run
        let mut s = Shake::default();
        s.motion((500.0, 300.0).into(), ms(0));
        let t = wiggle(&mut s, 0, 10, 200.0, 150);
        assert_eq!(s.zoom(ms(t)), 1.0);
        // too few reversals
        let mut s = Shake::default();
        s.motion((500.0, 300.0).into(), ms(0));
        let t = wiggle(&mut s, 0, 3, 200.0, 8);
        assert_eq!(s.zoom(ms(t)), 1.0);
    }
}
