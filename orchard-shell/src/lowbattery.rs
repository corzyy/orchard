//! Low-battery warnings for the computer and wireless peripherals, like
//! macOS: one notification per threshold crossed while discharging.

use std::collections::HashMap;

/// Laptop battery: warn at 20 %, 10 % and (critical) 5 %.
pub const SYSTEM: &[u32] = &[20, 10, 5];
/// Mouse / keyboard / headset: warn at 15 % and 5 %.
pub const DEVICE: &[u32] = &[15, 5];
/// A device must climb this far above the highest threshold (or charge)
/// before it can warn again, so a reading that flickers 15 ↔ 16 % stays quiet.
const HYSTERESIS: u32 = 3;

#[derive(Default)]
pub struct Watch {
    /// lowest threshold already announced per battery
    warned: HashMap<String, u32>,
}

impl Watch {
    /// Feed one reading. Returns the threshold to announce, if a new one was crossed.
    pub fn check(&mut self, key: &str, percent: u32, charging: bool, thresholds: &[u32]) -> Option<u32> {
        let top = thresholds.iter().copied().max()?;
        if charging || percent > top + HYSTERESIS {
            self.warned.remove(key);
            return None;
        }
        let crossed = thresholds.iter().copied().filter(|t| percent <= *t).min()?;
        match self.warned.get(key) {
            Some(&w) if w <= crossed => None,
            _ => {
                self.warned.insert(key.to_string(), crossed);
                Some(crossed)
            }
        }
    }

    /// Forget batteries that disappeared (unplugged receiver, device switched off).
    pub fn retain(&mut self, keys: &[&str]) {
        self.warned.retain(|k, _| keys.contains(&k.as_str()));
    }
}

/// Stable notification id per battery, so a lower level replaces the previous
/// warning. Far above the ids the D-Bus server hands out (counting from 1).
pub fn notification_id(key: &str) -> u32 {
    let h = key.bytes().fold(0x811c_9dc5u32, |h, b| (h ^ b as u32).wrapping_mul(0x0100_0193));
    0xF000_0000 | (h & 0x0FFF_FFFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warns_once_per_threshold_while_discharging() {
        let mut w = Watch::default();
        assert_eq!(w.check("BAT0", 50, false, SYSTEM), None);
        assert_eq!(w.check("BAT0", 20, false, SYSTEM), Some(20));
        assert_eq!(w.check("BAT0", 19, false, SYSTEM), None);
        assert_eq!(w.check("BAT0", 11, false, SYSTEM), None);
        assert_eq!(w.check("BAT0", 10, false, SYSTEM), Some(10));
        assert_eq!(w.check("BAT0", 4, false, SYSTEM), Some(5));
        assert_eq!(w.check("BAT0", 3, false, SYSTEM), None);
    }

    #[test]
    fn starting_low_warns_at_the_level_reached() {
        let mut w = Watch::default();
        assert_eq!(w.check("BAT0", 8, false, SYSTEM), Some(10));
        assert_eq!(w.check("BAT0", 9, false, SYSTEM), None);
    }

    #[test]
    fn charging_or_recovering_rearms() {
        let mut w = Watch::default();
        assert_eq!(w.check("mouse", 14, false, DEVICE), Some(15));
        // flicker just above the threshold stays quiet
        assert_eq!(w.check("mouse", 16, false, DEVICE), None);
        assert_eq!(w.check("mouse", 15, false, DEVICE), None);
        // plugged in: re-armed
        assert_eq!(w.check("mouse", 15, true, DEVICE), None);
        assert_eq!(w.check("mouse", 15, false, DEVICE), Some(15));
        // recovered well above: re-armed
        assert_eq!(w.check("mouse", 40, false, DEVICE), None);
        assert_eq!(w.check("mouse", 12, false, DEVICE), Some(15));
    }

    #[test]
    fn ids_are_stable_and_reserved() {
        assert_eq!(notification_id("hidpp_battery_0"), notification_id("hidpp_battery_0"));
        assert_ne!(notification_id("hidpp_battery_0"), notification_id("BAT0"));
        assert!(notification_id("x") >= 0xF000_0000);
    }
}
