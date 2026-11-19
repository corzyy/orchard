//! General and its sub panes: About, Date & Time, Language & Region,
//! Storage; plus Users & Groups.

use eframe::egui::{self, pos2, vec2, Align2, Color32, Rect, Sense, Ui};
use orchard_config::Settings;

use super::{error_line, hero, Cx};
use crate::{
    style::{bold, pal},
    sys::{self, output, read_trim, Bg, Job},
    widgets::{self, button, group, header, popup, popup_str, secondary, toggle},
    Page,
};

#[derive(Default)]
pub struct GeneralState {
    about: Bg<AboutInfo>,
    time: Bg<TimeInfo>,
    zones: Bg<Vec<String>>,
    time_job: Job,
    locale: Bg<LocaleInfo>,
    locales: Bg<Vec<String>>,
    locale_job: Job,
    storage: Bg<Vec<Mount>>,
    host_edit: Option<String>,
    host_job: Job,
}

// ---------------------------------------------------------------- General

pub fn general(ui: &mut Ui, cx: &mut Cx) {
    hero(ui, cx, Page::General, "Manage your overall setup and preferences for this computer, such as name, time and startup apps.");
    group(ui, |g| {
        for page in [Page::About, Page::Storage] {
            if g.nav(cx.icons, page.title(), Some(page.icon()), "").clicked() {
                *cx.nav = Some(page);
            }
        }
    });
    group(ui, |g| {
        for page in [Page::DateTime, Page::Language] {
            if g.nav(cx.icons, page.title(), Some(page.icon()), "").clicked() {
                *cx.nav = Some(page);
            }
        }
    });
    group(ui, |g| {
        for page in [Page::LoginItems, Page::DefaultApps] {
            if g.nav(cx.icons, page.title(), Some(page.icon()), "").clicked() {
                *cx.nav = Some(page);
            }
        }
    });
}

// ---------------------------------------------------------------- About

#[derive(Clone, Default)]
struct AboutInfo {
    hostname: String,
    os: String,
    kernel: String,
    model: String,
    chassis: String,
    cpu: String,
    gpus: Vec<String>,
    memory: String,
    uptime: String,
}

fn gib(bytes: f64) -> String {
    let g = bytes / 1024.0 / 1024.0 / 1024.0;
    if g >= 10.0 {
        format!("{g:.0} GB")
    } else {
        format!("{g:.1} GB")
    }
}

fn load_about() -> AboutInfo {
    let mut a = AboutInfo::default();
    if let Some(j) = output("hostnamectl", &["--json=short"]).and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()) {
        let s = |k: &str| j[k].as_str().unwrap_or_default().to_string();
        a.hostname = s("Hostname");
        a.os = s("OperatingSystemPrettyName");
        a.kernel = s("KernelRelease");
        a.chassis = s("Chassis");
        a.model = format!("{} {}", s("HardwareVendor"), s("HardwareModel")).trim().to_string();
    }
    if a.os.is_empty() {
        a.os = std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|s| s.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string())))
            .unwrap_or_default();
    }
    if let Ok(cpu) = std::fs::read_to_string("/proc/cpuinfo") {
        let name = cpu.lines().find_map(|l| l.strip_prefix("model name").map(|v| v.trim_start_matches([' ', '\t', ':']).to_string()));
        let cores = cpu.lines().filter(|l| l.starts_with("processor")).count();
        a.cpu = format!("{} ({cores} threads)", name.unwrap_or_default());
    }
    if let Ok(mem) = std::fs::read_to_string("/proc/meminfo") {
        if let Some(kb) = mem.lines().find_map(|l| l.strip_prefix("MemTotal:")).and_then(|v| v.trim().trim_end_matches(" kB").parse::<f64>().ok()) {
            a.memory = gib(kb * 1024.0);
        }
    }
    if let Some(l) = output("lspci", &["-mm"]) {
        for line in l.lines() {
            let f: Vec<&str> = line.split('"').collect();
            // slot "class" "vendor" "device" ...
            if f.len() > 5 && (f[1].contains("VGA") || f[1].contains("3D") || f[1].contains("Display")) {
                let vendor = f[3].replace(" Corporation", "").replace(", Inc.", "").replace(" [AMD/ATI]", "");
                a.gpus.push(format!("{vendor} {}", f[5]));
            }
        }
    }
    if let Some(up) = read_trim("/proc/uptime").and_then(|s| s.split(' ').next()?.parse::<f64>().ok()) {
        let m = (up / 60.0) as u64;
        a.uptime = if m >= 60 * 24 {
            format!("{} days, {} h", m / 1440, (m % 1440) / 60)
        } else {
            format!("{} h {} min", m / 60, m % 60)
        };
    }
    a
}

pub fn about(ui: &mut Ui, cx: &mut Cx) {
    let ctx = ui.ctx().clone();
    let gs = &mut cx.st.general;
    gs.about.every(&ctx, 60.0, load_about);
    if gs.host_job.take_done() {
        gs.about.invalidate();
    }
    let Some(a) = gs.about.value.clone() else {
        ui.spinner();
        return;
    };
    ui.add_space(8.0);
    ui.vertical_centered(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(96.0, 96.0), Sense::hover());
        let icon = if a.chassis == "laptop" { "computer-laptop" } else { "computer" };
        if !cx.icons.paint(ui, icon, r, pal().text2) {
            cx.icons.paint(ui, "computer", r, pal().text2);
        }
        ui.add_space(6.0);
        ui.label(egui::RichText::new(&a.hostname).font(bold(22.0)));
        secondary(ui, &a.model);
    });
    ui.add_space(14.0);
    group(ui, |g| {
        let gs = &mut cx.st.general;
        g.row("Name", |ui| match &mut gs.host_edit {
            Some(name) => {
                let mut done = false;
                if widgets::primary_button(ui, "Rename").clicked() {
                    done = true;
                }
                let r = widgets::text_field(ui, name, 180.0, "hostname");
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    done = true;
                }
                if done {
                    let n = name.trim().to_string();
                    if !n.is_empty() {
                        gs.host_job.run_cmd(ui.ctx(), "hostnamectl", &["set-hostname", &n]);
                    }
                    gs.host_edit = None;
                }
            }
            None => {
                if button(ui, "Edit…").clicked() {
                    gs.host_edit = Some(a.hostname.clone());
                }
                secondary(ui, &a.hostname);
            }
        });
        g.row("Processor", |ui| {
            secondary(ui, &a.cpu);
        });
        for (i, gpu) in a.gpus.iter().enumerate() {
            g.row(if i == 0 { "Graphics" } else { "" }, |ui| {
                secondary(ui, gpu);
            });
        }
        g.row("Memory", |ui| {
            secondary(ui, &a.memory);
        });
        g.row("Uptime", |ui| {
            secondary(ui, &a.uptime);
        });
    });
    error_line(ui, cx.st.general.host_job.error());
    header(ui, "Software");
    group(ui, |g| {
        g.row("Operating system", |ui| {
            secondary(ui, &a.os);
        });
        g.row("Kernel", |ui| {
            secondary(ui, &a.kernel);
        });
        g.row("Desktop", |ui| {
            secondary(ui, format!("Orchard {}", env!("CARGO_PKG_VERSION")));
        });
    });
}

// ---------------------------------------------------------------- Date & Time

#[derive(Clone, Default)]
struct TimeInfo {
    timezone: String,
    ntp: bool,
    can_ntp: bool,
    synced: bool,
}

fn load_time() -> TimeInfo {
    let s = output("timedatectl", &["show"]).unwrap_or_default();
    let get = |k: &str| s.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).unwrap_or_default().to_string();
    TimeInfo {
        timezone: get("Timezone"),
        ntp: get("NTP") == "yes",
        can_ntp: get("CanNTP") == "yes",
        synced: get("NTPSynchronized") == "yes",
    }
}

pub fn date_time(ui: &mut Ui, cx: &mut Cx) {
    let ctx = ui.ctx().clone();
    let gs = &mut cx.st.general;
    gs.time.every(&ctx, 10.0, load_time);
    gs.zones.every(&ctx, 3600.0, || {
        output("timedatectl", &["list-timezones"]).unwrap_or_default().lines().map(String::from).collect()
    });
    if gs.time_job.take_done() {
        gs.time.invalidate();
    }
    let Some(t) = gs.time.value.clone() else {
        ui.spinner();
        return;
    };
    ui.add_space(4.0);
    ui.vertical_centered(|ui| {
        let now = sys::output_localized("date", &["+%A, %e. %B %Y   %H:%M"]).unwrap_or_default();
        ui.label(egui::RichText::new(now.trim()).font(bold(18.0)));
    });
    ui.ctx().request_repaint_after(std::time::Duration::from_secs(5));
    ui.add_space(12.0);
    group(ui, |g| {
        let mut auto = t.ntp;
        g.row_sub(
            "Set time and date automatically",
            if t.synced { "Synchronised with a network time server" } else { "Not synchronised" },
            |ui| {
                if t.can_ntp && toggle(ui, &mut auto).changed() {
                    gs.time_job.run_cmd(ui.ctx(), "timedatectl", &["set-ntp", if auto { "true" } else { "false" }]);
                }
            },
        );
        let mut tz = t.timezone.clone();
        let zones = gs.zones.value.clone().unwrap_or_default();
        g.row("Time zone", |ui| {
            if popup_str(ui, "tz", &mut tz, &zones) {
                gs.time_job.run_cmd(ui.ctx(), "timedatectl", &["set-timezone", &tz]);
            }
        });
    });
    error_line(ui, cx.st.general.time_job.error());
    header(ui, "Clock");
    group(ui, |g| {
        clock_format_row(g, cx.s);
    });
    widgets::footnote(ui, "More clock options are in Menu Bar.");
}

pub fn clock_format_row(g: &mut widgets::Group, s: &mut Settings) {
    let mut v = s.menu_bar.clock_24h;
    g.row("Time format", |ui| {
        if popup(
            ui,
            "clock24",
            &mut v,
            &[
                (None, "Automatic (from region)".to_string()),
                (Some(false), "12-hour".to_string()),
                (Some(true), "24-hour".to_string()),
            ],
        ) {
            s.menu_bar.clock_24h = v;
        }
    });
}

// ---------------------------------------------------------------- Language

#[derive(Clone, Default)]
struct LocaleInfo {
    system: String,
    keymap: String,
    x11: String,
}

fn load_locale() -> LocaleInfo {
    let s = output("localectl", &["status"]).unwrap_or_default();
    let get = |k: &str| {
        s.lines()
            .find_map(|l| l.trim().strip_prefix(k).map(|v| v.trim_start_matches(':').trim().to_string()))
            .unwrap_or_default()
    };
    LocaleInfo {
        system: get("System Locale").trim_start_matches("LANG=").to_string(),
        keymap: get("VC Keymap"),
        x11: get("X11 Layout"),
    }
}

pub fn language(ui: &mut Ui, cx: &mut Cx) {
    let ctx = ui.ctx().clone();
    let gs = &mut cx.st.general;
    gs.locale.every(&ctx, 30.0, load_locale);
    gs.locales.every(&ctx, 3600.0, || {
        output("localectl", &["list-locales"]).unwrap_or_default().lines().map(String::from).collect()
    });
    if gs.locale_job.take_done() {
        gs.locale.invalidate();
    }
    let Some(l) = gs.locale.value.clone() else {
        ui.spinner();
        return;
    };
    hero(ui, cx, Page::Language, "Choose the language and formats used across the system.");
    let gs = &mut cx.st.general;
    group(ui, |g| {
        let mut cur = l.system.clone();
        let list = gs.locales.value.clone().unwrap_or_default();
        g.row_sub("System language", "Used by apps that follow the system locale", |ui| {
            if popup_str(ui, "locale", &mut cur, &list) {
                gs.locale_job.run_cmd(ui.ctx(), "localectl", &["set-locale", &format!("LANG={cur}")]);
            }
        });
        g.row("Your session", |ui| {
            secondary(ui, std::env::var("LANG").unwrap_or_default());
        });
        g.row("Console keymap", |ui| {
            secondary(ui, &l.keymap);
        });
        g.row("System keyboard layout", |ui| {
            secondary(ui, &l.x11);
        });
    });
    error_line(ui, gs.locale_job.error());
    widgets::footnote(ui, "Language changes take effect the next time you log in. Your keyboard layout for Orchard is set in Keyboard.");
}

// ---------------------------------------------------------------- Storage

#[derive(Clone)]
struct Mount {
    target: String,
    source: String,
    size: f64,
    used: f64,
}

fn load_storage() -> Vec<Mount> {
    let out = output(
        "df",
        &["-B1", "--output=source,size,used,target", "-x", "tmpfs", "-x", "devtmpfs", "-x", "efivarfs", "-x", "squashfs", "-x", "overlay"],
    )
    .unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    out.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 4 || !f[0].starts_with("/dev/") {
                return None;
            }
            // btrfs subvolumes share a device; show each device once
            if !seen.insert(f[0].to_string()) {
                return None;
            }
            Some(Mount {
                source: f[0].to_string(),
                size: f[1].parse().ok()?,
                used: f[2].parse().ok()?,
                target: f[3..].join(" "),
            })
        })
        .filter(|m| !m.target.starts_with("/boot"))
        .collect()
}

pub fn storage(ui: &mut Ui, cx: &mut Cx) {
    let ctx = ui.ctx().clone();
    cx.st.general.storage.every(&ctx, 20.0, load_storage);
    let Some(mounts) = cx.st.general.storage.value.clone() else {
        ui.spinner();
        return;
    };
    for m in mounts {
        group(ui, |g| {
            g.custom(|ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(if m.target == "/" { "Macintosh HD".replace("Macintosh", "System") } else { m.target.clone() }).font(bold(13.0)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        secondary(ui, format!("{} of {} used", gib(m.used), gib(m.size)));
                    });
                });
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
                let p = pal();
                ui.painter().rect_filled(r, 5, p.toggle_off);
                let frac = (m.used / m.size.max(1.0)) as f32;
                let used = Rect::from_min_size(r.min, vec2(r.width() * frac, r.height()));
                let color = if frac > 0.9 { Color32::from_rgb(0xFF, 0x45, 0x3A) } else { p.accent };
                ui.painter().rect_filled(used, 5, color);
                ui.painter().text(
                    pos2(r.right() - 6.0, r.center().y),
                    Align2::RIGHT_CENTER,
                    format!("{} available", gib(m.size - m.used)),
                    egui::FontId::proportional(10.5),
                    p.text2,
                );
                secondary(ui, format!("{} · {}", m.source, m.target));
            });
        });
    }
}

// ---------------------------------------------------------------- Users

#[derive(Clone, Default)]
pub struct UserInfo {
    pub login: String,
    pub full_name: String,
    pub uid: String,
    pub home: String,
    pub shell: String,
    pub groups: String,
}

impl UserInfo {
    pub fn load() -> UserInfo {
        let login = std::env::var("USER").unwrap_or_default();
        let mut u = UserInfo {
            login: login.clone(),
            ..Default::default()
        };
        if let Some(line) = output("getent", &["passwd", &login]) {
            let f: Vec<&str> = line.trim().split(':').collect();
            if f.len() >= 7 {
                u.uid = f[2].to_string();
                u.full_name = f[4].split(',').next().unwrap_or_default().to_string();
                u.home = f[5].to_string();
                u.shell = f[6].to_string();
            }
        }
        u.groups = output("id", &["-Gn"]).unwrap_or_default().trim().replace(' ', ", ");
        u
    }

    pub fn display_name(&self) -> String {
        if self.full_name.is_empty() {
            self.login.clone()
        } else {
            self.full_name.clone()
        }
    }
}

pub fn users(ui: &mut Ui, cx: &mut Cx) {
    let u = cx.st.user();
    let tex = cx.st.avatar(ui.ctx());
    ui.add_space(8.0);
    ui.vertical_centered(|ui| {
        widgets::avatar(ui, 84.0, &u.display_name(), tex.as_ref());
        ui.add_space(4.0);
        ui.label(egui::RichText::new(u.display_name()).font(bold(20.0)));
        secondary(ui, if u.groups.contains("wheel") { "Admin" } else { "Standard user" });
    });
    ui.add_space(14.0);
    group(ui, |g| {
        g.row("User name", |ui| {
            secondary(ui, &u.login);
        });
        g.row("User ID", |ui| {
            secondary(ui, &u.uid);
        });
        g.row("Home folder", |ui| {
            secondary(ui, &u.home);
        });
        g.row("Login shell", |ui| {
            secondary(ui, &u.shell);
        });
        g.row_sub("Groups", &u.groups, |_| {});
    });
    group(ui, |g| {
        let term = cx.s.apps.terminal.clone();
        g.row_sub("Password", "Opens a terminal running passwd", |ui| {
            if button(ui, "Change Password…").clicked() {
                sys::spawn_detached(&term, &["-e", "passwd"]);
            }
        });
        g.row_sub("Picture", "Put a square image at ~/.face to use it as your picture", |ui| {
            if button(ui, "Open Home Folder").clicked() {
                sys::spawn_detached("xdg-open", &[&u.home]);
            }
        });
    });
}
