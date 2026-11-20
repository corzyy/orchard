//! Keyboard and Mouse & Trackpad settings.

use std::time::Duration;

use orchard_config::{SecondaryClick, Settings};

use iced::widget::column;
use orchard_ui::{choices, form};

use crate::{
    pages::{edit, some, El},
    sys::{Bg, Wake},
};

#[derive(Default)]
pub struct InputState {
    xkb: Bg<Xkb>,
}

#[derive(Clone, Default)]
struct Xkb {
    layouts: Vec<(String, String)>,
    variants: Vec<(String, String, String)>,
}

fn load_xkb() -> Xkb {
    let mut x = Xkb::default();
    let Ok(s) = std::fs::read_to_string("/usr/share/X11/xkb/rules/evdev.lst") else {
        return x;
    };
    let mut section = "";
    for line in s.lines() {
        if let Some(name) = line.strip_prefix("! ") {
            section = match name.trim() {
                "layout" => "layout",
                "variant" => "variant",
                _ => "",
            };
            continue;
        }
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        let Some((code, desc)) = l.split_once(char::is_whitespace) else { continue };
        let desc = desc.trim();
        match section {
            "layout" => x.layouts.push((code.to_string(), desc.to_string())),
            "variant" => {
                if let Some((layout, d)) = desc.split_once(": ") {
                    x.variants.push((layout.to_string(), code.to_string(), d.to_string()));
                }
            }
            _ => {}
        }
    }
    x.layouts.sort_by(|a, b| a.1.cmp(&b.1));
    x
}

fn has_opt(opts: &str, o: &str) -> bool {
    opts.split(',').any(|x| x.trim() == o)
}

fn set_opt(opts: &mut String, o: &str, on: bool) {
    let mut v: Vec<String> = opts
        .split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty() && x != o)
        .collect();
    if on {
        v.push(o.to_string());
    }
    *opts = v.join(",");
}

pub fn keyboard(s: &Settings, st: &mut InputState, wake: &Wake) -> El {
    st.xkb.ensure(wake, Duration::from_secs(3600), load_xkb);
    let k = &s.keyboard;

    let rate = if k.repeat_rate == 0 { 30.0 } else { k.repeat_rate as f64 };
    let delay = 1000.0 - if k.repeat_delay == 0 { 250.0 } else { k.repeat_delay as f64 };
    let repeat = form::group([
        form::slider_row("Key repeat rate", None, 2.0..=80.0, rate, 1.0, Some(("Slow", "Fast")), |v| {
            edit(move |s| s.keyboard.repeat_rate = v.round() as i32)
        }),
        form::slider_row("Delay until repeat", None, 0.0..=850.0, delay, 1.0, Some(("Long", "Short")), |v| {
            edit(move |s| s.keyboard.repeat_delay = (1000.0 - v).round() as i32)
        }),
    ]);

    let xkb = st.xkb.value.clone().unwrap_or_default();
    let cur_layout = k.layout.split(',').next().unwrap_or_default().to_string();
    let mut layout_opts = vec![(String::new(), "Automatic (system layout)".to_string())];
    layout_opts.extend(xkb.layouts.iter().map(|(c, d)| (c.clone(), format!("{d} ({c})"))));
    let mut layout_rows = vec![form::popup_row("Input source", None, choices(layout_opts), &cur_layout, |v| {
        edit(move |s| {
            s.keyboard.layout = v.clone();
            s.keyboard.variant.clear();
        })
    })];
    if !k.layout.is_empty() {
        let mut vopts = vec![(String::new(), "Standard".to_string())];
        vopts.extend(
            xkb.variants
                .iter()
                .filter(|(l, _, _)| *l == k.layout)
                .map(|(_, v, d)| (v.clone(), d.clone())),
        );
        layout_rows.push(form::popup_row("Variant", None, choices(vopts), &k.variant, |v| {
            edit(move |s| s.keyboard.variant = v.clone())
        }));
    }

    let caps_opts: Vec<(String, String)> = vec![
        (String::new(), "Caps Lock".into()),
        ("ctrl:nocaps".into(), "Control".into()),
        ("caps:escape".into(), "Escape".into()),
        ("caps:backspace".into(), "Backspace".into()),
        ("caps:none".into(), "No Action".into()),
    ];
    let cur_caps = caps_opts
        .iter()
        .skip(1)
        .find(|(o, _)| has_opt(&k.options, o))
        .map(|(o, _)| o.clone())
        .unwrap_or_default();
    let caps_keys: Vec<String> = caps_opts.iter().skip(1).map(|(o, _)| o.clone()).collect();
    let option_toggle = |label: &'static str, sub: &'static str, opt: &'static str| {
        form::toggle_row(label, some(sub), has_opt(&k.options, opt), move |on| {
            edit(move |s| set_opt(&mut s.keyboard.options, opt, on))
        })
    };
    let mods = form::group([
        form::popup_row("Caps Lock key", None, choices(caps_opts.clone()), &cur_caps, move |v| {
            let caps_keys = caps_keys.clone();
            edit(move |s| {
                for o in &caps_keys {
                    set_opt(&mut s.keyboard.options, o, false);
                }
                if !v.is_empty() {
                    set_opt(&mut s.keyboard.options, &v, true);
                }
            })
        }),
        option_toggle(
            "Swap ⌘ Command and ⌥ Option",
            "Puts ⌘ next to the space bar, like on a Mac keyboard",
            "altwin:swap_alt_win",
        ),
        option_toggle("Right Alt is Compose", "Type special characters with Compose sequences", "compose:ralt"),
        form::toggle_row("Turn on Num Lock at login", None, k.numlock, |v| edit(move |s| s.keyboard.numlock = v)),
    ]);

    column![
        repeat,
        form::section("Text Input"),
        form::group(layout_rows),
        form::section("Modifier Keys"),
        mods,
    ]
    .into()
}

/// Rounds pointer speeds to 0.05 steps like the old slider did.
fn step20(v: f64) -> f64 {
    (v * 20.0).round() / 20.0
}

pub fn mouse(s: &Settings) -> El {
    let p = &s.pointer;
    let cur = if p.mouse_left_handed { "right" } else { "left" };
    form::group([
        form::slider_row("Tracking speed", None, -1.0..=1.0, p.mouse_speed, 0.05, Some(("Slow", "Fast")), |v| {
            edit(move |s| s.pointer.mouse_speed = step20(v))
        }),
        form::slider_row("Scrolling speed", None, 0.25..=3.0, p.scroll_factor, 0.05, Some(("Slow", "Fast")), |v| {
            edit(move |s| s.pointer.scroll_factor = step20(v))
        }),
        form::toggle_row("Natural scrolling", some("Content tracks finger movement"), p.mouse_natural_scroll, |v| {
            edit(move |s| s.pointer.mouse_natural_scroll = v)
        }),
        form::toggle_row("Pointer acceleration", None, p.mouse_acceleration, |v| {
            edit(move |s| s.pointer.mouse_acceleration = v)
        }),
        form::popup_row(
            "Primary mouse button",
            None,
            choices([("left", "Left"), ("right", "Right")]),
            cur,
            |v| edit(move |s| s.pointer.mouse_left_handed = v == "right"),
        ),
    ])
}

pub fn trackpad(s: &Settings) -> El {
    let p = &s.pointer;
    let cur_sc = match p.secondary_click {
        SecondaryClick::TwoFingers => "two",
        SecondaryClick::BottomRight => "corner",
    };
    column![
        form::section("Point & Click"),
        form::group([
            form::slider_row("Tracking speed", None, -1.0..=1.0, p.touchpad_speed, 0.05, Some(("Slow", "Fast")), |v| {
                edit(move |s| s.pointer.touchpad_speed = step20(v))
            }),
            form::popup_row(
                "Secondary click",
                None,
                choices([("two", "Two Fingers"), ("corner", "Bottom-Right Corner")]),
                cur_sc,
                |v| {
                    edit(move |s| {
                        s.pointer.secondary_click =
                            if v == "corner" { SecondaryClick::BottomRight } else { SecondaryClick::TwoFingers }
                    })
                },
            ),
            form::toggle_row("Tap to click", None, p.tap_to_click, |v| edit(move |s| s.pointer.tap_to_click = v)),
            form::toggle_row("Ignore trackpad while typing", None, p.disable_while_typing, |v| {
                edit(move |s| s.pointer.disable_while_typing = v)
            }),
        ]),
        form::section("Scroll & Gestures"),
        form::group([
            form::toggle_row("Natural scrolling", some("Content tracks finger movement"), p.touchpad_natural_scroll, |v| {
                edit(move |s| s.pointer.touchpad_natural_scroll = v)
            }),
            form::toggle_row(
                "Three-finger swipe gestures",
                some("Switch Spaces and open Mission Control"),
                s.desktop.gestures,
                |v| edit(move |s| s.desktop.gestures = v),
            ),
        ]),
    ]
    .into()
}
