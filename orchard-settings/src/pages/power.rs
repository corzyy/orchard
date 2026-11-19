//! Battery: internal and peripheral batteries (sysfs) and the power mode
//! (power-profiles-daemon / tuned-ppd over D-Bus).

use eframe::egui::{self, vec2, Color32, Rect, Sense, Stroke, StrokeKind, Ui};

use super::{error_line, hero, Cx};
use crate::{
    style::pal,
    sys::{self, read_trim, Bg, Job},
    widgets::{group, header, popup, secondary, toggle},
    Page,
};

#[derive(Default)]
pub struct PowerState {
    info: Bg<PowerInfo>,
    job: Job,
}

#[derive(Clone, Default)]
struct Battery {
    name: String,
    capacity: Option<u8>,
    status: String,
    health: Option<u8>,
    cycles: Option<u32>,
    peripheral: bool,
}

#[derive(Clone, Default)]
struct PowerInfo {
    batteries: Vec<Battery>,
    on_ac: Option<bool>,
    profile: Option<String>,
    profile_bus: Option<(&'static str, &'static str, &'static str)>,
}

const BUSES: [(&str, &str, &str); 2] = [
    ("org.freedesktop.UPower.PowerProfiles", "/org/freedesktop/UPower/PowerProfiles", "org.freedesktop.UPower.PowerProfiles"),
    ("net.hadess.PowerProfiles", "/net/hadess/PowerProfiles", "net.hadess.PowerProfiles"),
];

fn load() -> PowerInfo {
    let mut info = PowerInfo::default();
    if let Ok(rd) = std::fs::read_dir("/sys/class/power_supply") {
        let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            let r = |f: &str| read_trim(&p.join(f).to_string_lossy());
            match r("type").as_deref() {
                Some("Mains") => info.on_ac = Some(r("online").as_deref() == Some("1")),
                Some("Battery") => {
                    let full = r("energy_full").or_else(|| r("charge_full")).and_then(|v| v.parse::<f64>().ok());
                    let design = r("energy_full_design").or_else(|| r("charge_full_design")).and_then(|v| v.parse::<f64>().ok());
                    let peripheral = r("scope").as_deref() == Some("Device");
                    let model = r("model_name").unwrap_or_default();
                    info.batteries.push(Battery {
                        name: if model.is_empty() {
                            if peripheral { "Device".into() } else { "Internal battery".into() }
                        } else {
                            model
                        },
                        capacity: r("capacity").and_then(|v| v.parse().ok()).or_else(|| {
                            // some HID devices only report a level
                            match r("capacity_level").as_deref() {
                                Some("Full") => Some(100),
                                Some("High") => Some(80),
                                Some("Normal") => Some(55),
                                Some("Low") => Some(20),
                                Some("Critical") => Some(5),
                                _ => None,
                            }
                        }),
                        status: r("status").unwrap_or_default(),
                        health: match (full, design) {
                            (Some(f), Some(d)) if d > 0.0 => Some(((f / d) * 100.0).min(100.0) as u8),
                            _ => None,
                        },
                        cycles: r("cycle_count").and_then(|v| v.parse().ok()).filter(|c| *c > 0),
                        peripheral,
                    });
                }
                _ => {}
            }
        }
    }
    for bus in BUSES {
        if let Some(out) = sys::output("busctl", &["get-property", bus.0, bus.1, bus.2, "ActiveProfile"]) {
            // `s "balanced"`
            info.profile = out.split('"').nth(1).map(String::from);
            info.profile_bus = Some(bus);
            break;
        }
    }
    info
}

fn battery_glyph(ui: &mut Ui, pct: Option<u8>, charging: bool) {
    let p = pal();
    let (r, _) = ui.allocate_exact_size(vec2(30.0, 14.0), Sense::hover());
    let body = Rect::from_min_size(r.min, vec2(26.0, 14.0));
    ui.painter().rect_stroke(body, 4, Stroke::new(1.2_f32, p.text2), StrokeKind::Inside);
    ui.painter().rect_filled(Rect::from_min_size(egui::pos2(body.right() + 1.0, body.center().y - 2.5), vec2(2.0, 5.0)), 1, p.text2);
    if let Some(pct) = pct {
        let inner = body.shrink(2.5);
        let w = inner.width() * pct as f32 / 100.0;
        let color = if pct <= 20 && !charging {
            Color32::from_rgb(0xFF, 0x45, 0x3A)
        } else if charging {
            Color32::from_rgb(0x30, 0xD1, 0x58)
        } else {
            p.text
        };
        ui.painter().rect_filled(Rect::from_min_size(inner.min, vec2(w, inner.height())), 2, color);
    }
}

pub fn battery(ui: &mut Ui, cx: &mut Cx) {
    let ctx = ui.ctx().clone();
    let ps = &mut cx.st.power;
    ps.info.every(&ctx, 10.0, load);
    if ps.job.take_done() {
        ps.info.invalidate();
    }
    hero(ui, cx, Page::Battery, "Battery levels and the power mode of this computer.");
    let ps = &mut cx.st.power;
    let Some(info) = ps.info.value.clone() else {
        ui.spinner();
        return;
    };

    if let (Some(prof), Some(bus)) = (info.profile.clone(), info.profile_bus) {
        group(ui, |g| {
            let mut v = prof.clone();
            let opts = vec![
                ("power-saver".to_string(), "Low Power".to_string()),
                ("balanced".to_string(), "Automatic".to_string()),
                ("performance".to_string(), "High Power".to_string()),
            ];
            g.row_sub("Energy mode", "Low Power saves energy, High Power lets the CPU run faster", |ui| {
                if popup(ui, "profile", &mut v, &opts) {
                    let val = v.clone();
                    ps.job.start(ui.ctx(), move || {
                        sys::run("busctl", &["set-property", bus.0, bus.1, bus.2, "ActiveProfile", "s", &val])
                    });
                }
            });
            if let Some(ac) = info.on_ac {
                g.row("Power source", |ui| {
                    secondary(ui, if ac { "Power Adapter" } else { "Battery" });
                });
            }
        });
        error_line(ui, ps.job.error());
    }

    let internal: Vec<&Battery> = info.batteries.iter().filter(|b| !b.peripheral).collect();
    let devices: Vec<&Battery> = info.batteries.iter().filter(|b| b.peripheral).collect();
    if !internal.is_empty() {
        header(ui, "Battery");
        group(ui, |g| {
            for b in internal {
                let charging = b.status == "Charging";
                let sub = format!("{}{}", b.status, b.cycles.map(|c| format!(" · {c} cycles")).unwrap_or_default());
                g.row_sub(&b.name, &sub, |ui| {
                    if let Some(c) = b.capacity {
                        secondary(ui, format!("{c}%"));
                    }
                    battery_glyph(ui, b.capacity, charging);
                });
                if let Some(h) = b.health {
                    g.row("Battery health", |ui| {
                        secondary(ui, format!("{} ({h}% of design capacity)", if h >= 80 { "Normal" } else { "Service recommended" }));
                    });
                }
            }
        });
    } else {
        group(ui, |g| {
            g.row_sub("No internal battery", "This computer runs on mains power", |_| {});
        });
    }

    if !devices.is_empty() {
        header(ui, "Devices");
        group(ui, |g| {
            for b in devices {
                let charging = b.status == "Charging";
                g.row_sub(&b.name, if b.status.is_empty() { "Connected" } else { &b.status }, |ui| {
                    if let Some(c) = b.capacity {
                        secondary(ui, format!("{c}%"));
                    }
                    battery_glyph(ui, b.capacity, charging);
                });
            }
        });
    }

    header(ui, "Menu Bar");
    group(ui, |g| {
        let mut b = cx.s.menu_bar.battery_percentage;
        g.row("Show battery percentage", |ui| {
            if toggle(ui, &mut b).changed() {
                cx.s.menu_bar.battery_percentage = b;
            }
        });
    });
}
