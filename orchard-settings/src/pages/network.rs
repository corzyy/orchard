//! Wi‑Fi, Bluetooth and Network (NetworkManager + BlueZ command line tools).

use eframe::egui::{self, vec2, Color32, Sense, Ui};

use super::{error_line, hero, Cx};
use crate::{
    style::pal,
    sys::{self, nm_fields, output, Bg, Job},
    widgets::{self, button, group, header, primary_button, secondary, signal_bars, toggle, RowOpts},
    Page,
};

#[derive(Default)]
pub struct NetState {
    wifi: Bg<WifiInfo>,
    wifi_job: Job,
    pw_for: Option<String>,
    password: String,
    bt: Bg<BtInfo>,
    bt_job: Job,
    bt_scan: Job,
    net: Bg<NetInfo>,
    net_job: Job,
}

#[derive(Clone, Default)]
struct WifiNet {
    ssid: String,
    signal: u8,
    secure: bool,
    in_use: bool,
}

#[derive(Clone, Default)]
struct WifiInfo {
    has_device: bool,
    device: String,
    enabled: bool,
    networks: Vec<WifiNet>,
    known: Vec<String>,
}

fn load_wifi() -> WifiInfo {
    let mut info = WifiInfo::default();
    if let Some(devs) = output("nmcli", &["-t", "-f", "DEVICE,TYPE", "dev"]) {
        if let Some(d) = devs.lines().map(nm_fields).find(|f| f.get(1).map(|t| t == "wifi").unwrap_or(false)) {
            info.has_device = true;
            info.device = d[0].clone();
        }
    }
    info.enabled = output("nmcli", &["-t", "-f", "WIFI", "g"]).map(|s| s.trim() == "enabled").unwrap_or(false);
    if info.enabled && info.has_device {
        let list = output("nmcli", &["-t", "-f", "IN-USE,SSID,SIGNAL,SECURITY", "dev", "wifi", "list"]).unwrap_or_default();
        for f in list.lines().map(nm_fields) {
            if f.len() < 4 || f[1].is_empty() {
                continue;
            }
            let net = WifiNet {
                in_use: f[0].trim() == "*",
                ssid: f[1].clone(),
                signal: f[2].parse().unwrap_or(0),
                secure: !f[3].trim().is_empty() && f[3].trim() != "--",
            };
            match info.networks.iter_mut().find(|n| n.ssid == net.ssid) {
                Some(n) => {
                    n.in_use |= net.in_use;
                    n.signal = n.signal.max(net.signal);
                }
                None => info.networks.push(net),
            }
        }
        info.networks.sort_by(|a, b| b.in_use.cmp(&a.in_use).then(b.signal.cmp(&a.signal)));
    }
    let cons = output("nmcli", &["-t", "-f", "NAME,TYPE", "con", "show"]).unwrap_or_default();
    info.known = cons
        .lines()
        .map(nm_fields)
        .filter(|f| f.get(1).map(|t| t == "802-11-wireless").unwrap_or(false))
        .map(|f| f[0].clone())
        .collect();
    info
}

pub fn wifi(ui: &mut Ui, cx: &mut Cx) {
    let ctx = ui.ctx().clone();
    let n = &mut cx.st.net;
    n.wifi.every(&ctx, 6.0, load_wifi);
    if n.wifi_job.take_done() {
        n.wifi.invalidate();
    }
    hero(ui, cx, Page::Wifi, "Set up Wi‑Fi to connect to the internet without a cable.");
    let n = &mut cx.st.net;
    let Some(info) = n.wifi.value.clone() else {
        ui.spinner();
        return;
    };
    if !info.has_device {
        group(ui, |g| {
            g.row("No Wi‑Fi hardware found", |_| {});
        });
        return;
    }
    let current = info.networks.iter().find(|w| w.in_use).cloned();
    group(ui, |g| {
        let mut on = info.enabled;
        g.row_with(
            "Wi‑Fi",
            RowOpts {
                icon: Some(Page::Wifi.icon()),
                ..Default::default()
            },
            Some(cx.icons),
            |ui| {
                if toggle(ui, &mut on).changed() {
                    n.wifi_job.run_cmd(ui.ctx(), "nmcli", &["radio", "wifi", if on { "on" } else { "off" }]);
                }
            },
        );
        if let Some(cur) = &current {
            g.row_sub(&cur.ssid, "Connected", |ui| {
                if button(ui, "Disconnect").clicked() {
                    n.wifi_job.run_cmd(ui.ctx(), "nmcli", &["dev", "disconnect", &info.device]);
                }
                signal_bars(ui, cur.signal);
                if cur.secure {
                    lock_glyph(ui, cx.icons);
                }
            });
        }
    });
    error_line(ui, n.wifi_job.error());
    if !info.enabled {
        return;
    }

    let known_nets: Vec<String> = info.known.clone();
    if !known_nets.is_empty() {
        header(ui, "Known Networks");
        group(ui, |g| {
            for k in &known_nets {
                let active = current.as_ref().map(|c| &c.ssid == k).unwrap_or(false);
                g.row(k, |ui| {
                    if button(ui, "Forget").clicked() {
                        n.wifi_job.run_cmd(ui.ctx(), "nmcli", &["con", "delete", "id", k]);
                    }
                    if active {
                        widgets::checkmark(ui, ui.cursor().right_center() - vec2(10.0, 0.0), pal().accent);
                        ui.add_space(14.0);
                    }
                });
            }
        });
    }

    ui.horizontal(|ui| {
        header(ui, "Other Networks");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if n.wifi.busy() {
                ui.spinner();
            } else if button(ui, "Scan").clicked() {
                let dev = info.device.clone();
                n.wifi_job.start(ui.ctx(), move || sys::run("nmcli", &["dev", "wifi", "rescan", "ifname", &dev]));
            }
        });
    });
    group(ui, |g| {
        let others: Vec<&WifiNet> = info.networks.iter().filter(|w| !w.in_use).collect();
        if others.is_empty() {
            g.row("No other networks found", |_| {});
        }
        for w in others {
            let asking = n.pw_for.as_deref() == Some(w.ssid.as_str());
            g.row(&w.ssid, |ui| {
                if !asking && button(ui, "Connect").clicked() {
                    if info.known.contains(&w.ssid) {
                        n.wifi_job.run_cmd(ui.ctx(), "nmcli", &["con", "up", "id", &w.ssid]);
                    } else if w.secure {
                        n.pw_for = Some(w.ssid.clone());
                        n.password.clear();
                    } else {
                        n.wifi_job.run_cmd(ui.ctx(), "nmcli", &["dev", "wifi", "connect", &w.ssid]);
                    }
                }
                signal_bars(ui, w.signal);
                if w.secure {
                    lock_glyph(ui, cx.icons);
                }
            });
            if asking {
                g.custom(|ui| {
                    ui.label(format!("Enter the password for “{}”.", w.ssid));
                    ui.horizontal(|ui| {
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut n.password)
                                .password(true)
                                .desired_width(240.0)
                                .hint_text("Password"),
                        );
                        r.request_focus();
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if primary_button(ui, "Join").clicked() || enter {
                            let pw = std::mem::take(&mut n.password);
                            n.wifi_job.run_cmd(ui.ctx(), "nmcli", &["dev", "wifi", "connect", &w.ssid, "password", &pw]);
                            n.pw_for = None;
                        }
                        if button(ui, "Cancel").clicked() {
                            n.pw_for = None;
                        }
                    });
                });
            }
        }
    });
    advanced_button(ui);
}

fn lock_glyph(ui: &mut Ui, icons: &mut crate::icons::Icons) {
    let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
    icons.paint(ui, "system-lock-screen", r, pal().text2);
}

fn advanced_button(ui: &mut Ui) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        if button(ui, "Advanced…").clicked() {
            if which("nm-connection-editor") {
                sys::spawn_detached("nm-connection-editor", &[]);
            } else {
                sys::spawn_detached("kcmshell6", &["kcm_networkmanagement"]);
            }
        }
    });
}

pub fn which(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
        .unwrap_or(false)
}

// ---------------------------------------------------------------- Bluetooth

#[derive(Clone, Default)]
struct BtDev {
    mac: String,
    name: String,
    paired: bool,
    connected: bool,
    icon: String,
    battery: Option<u8>,
}

#[derive(Clone, Default)]
struct BtInfo {
    available: bool,
    powered: bool,
    devices: Vec<BtDev>,
}

fn bt_list(filter: &str) -> Vec<(String, String)> {
    let mut args = vec!["devices"];
    if !filter.is_empty() {
        args.push(filter);
    }
    output("bluetoothctl", &args)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("Device ")?;
            let (mac, name) = rest.split_once(' ')?;
            Some((mac.to_string(), name.to_string()))
        })
        .collect()
}

fn load_bt() -> BtInfo {
    let show = output("bluetoothctl", &["show"]).unwrap_or_default();
    let mut info = BtInfo {
        available: show.contains("Controller"),
        powered: show.lines().any(|l| l.trim() == "Powered: yes"),
        devices: Vec::new(),
    };
    if !info.powered {
        return info;
    }
    let paired: Vec<String> = bt_list("Paired").into_iter().map(|d| d.0).collect();
    let connected: Vec<String> = bt_list("Connected").into_iter().map(|d| d.0).collect();
    for (mac, name) in bt_list("") {
        let is_paired = paired.contains(&mac);
        let mut d = BtDev {
            paired: is_paired,
            connected: connected.contains(&mac),
            mac: mac.clone(),
            name,
            icon: String::new(),
            battery: None,
        };
        if is_paired {
            let detail = output("bluetoothctl", &["info", &mac]).unwrap_or_default();
            for l in detail.lines().map(str::trim) {
                if let Some(i) = l.strip_prefix("Icon: ") {
                    d.icon = i.to_string();
                }
                if let Some(b) = l.strip_prefix("Battery Percentage: ") {
                    // "0x5a (90)"
                    d.battery = b.split('(').nth(1).and_then(|x| x.trim_end_matches(')').parse().ok());
                }
            }
        }
        // skip anonymous beacons ("AA-BB-CC-…" names)
        let anonymous = d.name.replace('-', ":") == d.mac;
        if d.paired || !anonymous {
            info.devices.push(d);
        }
    }
    info.devices.sort_by(|a, b| b.connected.cmp(&a.connected).then(a.name.cmp(&b.name)));
    info
}

pub fn bluetooth(ui: &mut Ui, cx: &mut Cx) {
    let ctx = ui.ctx().clone();
    let n = &mut cx.st.net;
    n.bt.every(&ctx, 4.0, load_bt);
    if n.bt_job.take_done() || n.bt_scan.take_done() {
        n.bt.invalidate();
    }
    hero(ui, cx, Page::Bluetooth, "Connect to accessories you can use for activities such as streaming music, typing, and gaming.");
    let n = &mut cx.st.net;
    let Some(info) = n.bt.value.clone() else {
        ui.spinner();
        return;
    };
    if !info.available {
        group(ui, |g| {
            g.row("No Bluetooth controller found", |_| {});
        });
        return;
    }
    group(ui, |g| {
        let mut on = info.powered;
        g.row_with(
            "Bluetooth",
            RowOpts {
                icon: Some(Page::Bluetooth.icon()),
                ..Default::default()
            },
            Some(cx.icons),
            |ui| {
                if toggle(ui, &mut on).changed() {
                    n.bt_job.run_cmd(ui.ctx(), "bluetoothctl", &["power", if on { "on" } else { "off" }]);
                }
            },
        );
    });
    error_line(ui, n.bt_job.error());
    if !info.powered {
        return;
    }

    let mine: Vec<&BtDev> = info.devices.iter().filter(|d| d.paired).collect();
    if !mine.is_empty() {
        header(ui, "My Devices");
        group(ui, |g| {
            for d in mine {
                let status = match (d.connected, d.battery) {
                    (true, Some(b)) => format!("Connected · {b}%"),
                    (true, None) => "Connected".to_string(),
                    _ => "Not Connected".to_string(),
                };
                let icon = if d.icon.is_empty() { "bluetooth".to_string() } else { d.icon.clone() };
                let color = if d.connected { pal().accent } else { Color32::from_rgb(0x8E, 0x8E, 0x93) };
                g.row_with(
                    &d.name,
                    RowOpts {
                        icon: Some((icon.as_str(), color)),
                        sub: Some(&status),
                        ..Default::default()
                    },
                    Some(cx.icons),
                    |ui| {
                        if button(ui, "Forget").clicked() {
                            n.bt_job.run_cmd(ui.ctx(), "bluetoothctl", &["remove", &d.mac]);
                        }
                        let label = if d.connected { "Disconnect" } else { "Connect" };
                        if button(ui, label).clicked() {
                            let verb = if d.connected { "disconnect" } else { "connect" };
                            n.bt_job.run_cmd(ui.ctx(), "bluetoothctl", &[verb, &d.mac]);
                        }
                    },
                );
            }
        });
    }

    ui.horizontal(|ui| {
        header(ui, "Nearby Devices");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if n.bt_scan.busy() {
                ui.spinner();
                secondary(ui, "Searching…");
            } else if button(ui, "Search").clicked() {
                n.bt_scan.run_cmd(ui.ctx(), "bluetoothctl", &["--timeout", "10", "scan", "on"]);
            }
        });
    });
    group(ui, |g| {
        let nearby: Vec<&BtDev> = info.devices.iter().filter(|d| !d.paired).collect();
        if nearby.is_empty() {
            g.row("No devices found. Click Search and put your device in pairing mode.", |_| {});
        }
        for d in nearby {
            g.row(&d.name, |ui| {
                if button(ui, "Connect").clicked() {
                    let mac = d.mac.clone();
                    n.bt_job.start(ui.ctx(), move || {
                        sys::run("bluetoothctl", &["pair", &mac])?;
                        let _ = sys::run("bluetoothctl", &["trust", &mac]);
                        sys::run("bluetoothctl", &["connect", &mac])
                    });
                }
            });
        }
    });
    if n.bt_job.busy() {
        ui.horizontal(|ui| {
            ui.spinner();
            secondary(ui, "Working…");
        });
    }
}

// ---------------------------------------------------------------- Network

#[derive(Clone, Default)]
struct Device {
    name: String,
    kind: String,
    state: String,
    connection: String,
    ip: String,
}

#[derive(Clone, Default)]
struct Connection {
    name: String,
    kind: String,
    active: bool,
}

#[derive(Clone, Default)]
struct NetInfo {
    devices: Vec<Device>,
    vpns: Vec<Connection>,
    hostname: String,
}

fn load_net() -> NetInfo {
    let mut info = NetInfo::default();
    let devs = output("nmcli", &["-t", "-f", "DEVICE,TYPE,STATE,CONNECTION", "dev"]).unwrap_or_default();
    for f in devs.lines().map(nm_fields) {
        if f.len() < 4 {
            continue;
        }
        let kind = f[1].as_str();
        if !matches!(kind, "ethernet" | "wifi" | "gsm" | "bt" | "wifi-p2p") || kind == "wifi-p2p" {
            continue;
        }
        let ip = output("nmcli", &["-g", "IP4.ADDRESS", "dev", "show", &f[0]])
            .map(|s| s.lines().next().unwrap_or_default().split('/').next().unwrap_or_default().to_string())
            .unwrap_or_default();
        info.devices.push(Device {
            name: f[0].clone(),
            kind: kind.to_string(),
            state: f[2].clone(),
            connection: f[3].clone(),
            ip,
        });
    }
    let cons = output("nmcli", &["-t", "-f", "NAME,TYPE,ACTIVE", "con", "show"]).unwrap_or_default();
    for f in cons.lines().map(nm_fields) {
        if f.len() >= 3 && matches!(f[1].as_str(), "vpn" | "wireguard") {
            info.vpns.push(Connection {
                name: f[0].clone(),
                kind: f[1].clone(),
                active: f[2] == "yes",
            });
        }
    }
    info.hostname = sys::read_trim("/etc/hostname").unwrap_or_default();
    info
}

pub fn network(ui: &mut Ui, cx: &mut Cx) {
    let ctx = ui.ctx().clone();
    let n = &mut cx.st.net;
    n.net.every(&ctx, 5.0, load_net);
    if n.net_job.take_done() {
        n.net.invalidate();
    }
    let n = &mut cx.st.net;
    let Some(info) = n.net.value.clone() else {
        ui.spinner();
        return;
    };
    group(ui, |g| {
        if info.devices.is_empty() {
            g.row("No network interfaces", |_| {});
        }
        for d in &info.devices {
            let title = match d.kind.as_str() {
                "ethernet" => "Ethernet",
                "wifi" => "Wi‑Fi",
                "gsm" => "Cellular",
                "bt" => "Bluetooth PAN",
                _ => d.kind.as_str(),
            };
            let connected = d.state.starts_with("connected") && !d.state.contains("externally") || d.state == "connected";
            let sub = if connected {
                if d.ip.is_empty() { format!("Connected · {}", d.connection) } else { format!("Connected · {}", d.ip) }
            } else if d.state.contains("connecting") {
                "Connecting…".to_string()
            } else if d.state == "unavailable" {
                "Cable unplugged".to_string()
            } else {
                "Not Connected".to_string()
            };
            let dot = if connected {
                Color32::from_rgb(0x30, 0xD1, 0x58)
            } else if d.state.contains("connecting") {
                Color32::from_rgb(0xFF, 0xD6, 0x0A)
            } else {
                Color32::from_rgb(0xFF, 0x45, 0x3A)
            };
            let icon = if d.kind == "wifi" { "network-wireless" } else { "network-wired" };
            let r = g.row_with(
                &format!("{title}  ({})", d.name),
                RowOpts {
                    icon: Some((icon, Color32::from_rgb(0x0A, 0x84, 0xFF))),
                    sub: Some(&sub),
                    ..Default::default()
                },
                Some(cx.icons),
                |ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 4.5, dot);
                    if d.kind == "ethernet" && d.state != "unavailable" {
                        let (verb, label) = if connected { ("disconnect", "Disconnect") } else { ("connect", "Connect") };
                        if button(ui, label).clicked() {
                            n.net_job.run_cmd(ui.ctx(), "nmcli", &["dev", verb, &d.name]);
                        }
                    }
                },
            );
            let _ = r;
        }
    });
    error_line(ui, n.net_job.error());

    header(ui, "VPN");
    group(ui, |g| {
        if info.vpns.is_empty() {
            g.row("No VPN configurations", |_| {});
        }
        for v in &info.vpns {
            let mut on = v.active;
            let kind = if v.kind == "wireguard" { "WireGuard" } else { "VPN" };
            g.row_sub(&v.name, kind, |ui| {
                if toggle(ui, &mut on).changed() {
                    n.net_job.run_cmd(ui.ctx(), "nmcli", &["con", if on { "up" } else { "down" }, "id", &v.name]);
                }
            });
        }
    });

    header(ui, "Computer Name");
    group(ui, |g| {
        g.row_sub("Local hostname", "Devices on your network see this computer by this name.", |ui| {
            secondary(ui, &info.hostname);
        });
    });
    advanced_button(ui);
}
