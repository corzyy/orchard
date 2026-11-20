//! Lock & Power (idle lock, locker command, energy mode, battery) and
//! System (About, Date & Time, Default Apps, Login Items).

use std::{collections::HashMap, time::Duration};

use orchard_config::{
    desktop::{self, AutostartItem, DesktopEntry},
    PerformancePreset, Settings, VrrMode,
};

use iced::widget::{column, Column};
use iced::Length;
use orchard_ui::{choices, controls, form};

use crate::{
    pages::{act, edit, loading, some, tile, El, Page},
    sys::{self, output, read_trim, Bg, Job, Wake},
    Message,
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

pub fn lock_screen(s: &Settings, st: &SystemState) -> El {
    let opts = choices([0u32, 1, 2, 5, 10, 15, 20, 30, 60].iter().map(|&m| {
        let lbl = match m {
            0 => "Never".to_string(),
            1 => "1 minute".into(),
            60 => "1 hour".into(),
            n => format!("{n} minutes"),
        };
        (m.to_string(), lbl)
    }));
    column![
        form::section("Lock Screen"),
        form::group([
            form::popup_row(
                "Lock screen when inactive for",
                None,
                opts,
                &s.lock.lock_after_minutes.to_string(),
                |v| {
                    edit(move |s| {
                        if let Ok(m) = v.parse() {
                            s.lock.lock_after_minutes = m;
                        }
                    })
                },
            ),
            form::row(
                "Lock command",
                some("Leave empty for Orchard's built-in lock screen"),
                controls::text_field("orchard-lock", &s.lock.locker, |v| edit(move |s| s.lock.locker = v.clone()))
                    .width(200),
            ),
            form::button_row(
                "Lock Screen",
                some("Shortcut: ⌃⌘Q"),
                "Lock Now",
                Some(act(|_, st, _| {
                    let r = orchard_config::ipc_request(&serde_json::json!({"request": "action", "name": "lock"}));
                    st.system.lock_msg = match r {
                        Ok(v) if v["event"] == "ok" => None,
                        Ok(v) => Some(v["message"].as_str().unwrap_or("Orchard refused").to_string()),
                        Err(e) => Some(e.to_string()),
                    };
                })),
            ),
        ]),
        form::error(st.lock_msg.clone()),
    ]
    .into()
}

pub fn battery(s: &Settings, st: &mut SystemState, wake: &Wake) -> El {
    st.power.ensure(wake, Duration::from_secs(15), load_power);
    if st.power_job.take_done() {
        st.power.invalidate();
    }

    let mut col = Column::new();
    if let Some(info) = st.power.value.clone() {
        let mut rows: Vec<El> = Vec::new();
        if let (Some(prof), Some(bus)) = (info.profile.clone(), info.profile_bus) {
            rows.push(form::popup_row(
                "Energy mode",
                None,
                choices([("power-saver", "Low Power"), ("balanced", "Automatic"), ("performance", "High Power")]),
                &prof,
                move |v| {
                    act(move |_, st, wake| {
                        let v = v.clone();
                        st.system.power_job.start(wake, move || {
                            sys::run("busctl", &["set-property", bus.0, bus.1, bus.2, "ActiveProfile", "s", &v])
                        });
                    })
                },
            ));
        }
        if let Some(ac) = info.on_ac {
            rows.push(form::value_row("Power source", if ac { "Power Adapter" } else { "Battery" }));
        }
        for b in &info.batteries {
            let val = match (b.capacity, b.health) {
                (Some(c), Some(h)) if !b.peripheral => format!("{c}% · {} · Health {h}%", b.status),
                (Some(c), _) => format!("{c}% · {}", b.status),
                _ => b.status.clone(),
            };
            rows.push(form::value_row(b.name.clone(), val));
        }
        col = col
            .push(form::section("Power"))
            .push(form::group(rows))
            .push(form::error(st.power_job.error()));
    }

    col = col.push(form::section("Menu Bar")).push(form::group([form::toggle_row(
        "Show battery percentage",
        some("Display the charge level next to the battery icon"),
        s.menu_bar.battery_percentage,
        |v| edit(move |s| s.menu_bar.battery_percentage = v),
    )]));

    let perf = &s.performance;
    let cur_preset = match perf.preset {
        PerformancePreset::Balanced => "balanced",
        PerformancePreset::Performance => "performance",
        PerformancePreset::PowerSaver => "power_saver",
    };
    let cur_vrr = match perf.vrr {
        VrrMode::Off => "off",
        VrrMode::Fullscreen => "fullscreen",
        VrrMode::On => "on",
    };
    col.push(form::section("Performance"))
        .push(form::group([
            form::popup_row(
                "Performance preset",
                some("Configures frame pacing, VRR and blur defaults"),
                choices([("balanced", "Balanced"), ("performance", "Performance"), ("power_saver", "Power Saver")]),
                cur_preset,
                |v| {
                    edit(move |s| {
                        s.performance.apply_preset(match v.as_str() {
                            "performance" => PerformancePreset::Performance,
                            "power_saver" => PerformancePreset::PowerSaver,
                            _ => PerformancePreset::Balanced,
                        })
                    })
                },
            ),
            form::popup_row(
                "Variable Refresh Rate (VRR)",
                some("Adaptive Sync / FreeSync / G-Sync on supported displays"),
                choices([("off", "Off"), ("fullscreen", "Fullscreen only"), ("on", "Always")]),
                cur_vrr,
                |v| {
                    edit(move |s| {
                        s.performance.vrr = match v.as_str() {
                            "fullscreen" => VrrMode::Fullscreen,
                            "on" => VrrMode::On,
                            _ => VrrMode::Off,
                        }
                    })
                },
            ),
            form::toggle_row(
                "Direct scanout",
                some("Bypass compositor for fullscreen windows to reduce latency"),
                perf.direct_scanout,
                |v| edit(move |s| s.performance.direct_scanout = v),
            ),
            form::toggle_row(
                "Low-latency frame pacing",
                some("Minimise compositor repaint delay for games and fast input"),
                perf.low_latency,
                |v| edit(move |s| s.performance.low_latency = v),
            ),
            form::toggle_row(
                "Live frosted-glass blur",
                some("Turn off to use the static blurred wallpaper for lower GPU load"),
                perf.live_blur,
                |v| edit(move |s| s.performance.live_blur = v),
            ),
            form::toggle_row(
                "Auto power-saving on battery",
                some("Disable live blur and reduce shell polling while unplugged"),
                perf.battery_saver,
                |v| edit(move |s| s.performance.battery_saver = v),
            ),
        ]))
        .push(form::section("Game Mode"))
        .push(form::group([
            form::toggle_row(
                "Game Mode",
                some("Turns on by itself while a game runs full screen. Hot corners, live blur and the idle lock pause"),
                s.game_mode.enabled,
                |v| edit(move |s| s.game_mode.enabled = v),
            ),
            form::toggle_row(
                "Silence notifications",
                some("Hold back banners while playing; they wait in Notification Center"),
                s.game_mode.silence_notifications,
                |v| edit(move |s| s.game_mode.silence_notifications = v),
            ),
            form::toggle_row(
                "Variable Refresh Rate for games",
                some("Adaptive Sync on the game's display, even when VRR is off above"),
                s.game_mode.vrr,
                |v| edit(move |s| s.game_mode.vrr = v),
            ),
        ]))
        .into()
}

// ---------------------------------------------------------------- About + System

#[derive(Debug, Clone, Default)]
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

/// `PRETTY_NAME` from os-release (e.g. "Fedora Linux 45 (Workstation Edition)").
pub fn os_name() -> String {
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|s| s.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string())))
        .unwrap_or_default()
}

fn nav(pg: Page) -> El {
    let (icon, color) = pg.icon();
    form::nav_row(tile(icon, color, 22.0), pg.title(), None, Message::Go(pg))
}

pub fn general() -> El {
    let (icon, color) = Page::General.icon();
    column![
        form::hero(tile(icon, color, 54.0), Page::General.title(), Page::General.description()),
        form::section("System"),
        form::group([nav(Page::About), nav(Page::SoftwareUpdate)]),
        form::section("Preferences"),
        form::group([nav(Page::DateTime), nav(Page::LoginItems), nav(Page::DefaultApps)]),
    ]
    .into()
}

pub fn about(st: &mut SystemState, wake: &Wake) -> El {
    st.about.ensure(wake, Duration::from_secs(120), load_about);
    let Some(a) = &st.about.value else {
        return loading("Hardware");
    };
    let mut hardware = vec![form::value_row("Name", a.hostname.clone())];
    let mut software = Vec::new();
    let mut add = |label: &'static str, v: &str| {
        if !v.is_empty() {
            software.push(form::value_row(label, v.to_string()));
        }
    };
    if !a.model.is_empty() {
        hardware.push(form::value_row("Model", a.model.clone()));
    }
    hardware.push(form::value_row("Processor", crate::about::clean_cpu(&a.cpu)));
    hardware.push(form::value_row("Graphics", a.gpus.first().cloned().unwrap_or_else(|| "—".into())));
    hardware.push(form::value_row("Memory", a.memory.clone()));
    add("OS", &a.os);
    add("Kernel", &a.kernel);
    software.push(form::value_row("Desktop", format!("Orchard {}", env!("CARGO_PKG_VERSION"))));
    column![
        form::section("Hardware"),
        form::group(hardware),
        form::section("Software"),
        form::group(software),
    ]
    .into()
}

pub fn date_time(st: &mut SystemState, wake: &Wake) -> El {
    st.time.ensure(wake, Duration::from_secs(30), load_time);
    st.zones.ensure(wake, Duration::from_secs(3600), || {
        output("timedatectl", &["list-timezones"])
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    });
    if st.time_job.take_done() {
        st.time.invalidate();
    }
    let Some(t) = st.time.value.clone() else {
        return loading("Date & Time");
    };
    let mut rows: Vec<El> = Vec::new();
    if t.can_ntp {
        rows.push(form::toggle_row("Set time and date automatically", some("Uses network time (NTP)"), t.ntp, |on| {
            act(move |_, st, wake| {
                st.system.time_job.run_cmd(wake, "timedatectl", &["set-ntp", if on { "true" } else { "false" }])
            })
        }));
    }
    let zones = st.zones.value.clone().unwrap_or_default();
    if !zones.is_empty() {
        rows.push(form::popup_row("Time zone", None, choices(zones.into_iter().map(|z| (z.clone(), z))), &t.timezone, |tz| {
            act(move |_, st, wake| st.system.time_job.run_cmd(wake, "timedatectl", &["set-timezone", &tz]))
        }));
    } else {
        rows.push(form::value_row("Time zone", t.timezone.clone()));
    }
    column![form::section("Date & Time"), form::group(rows), form::error(st.time_job.error())].into()
}

pub fn default_apps(s: &Settings, st: &mut SystemState, wake: &Wake) -> El {
    st.apps.ensure(wake, Duration::from_secs(600), desktop::applications);
    st.defaults.ensure(wake, Duration::from_secs(60), load_defaults);
    if st.apps_job.take_done() {
        st.defaults.invalidate();
    }

    let apps = st.apps.value.clone().unwrap_or_default();
    let mut topts: Vec<(String, String)> = apps
        .iter()
        .filter(|e| e.categories().iter().any(|c| c == "TerminalEmulator"))
        .filter_map(|e| {
            let cmd = e.command()?;
            Some((cmd.split_whitespace().next()?.to_string(), e.name()))
        })
        .collect();
    let cur_term = orchard_config::resolve_terminal(&s.apps.terminal);
    if !topts.iter().any(|o| o.0 == cur_term) {
        topts.insert(0, (cur_term.clone(), cur_term.clone()));
    }
    let term_row = form::popup_row("Terminal", some("Opened with ⌘Return"), choices(topts), &cur_term, |v| {
        edit(move |s| s.apps.terminal = v.clone())
    });
    let mut rows: Vec<El> = Vec::new();

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
            let mimes: &'static [&'static str] = mimes;
            rows.push(form::popup_row(*label, None, choices(opts), &cur, move |v| {
                act(move |_, st, wake| {
                    if v.is_empty() {
                        return;
                    }
                    let v = v.clone();
                    st.system.apps_job.start(wake, move || {
                        let mut args = vec!["default", v.as_str()];
                        args.extend_from_slice(mimes);
                        let r = sys::run("xdg-mime", &args);
                        if mimes[0] == "x-scheme-handler/http" {
                            let _ = sys::run("xdg-settings", &["set", "default-web-browser", &v]);
                        }
                        r
                    });
                })
            }));
        }
    }
    let mut col = column![form::section("Terminal"), form::group([term_row])];
    if !rows.is_empty() {
        col = col.push(form::section("Default Applications")).push(form::group(rows));
    }
    col.push(form::error(st.apps_job.error())).into()
}

pub fn login_items(s: &Settings, st: &mut SystemState, wake: &Wake) -> El {
    st.autostart.ensure(wake, Duration::from_secs(60), desktop::autostart_items);

    let mut rows = vec![form::toggle_row(
        "Run login items at startup",
        some("Runs entries from ~/.config/autostart and /etc/xdg/autostart"),
        s.apps.run_autostart,
        |v| edit(move |s| s.apps.run_autostart = v),
    )];
    if let Some(items) = st.autostart.value.clone() {
        for item in items {
            let mut probe = item.entry.clone();
            probe.keys.retain(|(k, _)| k != "Hidden" && k != "X-GNOME-Autostart-enabled");
            if !probe.autostarts_in_orchard() {
                continue;
            }
            let sub: String = item.entry.command().unwrap_or_default().chars().take(60).collect();
            let name = item.entry.name();
            let on = !item.entry.hidden();
            rows.push(form::toggle_row(name, Some(sub), on, move |on| {
                let item = item.clone();
                act(move |_, st, _| {
                    if let Err(e) = set_autostart_hidden(&item, !on) {
                        st.system.auto_err = Some(e.to_string());
                    }
                    st.system.autostart.invalidate();
                })
            }));
        }
    }
    column![form::section("Open at Login"), form::group(rows), form::error(st.auto_err.clone())]
        .width(Length::Fill)
        .into()
}
