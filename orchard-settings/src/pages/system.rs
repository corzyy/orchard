//! Lock & Power (idle lock, locker command, energy mode, battery) and
//! System (About, Date & Time, Default Apps, Login Items).

use std::{collections::HashMap, time::Duration};

use orchard_config::{
    desktop::{self, AutostartItem, DesktopEntry},
    Settings,
};

use crate::{
    sys::{self, output, read_trim, Bg, Job, Wake},
    ui::{hash_id, Rect, Ui},
};

#[derive(Default)]
pub struct SystemState {
    power: Bg<PowerInfo>,
    power_job: Job,
    lock_msg: Option<String>,

    about: Bg<AboutInfo>,
    time: Bg<TimeInfo>,
    zones: Bg<Vec<String>>,
    time_job: Job,
    apps: Bg<Vec<DesktopEntry>>,
    defaults: Bg<HashMap<String, String>>,
    apps_job: Job,
    autostart: Bg<Vec<AutostartItem>>,
    auto_err: Option<String>,
}

// ---------------------------------------------------------------- Lock & Power

#[derive(Clone, Default)]
struct Battery {
    name: String,
    capacity: Option<u8>,
    status: String,
    health: Option<u8>,
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

fn load_power() -> PowerInfo {
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
                        capacity: r("capacity").and_then(|v| v.parse().ok()),
                        status: r("status").unwrap_or_default(),
                        health: match (full, design) {
                            (Some(f), Some(d)) if d > 0.0 => Some(((f / d) * 100.0).min(100.0) as u8),
                            _ => None,
                        },
                        peripheral,
                    });
                }
                _ => {}
            }
        }
    }
    for bus in BUSES {
        if let Some(out) = output("busctl", &["get-property", bus.0, bus.1, bus.2, "ActiveProfile"]) {
            info.profile = out.split('"').nth(1).map(String::from);
            info.profile_bus = Some(bus);
            break;
        }
    }
    info
}

pub fn lock_power(ui: &mut Ui, s: &mut Settings, st: &mut SystemState, wake: &Wake) {
    st.power.ensure(wake, Duration::from_secs(15), load_power);
    if st.power_job.take_done() {
        st.power.invalidate();
    }

    ui.section("Lock Screen");
    ui.group("lock-main", |ui| {
        let opts: Vec<(String, String)> = [0u32, 1, 2, 5, 10, 15, 20, 30, 60]
            .iter()
            .map(|&m| {
                let lbl = match m {
                    0 => "Never".into(),
                    1 => "1 minute".into(),
                    60 => "1 hour".into(),
                    n => format!("{n} minutes"),
                };
                (m.to_string(), lbl)
            })
            .collect();
        if let Some(v) = ui.row_popup("lock-idle", "Lock screen when inactive for", None, &s.lock.lock_after_minutes.to_string(), &opts) {
            if let Ok(m) = v.parse() {
                s.lock.lock_after_minutes = m;
            }
        }

        if s.lock.locker.trim() == orchard_config::LEGACY_LOCKER {
            s.lock.locker.clear();
        }
        let r = ui.row_base("Lock command", Some("Leave empty for Orchard's built-in lock screen"), None);
        let ir = Rect::new(r.right() - 14.0 - 200.0, r.cy() - 13.0, 200.0, 26.0);
        ui.text_input_at(hash_id("lock-cmd"), ir, &mut s.lock.locker, "orchard-lock", false);

        if ui.row_button("Lock Screen", Some("Shortcut: ⌃⌘Q"), "Lock Now") {
            let r = orchard_config::ipc_request(&serde_json::json!({"request": "action", "name": "lock"}));
            st.lock_msg = match r {
                Ok(v) if v["event"] == "ok" => None,
                Ok(v) => Some(v["message"].as_str().unwrap_or("Orchard refused").to_string()),
                Err(e) => Some(e.to_string()),
            };
        }
    });
    ui.error_line(st.lock_msg.as_deref());

    if let Some(info) = st.power.value.clone() {
        ui.section("Power & Battery");
        ui.group("pwr-main", |ui| {
            if let (Some(prof), Some(bus)) = (info.profile.clone(), info.profile_bus) {
                let opts = vec![
                    ("power-saver".into(), "Low Power".into()),
                    ("balanced".into(), "Automatic".into()),
                    ("performance".into(), "High Power".into()),
                ];
                if let Some(v) = ui.row_popup("pwr-prof", "Energy mode", None, &prof, &opts) {
                    st.power_job.start(wake, move || {
                        sys::run("busctl", &["set-property", bus.0, bus.1, bus.2, "ActiveProfile", "s", &v])
                    });
                }
            }
            if let Some(ac) = info.on_ac {
                ui.row_value("Power source", if ac { "Power Adapter" } else { "Battery" });
            }
            for b in &info.batteries {
                let val = match (b.capacity, b.health) {
                    (Some(c), Some(h)) if !b.peripheral => format!("{c}% · {} · Health {h}%", b.status),
                    (Some(c), _) => format!("{c}% · {}", b.status),
                    _ => b.status.clone(),
                };
                ui.row_value(&b.name, &val);
            }
            ui.row_toggle("Show battery percentage in Menu Bar", None, &mut s.menu_bar.battery_percentage);
        });
        ui.error_line(st.power_job.error().as_deref());
    }
}

// ---------------------------------------------------------------- About + System

#[derive(Clone, Default)]
pub struct AboutInfo {
    pub hostname: String,
    pub os: String,
    pub kernel: String,
    pub model: String,
    pub chassis: String,
    pub cpu: String,
    pub gpus: Vec<String>,
    pub memory: String,
    pub uptime: String,
}

fn gib(bytes: f64) -> String {
    let g = bytes / 1024.0 / 1024.0 / 1024.0;
    if g >= 10.0 {
        format!("{g:.0} GB")
    } else {
        format!("{g:.1} GB")
    }
}

pub fn load_about() -> AboutInfo {
    let mut a = AboutInfo::default();
    if let Some(j) = output("hostnamectl", &["--json=short"])
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
    {
        let s = |k: &str| j[k].as_str().unwrap_or_default().to_string();
        a.hostname = s("Hostname");
        a.os = s("OperatingSystemPrettyName");
        a.kernel = s("KernelRelease");
        a.chassis = s("Chassis");
        a.model = format!("{} {}", s("HardwareVendor"), s("HardwareModel")).trim().to_string();
    }
    if a.hostname.is_empty() {
        a.hostname = read_trim("/etc/hostname").unwrap_or_default();
    }
    if a.kernel.is_empty() {
        a.kernel = output("uname", &["-r"]).map(|s| s.trim().to_string()).unwrap_or_default();
    }
    if a.os.is_empty() {
        a.os = std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|s| s.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string())))
            .unwrap_or_default();
    }
    if let Ok(cpu) = std::fs::read_to_string("/proc/cpuinfo") {
        let name = cpu
            .lines()
            .find_map(|l| l.strip_prefix("model name").map(|v| v.trim_start_matches([' ', '\t', ':']).to_string()));
        let cores = cpu.lines().filter(|l| l.starts_with("processor")).count();
        a.cpu = format!("{} ({cores} threads)", name.unwrap_or_default());
    }
    if let Ok(mem) = std::fs::read_to_string("/proc/meminfo") {
        if let Some(kb) = mem
            .lines()
            .find_map(|l| l.strip_prefix("MemTotal:"))
            .and_then(|v| v.trim().trim_end_matches(" kB").parse::<f64>().ok())
        {
            a.memory = gib(kb * 1024.0);
        }
    }
    if let Some(l) = output("lspci", &["-mm"]) {
        for line in l.lines() {
            let f: Vec<&str> = line.split('"').collect();
            if f.len() > 5 && (f[1].contains("VGA") || f[1].contains("3D") || f[1].contains("Display")) {
                let vendor = f[3].replace(" Corporation", "").replace(", Inc.", "").replace(" [AMD/ATI]", "");
                a.gpus.push(format!("{vendor} {}", f[5]));
            }
        }
    }
    if let Some(up) = read_trim("/proc/uptime").and_then(|s| s.split(' ').next()?.parse::<f64>().ok()) {
        let m = (up / 60.0) as u64;
        a.uptime = if m >= 60 * 24 {
            format!("{}d {}h", m / 1440, (m % 1440) / 60)
        } else {
            format!("{}h {}m", m / 60, m % 60)
        };
    }
    a
}

#[derive(Clone, Default)]
struct TimeInfo {
    timezone: String,
    ntp: bool,
    can_ntp: bool,
}

fn load_time() -> TimeInfo {
    let s = output("timedatectl", &["show"]).unwrap_or_default();
    let get = |k: &str| {
        s.lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")))
            .unwrap_or_default()
            .to_string()
    };
    TimeInfo {
        timezone: get("Timezone"),
        ntp: get("NTP") == "yes",
        can_ntp: get("CanNTP") == "yes",
    }
}

const DEFAULT_KINDS: &[(&str, &[&str])] = &[
    ("Web browser", &["x-scheme-handler/http", "x-scheme-handler/https", "text/html"]),
    ("File manager", &["inode/directory"]),
    ("Text editor", &["text/plain"]),
];

fn load_defaults() -> HashMap<String, String> {
    DEFAULT_KINDS
        .iter()
        .map(|(_, mimes)| {
            let v = output("xdg-mime", &["query", "default", mimes[0]])
                .unwrap_or_default()
                .trim()
                .to_string();
            (mimes[0].to_string(), v)
        })
        .collect()
}

fn set_autostart_hidden(item: &AutostartItem, hidden: bool) -> std::io::Result<()> {
    let dir = desktop::user_autostart_dir();
    std::fs::create_dir_all(&dir)?;
    let target = dir.join(&item.entry.id);
    if !hidden && item.in_user_dir && item.in_system_dir {
        let only_hides = item.entry.keys.iter().any(|(k, v)| k == "Hidden" && v == "true");
        if only_hides {
            return std::fs::remove_file(target);
        }
    }
    let src = std::fs::read_to_string(&item.entry.path)?;
    let mut out = String::new();
    let mut in_main = false;
    let mut wrote = false;
    for line in src.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            if in_main && !wrote {
                out.push_str(&format!("Hidden={hidden}\n"));
                wrote = true;
            }
            in_main = l == "[Desktop Entry]";
        } else if in_main && (l.starts_with("Hidden=") || l.starts_with("X-GNOME-Autostart-enabled=")) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !wrote {
        out.push_str(&format!("Hidden={hidden}\n"));
    }
    std::fs::write(target, out)
}

pub fn system(ui: &mut Ui, s: &mut Settings, st: &mut SystemState, wake: &Wake) {
    st.about.ensure(wake, Duration::from_secs(120), load_about);
    st.time.ensure(wake, Duration::from_secs(30), load_time);
    st.zones.ensure(wake, Duration::from_secs(3600), || {
        output("timedatectl", &["list-timezones"])
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    });
    st.apps.ensure(wake, Duration::from_secs(600), desktop::applications);
    st.defaults.ensure(wake, Duration::from_secs(60), load_defaults);
    st.autostart.ensure(wake, Duration::from_secs(60), desktop::autostart_items);

    if st.time_job.take_done() {
        st.time.invalidate();
    }
    if st.apps_job.take_done() {
        st.defaults.invalidate();
    }

    // About
    ui.section("About");
    ui.group("sys-about", |ui| {
        if let Some(a) = &st.about.value {
            ui.row_value("Device", &a.hostname);
            if !a.cpu.is_empty() {
                ui.row_value("Processor", &crate::about::clean_cpu(&a.cpu));
            }
            if let Some(g) = a.gpus.first() {
                ui.row_value("Graphics", g);
            }
            if !a.memory.is_empty() {
                ui.row_value("Memory", &a.memory);
            }
            if !a.os.is_empty() {
                ui.row_value("OS", &a.os);
            }
            if !a.kernel.is_empty() {
                ui.row_value("Kernel", &a.kernel);
            }
            ui.row_value("Desktop", &format!("Orchard {}", env!("CARGO_PKG_VERSION")));
        } else {
            ui.row_value("Hardware", "Loading…");
        }
    });

    // Default Apps
    ui.section("Default Apps");
    ui.group("sys-apps", |ui| {
        let apps = st.apps.value.clone().unwrap_or_default();
        let terms: Vec<(String, String)> = apps
            .iter()
            .filter(|e| e.categories().iter().any(|c| c == "TerminalEmulator"))
            .filter_map(|e| {
                let cmd = e.command()?;
                Some((cmd.split_whitespace().next()?.to_string(), e.name()))
            })
            .collect();
        let mut topts = terms;
        if !topts.iter().any(|o| o.0 == s.apps.terminal) {
            topts.insert(0, (s.apps.terminal.clone(), s.apps.terminal.clone()));
        }
        if let Some(v) = ui.row_popup("app-term", "Terminal", Some("Opened with ⌘Return"), &s.apps.terminal, &topts) {
            s.apps.terminal = v;
        }

        if let Some(defs) = st.defaults.value.clone() {
            for (label, mimes) in DEFAULT_KINDS {
                let mut opts: Vec<(String, String)> = apps
                    .iter()
                    .filter(|e| e.mime_types().iter().any(|m| m == mimes[0]))
                    .map(|e| (e.id.clone(), e.name()))
                    .collect();
                let cur = defs.get(mimes[0]).cloned().unwrap_or_default();
                if cur.is_empty() {
                    opts.insert(0, (String::new(), "None".into()));
                } else if !opts.iter().any(|o| o.0 == cur) {
                    opts.insert(0, (cur.clone(), cur.trim_end_matches(".desktop").to_string()));
                }
                if let Some(v) = ui.row_popup(&format!("def-{label}"), label, None, &cur, &opts) {
                    if !v.is_empty() {
                        let mimes = *mimes;
                        st.apps_job.start(wake, move || {
                            let mut args = vec!["default", v.as_str()];
                            args.extend_from_slice(mimes);
                            let r = sys::run("xdg-mime", &args);
                            if mimes[0] == "x-scheme-handler/http" {
                                let _ = sys::run("xdg-settings", &["set", "default-web-browser", &v]);
                            }
                            r
                        });
                    }
                }
            }
        }
    });
    ui.error_line(st.apps_job.error().as_deref());

    // Date & Time
    if let Some(t) = st.time.value.clone() {
        ui.section("Date & Time");
        ui.group("sys-time", |ui| {
            let mut ntp = t.ntp;
            if t.can_ntp && ui.row_toggle("Set time automatically (NTP)", None, &mut ntp) {
                st.time_job
                    .run_cmd(wake, "timedatectl", &["set-ntp", if ntp { "true" } else { "false" }]);
            }
            let zones: Vec<(String, String)> = st
                .zones
                .value
                .clone()
                .unwrap_or_default()
                .into_iter()
                .map(|z| (z.clone(), z))
                .collect();
            if !zones.is_empty() {
                if let Some(tz) = ui.row_popup("sys-tz", "Time zone", None, &t.timezone, &zones) {
                    st.time_job.run_cmd(wake, "timedatectl", &["set-timezone", &tz]);
                }
            } else {
                ui.row_value("Time zone", &t.timezone);
            }
        });
        ui.error_line(st.time_job.error().as_deref());
    }

    // Login Items
    ui.section("Login Items");
    let mut reload_auto = false;
    ui.group("sys-login", |ui| {
        ui.row_toggle(
            "Run login items at startup",
            Some("Runs entries from ~/.config/autostart and /etc/xdg/autostart"),
            &mut s.apps.run_autostart,
        );
        if let Some(items) = st.autostart.value.clone() {
            for item in &items {
                let mut probe = item.entry.clone();
                probe
                    .keys
                    .retain(|(k, _)| k != "Hidden" && k != "X-GNOME-Autostart-enabled");
                if !probe.autostarts_in_orchard() {
                    continue;
                }
                let mut on = !item.entry.hidden();
                let sub = item.entry.command().unwrap_or_default();
                let sub_trim: String = sub.chars().take(60).collect();
                if ui.row_toggle(&item.entry.name(), Some(&sub_trim), &mut on) {
                    if let Err(e) = set_autostart_hidden(item, !on) {
                        st.auto_err = Some(e.to_string());
                    }
                    reload_auto = true;
                }
            }
        }
    });
    if reload_auto {
        st.autostart.invalidate();
    }
    ui.error_line(st.auto_err.as_deref());
}
