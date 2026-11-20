//! Menu bar status items: clock, battery, network, volume.

use chrono::{Datelike, Local, Timelike};

fn german() -> bool {
    std::env::var("LC_TIME")
        .or_else(|_| std::env::var("LANG"))
        .map(|l| l.starts_with("de"))
        .unwrap_or(false)
}

fn locale_24h() -> bool {
    let l = std::env::var("LC_TIME").or_else(|_| std::env::var("LANG")).unwrap_or_default();
    // 12-hour clocks: US, Canada (en), Australia, India, Philippines …
    !["en_US", "en_CA", "en_AU", "en_IN", "en_PH", "en_NZ"].iter().any(|p| l.starts_with(p))
}

/// macOS style: "Do. 19. Nov. 10:42" (de) or "Thu Nov 19 10:42 AM" (en),
/// shaped by Settings ▸ Menu Bar.
pub fn clock_text(cfg: &orchard_config::MenuBarSettings) -> String {
    let now = Local::now();
    let h24 = cfg.clock_24h.unwrap_or_else(locale_24h);
    let time = match (h24, cfg.show_seconds) {
        (true, false) => format!("{:02}:{:02}", now.hour(), now.minute()),
        (true, true) => format!("{:02}:{:02}:{:02}", now.hour(), now.minute(), now.second()),
        (false, secs) => {
            let (pm, h) = now.hour12();
            let mut t = format!("{h}:{:02}", now.minute());
            if secs {
                t.push_str(&format!(":{:02}", now.second()));
            }
            t.push_str(if pm { " PM" } else { " AM" });
            t
        }
    };
    let mut date = String::new();
    if german() {
        let days = ["Mo.", "Di.", "Mi.", "Do.", "Fr.", "Sa.", "So."];
        let months = [
            "Jan.", "Feb.", "März", "Apr.", "Mai", "Juni", "Juli", "Aug.", "Sept.", "Okt.", "Nov.", "Dez.",
        ];
        if cfg.show_weekday {
            date.push_str(days[now.weekday().num_days_from_monday() as usize]);
            date.push(' ');
        }
        if cfg.show_date {
            date.push_str(&format!("{}. {} ", now.day(), months[now.month0() as usize]));
        }
    } else {
        if cfg.show_weekday {
            date.push_str(&now.format("%a ").to_string());
        }
        if cfg.show_date {
            date.push_str(&now.format("%b %-d ").to_string());
        }
    }
    if date.is_empty() {
        time
    } else {
        format!("{} {time}", date)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Battery {
    pub percent: u32,
    pub charging: bool,
}

pub fn battery() -> Option<Battery> {
    let rd = std::fs::read_dir("/sys/class/power_supply").ok()?;
    for e in rd.flatten() {
        let p = e.path();
        let kind = std::fs::read_to_string(p.join("type")).unwrap_or_default();
        if kind.trim() != "Battery" {
            continue;
        }
        let percent = std::fs::read_to_string(p.join("capacity")).ok()?.trim().parse().ok()?;
        let status = std::fs::read_to_string(p.join("status")).unwrap_or_default();
        return Some(Battery {
            percent,
            charging: status.trim() == "Charging" || status.trim() == "Full",
        });
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Network {
    Wifi(u8),
    Wired,
    Offline,
}

fn is_virtual_iface(name: &str) -> bool {
    name == "lo"
        || name.starts_with("virbr")
        || name.starts_with("docker")
        || name.starts_with("veth")
        || name.starts_with("br-")
        || name.starts_with("tailscale")
        || name.starts_with("wg")
        || name.starts_with("tun")
        || name.starts_with("tap")
        || name.starts_with("utun")
        || name.starts_with("dummy")
        || name.starts_with("podman")
        || name.starts_with("cni")
        || name.starts_with("flannel")
        || name.starts_with("lxc")
}

/// Signal level (1..=3) for wireless interfaces reported by `/proc/net/wireless`.
fn wireless_levels() -> Vec<(String, u8)> {
    let Ok(s) = std::fs::read_to_string("/proc/net/wireless") else {
        return Vec::new();
    };
    s.lines()
        .skip(2)
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let iface = parts.next()?.trim_end_matches(':').to_string();
            let _status = parts.next()?;
            // link quality, usually out of 70
            let q: f32 = parts.next()?.trim_end_matches('.').parse().ok()?;
            let level = ((q / 70.0) * 3.0).round().clamp(1.0, 3.0) as u8;
            Some((iface, level))
        })
        .collect()
}

pub fn network() -> Network {
    let levels = wireless_levels();
    let mut wired_up = false;
    let mut wifi_level: Option<u8> = None;

    if let Ok(rd) = std::fs::read_dir("/sys/class/net") {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if is_virtual_iface(&name) {
                continue;
            }
            let state = std::fs::read_to_string(e.path().join("operstate")).unwrap_or_default();
            if state.trim() != "up" {
                continue;
            }
            if e.path().join("wireless").exists() {
                let lvl = levels.iter().find(|(n, _)| n == &name).map(|(_, l)| *l).unwrap_or(3);
                wifi_level = Some(wifi_level.map_or(lvl, |prev| prev.max(lvl)));
            } else {
                wired_up = true;
            }
        }
    }

    if wired_up {
        Network::Wired
    } else if let Some(level) = wifi_level {
        Network::Wifi(level)
    } else if let Some((_, level)) = levels.first() {
        Network::Wifi(*level)
    } else {
        Network::Offline
    }
}

/// Current default sink volume `(0.0..=1.0+, muted)` via WirePlumber (`wpctl`).
/// Run on a helper thread so the Wayland event loop never blocks.
pub fn volume() -> Option<(f32, bool)> {
    let out = std::process::Command::new("timeout")
        .args(["2", "wpctl", "get-volume", "@DEFAULT_AUDIO_SINK@"])
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    crate::osd::parse_wpctl(&String::from_utf8_lossy(&out.stdout))
}
