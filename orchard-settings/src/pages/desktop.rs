//! Desktop & Dock (including inline Hot Corners), Menu Bar, and Displays.

use std::time::Duration;

use orchard_config::{
    CornerAction, DockAutohide, DockPosition, NightShiftSchedule, NightShiftSettings, Settings,
    TitlebarDoubleClick,
};

use iced::widget::column;
use orchard_ui::{choices, form};

use crate::{
    pages::{art, edit, act, loading, some, El},
    sys::{Bg, Wake},
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

pub fn desktop_dock(s: &Settings) -> El {
    let d = &s.dock;
    let cur_pos = match d.position {
        DockPosition::Left => "left",
        DockPosition::Bottom => "bottom",
        DockPosition::Right => "right",
    };
    let cur_hide = match d.autohide {
        DockAutohide::Never => "never",
        DockAutohide::AvoidWindows => "avoid",
        DockAutohide::Always => "always",
    };
    // The magnification slider starts at the base size: dragged fully left,
    // icons no longer grow under the pointer.
    let mag_lo = d.icon_size as f64;
    let mag_hi = 128.0_f64.max(mag_lo);
    let dock_rows = vec![
        form::slider_row("Size", None, 32.0..=96.0, d.icon_size as f64, 1.0, Some(("Small", "Large")), |v| {
            edit(move |s| {
                // keep the magnification strength when the base size changes
                let ratio = if s.dock.magnified_size > s.dock.icon_size + 0.5 {
                    s.dock.magnified_size / s.dock.icon_size
                } else {
                    1.0
                };
                let icon = v.round() as f32;
                s.dock.icon_size = icon;
                s.dock.magnified_size = (icon * ratio).clamp(icon, 128.0);
            })
        }),
        form::slider_row(
            "Magnification",
            None,
            mag_lo..=mag_hi,
            (d.magnified_size as f64).clamp(mag_lo, mag_hi),
            1.0,
            Some(("Off", "Large")),
            |v| edit(move |s| s.dock.magnified_size = v.round() as f32),
        ),
        form::popup_row(
            "Position on screen",
            None,
            choices([("left", "Left"), ("bottom", "Bottom"), ("right", "Right")]),
            cur_pos,
            |v| {
                edit(move |s| {
                    s.dock.position = match v.as_str() {
                        "left" => DockPosition::Left,
                        "right" => DockPosition::Right,
                        _ => DockPosition::Bottom,
                    }
                })
            },
        ),
        form::popup_row(
            "Hide the Dock",
            None,
            choices([("never", "Never"), ("avoid", "Avoid windows"), ("always", "Always")]),
            cur_hide,
            |v| {
                edit(move |s| {
                    s.dock.autohide = match v.as_str() {
                        "avoid" => DockAutohide::AvoidWindows,
                        "always" => DockAutohide::Always,
                        _ => DockAutohide::Never,
                    }
                })
            },
        ),
        form::toggle_row("Show Trash in Dock", None, d.show_trash, |v| {
            edit(move |s| s.dock.show_trash = v)
        }),
    ];

    let cur_dbl = match s.windows.double_click_titlebar {
        TitlebarDoubleClick::Zoom => "zoom",
        TitlebarDoubleClick::Minimize => "minimize",
        TitlebarDoubleClick::Nothing => "nothing",
    };
    let windows = form::group([
        form::popup_row(
            "Double-click a window's title bar to",
            None,
            choices([("zoom", "Zoom"), ("minimize", "Minimise"), ("nothing", "Do Nothing")]),
            cur_dbl,
            |v| {
                edit(move |s| {
                    s.windows.double_click_titlebar = match v.as_str() {
                        "minimize" => TitlebarDoubleClick::Minimize,
                        "nothing" => TitlebarDoubleClick::Nothing,
                        _ => TitlebarDoubleClick::Zoom,
                    }
                })
            },
        ),
        form::toggle_row(
            "Focus follows pointer",
            some("Keyboard focus moves to the window under the pointer"),
            s.windows.focus_follows_mouse,
            |v| edit(move |s| s.windows.focus_follows_mouse = v),
        ),
    ]);

    let mission = form::group([form::toggle_row(
        "When switching to an application, switch to a Space with open windows for the application",
        None,
        s.desktop.switch_space_on_activate,
        |v| edit(move |s| s.desktop.switch_space_on_activate = v),
    )]);

    let c_opts = choices(CornerAction::ALL.iter().map(|a| (corner_key(*a), a.label())));
    let hc = &s.desktop.hot_corners;
    type Pick = fn(&mut orchard_config::HotCorners) -> &mut CornerAction;
    let corner = |label: &'static str, cur: CornerAction, pick: Pick| {
        form::popup_row(label, None, c_opts.clone(), corner_key(cur), move |v| {
            edit(move |s| *pick(&mut s.desktop.hot_corners) = corner_from_key(&v))
        })
    };
    let corners = form::group([
        corner("Top-left corner", hc.top_left, |h| &mut h.top_left),
        corner("Top-right corner", hc.top_right, |h| &mut h.top_right),
        corner("Bottom-left corner", hc.bottom_left, |h| &mut h.bottom_left),
        corner("Bottom-right corner", hc.bottom_right, |h| &mut h.bottom_right),
    ]);

    column![
        form::section("Dock"),
        form::group(dock_rows),
        form::section("Desktop"),
        form::group([form::toggle_row("Show items on desktop", None, s.windows.show_desktop_icons, |v| {
            edit(move |s| s.windows.show_desktop_icons = v)
        })]),
        form::section("Windows"),
        windows,
        form::section("Mission Control"),
        mission,
        form::section("Hot Corners"),
        corners,
    ]
    .into()
}

// ---------------------------------------------------------------- Menu Bar

pub fn menu_bar(s: &Settings) -> El {
    let m = &s.menu_bar;
    let cur = match m.clock_24h {
        None => "auto",
        Some(false) => "12",
        Some(true) => "24",
    };
    column![
        form::section("Status Icons"),
        form::group([
            form::toggle_row("Network", some("Show Wi‑Fi or Ethernet in Menu Bar"), m.show_network, |v| {
                edit(move |s| s.menu_bar.show_network = v)
            }),
            form::toggle_row("Sound", some("Show Volume in Menu Bar"), m.show_volume, |v| {
                edit(move |s| s.menu_bar.show_volume = v)
            }),
            form::toggle_row("Battery", some("Show Battery in Menu Bar"), m.show_battery, |v| {
                edit(move |s| s.menu_bar.show_battery = v)
            }),
            form::toggle_row("Show battery percentage", None, m.battery_percentage, |v| {
                edit(move |s| s.menu_bar.battery_percentage = v)
            }),
        ]),
        form::section("Clock"),
        form::group([
            form::popup_row(
                "Time format",
                None,
                choices([("auto", "Automatic (from region)"), ("12", "12-hour"), ("24", "24-hour")]),
                cur,
                |v| {
                    edit(move |s| {
                        s.menu_bar.clock_24h = match v.as_str() {
                            "12" => Some(false),
                            "24" => Some(true),
                            _ => None,
                        }
                    })
                },
            ),
            form::toggle_row("Show date", None, m.show_date, |v| edit(move |s| s.menu_bar.show_date = v)),
            form::toggle_row("Show day of the week", None, m.show_weekday, |v| {
                edit(move |s| s.menu_bar.show_weekday = v)
            }),
            form::toggle_row("Display time with seconds", None, m.show_seconds, |v| {
                edit(move |s| s.menu_bar.show_seconds = v)
            }),
        ]),
    ]
    .into()
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
    vrr_supported: bool,
    vrr_enabled: bool,
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
                        vrr_supported: o["vrr_supported"].as_bool().unwrap_or(false),
                        vrr_enabled: o["vrr_enabled"].as_bool().unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn displays(s: &Settings, st: &mut DesktopState, wake: &Wake) -> El {
    st.outputs.ensure(wake, Duration::from_secs(5), load_outputs);
    let Some(outputs) = st.outputs.value.clone() else {
        return loading("Displays");
    };
    if outputs.is_empty() {
        return form::group([form::row(
            "No displays reported",
            some("Display settings require the Orchard compositor."),
            iced::widget::Space::new(0, 0),
        )]);
    }

    let screens = outputs
        .iter()
        .map(|o| art::Screen { name: o.name.clone(), w: o.w, h: o.h, primary: o.primary })
        .collect();
    let mut col = column![
        form::section("Arrangement"),
        form::group([form::custom_row(art::arrangement(screens))]),
    ];

    let multi = outputs.len() > 1;
    for o in &outputs {
        let cur = s.displays.scale.get(&o.name).copied().unwrap_or(o.scale);
        let cur_pct = (cur * 100.0).round() as i32;
        let opts = choices([100, 125, 150, 175, 200, 250, 300].iter().map(|pct| {
            let lw = (o.w as f64 * o.scale / (*pct as f64 / 100.0)).round();
            let lh = (o.h as f64 * o.scale / (*pct as f64 / 100.0)).round();
            (pct.to_string(), format!("{pct}%  — {lw} × {lh}"))
        }));
        let name = o.name.clone();
        let mut rows = vec![form::popup_row(
            "Scale",
            some("Larger text and controls on high-resolution displays"),
            opts,
            &cur_pct.to_string(),
            move |v| {
                let name = name.clone();
                act(move |s, st, _| {
                    if let Ok(pct) = v.parse::<i32>() {
                        s.displays.scale.insert(name.clone(), pct as f64 / 100.0);
                        st.desktop.outputs.invalidate();
                    }
                })
            },
        )];

        let (pw, ph) = if o.mode_w > 0 {
            (o.mode_w as f64, o.mode_h as f64)
        } else {
            ((o.w as f64 * o.scale).round(), (o.h as f64 * o.scale).round())
        };
        rows.push(form::value_row("Resolution", format!("{pw} × {ph}")));

        if o.refresh_rates.len() > 1 {
            let saved = s.displays.refresh.get(&o.name).copied();
            let cur_r = saved
                .and_then(|want| o.refresh_rates.iter().copied().min_by_key(|r| (r - want).abs()))
                .unwrap_or(o.refresh);
            let ropts = choices(o.refresh_rates.iter().map(|r| (r.to_string(), hz(*r))));
            let name = o.name.clone();
            rows.push(form::popup_row("Refresh rate", None, ropts, &cur_r.to_string(), move |v| {
                let name = name.clone();
                act(move |s, st, _| {
                    if let Ok(mhz) = v.parse() {
                        s.displays.refresh.insert(name.clone(), mhz);
                        st.desktop.outputs.invalidate();
                    }
                })
            }));
        } else if o.refresh > 0 {
            rows.push(form::value_row("Refresh rate", hz(o.refresh)));
        }

        let vrr_status = if o.vrr_enabled {
            "Active"
        } else if o.vrr_supported {
            "Supported"
        } else {
            "Not supported"
        };
        rows.push(form::value_row("Adaptive Sync (VRR)", vrr_status));

        if multi {
            let name = o.name.clone();
            let other = outputs.iter().find(|x| x.name != o.name).map(|x| x.name.clone());
            rows.push(form::toggle_row("Main display", some("Shows the menu bar and Dock"), o.primary, move |main| {
                let (name, other) = (name.clone(), other.clone());
                act(move |s, st, _| {
                    s.displays.primary = if main { Some(name.clone()) } else { other.clone() };
                    st.desktop.outputs.invalidate();
                })
            }));
        }

        col = col.push(form::section(o.name.clone())).push(form::group(rows));
    }
    col.push(form::section("Night Shift")).push(night_shift(s)).into()
}

/// Displays → Night Shift (macOS layout: schedule, manual switch, warmth).
fn night_shift(s: &Settings) -> El {
    let ns = &s.night_shift;
    let custom = ns.schedule == NightShiftSchedule::Custom;
    let mut rows = vec![form::popup_row(
        "Schedule",
        some("Shifts the colours of your display to the warmer end of the spectrum after dark"),
        choices([("off", "Off"), ("custom", "Custom")]),
        if custom { "custom" } else { "off" },
        |v| {
            edit(move |s| {
                s.night_shift.schedule = if v == "custom" { NightShiftSchedule::Custom } else { NightShiftSchedule::Off };
                // a manual switch made for the old schedule no longer applies
                s.night_shift.manual = None;
            })
        },
    )];
    if custom {
        let times = || choices((0..48).map(|i| ((i * 30).to_string(), NightShiftSettings::clock(i * 30))));
        let snap = |m: u32| (m / 30 * 30).to_string();
        rows.push(form::popup_row("From", None, times(), &snap(ns.from_minute), |v| {
            edit(move |s| {
                if let Ok(m) = v.parse() {
                    s.night_shift.from_minute = m;
                    s.night_shift.manual = None;
                }
            })
        }));
        rows.push(form::popup_row("To", None, times(), &snap(ns.to_minute), |v| {
            edit(move |s| {
                if let Ok(m) = v.parse() {
                    s.night_shift.to_minute = m;
                    s.night_shift.manual = None;
                }
            })
        }));
    }
    let on = ns.active_now();
    let manual_label = if on {
        format!("On until {}", NightShiftSettings::clock(ns.to_minute))
    } else {
        "Turn on until tomorrow".to_string()
    };
    rows.push(form::toggle_row("Manual", Some(manual_label), on, |on| {
        edit(move |s| s.night_shift.set_manual_now(on))
    }));
    rows.push(form::slider_row(
        "Colour temperature",
        None,
        0.0..=1.0,
        ns.warmth as f64,
        0.05,
        Some(("Less Warm", "More Warm")),
        |v| edit(move |s| s.night_shift.warmth = v as f32),
    ));
    form::group(rows)
}
