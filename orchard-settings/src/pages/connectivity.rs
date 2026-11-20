//! Wi‑Fi & Network (nmcli), Bluetooth (bluetoothctl), and Sound (pactl).

use std::time::{Duration, Instant};

use crate::{
    sys::{self, nm_fields, output, Bg, Job, Wake},
    theme::Weight,
    ui::{hash_id, Rect, Ui},
};

#[derive(Default)]
pub struct ConnState {
    wifi: Bg<WifiInfo>,
    wifi_job: Job,
    pw_for: Option<String>,
    password: String,

    bt: Bg<BtInfo>,
    bt_job: Job,
    bt_scan: Job,

    sound: Bg<SoundInfo>,
    out_vol: Option<(f64, Instant)>,
    in_vol: Option<(f64, Instant)>,
}

// ---------------------------------------------------------------- Wi-Fi & Network

#[derive(Clone, Default)]
struct WifiNet {
    ssid: String,
    signal: u8,
    secure: bool,
    in_use: bool,
}

#[derive(Clone, Default)]
struct EthDev {
    name: String,
    state: String,
    ip: String,
}

#[derive(Clone, Default)]
struct VpnCon {
    name: String,
    kind: String,
    active: bool,
}

#[derive(Clone, Default)]
struct WifiInfo {
    has_wifi: bool,
    device: String,
    enabled: bool,
    networks: Vec<WifiNet>,
    known: Vec<String>,
    ethernet: Vec<EthDev>,
    vpns: Vec<VpnCon>,
}

fn load_wifi() -> WifiInfo {
    let mut info = WifiInfo::default();
    if let Some(devs) = output("nmcli", &["-t", "-f", "DEVICE,TYPE,STATE", "dev"]) {
        for f in devs.lines().map(nm_fields) {
            if f.len() < 3 {
                continue;
            }
            match f[1].as_str() {
                "wifi" if !info.has_wifi => {
                    info.has_wifi = true;
                    info.device = f[0].clone();
                }
                "ethernet" => {
                    let ip = output("nmcli", &["-g", "IP4.ADDRESS", "dev", "show", &f[0]])
                        .map(|s| s.lines().next().unwrap_or_default().split('/').next().unwrap_or_default().to_string())
                        .unwrap_or_default();
                    info.ethernet.push(EthDev {
                        name: f[0].clone(),
                        state: f[2].clone(),
                        ip,
                    });
                }
                _ => {}
            }
        }
    }
    info.enabled = output("nmcli", &["-t", "-f", "WIFI", "g"])
        .map(|s| s.trim() == "enabled")
        .unwrap_or(false);
    if info.enabled && info.has_wifi {
        let list = output("nmcli", &["-t", "-f", "IN-USE,SSID,SIGNAL,SECURITY", "dev", "wifi", "list"])
            .unwrap_or_default();
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
        info.networks
            .sort_by(|a, b| b.in_use.cmp(&a.in_use).then(b.signal.cmp(&a.signal)));
    }
    let cons = output("nmcli", &["-t", "-f", "NAME,TYPE,ACTIVE", "con", "show"]).unwrap_or_default();
    for f in cons.lines().map(nm_fields) {
        if f.len() < 3 {
            continue;
        }
        match f[1].as_str() {
            "802-11-wireless" => info.known.push(f[0].clone()),
            "vpn" | "wireguard" => info.vpns.push(VpnCon {
                name: f[0].clone(),
                kind: f[1].clone(),
                active: f[2] == "yes",
            }),
            _ => {}
        }
    }
    info
}

pub fn wifi(ui: &mut Ui, st: &mut ConnState, wake: &Wake) {
    st.wifi.ensure(wake, Duration::from_secs(12), load_wifi);
    if st.wifi_job.take_done() {
        st.wifi.invalidate();
    }
    let Some(info) = st.wifi.value.clone() else {
        ui.group("wifi-load", |ui| {
            ui.row_value("Network", "Loading…");
        });
        return;
    };

    if info.has_wifi {
        let current = info.networks.iter().find(|w| w.in_use).cloned();
        ui.group("wifi-main", |ui| {
            let mut on = info.enabled;
            if ui.row_toggle("Wi‑Fi", None, &mut on) {
                st.wifi_job
                    .run_cmd(wake, "nmcli", &["radio", "wifi", if on { "on" } else { "off" }]);
            }
            if let Some(cur) = &current {
                let r = ui.row_base(&cur.ssid, Some("Connected"), None);
                let bw = 86.0;
                let br = Rect::new(r.right() - 14.0 - bw, r.cy() - 12.0, bw, 24.0);
                if ui.button_at(br, "Disconnect", false, true) {
                    st.wifi_job.run_cmd(wake, "nmcli", &["dev", "disconnect", &info.device]);
                }
                ui.signal_bars(br.x - 26.0, r.cy(), cur.signal);
            }
        });
        ui.error_line(st.wifi_job.error().as_deref());

        if info.enabled {
            // Available networks
            let sy = ui.y;
            ui.section("Available Networks");
            let scan_r = Rect::new(ui.x + ui.w - 72.0, sy + 2.0, 72.0, 22.0);
            if ui.button_at(scan_r, if st.wifi.busy() { "Scanning…" } else { "Refresh" }, false, !st.wifi.busy()) {
                let dev = info.device.clone();
                st.wifi_job
                    .start(wake, move || sys::run("nmcli", &["dev", "wifi", "rescan", "ifname", &dev]));
            }

            let others: Vec<&WifiNet> = info.networks.iter().filter(|w| !w.in_use).collect();
            ui.group("wifi-others", |ui| {
                if others.is_empty() {
                    ui.row_value("No other networks found", "");
                }
                for w in others {
                    let asking = st.pw_for.as_deref() == Some(w.ssid.as_str());
                    let r = ui.row_base(&w.ssid, if w.secure { Some("Secured") } else { None }, None);
                    let bw = 76.0;
                    let br = Rect::new(r.right() - 14.0 - bw, r.cy() - 12.0, bw, 24.0);
                    if !asking && ui.button_at(br, "Connect", false, true) {
                        if info.known.contains(&w.ssid) {
                            st.wifi_job.run_cmd(wake, "nmcli", &["con", "up", "id", &w.ssid]);
                        } else if w.secure {
                            st.pw_for = Some(w.ssid.clone());
                            st.password.clear();
                            ui.st.focused_input = Some(hash_id("wifi-pw"));
                        } else {
                            st.wifi_job.run_cmd(wake, "nmcli", &["dev", "wifi", "connect", &w.ssid]);
                        }
                    }
                    ui.signal_bars(br.x - 26.0, r.cy(), w.signal);

                    if asking {
                        let pr = ui.custom_row(44.0);
                        let input_r = Rect::new(pr.x + 14.0, pr.y + 8.0, (pr.w - 178.0).max(120.0), 28.0);
                        let enter = ui.text_input_at(hash_id("wifi-pw"), input_r, &mut st.password, "Password", true);
                        let join_r = Rect::new(input_r.right() + 8.0, pr.y + 9.0, 62.0, 26.0);
                        let cancel_r = Rect::new(join_r.right() + 6.0, pr.y + 9.0, 68.0, 26.0);
                        if ui.button_at(join_r, "Join", true, !st.password.is_empty()) || (enter && !st.password.is_empty()) {
                            let pw = std::mem::take(&mut st.password);
                            st.wifi_job.run_cmd(
                                wake,
                                "nmcli",
                                &["dev", "wifi", "connect", &w.ssid, "password", &pw],
                            );
                            st.pw_for = None;
                            ui.st.focused_input = None;
                        }
                        if ui.button_at(cancel_r, "Cancel", false, true) {
                            st.pw_for = None;
                            ui.st.focused_input = None;
                        }
                    }
                }
            });

            if !info.known.is_empty() {
                ui.section("Known Networks");
                ui.group("wifi-known", |ui| {
                    for k in &info.known {
                        if ui.row_button(k, None, "Forget") {
                            st.wifi_job.run_cmd(wake, "nmcli", &["con", "delete", "id", k]);
                        }
                    }
                });
            }
        }
    }

    // Ethernet
    if !info.ethernet.is_empty() {
        ui.section("Ethernet");
        ui.group("net-eth", |ui| {
            for e in &info.ethernet {
                let connected = e.state == "connected" || (e.state.starts_with("connected") && !e.state.contains("externally"));
                let sub = if connected {
                    if e.ip.is_empty() { "Connected".to_string() } else { format!("Connected · {}", e.ip) }
                } else if e.state == "unavailable" {
                    "Cable unplugged".to_string()
                } else {
                    "Disconnected".to_string()
                };
                if e.state != "unavailable" {
                    let btn = if connected { "Disconnect" } else { "Connect" };
                    if ui.row_button(&format!("Ethernet ({})", e.name), Some(&sub), btn) {
                        let verb = if connected { "disconnect" } else { "connect" };
                        st.wifi_job.run_cmd(wake, "nmcli", &["dev", verb, &e.name]);
                    }
                } else {
                    ui.row_base(&format!("Ethernet ({})", e.name), Some(&sub), None);
                }
            }
        });
    }

    // VPN
    if !info.vpns.is_empty() {
        ui.section("VPN");
        ui.group("net-vpn", |ui| {
            for v in &info.vpns {
                let mut on = v.active;
                let kind = if v.kind == "wireguard" { "WireGuard" } else { "VPN" };
                if ui.row_toggle(&v.name, Some(kind), &mut on) {
                    st.wifi_job.run_cmd(
                        wake,
                        "nmcli",
                        &["con", if on { "up" } else { "down" }, "id", &v.name],
                    );
                }
            }
        });
    }
}

// ---------------------------------------------------------------- Bluetooth

#[derive(Clone, Default)]
struct BtDev {
    mac: String,
    name: String,
    paired: bool,
    connected: bool,
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
            battery: None,
        };
        if is_paired {
            let detail = output("bluetoothctl", &["info", &mac]).unwrap_or_default();
            for l in detail.lines().map(str::trim) {
                if let Some(b) = l.strip_prefix("Battery Percentage: ") {
                    d.battery = b.split('(').nth(1).and_then(|x| x.trim_end_matches(')').parse().ok());
                }
            }
        }
        let anon = d.name.replace('-', ":") == d.mac;
        if d.paired || !anon {
            info.devices.push(d);
        }
    }
    info.devices
        .sort_by(|a, b| b.connected.cmp(&a.connected).then(a.name.cmp(&b.name)));
    info
}

pub fn bluetooth(ui: &mut Ui, st: &mut ConnState, wake: &Wake) {
    st.bt.ensure(wake, Duration::from_secs(8), load_bt);
    if st.bt_job.take_done() || st.bt_scan.take_done() {
        st.bt.invalidate();
    }
    let Some(info) = st.bt.value.clone() else {
        ui.group("bt-load", |ui| {
            ui.row_value("Bluetooth", "Loading…");
        });
        return;
    };
    if !info.available {
        ui.group("bt-none", |ui| {
            ui.row_value("No Bluetooth controller found", "");
        });
        return;
    }

    ui.group("bt-main", |ui| {
        let mut on = info.powered;
        if ui.row_toggle("Bluetooth", None, &mut on) {
            st.bt_job
                .run_cmd(wake, "bluetoothctl", &["power", if on { "on" } else { "off" }]);
        }
    });
    ui.error_line(st.bt_job.error().as_deref());
    if !info.powered {
        return;
    }

    let mine: Vec<&BtDev> = info.devices.iter().filter(|d| d.paired).collect();
    if !mine.is_empty() {
        ui.section("My Devices");
        ui.group("bt-mine", |ui| {
            for d in mine {
                let status = match (d.connected, d.battery) {
                    (true, Some(b)) => format!("Connected · {b}%"),
                    (true, None) => "Connected".into(),
                    _ => "Not Connected".into(),
                };
                let r = ui.row_base(&d.name, Some(&status), None);
                let f_r = Rect::new(r.right() - 14.0 - 64.0, r.cy() - 12.0, 64.0, 24.0);
                if ui.button_at(f_r, "Forget", false, true) {
                    st.bt_job.run_cmd(wake, "bluetoothctl", &["remove", &d.mac]);
                }
                let lbl = if d.connected { "Disconnect" } else { "Connect" };
                let cw = if d.connected { 86.0 } else { 72.0 };
                let c_r = Rect::new(f_r.x - 6.0 - cw, r.cy() - 12.0, cw, 24.0);
                if ui.button_at(c_r, lbl, false, true) {
                    let verb = if d.connected { "disconnect" } else { "connect" };
                    st.bt_job.run_cmd(wake, "bluetoothctl", &[verb, &d.mac]);
                }
            }
        });
    }

    let sy = ui.y;
    ui.section("Nearby Devices");
    let scan_r = Rect::new(ui.x + ui.w - 82.0, sy + 2.0, 82.0, 22.0);
    if ui.button_at(
        scan_r,
        if st.bt_scan.busy() { "Scanning…" } else { "Scan" },
        false,
        !st.bt_scan.busy(),
    ) {
        st.bt_scan
            .run_cmd(wake, "bluetoothctl", &["--timeout", "10", "scan", "on"]);
    }

    let nearby: Vec<&BtDev> = info.devices.iter().filter(|d| !d.paired).collect();
    ui.group("bt-nearby", |ui| {
        if nearby.is_empty() {
            ui.row_base("No nearby devices", Some("Click Scan and put your device in pairing mode."), None);
        }
        for d in nearby {
            if ui.row_button(&d.name, None, "Pair") {
                let mac = d.mac.clone();
                st.bt_job.start(wake, move || {
                    sys::run("bluetoothctl", &["pair", &mac])?;
                    let _ = sys::run("bluetoothctl", &["trust", &mac]);
                    sys::run("bluetoothctl", &["connect", &mac])
                });
            }
        }
    });
}

// ---------------------------------------------------------------- Sound

#[derive(Clone, Default)]
struct Dev {
    name: String,
    desc: String,
    volume: f64,
    mute: bool,
}

#[derive(Clone, Default)]
struct SoundInfo {
    sinks: Vec<Dev>,
    sources: Vec<Dev>,
    default_sink: String,
    default_source: String,
}

fn parse_pactl(kind: &str) -> Vec<Dev> {
    let Some(json) = sys::output_localized("pactl", &["-f", "json", "list", kind]) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) else {
        return Vec::new();
    };
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|d| {
                    let name = d["name"].as_str()?.to_string();
                    if name.ends_with(".monitor") || name == "auto_null" {
                        return None;
                    }
                    let vols: Vec<f64> = d["volume"]
                        .as_object()
                        .map(|o| o.values().filter_map(|c| c["value"].as_f64()).collect())
                        .unwrap_or_default();
                    let volume = if vols.is_empty() {
                        0.0
                    } else {
                        vols.iter().sum::<f64>() / vols.len() as f64 / 65536.0
                    };
                    Some(Dev {
                        desc: d["description"].as_str().unwrap_or(&name).to_string(),
                        name,
                        volume,
                        mute: d["mute"].as_bool().unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn load_sound() -> SoundInfo {
    SoundInfo {
        sinks: parse_pactl("sinks"),
        sources: parse_pactl("sources"),
        default_sink: output("pactl", &["get-default-sink"]).unwrap_or_default().trim().to_string(),
        default_source: output("pactl", &["get-default-source"]).unwrap_or_default().trim().to_string(),
    }
}

fn pactl_async(args: Vec<String>) {
    std::thread::spawn(move || {
        let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let _ = sys::run("pactl", &a);
    });
}

fn sound_section(ui: &mut Ui, st: &mut ConnState, is_output: bool) {
    let Some(info) = st.sound.value.clone() else { return };
    let (devs, default, kind) = if is_output {
        (&info.sinks, &info.default_sink, "sink")
    } else {
        (&info.sources, &info.default_source, "source")
    };
    let current = devs.iter().find(|d| &d.name == default).or(devs.first()).cloned();

    ui.section(if is_output { "Output" } else { "Input" });
    ui.group(if is_output { "snd-out-devs" } else { "snd-in-devs" }, |ui| {
        if devs.is_empty() {
            ui.row_value(if is_output { "No output devices" } else { "No input devices" }, "");
        }
        for d in devs {
            let sel = current.as_ref().map(|c| c.name == d.name).unwrap_or(false);
            if ui.row_clickable(&d.desc, None, sel) && !sel {
                pactl_async(vec![format!("set-default-{kind}"), d.name.clone()]);
                if let Some(v) = st.sound.value.as_mut() {
                    if is_output {
                        v.default_sink = d.name.clone();
                    } else {
                        v.default_source = d.name.clone();
                    }
                }
                st.sound.invalidate();
            }
        }
    });

    let Some(cur) = current else { return };
    let held = if is_output { &mut st.out_vol } else { &mut st.in_vol };
    if held.map(|(_, t)| t.elapsed().as_secs_f32() > 1.5).unwrap_or(false) {
        *held = None;
    }
    let mut vol = held.map(|(v, _)| v).unwrap_or(cur.volume).min(1.0);
    let mut enabled = !cur.mute;

    ui.group(if is_output { "snd-out-vol" } else { "snd-in-vol" }, |ui| {
        let r = ui.row_base(if is_output { "Output volume" } else { "Input volume" }, None, None);
        let tr = Rect::new(r.right() - 14.0 - 34.0, r.cy() - 9.5, 34.0, 19.0);
        if ui.toggle_at(tr, &mut enabled) {
            pactl_async(vec![
                format!("set-{kind}-mute"),
                cur.name.clone(),
                if enabled { "0".into() } else { "1".into() },
            ]);
            if let Some(v) = st.sound.value.as_mut() {
                let list = if is_output { &mut v.sinks } else { &mut v.sources };
                if let Some(d) = list.iter_mut().find(|d| d.name == cur.name) {
                    d.mute = !enabled;
                }
            }
        }
        let sw = 180.0f32.min(ui.w * 0.36);
        let sr = Rect::new(tr.x - 14.0 - sw, r.cy() - 10.0, sw, 20.0);
        let id = hash_id(if is_output { "vol-out" } else { "vol-in" });
        if ui.slider_at(id, sr, &mut vol, 0.0, 1.0, "", "") {
            *held = Some((vol, Instant::now()));
            pactl_async(vec![
                format!("set-{kind}-volume"),
                cur.name.clone(),
                format!("{}%", (vol * 100.0).round()),
            ]);
        }
        ui.text_right(
            &format!("{:.0}%", vol * 100.0),
            sr.x - 10.0,
            r.cy(),
            12.0,
            Weight::Regular,
            ui.pal.text2,
        );
    });
}

pub fn sound(ui: &mut Ui, st: &mut ConnState, wake: &Wake) {
    st.sound.ensure(wake, Duration::from_secs(4), load_sound);
    if st.sound.value.is_none() {
        ui.group("snd-load", |ui| {
            ui.row_value("Sound", "Loading…");
        });
        return;
    }
    sound_section(ui, st, true);
    sound_section(ui, st, false);
}
