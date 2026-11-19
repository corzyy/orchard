//! Keyboard, Mouse, Trackpad.

use eframe::egui::{self, Ui};
use orchard_config::SecondaryClick;

use super::{hero, Cx};
use crate::{
    style::pal,
    sys::Bg,
    widgets::{footnote, group, header, popup, secondary, slider, toggle, SliderOpts},
    Page,
};

#[derive(Default)]
pub struct InputState {
    xkb: Bg<Xkb>,
    devices: Bg<Devices>,
}

#[derive(Clone, Default)]
struct Xkb {
    /// (code, description)
    layouts: Vec<(String, String)>,
    /// (layout, variant, description)
    variants: Vec<(String, String, String)>,
}

fn load_xkb() -> Xkb {
    let mut x = Xkb::default();
    let Ok(s) = std::fs::read_to_string("/usr/share/X11/xkb/rules/evdev.lst") else { return x };
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

#[derive(Clone, Default)]
struct Devices {
    mice: Vec<String>,
    touchpads: Vec<String>,
}

fn load_devices() -> Devices {
    let mut d = Devices::default();
    let Ok(s) = std::fs::read_to_string("/proc/bus/input/devices") else { return d };
    for block in s.split("\n\n") {
        let name = block
            .lines()
            .find_map(|l| l.strip_prefix("N: Name="))
            .map(|n| n.trim_matches('"').to_string())
            .unwrap_or_default();
        let handlers = block.lines().find_map(|l| l.strip_prefix("H: Handlers=")).unwrap_or_default();
        if !handlers.contains("mouse") {
            continue;
        }
        let lower = name.to_lowercase();
        if lower.contains("touchpad") || lower.contains("trackpad") {
            d.touchpads.push(name);
        } else if !d.mice.contains(&name) {
            d.mice.push(name);
        }
    }
    d
}

fn sl(width: f32, left: &'static str, right: &'static str) -> SliderOpts<'static> {
    SliderOpts {
        width,
        ticks: 0,
        snap: false,
        left,
        right,
    }
}

fn has_opt(opts: &str, o: &str) -> bool {
    opts.split(',').any(|x| x.trim() == o)
}

fn set_opt(opts: &mut String, o: &str, on: bool) {
    let mut v: Vec<String> = opts.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty() && x != o).collect();
    if on {
        v.push(o.to_string());
    }
    *opts = v.join(",");
}

pub fn keyboard(ui: &mut Ui, cx: &mut Cx) {
    let ctx = ui.ctx().clone();
    cx.st.input.xkb.every(&ctx, 3600.0, load_xkb);
    let k = &mut cx.s.keyboard;
    group(ui, |g| {
        let mut rate = if k.repeat_rate == 0 { 30.0 } else { k.repeat_rate as f64 };
        g.row("Key repeat rate", |ui| {
            if slider(ui, &mut rate, 2.0..=80.0, sl(220.0, "Slow", "Fast")).changed() {
                k.repeat_rate = rate.round() as i32;
            }
        });
        // left = long delay like macOS
        let mut delay = 1000.0 - if k.repeat_delay == 0 { 250.0 } else { k.repeat_delay as f64 };
        g.row("Delay until repeat", |ui| {
            if slider(ui, &mut delay, 0.0..=850.0, sl(220.0, "Long", "Short")).changed() {
                k.repeat_delay = (1000.0 - delay).round() as i32;
            }
        });
    });

    header(ui, "Text Input");
    let xkb = cx.st.input.xkb.value.clone().unwrap_or_default();
    group(ui, |g| {
        let mut layout = k.layout.split(',').next().unwrap_or_default().to_string();
        let mut opts: Vec<(String, String)> = vec![(String::new(), "Automatic (system layout)".into())];
        opts.extend(xkb.layouts.iter().map(|(c, d)| (c.clone(), format!("{d}  ({c})"))));
        g.row("Input source", |ui| {
            if popup(ui, "layout", &mut layout, &opts) {
                k.layout = layout.clone();
                k.variant.clear();
            }
        });
        if !k.layout.is_empty() {
            let mut variant = k.variant.clone();
            let mut vopts: Vec<(String, String)> = vec![(String::new(), "Standard".into())];
            vopts.extend(
                xkb.variants
                    .iter()
                    .filter(|(l, _, _)| *l == k.layout)
                    .map(|(_, v, d)| (v.clone(), d.clone())),
            );
            g.row("Variant", |ui| {
                if popup(ui, "variant", &mut variant, &vopts) {
                    k.variant = variant;
                }
            });
        }
    });

    header(ui, "Modifier Keys");
    group(ui, |g| {
        let caps_opts = vec![
            (String::new(), "Caps Lock".to_string()),
            ("ctrl:nocaps".to_string(), "Control".to_string()),
            ("caps:escape".to_string(), "Escape".to_string()),
            ("caps:backspace".to_string(), "Backspace".to_string()),
            ("caps:none".to_string(), "No Action".to_string()),
        ];
        let mut caps = caps_opts
            .iter()
            .skip(1)
            .find(|(o, _)| has_opt(&k.options, o))
            .map(|(o, _)| o.clone())
            .unwrap_or_default();
        g.row("Caps Lock key", |ui| {
            if popup(ui, "caps", &mut caps, &caps_opts) {
                for (o, _) in caps_opts.iter().skip(1) {
                    set_opt(&mut k.options, o, false);
                }
                if !caps.is_empty() {
                    set_opt(&mut k.options, &caps, true);
                }
            }
        });
        let mut swap = has_opt(&k.options, "altwin:swap_alt_win");
        g.row_sub("Swap ⌘ Command and ⌥ Option", "Puts ⌘ next to the space bar, like on a Mac keyboard", |ui| {
            if toggle(ui, &mut swap).changed() {
                set_opt(&mut k.options, "altwin:swap_alt_win", swap);
            }
        });
        let mut compose = has_opt(&k.options, "compose:ralt");
        g.row_sub("Right Alt is Compose", "Type special characters with sequences such as Compose ' e → é", |ui| {
            if toggle(ui, &mut compose).changed() {
                set_opt(&mut k.options, "compose:ralt", compose);
            }
        });
        let mut nl = k.numlock;
        g.row("Turn on Num Lock at login", |ui| {
            if toggle(ui, &mut nl).changed() {
                k.numlock = nl;
            }
        });
    });

    header(ui, "Keyboard Shortcuts");
    group(ui, |g| {
        for (keys, what) in [
            ("⌘ Space", "Launcher"),
            ("⌘ Tab", "Switch apps"),
            ("⌘ `", "Cycle windows of the current app"),
            ("⌘ Q / ⌘ W", "Quit app / close window"),
            ("⌘ M", "Minimise"),
            ("⌃⌘ F", "Full screen"),
            ("⌥⌘ F", "Zoom"),
            ("⌘ ↑  or  F3", "Mission Control"),
            ("⌘ ↓", "Application windows"),
            ("⌃⌘ ← / →", "Previous / next Space"),
            ("⌃⌘⇧ ← / →", "Move window to Space"),
            ("⌘⇧ 3", "Screenshot"),
            ("⌃⌘ Q", "Lock screen"),
            ("⌘ Return", "Terminal"),
        ] {
            g.row(what, |ui| {
                ui.label(egui::RichText::new(keys).color(pal().text2).monospace());
            });
        }
    });
}

pub fn mouse(ui: &mut Ui, cx: &mut Cx) {
    let ctx = ui.ctx().clone();
    cx.st.input.devices.every(&ctx, 10.0, load_devices);
    let p = &mut cx.s.pointer;
    group(ui, |g| {
        let mut speed = p.mouse_speed;
        g.row("Tracking speed", |ui| {
            if slider(ui, &mut speed, -1.0..=1.0, sl(220.0, "Slow", "Fast")).changed() {
                p.mouse_speed = (speed * 20.0).round() / 20.0;
            }
        });
        let mut f = p.scroll_factor;
        g.row("Scrolling speed", |ui| {
            if slider(ui, &mut f, 0.25..=3.0, sl(220.0, "Slow", "Fast")).changed() {
                p.scroll_factor = (f * 20.0).round() / 20.0;
            }
        });
        g.row_sub("Natural scrolling", "Content tracks finger movement", |ui| {
            toggle(ui, &mut p.mouse_natural_scroll);
        });
        g.row_sub("Pointer acceleration", "Moving the mouse faster moves the pointer further", |ui| {
            toggle(ui, &mut p.mouse_acceleration);
        });
        let mut lh = p.mouse_left_handed;
        g.row("Primary mouse button", |ui| {
            if popup(ui, "primary", &mut lh, &[(false, "Left".into()), (true, "Right".into())]) {
                p.mouse_left_handed = lh;
            }
        });
    });
    if let Some(d) = &cx.st.input.devices.value {
        if !d.mice.is_empty() {
            header(ui, "Connected");
            group(ui, |g| {
                for m in &d.mice {
                    g.row(m, |_| {});
                }
            });
        }
    }
}

pub fn trackpad(ui: &mut Ui, cx: &mut Cx) {
    let ctx = ui.ctx().clone();
    cx.st.input.devices.every(&ctx, 10.0, load_devices);
    let none = cx.st.input.devices.value.as_ref().map(|d| d.touchpads.is_empty()).unwrap_or(false);
    hero(ui, cx, Page::Trackpad, "Tracking, clicking and scrolling with a trackpad.");
    if none {
        footnote(ui, "No trackpad is connected right now. These settings apply when one is.");
        ui.add_space(6.0);
    }
    let p = &mut cx.s.pointer;
    header(ui, "Point & Click");
    group(ui, |g| {
        let mut speed = p.touchpad_speed;
        g.row("Tracking speed", |ui| {
            if slider(ui, &mut speed, -1.0..=1.0, sl(220.0, "Slow", "Fast")).changed() {
                p.touchpad_speed = (speed * 20.0).round() / 20.0;
            }
        });
        let mut sc = p.secondary_click;
        g.row("Secondary click", |ui| {
            if popup(
                ui,
                "secondary",
                &mut sc,
                &[
                    (SecondaryClick::TwoFingers, "Click or Tap with Two Fingers".into()),
                    (SecondaryClick::BottomRight, "Click in Bottom Right Corner".into()),
                ],
            ) {
                p.secondary_click = sc;
            }
        });
        g.row("Tap to click", |ui| {
            toggle(ui, &mut p.tap_to_click);
        });
        g.row("Ignore trackpad while typing", |ui| {
            toggle(ui, &mut p.disable_while_typing);
        });
    });
    header(ui, "Scroll & Gestures");
    group(ui, |g| {
        g.row_sub("Natural scrolling", "Content tracks finger movement", |ui| {
            toggle(ui, &mut p.touchpad_natural_scroll);
        });
        g.row_sub(
            "Swipe between Spaces and open Mission Control",
            "Three fingers left / right / up / down",
            |ui| {
                toggle(ui, &mut cx.s.desktop.gestures);
            },
        );
    });
    if let Some(d) = &cx.st.input.devices.value {
        for t in &d.touchpads {
            secondary(ui, t);
        }
    }
}
