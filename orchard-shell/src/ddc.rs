//! Monitor brightness control via `brightnessctl` (built-in backlight) and
//! `ddcutil` (external monitors over DDC/CI).
//!
//! DDC/CI transactions over I2C take ~50–200 ms and fail if two `ddcutil`
//! processes touch the same bus concurrently, so slider updates are coalesced
//! per bus on a single background worker thread.

use std::{
    collections::HashMap,
    process::Command,
    sync::{Condvar, Mutex, OnceLock},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq)]
pub struct DisplayBrightness {
    /// `None` for `/sys/class/backlight` (brightnessctl), `Some(bus)` for `/dev/i2c-<bus>` (ddcutil).
    pub bus: Option<u8>,
    /// Human-readable display name (e.g. monitor model or "Built-in Display").
    pub name: String,
    /// Current brightness in `0.0..=1.0`.
    pub value: f32,
}

#[derive(Debug, Clone)]
struct DdcInfo {
    bus: u8,
    name: String,
}

/// `(when, monitors, detection succeeded)`
static DDC_CACHE: OnceLock<Mutex<Option<(Instant, Vec<DdcInfo>, bool)>>> = OnceLock::new();
const CACHE_TTL: Duration = Duration::from_secs(30);
/// A failed `ddcutil detect` is retried soon (so sliders come back), but not on
/// every refresh.
const RETRY_TTL: Duration = Duration::from_secs(3);

fn run_cmd(prog: &str, args: &[&str]) -> Option<String> {
    // an I2C hiccup must never wedge the Control Center worker: a hung ddcutil
    // would otherwise leave the panel waiting for a state that never arrives
    let out = Command::new("timeout").arg("3").arg(prog).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Parse `ddcutil detect --terse` output into `(i2c_bus, model_name)` entries.
pub fn parse_ddc_detect(s: &str) -> Vec<(u8, String)> {
    let mut out = Vec::new();
    let mut in_valid = false;
    let mut cur_display: Option<String> = None;
    let mut cur_bus: Option<u8> = None;
    let mut cur_name: Option<String> = None;

    let flush = |out: &mut Vec<(u8, String)>, in_valid: bool, disp: &mut Option<String>, bus: &mut Option<u8>, name: &mut Option<String>| {
        if in_valid {
            if let Some(b) = bus.take() {
                let label = name
                    .take()
                    .filter(|n| !n.is_empty())
                    .or_else(|| disp.take())
                    .unwrap_or_else(|| format!("Display {b}"));
                out.push((b, label));
                return;
            }
        }
        *disp = None;
        *bus = None;
        *name = None;
    };

    for raw in s.lines() {
        let line = raw.trim();
        if line.starts_with("Display ") {
            flush(&mut out, in_valid, &mut cur_display, &mut cur_bus, &mut cur_name);
            in_valid = true;
            cur_display = Some(line.to_string());
        } else if line.starts_with("Invalid display") {
            flush(&mut out, in_valid, &mut cur_display, &mut cur_bus, &mut cur_name);
            in_valid = false;
        } else if !in_valid {
            continue;
        } else if let Some(rest) = line.strip_prefix("I2C bus:") {
            if let Some(num) = rest.trim().strip_prefix("/dev/i2c-") {
                cur_bus = num.trim().parse().ok();
            }
        } else if let Some(rest) = line.strip_prefix("Monitor:") {
            // Format: "<mfg>:<model>:<serial>"
            let parts: Vec<&str> = rest.trim().split(':').collect();
            let model = parts.get(1).map(|m| m.trim()).filter(|m| !m.is_empty());
            let mfg = parts.first().map(|m| m.trim()).filter(|m| !m.is_empty());
            cur_name = model
                .map(|m| m.to_string())
                .or_else(|| mfg.map(|m| m.to_string()));
        }
    }
    flush(&mut out, in_valid, &mut cur_display, &mut cur_bus, &mut cur_name);
    out
}

/// `(known, monitors)`: `known` is false when `ddcutil detect` failed and the
/// previously cached list (possibly empty) is all we have. That way a transient
/// I2C error does not make the brightness sliders disappear for 30 seconds.
fn detected_ddc_monitors() -> (bool, Vec<DdcInfo>) {
    let cache = DDC_CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(g) = cache.lock() {
        if let Some((at, list, ok)) = g.as_ref() {
            let ttl = if *ok { CACHE_TTL } else { RETRY_TTL };
            if at.elapsed() < ttl {
                return (*ok, list.clone());
            }
        }
    }
    match run_cmd("ddcutil", &["detect", "--terse"]) {
        Some(out) => {
            let list: Vec<DdcInfo> = parse_ddc_detect(&out)
                .into_iter()
                .map(|(bus, name)| DdcInfo { bus, name })
                .collect();
            if let Ok(mut g) = cache.lock() {
                *g = Some((Instant::now(), list.clone(), true));
            }
            (true, list)
        }
        // Keep the last known monitors, mark the result incomplete so the UI
        // does not drop their sliders, and retry in a few seconds.
        None => {
            let list = cache
                .lock()
                .ok()
                .and_then(|g| g.as_ref().map(|(_, l, _)| l.clone()))
                .unwrap_or_default();
            if let Ok(mut g) = cache.lock() {
                *g = Some((Instant::now(), list.clone(), false));
            }
            (false, list)
        }
    }
}

fn query_ddc_vcp10(bus: u8) -> Option<f32> {
    let bus_s = bus.to_string();
    let out = run_cmd(
        "ddcutil",
        &["--bus", &bus_s, "--noverify", "--sleep-multiplier", "0.2", "getvcp", "10", "--brief"],
    )?;
    crate::osd::parse_ddcutil(&out)
}

/// Query all controllable displays (internal backlight + DDC/CI external
/// monitors). The flag is false when any probe failed, so the caller can keep
/// the last known brightness rows instead of dropping the monitors.
pub fn query_displays() -> (Vec<DisplayBrightness>, bool) {
    let mut displays = Vec::new();
    let mut complete = true;
    let has_bl = std::fs::read_dir("/sys/class/backlight")
        .map(|mut d| d.next().is_some())
        .unwrap_or(false);
    if has_bl {
        match run_cmd("brightnessctl", &["-m"]).and_then(|o| crate::osd::parse_brightnessctl(&o)) {
            Some(v) => displays.push(DisplayBrightness {
                bus: None,
                name: "Built-in Display".into(),
                value: v,
            }),
            None => complete = false,
        }
    }

    let (known, ddc_list) = detected_ddc_monitors();
    complete &= known;
    if ddc_list.len() <= 1 {
        for m in ddc_list {
            match query_ddc_vcp10(m.bus) {
                Some(v) => displays.push(DisplayBrightness {
                    bus: Some(m.bus),
                    name: m.name,
                    value: v,
                }),
                None => complete = false,
            }
        }
    } else {
        // Query multiple monitors in parallel so Control Center stays snappy
        let handles: Vec<_> = ddc_list
            .into_iter()
            .map(|m| {
                std::thread::spawn(move || {
                    query_ddc_vcp10(m.bus).map(|value| DisplayBrightness {
                        bus: Some(m.bus),
                        name: m.name,
                        value,
                    })
                })
            })
            .collect();
        for h in handles {
            match h.join() {
                Ok(Some(d)) => displays.push(d),
                _ => complete = false,
            }
        }
    }
    (displays, complete)
}

struct PendingQueue {
    pending: Mutex<HashMap<Option<u8>, f32>>,
    cv: Condvar,
}

fn queue() -> &'static PendingQueue {
    static Q: OnceLock<PendingQueue> = OnceLock::new();
    Q.get_or_init(|| {
        let q = PendingQueue {
            pending: Mutex::new(HashMap::new()),
            cv: Condvar::new(),
        };
        let _ = std::thread::Builder::new().name("ddc-brightness".into()).spawn(|| worker_loop());
        q
    })
}

fn worker_loop() {
    let q = queue();
    loop {
        let batch: Vec<(Option<u8>, f32)> = {
            let mut g = q.pending.lock().unwrap();
            while g.is_empty() {
                g = q.cv.wait(g).unwrap();
            }
            g.drain().collect()
        };
        for (bus, value) in batch {
            apply_now(bus, value);
        }
    }
}

fn apply_now(bus: Option<u8>, value: f32) {
    let pct = (value.clamp(0.0, 1.0) * 100.0).round() as i32;
    match bus {
        None => {
            let _ = run_cmd("brightnessctl", &["-q", "set", &format!("{}%", pct.max(1))]);
        }
        Some(b) => {
            let bus_s = b.to_string();
            let pct_s = pct.clamp(0, 100).to_string();
            let _ = run_cmd(
                "ddcutil",
                &[
                    "--bus",
                    &bus_s,
                    "--noverify",
                    "--sleep-multiplier",
                    "0.2",
                    "setvcp",
                    "10",
                    &pct_s,
                ],
            );
        }
    }
}

/// Queue a coalesced brightness change for `bus` (`None` = backlight, `Some(b)` = DDC/CI bus).
pub fn set_brightness(bus: Option<u8>, value: f32) {
    let q = queue();
    if let Ok(mut g) = q.pending.lock() {
        g.insert(bus, value.clamp(0.0, 1.0));
        q.cv.notify_one();
    }
}

/// Relative brightness step for media keys across all DDC monitors; returns the first monitor's new level.
pub fn adjust_all_ddc(sign: char, amount: i32) -> Option<f32> {
    let (_, monitors) = detected_ddc_monitors();
    if monitors.is_empty() {
        let sign_s = sign.to_string();
        let amt_s = amount.to_string();
        run_cmd("ddcutil", &["--noverify", "setvcp", "10", &sign_s, &amt_s])?;
        return run_cmd("ddcutil", &["getvcp", "10", "--brief"]).and_then(|s| crate::osd::parse_ddcutil(&s));
    }
    let handles: Vec<_> = monitors
        .into_iter()
        .map(|m| {
            std::thread::spawn(move || {
                let bus_s = m.bus.to_string();
                let sign_s = sign.to_string();
                let amt_s = amount.to_string();
                run_cmd(
                    "ddcutil",
                    &[
                        "--bus",
                        &bus_s,
                        "--noverify",
                        "--sleep-multiplier",
                        "0.2",
                        "setvcp",
                        "10",
                        &sign_s,
                        &amt_s,
                    ],
                )?;
                query_ddc_vcp10(m.bus)
            })
        })
        .collect();
    let mut first = None;
    for h in handles {
        if let Ok(Some(v)) = h.join() {
            if first.is_none() {
                first = Some(v);
            }
        }
    }
    first
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ddcutil_terse() {
        let sample = "\
Display 1
   I2C bus:             /dev/i2c-4
   DRM connector:       card1-DP-1
   Monitor:             GSM:LG ULTRAGEAR:123456

Invalid display
   I2C bus:             /dev/i2c-5
   Monitor:             :::

Display 2
   I2C bus:             /dev/i2c-7
   Monitor:             DEL:DELL U2723QE:987654
";
        assert_eq!(
            parse_ddc_detect(sample),
            vec![(4, "LG ULTRAGEAR".to_string()), (7, "DELL U2723QE".to_string())]
        );
    }
}
