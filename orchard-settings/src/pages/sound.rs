//! Sound: output / input device and volume via pactl (PipeWire-Pulse).

use std::time::Instant;

use eframe::egui::{self, vec2, Sense, Ui};

use super::{hero, Cx};
use crate::{
    style::pal,
    sys::{self, Bg},
    widgets::{self, group, header, slider, toggle, SliderOpts},
    Page,
};

#[derive(Default)]
pub struct SoundState {
    info: Bg<SoundInfo>,
    /// locally-held volumes while the user is dragging
    out_vol: Option<(f64, Instant)>,
    in_vol: Option<(f64, Instant)>,
}

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

fn parse(kind: &str) -> Vec<Dev> {
    let Some(json) = sys::output_localized("pactl", &["-f", "json", "list", kind]) else { return Vec::new() };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) else { return Vec::new() };
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
                    let volume = if vols.is_empty() { 0.0 } else { vols.iter().sum::<f64>() / vols.len() as f64 / 65536.0 };
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

fn load() -> SoundInfo {
    SoundInfo {
        sinks: parse("sinks"),
        sources: parse("sources"),
        default_sink: sys::output("pactl", &["get-default-sink"]).unwrap_or_default().trim().to_string(),
        default_source: sys::output("pactl", &["get-default-source"]).unwrap_or_default().trim().to_string(),
    }
}

fn set_async(args: Vec<String>) {
    std::thread::spawn(move || {
        let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let _ = sys::run("pactl", &a);
    });
}

fn device_section(ui: &mut Ui, cx: &mut Cx, output: bool) {
    let s = &mut cx.st.sound;
    let Some(info) = s.info.value.clone() else { return };
    let (devs, default, kind) = if output {
        (&info.sinks, &info.default_sink, "sink")
    } else {
        (&info.sources, &info.default_source, "source")
    };
    let current = devs.iter().find(|d| &d.name == default).or(devs.first()).cloned();

    header(ui, if output { "Output" } else { "Input" });
    group(ui, |g| {
        if devs.is_empty() {
            g.row(if output { "No output devices" } else { "No input devices" }, |_| {});
        }
        for d in devs {
            let selected = current.as_ref().map(|c| c.name == d.name).unwrap_or(false);
            let resp = g.row_with(
                &d.desc,
                widgets::RowOpts {
                    clickable: true,
                    ..Default::default()
                },
                None,
                |ui| {
                    if selected {
                        let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                        widgets::checkmark(ui, r.center(), pal().accent);
                    }
                },
            );
            if resp.clicked() && !selected {
                set_async(vec![format!("set-default-{kind}"), d.name.clone()]);
                if let Some(v) = s.info.value.as_mut() {
                    if output {
                        v.default_sink = d.name.clone();
                    } else {
                        v.default_source = d.name.clone();
                    }
                }
                s.info.invalidate();
            }
        }
    });

    let Some(cur) = current else { return };
    group(ui, |g| {
        let held = if output { &mut s.out_vol } else { &mut s.in_vol };
        if held.map(|(_, t)| t.elapsed().as_secs_f32() > 1.5).unwrap_or(false) {
            *held = None;
        }
        let mut vol = held.map(|(v, _)| v).unwrap_or(cur.volume).min(1.0);
        g.row(if output { "Output volume" } else { "Input volume" }, |ui| {
            let mut mute = !cur.mute;
            if toggle(ui, &mut mute).on_hover_text(if output { "Sound on" } else { "Microphone on" }).changed() {
                set_async(vec![format!("set-{kind}-mute"), cur.name.clone(), if mute { "0".into() } else { "1".into() }]);
                s.info.invalidate();
            }
            let r = slider(
                ui,
                &mut vol,
                0.0..=1.0,
                SliderOpts {
                    width: 220.0,
                    ticks: 0,
                    snap: false,
                    left: "",
                    right: "",
                },
            );
            if r.changed() {
                *held = Some((vol, Instant::now()));
                set_async(vec![format!("set-{kind}-volume"), cur.name.clone(), format!("{}%", (vol * 100.0).round())]);
            } else if r.dragged() {
                *held = Some((vol, Instant::now()));
            }
            widgets::secondary(ui, format!("{:.0}%", vol * 100.0));
        });
        if output {
            g.row_sub(
                "Volume above 100%",
                "Use the volume keys or pactl to boost quiet devices.",
                |ui| {
                    widgets::secondary(ui, format!("now {:.0}%", cur.volume * 100.0));
                },
            );
        }
    });
}

pub fn sound(ui: &mut Ui, cx: &mut Cx) {
    let ctx = ui.ctx().clone();
    cx.st.sound.info.every(&ctx, 2.0, load);
    hero(ui, cx, Page::Sound, "Choose output and input devices and set their volume.");
    if cx.st.sound.info.value.is_none() {
        ui.spinner();
        return;
    }
    device_section(ui, cx, true);
    device_section(ui, cx, false);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        if widgets::button(ui, "Advanced…").clicked() {
            if super::network::which("pavucontrol") {
                sys::spawn_detached("pavucontrol", &[]);
            } else {
                sys::spawn_detached("kcmshell6", &["kcm_pulseaudio"]);
            }
        }
    });
}
