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

pub fn network() -> Network {
    if let Ok(s) = std::fs::read_to_string("/proc/net/wireless") {
        if let Some(line) = s.lines().nth(2) {
            // link quality, usually out of 70
            let q: f32 = line
                .split_whitespace()
                .nth(2)
                .and_then(|v| v.trim_end_matches('.').parse().ok())
                .unwrap_or(0.0);
            return Network::Wifi(((q / 70.0) * 3.0).round().clamp(1.0, 3.0) as u8);
        }
    }
    if let Ok(rd) = std::fs::read_dir("/sys/class/net") {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name == "lo" || name.starts_with("virbr") || name.starts_with("docker") || name.starts_with("veth") {
                continue;
            }
            let state = std::fs::read_to_string(e.path().join("operstate")).unwrap_or_default();
            if state.trim() == "up" {
                return Network::Wired;
            }
        }
    }
    Network::Offline
}
