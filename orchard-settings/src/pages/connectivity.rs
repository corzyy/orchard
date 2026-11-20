//! Wi‑Fi & Network (nmcli), Bluetooth (bluetoothctl), and Sound (pactl).

use std::time::{Duration, Instant};

use iced::widget::{column, row, text, text_input, Column, Space};
use iced::{Alignment, Length};
use orchard_ui::{controls, form, palette, palette::from_rgb};

use crate::{
    pages::{act, loading, some, tile, El, Page},
    sys::{self, nm_fields, output, Bg, Job, Wake},
    Message,
};

const BLUE: [u8; 3] = [0x0A, 0x84, 0xFF];

fn password_id() -> text_input::Id {
    text_input::Id::new("wifi-password")
}

/// Push button that runs a page action.
fn btn(label: &'static str, msg: Option<Message>) -> El {
    controls::push_button(label, msg).into()
}

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

pub fn wifi(st: &mut ConnState, wake: &Wake) -> El {
    st.wifi.ensure(wake, Duration::from_secs(12), load_wifi);
    if st.wifi_job.take_done() {
        st.wifi.invalidate();
    }
    let Some(info) = st.wifi.value.clone() else {
        return loading("Wi‑Fi");
    };
    if !info.has_wifi {
        return form::group([form::value_row("No Wi‑Fi hardware found", "")]);
    }

    let current = info.networks.iter().find(|w| w.in_use).cloned();
    let status_sub = if !info.enabled {
        "Off"
    } else if current.is_some() {
        "Connected"
    } else {
        "Not Connected"
    };
    let mut main = vec![form::header_row(
        tile("network-wireless", from_rgb(BLUE), 30.0),
        "Wi‑Fi",
        some(status_sub),
        controls::switch(info.enabled, |on| {
            act(move |_, st, wake| st.conn.wifi_job.run_cmd(wake, "nmcli", &["radio", "wifi", if on { "on" } else { "off" }]))
        }),
    )];
    if let Some(cur) = &current {
        let dev = info.device.clone();
        main.push(form::row(
            cur.ssid.clone(),
            some("Connected"),
            row![
                controls::signal_bars(cur.signal),
                btn(
                    "Disconnect",
                    Some(act(move |_, st, wake| st.conn.wifi_job.run_cmd(wake, "nmcli", &["dev", "disconnect", &dev]))),
                ),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        ));
    }
    let mut col = column![form::group(main), form::error(st.wifi_job.error())];
    if !info.enabled {
        return col.into();
    }

    if !info.known.is_empty() {
        col = col.push(form::section("Known Networks")).push(form::group(info.known.iter().map(|k| {
            let k2 = k.clone();
            form::button_row(
                k.clone(),
                None,
                "Forget",
                Some(act(move |_, st, wake| st.conn.wifi_job.run_cmd(wake, "nmcli", &["con", "delete", "id", &k2]))),
            )
        })));
    }

    let scanning = st.wifi.busy();
    let dev = info.device.clone();
    col = col.push(form::section_with(
        "Other Networks",
        btn(
            if scanning { "Scanning…" } else { "Refresh" },
            (!scanning).then(|| {
                act(move |_, st, wake| {
                    let dev = dev.clone();
                    st.conn.wifi_job.start(wake, move || sys::run("nmcli", &["dev", "wifi", "rescan", "ifname", &dev]));
                })
            }),
        ),
    ));

    let others: Vec<&WifiNet> = info.networks.iter().filter(|w| !w.in_use).collect();
    let mut rows: Vec<El> = Vec::new();
    if others.is_empty() {
        rows.push(form::value_row("No other networks found", ""));
    }
    for w in others {
        let asking = st.pw_for.as_deref() == Some(w.ssid.as_str());
        let ssid = w.ssid.clone();
        let known = info.known.contains(&w.ssid);
        let secure = w.secure;
        let connect = act(move |_, st, wake| {
            let c = &mut st.conn;
            if known {
                c.wifi_job.run_cmd(wake, "nmcli", &["con", "up", "id", &ssid]);
            } else if secure {
                c.pw_for = Some(ssid.clone());
                c.password.clear();
                st.focus = Some(password_id());
            } else {
                c.wifi_job.run_cmd(wake, "nmcli", &["dev", "wifi", "connect", &ssid]);
            }
        });
        let mut trailing = row![controls::signal_bars(w.signal)].spacing(12).align_y(Alignment::Center);
        if !asking {
            trailing = trailing.push(btn("Connect", Some(connect)));
        }
        rows.push(form::row(w.ssid.clone(), w.secure.then(|| "Secured".to_string()), trailing));

        if asking {
            let ssid = w.ssid.clone();
            let join = (!st.password.is_empty()).then(|| {
                act(move |_, st, wake| {
                    let c = &mut st.conn;
                    let pw = std::mem::take(&mut c.password);
                    if !pw.is_empty() {
                        c.wifi_job.run_cmd(wake, "nmcli", &["dev", "wifi", "connect", &ssid, "password", &pw]);
                    }
                    c.pw_for = None;
                })
            });
            let field = controls::secure_field("Password", &st.password, |v| {
                act(move |_, st, _| st.conn.password = v.clone())
            })
            .id(password_id())
            .on_submit_maybe(join.clone())
            .width(Length::Fill);
            rows.push(form::custom_row(
                row![
                    field,
                    controls::primary_button("Join", join),
                    btn("Cancel", Some(act(|_, st, _| st.conn.pw_for = None))),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            ));
        }
    }
    col.push(form::group(rows)).into()
}

pub fn network(st: &mut ConnState, wake: &Wake) -> El {
    st.wifi.ensure(wake, Duration::from_secs(12), load_wifi);
    if st.wifi_job.take_done() {
        st.wifi.invalidate();
    }
    let Some(info) = st.wifi.value.clone() else {
        return loading("Network");
    };

    let mut col = Column::new();
    if info.has_wifi {
        let wifi_status = if !info.enabled {
            "Off".to_string()
        } else if let Some(cur) = info.networks.iter().find(|w| w.in_use) {
            cur.ssid.clone()
        } else {
            "Not Connected".to_string()
        };
        col = col.push(form::section("Interfaces")).push(form::group([form::nav_row(
            tile("network-wireless", from_rgb(BLUE), 22.0),
            "Wi‑Fi",
            Some(wifi_status),
            Message::Go(Page::Wifi),
        )]));
    }

    if !info.ethernet.is_empty() {
        col = col.push(form::section("Ethernet")).push(form::group(info.ethernet.iter().map(|e| {
            let connected =
                e.state == "connected" || (e.state.starts_with("connected") && !e.state.contains("externally"));
            let sub = if connected {
                if e.ip.is_empty() { "Connected".to_string() } else { format!("Connected · {}", e.ip) }
            } else if e.state == "unavailable" {
                "Cable unplugged".to_string()
            } else {
                "Disconnected".to_string()
            };
            let label = format!("Ethernet ({})", e.name);
            if e.state != "unavailable" {
                let name = e.name.clone();
                let verb = if connected { "disconnect" } else { "connect" };
                form::button_row(
                    label,
                    Some(sub),
                    if connected { "Disconnect" } else { "Connect" },
                    Some(act(move |_, st, wake| st.conn.wifi_job.run_cmd(wake, "nmcli", &["dev", verb, &name]))),
                )
            } else {
                form::row(label, Some(sub), Space::new(0, 0))
            }
        })));
    }

    if !info.vpns.is_empty() {
        col = col.push(form::section("VPN")).push(form::group(info.vpns.iter().map(|v| {
            let kind = if v.kind == "wireguard" { "WireGuard" } else { "VPN" };
            let name = v.name.clone();
            form::toggle_row(v.name.clone(), some(kind), v.active, move |on| {
                let name = name.clone();
                act(move |_, st, wake| {
                    st.conn.wifi_job.run_cmd(wake, "nmcli", &["con", if on { "up" } else { "down" }, "id", &name])
                })
            })
        })));
    }

    if !info.has_wifi && info.ethernet.is_empty() && info.vpns.is_empty() {
        col = col.push(form::group([form::value_row("No network interfaces found", "")]));
    }
    col.push(form::error(st.wifi_job.error())).into()
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

pub fn bluetooth(st: &mut ConnState, wake: &Wake) -> El {
    st.bt.ensure(wake, Duration::from_secs(8), load_bt);
    if st.bt_job.take_done() || st.bt_scan.take_done() {
        st.bt.invalidate();
    }
    let Some(info) = st.bt.value.clone() else {
        return loading("Bluetooth");
    };
    if !info.available {
        return form::group([form::value_row("No Bluetooth controller found", "")]);
    }

    let sub = if info.powered { "Discoverable while Bluetooth settings is open" } else { "Off" };
    let mut col = column![
        form::group([form::header_row(
            tile("bluetooth", from_rgb(BLUE), 30.0),
            "Bluetooth",
            some(sub),
            controls::switch(info.powered, |on| {
                act(move |_, st, wake| st.conn.bt_job.run_cmd(wake, "bluetoothctl", &["power", if on { "on" } else { "off" }]))
            }),
        )]),
        form::error(st.bt_job.error()),
    ];
    if !info.powered {
        return col.into();
    }

    let mine: Vec<&BtDev> = info.devices.iter().filter(|d| d.paired).collect();
    if !mine.is_empty() {
        col = col.push(form::section("My Devices")).push(form::group(mine.into_iter().map(|d| {
            let status = match (d.connected, d.battery) {
                (true, Some(b)) => format!("Connected · {b}%"),
                (true, None) => "Connected".into(),
                _ => "Not Connected".into(),
            };
            let (mac, mac2) = (d.mac.clone(), d.mac.clone());
            let verb = if d.connected { "disconnect" } else { "connect" };
            form::row(
                d.name.clone(),
                Some(status),
                row![
                    btn(
                        if d.connected { "Disconnect" } else { "Connect" },
                        Some(act(move |_, st, wake| st.conn.bt_job.run_cmd(wake, "bluetoothctl", &[verb, &mac]))),
                    ),
                    btn("Forget", Some(act(move |_, st, wake| st.conn.bt_job.run_cmd(wake, "bluetoothctl", &["remove", &mac2])))),
                ]
                .spacing(6),
            )
        })));
    }

    let scanning = st.bt_scan.busy();
    col = col.push(form::section_with(
        "Nearby Devices",
        btn(
            if scanning { "Scanning…" } else { "Scan" },
            (!scanning).then(|| {
                act(|_, st, wake| st.conn.bt_scan.run_cmd(wake, "bluetoothctl", &["--timeout", "10", "scan", "on"]))
            }),
        ),
    ));

    let nearby: Vec<&BtDev> = info.devices.iter().filter(|d| !d.paired).collect();
    let mut rows: Vec<El> = Vec::new();
    if nearby.is_empty() {
        rows.push(form::row(
            "No nearby devices",
            some("Click Scan and put your device in pairing mode."),
            Space::new(0, 0),
        ));
    }
    for d in nearby {
        let mac = d.mac.clone();
        rows.push(form::button_row(
            d.name.clone(),
            None,
            "Pair",
            Some(act(move |_, st, wake| {
                let mac = mac.clone();
                st.conn.bt_job.start(wake, move || {
                    sys::run("bluetoothctl", &["pair", &mac])?;
                    let _ = sys::run("bluetoothctl", &["trust", &mac]);
                    sys::run("bluetoothctl", &["connect", &mac])
                });
            })),
        ));
    }
    col.push(form::group(rows)).into()
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

fn sound_section(st: &mut ConnState, is_output: bool) -> El {
    let Some(info) = st.sound.value.clone() else {
        return Space::new(0, 0).into();
    };
    let (devs, default, kind) = if is_output {
        (&info.sinks, &info.default_sink, "sink")
    } else {
        (&info.sources, &info.default_source, "source")
    };
    let current = devs.iter().find(|d| &d.name == default).or(devs.first()).cloned();

    let mut dev_rows: Vec<El> = Vec::new();
    if devs.is_empty() {
        dev_rows.push(form::value_row(if is_output { "No output devices" } else { "No input devices" }, ""));
    }
    for d in devs {
        let sel = current.as_ref().map(|c| c.name == d.name).unwrap_or(false);
        let name = d.name.clone();
        dev_rows.push(form::check_row(
            d.desc.clone(),
            None,
            sel,
            act(move |_, st, _| {
                let c = &mut st.conn;
                pactl_async(vec![format!("set-default-{kind}"), name.clone()]);
                if let Some(v) = c.sound.value.as_mut() {
                    if is_output {
                        v.default_sink = name.clone();
                    } else {
                        v.default_source = name.clone();
                    }
                }
                c.sound.invalidate();
            }),
        ));
    }

    if let Some(cur) = current {
        let held = if is_output { &mut st.out_vol } else { &mut st.in_vol };
        if held.map(|(_, t)| t.elapsed().as_secs_f32() > 1.5).unwrap_or(false) {
            *held = None;
        }
        let vol = held.map(|(v, _)| v).unwrap_or(cur.volume).min(1.0);
        let (n1, n2) = (cur.name.clone(), cur.name.clone());
        let p = palette();
        dev_rows.push(form::row(
            if is_output { "Output volume" } else { "Input volume" },
            None,
            row![
                text(format!("{:.0}%", vol * 100.0)).size(12).color(p.text_secondary),
                controls::slider(0.0..=1.0, vol, 0.01, move |v| {
                    let name = n1.clone();
                    act(move |_, st, _| {
                        let held = if is_output { &mut st.conn.out_vol } else { &mut st.conn.in_vol };
                        *held = Some((v, Instant::now()));
                        pactl_async(vec![format!("set-{kind}-volume"), name.clone(), format!("{}%", (v * 100.0).round())]);
                    })
                })
                .width(180),
                controls::switch(!cur.mute, move |enabled| {
                    let name = n2.clone();
                    act(move |_, st, _| {
                        pactl_async(vec![
                            format!("set-{kind}-mute"),
                            name.clone(),
                            if enabled { "0".into() } else { "1".into() },
                        ]);
                        if let Some(v) = st.conn.sound.value.as_mut() {
                            let list = if is_output { &mut v.sinks } else { &mut v.sources };
                            if let Some(d) = list.iter_mut().find(|d| d.name == name) {
                                d.mute = !enabled;
                            }
                        }
                    })
                }),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        ));
    }

    column![
        form::section(if is_output { "Output" } else { "Input" }),
        form::group(dev_rows),
    ]
    .into()
}

pub fn sound(st: &mut ConnState, wake: &Wake) -> El {
    st.sound.ensure(wake, Duration::from_secs(4), load_sound);
    if st.sound.value.is_none() {
        return loading("Sound");
    }
    column![sound_section(st, true), sound_section(st, false)].into()
}
