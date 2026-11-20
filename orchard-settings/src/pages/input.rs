//! Keyboard and Mouse & Trackpad settings.

use std::time::Duration;

use orchard_config::{SecondaryClick, Settings};

use crate::{
    sys::{Bg, Wake},
    ui::Ui,
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

pub fn keyboard(ui: &mut Ui, s: &mut Settings, st: &mut InputState, wake: &Wake) {
    st.xkb.ensure(wake, Duration::from_secs(3600), load_xkb);
    let k = &mut s.keyboard;

    ui.group("kb-repeat", |ui| {
        let mut rate = if k.repeat_rate == 0 { 30.0 } else { k.repeat_rate as f64 };
        if ui.row_slider("kb-rate", "Key repeat rate", None, &mut rate, 2.0, 80.0, "Slow", "Fast") {
            k.repeat_rate = rate.round() as i32;
        }
        let mut delay = 1000.0 - if k.repeat_delay == 0 { 250.0 } else { k.repeat_delay as f64 };
        if ui.row_slider("kb-delay", "Delay until repeat", None, &mut delay, 0.0, 850.0, "Long", "Short") {
            k.repeat_delay = (1000.0 - delay).round() as i32;
        }
    });

    ui.section("Text Input");
    let xkb = st.xkb.value.clone().unwrap_or_default();
    ui.group("kb-layout", |ui| {
        let cur_layout = k.layout.split(',').next().unwrap_or_default().to_string();
        let mut opts = vec![(String::new(), "Automatic (system layout)".into())];
        opts.extend(xkb.layouts.iter().map(|(c, d)| (c.clone(), format!("{d} ({c})"))));
        if let Some(v) = ui.row_popup("kb-src", "Input source", None, &cur_layout, &opts) {
            k.layout = v;
            k.variant.clear();
        }
        if !k.layout.is_empty() {
            let mut vopts = vec![(String::new(), "Standard".into())];
            vopts.extend(
                xkb.variants
                    .iter()
                    .filter(|(l, _, _)| *l == k.layout)
                    .map(|(_, v, d)| (v.clone(), d.clone())),
            );
            if let Some(v) = ui.row_popup("kb-var", "Variant", None, &k.variant, &vopts) {
                k.variant = v;
            }
        }
    });

    ui.section("Modifier Keys");
    ui.group("kb-mods", |ui| {
        let caps_opts = vec![
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
        if let Some(v) = ui.row_popup("kb-caps", "Caps Lock key", None, &cur_caps, &caps_opts) {
            for (o, _) in caps_opts.iter().skip(1) {
                set_opt(&mut k.options, o, false);
            }
            if !v.is_empty() {
                set_opt(&mut k.options, &v, true);
            }
        }
        let mut swap = has_opt(&k.options, "altwin:swap_alt_win");
        if ui.row_toggle(
            "Swap ⌘ Command and ⌥ Option",
            Some("Puts ⌘ next to the space bar, like on a Mac keyboard"),
            &mut swap,
        ) {
            set_opt(&mut k.options, "altwin:swap_alt_win", swap);
        }
        let mut compose = has_opt(&k.options, "compose:ralt");
        if ui.row_toggle(
            "Right Alt is Compose",
            Some("Type special characters with Compose sequences"),
            &mut compose,
        ) {
            set_opt(&mut k.options, "compose:ralt", compose);
        }
        ui.row_toggle("Turn on Num Lock at login", None, &mut k.numlock);
    });
}

pub fn pointer(ui: &mut Ui, s: &mut Settings) {
    let p = &mut s.pointer;

    ui.section("Mouse");
    ui.group("ptr-mouse", |ui| {
        let mut speed = p.mouse_speed;
        if ui.row_slider("mouse-speed", "Tracking speed", None, &mut speed, -1.0, 1.0, "Slow", "Fast") {
            p.mouse_speed = (speed * 20.0).round() / 20.0;
        }
        let mut scroll = p.scroll_factor;
        if ui.row_slider("scroll-speed", "Scrolling speed", None, &mut scroll, 0.25, 3.0, "Slow", "Fast") {
            p.scroll_factor = (scroll * 20.0).round() / 20.0;
        }
        ui.row_toggle("Natural scrolling", Some("Content tracks finger movement"), &mut p.mouse_natural_scroll);
        ui.row_toggle("Pointer acceleration", None, &mut p.mouse_acceleration);
        let btn_opts = vec![("left".into(), "Left".into()), ("right".into(), "Right".into())];
        let cur = if p.mouse_left_handed { "right" } else { "left" };
        if let Some(v) = ui.row_popup("mouse-btn", "Primary mouse button", None, cur, &btn_opts) {
            p.mouse_left_handed = v == "right";
        }
    });

    ui.section("Trackpad");
    ui.group("ptr-pad", |ui| {
        let mut tspeed = p.touchpad_speed;
        if ui.row_slider("pad-speed", "Tracking speed", None, &mut tspeed, -1.0, 1.0, "Slow", "Fast") {
            p.touchpad_speed = (tspeed * 20.0).round() / 20.0;
        }
        let sc_opts = vec![
            ("two".into(), "Two Fingers".into()),
            ("corner".into(), "Bottom-Right Corner".into()),
        ];
        let cur_sc = match p.secondary_click {
            SecondaryClick::TwoFingers => "two",
            SecondaryClick::BottomRight => "corner",
        };
        if let Some(v) = ui.row_popup("pad-sec", "Secondary click", None, cur_sc, &sc_opts) {
            p.secondary_click = if v == "corner" {
                SecondaryClick::BottomRight
            } else {
                SecondaryClick::TwoFingers
            };
        }
        ui.row_toggle("Tap to click", None, &mut p.tap_to_click);
        ui.row_toggle("Natural scrolling", None, &mut p.touchpad_natural_scroll);
        ui.row_toggle("Ignore trackpad while typing", None, &mut p.disable_while_typing);
        ui.row_toggle(
            "Three-finger swipe gestures",
            Some("Switch Spaces and open Mission Control"),
            &mut s.desktop.gestures,
        );
    });
}
