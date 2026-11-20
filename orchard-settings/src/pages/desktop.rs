//! Desktop & Dock (including inline Hot Corners), Menu Bar, and Displays.

use std::time::Duration;

use orchard_config::{
    CornerAction, DockAutohide, DockPosition, MinimizeEffect, Settings, TitlebarDoubleClick,
};

use crate::{
    pages::appearance::draw_mini_desktop,
    sys::{Bg, Wake},
    theme::{rgb, Weight},
    ui::{Rect, Ui},
};

#[derive(Default)]
pub struct DesktopState {
    outputs: Bg<Vec<Output>>,
}

fn corner_key(a: CornerAction) -> &'static str {
    match a {
        CornerAction::None => "none",
        CornerAction::MissionControl => "mc",
        CornerAction::AppExpose => "expose",
        CornerAction::Launcher => "launcher",
        CornerAction::LockScreen => "lock",
        CornerAction::Launchpad => "launchpad",
    }
}

fn corner_from_key(k: &str) -> CornerAction {
    match k {
        "mc" => CornerAction::MissionControl,
        "expose" => CornerAction::AppExpose,
        "launcher" => CornerAction::Launcher,
        "lock" => CornerAction::LockScreen,
        "launchpad" => CornerAction::Launchpad,
        _ => CornerAction::None,
    }
}

pub fn desktop_dock(ui: &mut Ui, s: &mut Settings) {
    ui.section("Dock");
    ui.group("dock-main", |ui| {
        let mut size = s.dock.icon_size as f64;
        if ui.row_slider("dock-size", "Size", None, &mut size, 32.0, 96.0, "Small", "Large") {
            s.dock.icon_size = size.round() as f32;
            s.dock.magnified_size = s.dock.magnified_size.max(s.dock.icon_size);
        }
        ui.row_toggle("Magnification", None, &mut s.dock.magnification);
        if s.dock.magnification {
            let mut mag = s.dock.magnified_size as f64;
            let lo = s.dock.icon_size as f64;
            if ui.row_slider("dock-mag", "Magnified size", None, &mut mag, lo, 128.0, "Small", "Large") {
                s.dock.magnified_size = mag.round() as f32;
            }
        }

        let pos_opts = vec![
            ("left".into(), "Left".into()),
            ("bottom".into(), "Bottom".into()),
            ("right".into(), "Right".into()),
        ];
        let cur_pos = match s.dock.position {
            DockPosition::Left => "left",
            DockPosition::Bottom => "bottom",
            DockPosition::Right => "right",
        };
        if let Some(v) = ui.row_popup("dock-pos", "Position on screen", None, cur_pos, &pos_opts) {
            s.dock.position = match v.as_str() {
                "left" => DockPosition::Left,
                "right" => DockPosition::Right,
                _ => DockPosition::Bottom,
            };
        }

        let eff_opts = vec![
            ("genie".into(), "Genie Effect".into()),
            ("scale".into(), "Scale Effect".into()),
        ];
        let cur_eff = match s.dock.minimize_effect {
            MinimizeEffect::Genie => "genie",
            MinimizeEffect::Scale => "scale",
        };
        if let Some(v) = ui.row_popup("min-eff", "Minimise windows using", None, cur_eff, &eff_opts) {
            s.dock.minimize_effect = if v == "scale" { MinimizeEffect::Scale } else { MinimizeEffect::Genie };
        }

        let hide_opts = vec![
            ("never".into(), "Never".into()),
            ("avoid".into(), "Avoid windows".into()),
            ("always".into(), "Always".into()),
        ];
        let cur_hide = match s.dock.autohide {
            DockAutohide::Never => "never",
            DockAutohide::AvoidWindows => "avoid",
            DockAutohide::Always => "always",
        };
        if let Some(v) = ui.row_popup("dock-hide", "Hide the Dock", None, cur_hide, &hide_opts) {
            s.dock.autohide = match v.as_str() {
                "avoid" => DockAutohide::AvoidWindows,
                "always" => DockAutohide::Always,
                _ => DockAutohide::Never,
            };
        }

        ui.row_toggle("Animate opening applications", None, &mut s.dock.animate_launch);
        ui.row_toggle("Show indicators for open applications", None, &mut s.dock.show_indicators);
    });

    ui.section("Windows & Mission Control");
    ui.group("dock-win", |ui| {
        let dbl_opts = vec![
            ("zoom".into(), "Zoom".into()),
            ("minimize".into(), "Minimise".into()),
            ("nothing".into(), "Do Nothing".into()),
        ];
        let cur_dbl = match s.windows.double_click_titlebar {
            TitlebarDoubleClick::Zoom => "zoom",
            TitlebarDoubleClick::Minimize => "minimize",
            TitlebarDoubleClick::Nothing => "nothing",
        };
        if let Some(v) = ui.row_popup("titlebar-dbl", "Double-click a window's title bar to", None, cur_dbl, &dbl_opts) {
            s.windows.double_click_titlebar = match v.as_str() {
                "minimize" => TitlebarDoubleClick::Minimize,
                "nothing" => TitlebarDoubleClick::Nothing,
                _ => TitlebarDoubleClick::Zoom,
            };
        }
        ui.row_toggle(
            "Focus follows pointer",
            Some("Keyboard focus moves to the window under the pointer"),
            &mut s.windows.focus_follows_mouse,
        );
        ui.row_toggle(
            "Switch Space when activating an app",
            Some("Jump to a Space that has open windows for the app"),
            &mut s.desktop.switch_space_on_activate,
        );
    });

    ui.section("Hot Corners");
    let c_opts: Vec<(String, String)> = CornerAction::ALL
        .iter()
        .map(|a| (corner_key(*a).to_string(), a.label().to_string()))
        .collect();
    ui.group("hot-corners", |ui| {
        let hc = &mut s.desktop.hot_corners;
        if let Some(v) = ui.row_popup("hc-tl", "Top-left corner", None, corner_key(hc.top_left), &c_opts) {
            hc.top_left = corner_from_key(&v);
        }
        if let Some(v) = ui.row_popup("hc-tr", "Top-right corner", None, corner_key(hc.top_right), &c_opts) {
            hc.top_right = corner_from_key(&v);
        }
        if let Some(v) = ui.row_popup("hc-bl", "Bottom-left corner", None, corner_key(hc.bottom_left), &c_opts) {
            hc.bottom_left = corner_from_key(&v);
        }
        if let Some(v) = ui.row_popup("hc-br", "Bottom-right corner", None, corner_key(hc.bottom_right), &c_opts) {
            hc.bottom_right = corner_from_key(&v);
        }
    });
}

// ---------------------------------------------------------------- Menu Bar

pub fn menu_bar(ui: &mut Ui, s: &mut Settings) {
    ui.section("Status Icons");
    ui.group("mb-icons", |ui| {
        ui.row_toggle("Network", Some("Show Wi‑Fi or Ethernet in Menu Bar"), &mut s.menu_bar.show_network);
        ui.row_toggle("Sound", Some("Show Volume in Menu Bar"), &mut s.menu_bar.show_volume);
        ui.row_toggle("Battery", Some("Show Battery in Menu Bar"), &mut s.menu_bar.show_battery);
        ui.row_toggle("Show battery percentage", None, &mut s.menu_bar.battery_percentage);
    });

    ui.section("Clock");
    ui.group("mb-clock", |ui| {
        let fmt_opts = vec![
            ("auto".into(), "Automatic (from region)".into()),
            ("12".into(), "12-hour".into()),
            ("24".into(), "24-hour".into()),
        ];
        let cur = match s.menu_bar.clock_24h {
            None => "auto",
            Some(false) => "12",
            Some(true) => "24",
        };
        if let Some(v) = ui.row_popup("clock-24h", "Time format", None, cur, &fmt_opts) {
            s.menu_bar.clock_24h = match v.as_str() {
                "12" => Some(false),
                "24" => Some(true),
                _ => None,
            };
        }
        ui.row_toggle("Show date", None, &mut s.menu_bar.show_date);
        ui.row_toggle("Show day of the week", None, &mut s.menu_bar.show_weekday);
        ui.row_toggle("Display time with seconds", None, &mut s.menu_bar.show_seconds);
    });
}

// ---------------------------------------------------------------- Displays

#[derive(Clone)]
struct Output {
    name: String,
    w: i32,
    h: i32,
    scale: f64,
    refresh: i32,
    refresh_rates: Vec<i32>,
    mode_w: i32,
    mode_h: i32,
    primary: bool,
}

fn hz(mhz: i32) -> String {
    let v = mhz as f64 / 1000.0;
    if (v - v.round()).abs() < 0.05 {
        format!("{} Hz", v.round())
    } else {
        format!("{v:.2} Hz")
    }
}

fn load_outputs() -> Vec<Output> {
    let Ok(v) = orchard_config::ipc_request(&serde_json::json!({"request": "outputs"})) else {
        return Vec::new();
    };
    v["outputs"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|o| {
                    Some(Output {
                        name: o["name"].as_str()?.to_string(),
                        w: o["w"].as_i64()? as i32,
                        h: o["h"].as_i64()? as i32,
                        scale: o["scale"].as_f64().unwrap_or(1.0),
                        refresh: o["refresh"].as_i64().unwrap_or(0) as i32,
                        refresh_rates: o["refresh_rates"]
                            .as_array()
                            .map(|a| a.iter().filter_map(|r| r.as_i64().map(|r| r as i32)).collect())
                            .unwrap_or_default(),
                        mode_w: o["mode_w"].as_i64().unwrap_or(0) as i32,
                        mode_h: o["mode_h"].as_i64().unwrap_or(0) as i32,
                        primary: o["primary"].as_bool().unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn displays(ui: &mut Ui, s: &mut Settings, st: &mut DesktopState, wake: &Wake) {
    st.outputs.ensure(wake, Duration::from_secs(5), load_outputs);
    let Some(outputs) = st.outputs.value.clone() else {
        ui.group("disp-load", |ui| {
            ui.row_value("Displays", "Loading…");
        });
        return;
    };
    if outputs.is_empty() {
        ui.group("disp-none", |ui| {
            ui.row_base("No displays reported", Some("Display settings require the Orchard compositor."), None);
        });
        return;
    }

    // Arrangement preview
    ui.group("disp-arr", |ui| {
        let r = ui.custom_row(138.0);
        let total_w: f32 = outputs.iter().map(|o| o.w.max(1) as f32).sum::<f32>()
            + 20.0 * (outputs.len().saturating_sub(1) as f32);
        let max_h = outputs.iter().map(|o| o.h.max(1)).max().unwrap_or(1) as f32;
        let k = ((r.w - 48.0) / total_w).min((r.h - 36.0) / max_h);
        let mut x = r.cx() - total_w * k * 0.5;
        for o in &outputs {
            let sw = o.w.max(1) as f32 * k;
            let sh = o.h.max(1) as f32 * k;
            let rr = Rect::new(x, r.cy() - sh * 0.5 - 8.0, sw, sh);
            ui.fill_rect(rr, 6.0, rgb([0x2C, 0x2C, 0x2E]));
            draw_mini_desktop(ui, rr.inset(3.5), Some(ui.pal.dark));
            if o.primary && outputs.len() > 1 {
                let inner = rr.inset(3.5);
                ui.fill_rect(Rect::new(inner.x, inner.y, inner.w, 4.5), 2.0, [255, 255, 255, 210]);
            }
            ui.text_center(&o.name, rr.cx(), rr.bottom() + 12.0, 11.5, Weight::Regular, ui.pal.text2);
            x += sw + 20.0 * k;
        }
    });

    for o in &outputs {
        ui.section(&o.name);
        ui.group(&format!("disp-{}", o.name), |ui| {
            let cur = s.displays.scale.get(&o.name).copied().unwrap_or(o.scale);
            let cur_pct = (cur * 100.0).round() as i32;
            let opts: Vec<(String, String)> = [100, 125, 150, 175, 200, 250, 300]
                .iter()
                .map(|pct| {
                    let lw = (o.w as f64 * o.scale / (*pct as f64 / 100.0)).round();
                    let lh = (o.h as f64 * o.scale / (*pct as f64 / 100.0)).round();
                    (pct.to_string(), format!("{pct}%  — {lw} × {lh}"))
                })
                .collect();
            if let Some(v) = ui.row_popup(
                &format!("scale-{}", o.name),
                "Scale",
                Some("Larger text and controls on high-resolution displays"),
                &cur_pct.to_string(),
                &opts,
            ) {
                if let Ok(pct) = v.parse::<i32>() {
                    s.displays.scale.insert(o.name.clone(), pct as f64 / 100.0);
                    st.outputs.invalidate();
                }
            }

            let (pw, ph) = if o.mode_w > 0 {
                (o.mode_w as f64, o.mode_h as f64)
            } else {
                ((o.w as f64 * o.scale).round(), (o.h as f64 * o.scale).round())
            };
            ui.row_value("Resolution", &format!("{pw} × {ph}"));

            if o.refresh_rates.len() > 1 {
                let saved = s.displays.refresh.get(&o.name).copied();
                let cur_r = saved
                    .and_then(|want| o.refresh_rates.iter().copied().min_by_key(|r| (r - want).abs()))
                    .unwrap_or(o.refresh);
                let ropts: Vec<(String, String)> = o.refresh_rates.iter().map(|r| (r.to_string(), hz(*r))).collect();
                if let Some(v) = ui.row_popup(&format!("hz-{}", o.name), "Refresh rate", None, &cur_r.to_string(), &ropts) {
                    if let Ok(mhz) = v.parse() {
                        s.displays.refresh.insert(o.name.clone(), mhz);
                        st.outputs.invalidate();
                    }
                }
            } else if o.refresh > 0 {
                ui.row_value("Refresh rate", &hz(o.refresh));
            }

            if outputs.len() > 1 {
                let mut main = o.primary;
                if ui.row_toggle("Main display", Some("Shows the menu bar and Dock"), &mut main) {
                    if main {
                        s.displays.primary = Some(o.name.clone());
                    } else {
                        s.displays.primary = outputs.iter().find(|x| x.name != o.name).map(|x| x.name.clone());
                    }
                    st.outputs.invalidate();
                }
            }
        });
    }
}
