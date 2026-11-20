//! Orchard's own options: Appearance, Accessibility, Menu Bar, Desktop &
//! Dock, Displays, Wallpaper, Lock Screen.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::mpsc,
};

use eframe::egui::{self, pos2, vec2, Align2, Color32, ColorImage, CornerRadius, Rect, Sense, Stroke, StrokeKind, TextureHandle, Ui};
use orchard_config::{Accent, CornerAction, DockAutohide, DockPosition, MinimizeEffect, ThemeMode, TitlebarDoubleClick};

use super::{hero, Cx};
use crate::{
    style::{bold, pal},
    sys::{self, Bg},
    widgets::{
        button, button_enabled, footnote, group, header, mix, popup, primary_button, rgb, secondary, slider, text_field,
        toggle, RowOpts, SliderOpts,
    },
    Page,
};

#[derive(Clone, Default)]
struct ThemesInfo {
    font: String,
    icons: Vec<String>,
    cursors: Vec<String>,
}

fn load_themes() -> ThemesInfo {
    ThemesInfo {
        font: sys::output("fc-match", &["-f", "%{family}", "sans-serif"]).unwrap_or_default(),
        icons: installed_themes(false),
        cursors: installed_themes(true),
    }
}

#[derive(Default)]
pub struct DesktopState {
    corners_open: bool,
    themes: Bg<ThemesInfo>,
    outputs: Bg<Vec<Output>>,
    wallpaper: WallpaperState,
    lock_msg: Option<String>,
}

fn sl(width: f32, ticks: usize, snap: bool, left: &'static str, right: &'static str) -> SliderOpts<'static> {
    SliderOpts {
        width,
        ticks,
        snap,
        left,
        right,
    }
}

// ---------------------------------------------------------------- Appearance

fn mini_desktop(ui: &Ui, r: Rect, dark: Option<bool>) {
    let painter = ui.painter();
    // wallpaper-ish backdrop
    painter.rect_filled(r, 6, rgb([0x4A, 0x7B, 0xD8]));
    painter.rect_filled(
        Rect::from_min_max(pos2(r.left(), r.center().y), r.max),
        egui::CornerRadius { nw: 0, ne: 0, sw: 6, se: 6 },
        rgb([0x8A, 0x5C, 0xC8]),
    );
    let draw_window = |clip: Rect, dark: bool| {
        let p = painter.with_clip_rect(clip);
        let w = Rect::from_min_size(r.min + vec2(r.width() * 0.16, r.height() * 0.2), vec2(r.width() * 0.68, r.height() * 0.62));
        let (body, bar) = if dark { (rgb([0x2A, 0x2A, 0x2C]), rgb([0x3A, 0x3A, 0x3C])) } else { (Color32::WHITE, rgb([0xE8, 0xE6, 0xE8])) };
        p.rect_filled(w, 4, body);
        p.rect_filled(
            Rect::from_min_size(w.min, vec2(w.width(), 8.0)),
            egui::CornerRadius { nw: 4, ne: 4, sw: 0, se: 0 },
            bar,
        );
        for (i, c) in [[0xFF, 0x5F, 0x57], [0xFE, 0xBC, 0x2E], [0x28, 0xC8, 0x40]].iter().enumerate() {
            p.circle_filled(pos2(w.left() + 5.0 + i as f32 * 5.0, w.top() + 4.0), 1.6, rgb(*c));
        }
        let line = if dark { Color32::from_white_alpha(40) } else { Color32::from_black_alpha(30) };
        for i in 0..3 {
            let y = w.top() + 14.0 + i as f32 * 6.0;
            p.rect_filled(Rect::from_min_size(pos2(w.left() + 6.0, y), vec2(w.width() * (0.7 - i as f32 * 0.15), 2.5)), 1, line);
        }
    };
    match dark {
        Some(d) => draw_window(r, d),
        None => {
            draw_window(Rect::from_min_max(r.min, pos2(r.center().x, r.max.y)), false);
            draw_window(Rect::from_min_max(pos2(r.center().x, r.min.y), r.max), true);
        }
    }
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

pub fn appearance(ui: &mut Ui, cx: &mut Cx) {
    let p = pal();
    group(ui, |g| {
        g.custom(|ui| {
            // three thumbnails, right-aligned, painted at fixed positions
            const TW: f32 = 76.0;
            const TH: f32 = 50.0;
            const GAP: f32 = 14.0;
            let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), TH + 22.0), Sense::hover());
            ui.painter().text(
                pos2(row.left(), row.top() + TH / 2.0),
                Align2::LEFT_CENTER,
                "Appearance",
                egui::TextStyle::Body.resolve(ui.style()),
                p.text,
            );
            let modes = [
                (ThemeMode::Auto, "Auto", None),
                (ThemeMode::Light, "Light", Some(false)),
                (ThemeMode::Dark, "Dark", Some(true)),
            ];
            let total = 3.0 * TW + 2.0 * GAP;
            for (i, (mode, label, dark)) in modes.into_iter().enumerate() {
                let x = row.right() - total + i as f32 * (TW + GAP);
                let r = Rect::from_min_size(pos2(x, row.top() + 2.0), vec2(TW, TH));
                let resp = ui.interact(r, ui.id().with(("theme", i)), Sense::click());
                mini_desktop(ui, r, dark);
                let sel = cx.s.appearance.mode == mode;
                if sel {
                    ui.painter().rect_stroke(r.expand(2.5), 8, Stroke::new(2.5_f32, p.accent), StrokeKind::Middle);
                }
                if resp.clicked() {
                    cx.s.appearance.mode = mode;
                }
                ui.painter().text(
                    pos2(r.center().x, r.bottom() + 11.0),
                    Align2::CENTER_CENTER,
                    label,
                    if sel { bold(11.5) } else { egui::FontId::proportional(11.5) },
                    if sel { p.text } else { p.text2 },
                );
            }
        });
        g.row("Accent colour", |ui| {
            ui.spacing_mut().item_spacing.x = 7.0;
            for a in Accent::ALL.iter().rev() {
                let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
                let c = rgb(a.rgb());
                ui.painter().circle_filled(r.center(), 7.5, c);
                ui.painter().circle_stroke(r.center(), 7.5, Stroke::new(0.8_f32, mix(c, Color32::BLACK, 0.2)));
                if cx.s.appearance.accent == *a {
                    ui.painter().circle_filled(r.center(), 2.8, Color32::WHITE);
                }
                if resp.on_hover_text(a.name()).clicked() {
                    cx.s.appearance.accent = *a;
                }
            }
        });
    });
    footnote(ui, "Auto follows the colour scheme of your apps (org.gnome.desktop.interface color-scheme). In an Orchard session, Light and Dark switch GTK, Qt and Electron apps along with Orchard; other desktops keep their own setting.");

    header(ui, "Windows");
    group(ui, |g| {
        let mut r = cx.s.appearance.corner_radius as f64;
        g.row("Window corners", |ui| {
            if slider(ui, &mut r, 0.0..=16.0, sl(200.0, 5, true, "Square", "Round")).changed() {
                cx.s.appearance.corner_radius = r as f32;
            }
        });
        let mut t = cx.s.appearance.reduce_transparency;
        g.row_sub("Reduce transparency", "Use solid colours for the menu bar and Dock", |ui| {
            if toggle(ui, &mut t).changed() {
                cx.s.appearance.reduce_transparency = t;
            }
        });
    });

    let ctx = ui.ctx().clone();
    cx.st.desktop.themes.every(&ctx, 60.0, load_themes);
    let themes = cx.st.desktop.themes.value.clone().unwrap_or_default();

    header(ui, "Themes");
    group(ui, |g| {
        g.row("Icons", |ui| {
            let def = orchard_config::DEFAULT_ICON_THEME;
            let mut options: Vec<(Option<String>, String)> = vec![(None, format!("{def} (default)"))];
            options.extend(themes.icons.iter().filter(|t| t.as_str() != def).map(|t| (Some(t.clone()), t.clone())));
            let mut v = cx.s.appearance.icon_theme.clone();
            if popup(ui, "icon-theme", &mut v, &options) {
                cx.s.appearance.icon_theme = v;
            }
        });
        g.row("Pointer", |ui| {
            let def = orchard_config::DEFAULT_CURSOR_THEME;
            let mut options: Vec<(Option<String>, String)> = vec![(None, format!("{def} (default)"))];
            options.extend(themes.cursors.iter().filter(|t| t.as_str() != def).map(|t| (Some(t.clone()), t.clone())));
            let mut v = cx.s.appearance.cursor_theme.clone();
            if popup(ui, "cursor-theme", &mut v, &options) {
                cx.s.appearance.cursor_theme = v;
            }
        });
        g.row("Pointer size", |ui| {
            let options: Vec<(u32, String)> = [24u32, 32, 36, 48, 64].iter().map(|n| (*n, n.to_string())).collect();
            let mut v = cx.s.appearance.cursor_size();
            if popup(ui, "cursor-size", &mut v, &options) {
                cx.s.appearance.cursor_size = v;
            }
        });
        g.row("Interface font", |ui| {
            secondary(ui, themes.font.split(',').next().unwrap_or_default());
        });
    });
    footnote(ui, "Icon and pointer themes apply to Orchard and to the apps in an Orchard session. Other desktops keep their own themes.");
}

/// Installed icon themes (`cursors == false`) or pointer themes (`true`), from
/// ~/.local/share/icons, ~/.icons and $XDG_DATA_DIRS.
fn installed_themes(cursors: bool) -> Vec<String> {
    let mut dirs = vec![home().join(".local/share/icons"), home().join(".icons")];
    let data = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(data.split(':').filter(|d| !d.is_empty()).map(|d| std::path::Path::new(d).join("icons")));
    let mut out: Vec<String> = Vec::new();
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name == "hicolor" || name == "default" || !p.join("index.theme").is_file() || out.contains(&name) {
                continue;
            }
            let has_cursors = p.join("cursors").is_dir();
            let has_icons = p.join("apps").exists() || p.join("scalable").exists() || p.join("48x48").exists();
            if (cursors && !has_cursors) || (!cursors && !has_icons) {
                continue;
            }
            out.push(name);
        }
    }
    out.sort_by_key(|s| s.to_lowercase());
    out
}

// ---------------------------------------------------------------- Accessibility

pub fn accessibility(ui: &mut Ui, cx: &mut Cx) {
    hero(ui, cx, Page::Accessibility, "Personalise Orchard in ways that work best for you.");
    header(ui, "Display");
    group(ui, |g| {
        let mut m = cx.s.appearance.reduce_motion;
        g.row_sub("Reduce motion", "Replace the genie, spring and sliding animations with simple fades", |ui| {
            if toggle(ui, &mut m).changed() {
                cx.s.appearance.reduce_motion = m;
            }
        });
        let mut t = cx.s.appearance.reduce_transparency;
        g.row_sub("Reduce transparency", "Use solid backgrounds instead of frosted glass", |ui| {
            if toggle(ui, &mut t).changed() {
                cx.s.appearance.reduce_transparency = t;
            }
        });
    });
    header(ui, "Pointer Control");
    group(ui, |g| {
        let mut f = cx.s.pointer.scroll_factor;
        g.row("Scrolling speed", |ui| {
            if slider(ui, &mut f, 0.25..=3.0, sl(220.0, 0, false, "Slow", "Fast")).changed() {
                cx.s.pointer.scroll_factor = (f * 20.0).round() / 20.0;
            }
        });
        let mut ffm = cx.s.windows.focus_follows_mouse;
        g.row_sub("Focus follows pointer", "Typing goes to the window under the pointer, without raising it", |ui| {
            if toggle(ui, &mut ffm).changed() {
                cx.s.windows.focus_follows_mouse = ffm;
            }
        });
    });
}

// ---------------------------------------------------------------- Menu bar

pub fn menu_bar(ui: &mut Ui, cx: &mut Cx) {
    hero(ui, cx, Page::MenuBar, "Choose what the menu bar shows on the right.");
    header(ui, "Control Center Modules");
    group(ui, |g| {
        let mut net = cx.s.menu_bar.show_network;
        g.row_with(
            "Network",
            RowOpts {
                sub: Some("Show Wi-Fi or Ethernet in Menu Bar"),
                icon: Some(Page::Wifi.icon()),
                ..Default::default()
            },
            Some(cx.icons),
            |ui| {
                if toggle(ui, &mut net).changed() {
                    cx.s.menu_bar.show_network = net;
                }
            },
        );
        let mut vol = cx.s.menu_bar.show_volume;
        g.row_with(
            "Sound",
            RowOpts {
                sub: Some("Show Volume in Menu Bar"),
                icon: Some(Page::Sound.icon()),
                ..Default::default()
            },
            Some(cx.icons),
            |ui| {
                if toggle(ui, &mut vol).changed() {
                    cx.s.menu_bar.show_volume = vol;
                }
            },
        );
        let mut bat = cx.s.menu_bar.show_battery;
        g.row_with(
            "Battery",
            RowOpts {
                sub: Some("Show Battery in Menu Bar"),
                icon: Some(Page::Battery.icon()),
                ..Default::default()
            },
            Some(cx.icons),
            |ui| {
                if toggle(ui, &mut bat).changed() {
                    cx.s.menu_bar.show_battery = bat;
                }
            },
        );
    });
    header(ui, "Clock");
    group(ui, |g| {
        super::general::clock_format_row(g, cx.s);
        let mb = &mut cx.s.menu_bar;
        let mut d = mb.show_date;
        g.row("Show date", |ui| {
            if toggle(ui, &mut d).changed() {
                mb.show_date = d;
            }
        });
        let mut w = mb.show_weekday;
        g.row("Show the day of the week", |ui| {
            if toggle(ui, &mut w).changed() {
                mb.show_weekday = w;
            }
        });
        let mut s = mb.show_seconds;
        g.row("Display the time with seconds", |ui| {
            if toggle(ui, &mut s).changed() {
                mb.show_seconds = s;
            }
        });
    });
    header(ui, "Battery");
    group(ui, |g| {
        let mut b = cx.s.menu_bar.battery_percentage;
        g.row("Show percentage", |ui| {
            if toggle(ui, &mut b).changed() {
                cx.s.menu_bar.battery_percentage = b;
            }
        });
    });
}

// ---------------------------------------------------------------- Desktop & Dock

fn corner_sheet(ctx: &egui::Context, cx: &mut Cx) {
    let mut open = cx.st.desktop.corners_open;
    if !open {
        return;
    }
    let opts: Vec<(CornerAction, String)> = CornerAction::ALL.iter().map(|a| (*a, a.label().to_string())).collect();
    let p = pal();
    let modal = egui::Modal::new(egui::Id::new("hot-corners"))
        .frame(egui::Frame::popup(&ctx.style()).fill(p.content).inner_margin(20.0).corner_radius(12))
        .show(ctx, |ui| {
            ui.set_width(460.0);
            ui.label(egui::RichText::new("Hot Corners").font(bold(15.0)));
            secondary(ui, "Move the pointer into a corner of the screen to start an action.");
            ui.add_space(14.0);
            let hc = &mut cx.s.desktop.hot_corners;
            let row = |ui: &mut Ui, l: &mut CornerAction, r: &mut CornerAction, id: &str| {
                ui.horizontal(|ui| {
                    popup(ui, format!("{id}-l"), l, &opts);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        popup(ui, format!("{id}-r"), r, &opts);
                    });
                });
            };
            row(ui, &mut hc.top_left, &mut hc.top_right, "top");
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 120.0), Sense::hover());
            let screen = Rect::from_center_size(r.center(), vec2(190.0, 116.0));
            ui.painter().rect_filled(screen, 8, Color32::from_rgb(0x2C, 0x2C, 0x2E));
            mini_desktop(ui, screen.shrink(5.0), Some(p.dark));
            row(ui, &mut hc.bottom_left, &mut hc.bottom_right, "bottom");
            ui.add_space(16.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if primary_button(ui, "Done").clicked() {
                    open = false;
                }
            });
        });
    if modal.should_close() {
        open = false;
    }
    cx.st.desktop.corners_open = open;
}

pub fn desktop_dock(ui: &mut Ui, cx: &mut Cx) {
    let ctx = ui.ctx().clone();
    header(ui, "Dock");
    group(ui, |g| {
        let d = &mut cx.s.dock;
        let mut size = d.icon_size as f64;
        g.row("Size", |ui| {
            if slider(ui, &mut size, 32.0..=96.0, sl(220.0, 0, false, "Small", "Large")).changed() {
                d.icon_size = size.round() as f32;
                d.magnified_size = d.magnified_size.max(d.icon_size);
            }
        });
        let mut mag = d.magnified_size as f64;
        let lo = d.icon_size as f64;
        g.row("Magnification", |ui| {
            ui.add_enabled_ui(d.magnification, |ui| {
                if slider(ui, &mut mag, lo..=128.0, sl(220.0, 0, false, "Small", "Large")).changed() {
                    d.magnified_size = mag.round() as f32;
                }
            });
            toggle(ui, &mut d.magnification);
        });
        let mut pos = d.position;
        g.row("Position on screen", |ui| {
            if popup(
                ui,
                "dock-pos",
                &mut pos,
                &[
                    (DockPosition::Left, "Left".into()),
                    (DockPosition::Bottom, "Bottom".into()),
                    (DockPosition::Right, "Right".into()),
                ],
            ) {
                d.position = pos;
            }
        });
        let mut eff = d.minimize_effect;
        g.row("Minimise windows using", |ui| {
            if popup(
                ui,
                "min-effect",
                &mut eff,
                &[(MinimizeEffect::Genie, "Genie Effect".into()), (MinimizeEffect::Scale, "Scale Effect".into())],
            ) {
                d.minimize_effect = eff;
            }
        });
        let mut hide = d.autohide;
        g.row("Hide the Dock", |ui| {
            if popup(
                ui,
                "dock-hide",
                &mut hide,
                &[
                    (DockAutohide::Never, "Never".into()),
                    (DockAutohide::AvoidWindows, "Avoid windows".into()),
                    (DockAutohide::Always, "Always".into()),
                ],
            ) {
                d.autohide = hide;
            }
        });
        g.row("Animate opening applications", |ui| {
            toggle(ui, &mut d.animate_launch);
        });
        g.row("Show indicators for open applications", |ui| {
            toggle(ui, &mut d.show_indicators);
        });
    });

    header(ui, "Windows");
    group(ui, |g| {
        let w = &mut cx.s.windows;
        let mut dc = w.double_click_titlebar;
        g.row("Double-click a window's title bar to", |ui| {
            if popup(
                ui,
                "dbl",
                &mut dc,
                &[
                    (TitlebarDoubleClick::Zoom, "Zoom".into()),
                    (TitlebarDoubleClick::Minimize, "Minimise".into()),
                    (TitlebarDoubleClick::Nothing, "Do Nothing".into()),
                ],
            ) {
                w.double_click_titlebar = dc;
            }
        });
        g.row_sub("Focus follows pointer", "Keyboard focus moves to the window under the pointer", |ui| {
            toggle(ui, &mut w.focus_follows_mouse);
        });
    });

    header(ui, "Mission Control");
    group(ui, |g| {
        let dsk = &mut cx.s.desktop;
        g.row_sub(
            "Trackpad gestures",
            "Three-finger swipes switch Spaces and open Mission Control",
            |ui| {
                toggle(ui, &mut dsk.gestures);
            },
        );
        g.row_sub(
            "When switching to an application, switch to a Space with its windows",
            "",
            |ui| {
                toggle(ui, &mut dsk.switch_space_on_activate);
            },
        );
    });
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        if button(ui, "Hot Corners…").clicked() {
            cx.st.desktop.corners_open = true;
        }
    });
    corner_sheet(&ctx, cx);
}

// ---------------------------------------------------------------- Displays

#[derive(Clone)]
struct Output {
    name: String,
    w: i32,
    h: i32,
    scale: f64,
    /// mHz
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
    let Ok(v) = orchard_config::ipc_request(&serde_json::json!({"request": "outputs"})) else { return Vec::new() };
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

pub fn displays(ui: &mut Ui, cx: &mut Cx) {
    let ctx = ui.ctx().clone();
    cx.st.desktop.outputs.every(&ctx, 3.0, load_outputs);
    let Some(outputs) = cx.st.desktop.outputs.value.clone() else {
        ui.spinner();
        return;
    };
    if outputs.is_empty() {
        group(ui, |g| {
            g.row_sub("No displays reported", "Is Orchard running? Display settings need the compositor.", |_| {});
        });
        return;
    }
    let p = pal();
    // arrangement preview
    group(ui, |g| {
        g.custom(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 150.0), Sense::hover());
            let total_w: f32 = outputs.iter().map(|o| o.w as f32).sum::<f32>() + 20.0 * (outputs.len() as f32 - 1.0);
            let max_h = outputs.iter().map(|o| o.h).max().unwrap_or(1) as f32;
            let k = ((r.width() - 40.0) / total_w).min((r.height() - 30.0) / max_h);
            let mut x = r.center().x - total_w * k / 2.0;
            for o in &outputs {
                let s = vec2(o.w as f32 * k, o.h as f32 * k);
                let rr = Rect::from_min_size(pos2(x, r.center().y - s.y / 2.0 - 8.0), s);
                ui.painter().rect_filled(rr, 6, Color32::from_rgb(0x2C, 0x2C, 0x2E));
                mini_desktop(ui, rr.shrink(4.0), Some(p.dark));
                if o.primary && outputs.len() > 1 {
                    // the main display carries the menu bar
                    let inner = rr.shrink(4.0);
                    ui.painter().rect_filled(
                        Rect::from_min_size(inner.min, vec2(inner.width(), 5.0)),
                        egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 },
                        Color32::from_white_alpha(200),
                    );
                }
                ui.painter().text(
                    pos2(rr.center().x, rr.bottom() + 12.0),
                    Align2::CENTER_CENTER,
                    &o.name,
                    egui::FontId::proportional(11.5),
                    p.text2,
                );
                x += s.x + 20.0 * k;
            }
        });
    });
    for o in &outputs {
        header(ui, &o.name);
        group(ui, |g| {
            let cur = cx.s.displays.scale.get(&o.name).copied().unwrap_or(o.scale);
            let mut v = (cur * 100.0).round() as i32;
            let opts: Vec<(i32, String)> = [100, 125, 150, 175, 200, 250, 300]
                .iter()
                .map(|s| {
                    let lw = (o.w as f64 * o.scale / (*s as f64 / 100.0)).round();
                    let lh = (o.h as f64 * o.scale / (*s as f64 / 100.0)).round();
                    (*s, format!("{s}%  — looks like {lw} × {lh}"))
                })
                .collect();
            g.row_sub("Scale", "Larger text and controls on high-resolution displays", |ui| {
                if popup(ui, ("scale", &o.name), &mut v, &opts) {
                    cx.s.displays.scale.insert(o.name.clone(), v as f64 / 100.0);
                    cx.st.desktop.outputs.invalidate();
                }
            });
            let (pw, ph) = if o.mode_w > 0 {
                (o.mode_w as f64, o.mode_h as f64)
            } else {
                ((o.w as f64 * o.scale).round(), (o.h as f64 * o.scale).round())
            };
            g.row("Resolution", |ui| {
                secondary(ui, format!("{pw} × {ph}"));
            });
            g.row_sub("Refresh rate", "Higher rates make motion smoother", |ui| {
                if o.refresh_rates.len() > 1 {
                    // show the saved choice while the compositor catches up
                    let saved = cx.s.displays.refresh.get(&o.name).copied();
                    let mut v = saved
                        .and_then(|want| o.refresh_rates.iter().copied().min_by_key(|r| (r - want).abs()))
                        .unwrap_or(o.refresh);
                    let opts: Vec<(i32, String)> = o.refresh_rates.iter().map(|r| (*r, hz(*r))).collect();
                    if popup(ui, ("refresh", &o.name), &mut v, &opts) {
                        cx.s.displays.refresh.insert(o.name.clone(), v);
                        cx.st.desktop.outputs.invalidate();
                    }
                } else if o.refresh > 0 {
                    secondary(ui, hz(o.refresh));
                } else {
                    secondary(ui, "—");
                }
            });
            if outputs.len() > 1 {
                g.row_sub("Main display", "Shows the menu bar and Dock", |ui| {
                    let mut on = o.primary;
                    if toggle(ui, &mut on).changed() {
                        if on {
                            cx.s.displays.primary = Some(o.name.clone());
                        } else {
                            // hand the role to the next display
                            cx.s.displays.primary =
                                outputs.iter().find(|x| x.name != o.name).map(|x| x.name.clone());
                        }
                        cx.st.desktop.outputs.invalidate();
                    }
                });
            }
            g.row("Workspace size", |ui| {
                secondary(ui, format!("{} × {} points", o.w, o.h));
            });
        });
    }
    footnote(ui, "Orchard uses each display's preferred resolution.");
}

// ---------------------------------------------------------------- Wallpaper

const WALLPAPER_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp"];

fn is_wallpaper_file(p: &Path) -> bool {
    p.is_file()
        && p.extension()
            .and_then(|e| e.to_str())
            .map(|e| WALLPAPER_EXTS.contains(&e.to_lowercase().as_str()))
            .unwrap_or(false)
}

enum Thumb {
    Loading,
    Ready(TextureHandle),
    Failed,
}

#[derive(Default)]
struct WallpaperState {
    /// Discovered system / Pictures wallpapers.
    system: Bg<Vec<PathBuf>>,
    /// Async file picker result (selected paths).
    picker: Bg<Result<Vec<PathBuf>, String>>,
    /// Per-path thumbnail cache (shared by preview, added grid, and system grid).
    thumbs: HashMap<PathBuf, Thumb>,
    thumb_tx: Option<mpsc::Sender<PathBuf>>,
    thumb_rx: Option<mpsc::Receiver<(PathBuf, Option<ColorImage>)>>,
    /// Preview texture for Orchard's built-in gradient.
    gradient: Option<TextureHandle>,
    /// Last error from adding a wallpaper.
    err: Option<String>,
}

impl WallpaperState {
    fn ensure_worker(&mut self, ctx: &egui::Context) {
        if self.thumb_tx.is_some() {
            return;
        }
        let (req_tx, req_rx) = mpsc::channel::<PathBuf>();
        let (res_tx, res_rx) = mpsc::channel::<(PathBuf, Option<ColorImage>)>();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            while let Ok(path) = req_rx.recv() {
                let ci = decode_thumb(&path);
                if res_tx.send((path, ci)).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
        self.thumb_tx = Some(req_tx);
        self.thumb_rx = Some(res_rx);
    }

    fn poll_thumbs(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.thumb_rx else { return };
        while let Ok((path, ci)) = rx.try_recv() {
            let state = match ci {
                Some(img) => {
                    let tex = ctx.load_texture(format!("wp:{}", path.display()), img, egui::TextureOptions::LINEAR);
                    Thumb::Ready(tex)
                }
                None => Thumb::Failed,
            };
            self.thumbs.insert(path, state);
        }
    }

    fn thumb(&mut self, ctx: &egui::Context, path: &Path) -> &Thumb {
        if !self.thumbs.contains_key(path) {
            self.ensure_worker(ctx);
            self.thumbs.insert(path.to_path_buf(), Thumb::Loading);
            if let Some(tx) = &self.thumb_tx {
                let _ = tx.send(path.to_path_buf());
            }
        }
        self.thumbs.get(path).unwrap()
    }

    fn gradient_tex(&mut self, ctx: &egui::Context) -> &TextureHandle {
        self.gradient.get_or_insert_with(|| {
            let (w, h) = (160usize, 100usize);
            let mut pixels = Vec::with_capacity(w * h);
            for y in 0..h {
                for x in 0..w {
                    let t = (x as f32 / w as f32 * 0.6 + y as f32 / h as f32 * 0.4).clamp(0.0, 1.0);
                    let a = [64.0, 92.0, 160.0];
                    let b = [168.0, 112.0, 176.0];
                    let c = |i: usize| (a[i] + (b[i] - a[i]) * t) as u8;
                    pixels.push(Color32::from_rgb(c(0), c(1), c(2)));
                }
            }
            ctx.load_texture("wp:gradient", ColorImage::new([w, h], pixels), egui::TextureOptions::LINEAR)
        })
    }

    fn start_picker(&mut self, ctx: &egui::Context) {
        if self.picker.busy() {
            return;
        }
        self.err = None;
        self.picker.value = None;
        self.picker.start(ctx, pick_wallpapers);
    }
}

fn decode_thumb(path: &Path) -> Option<ColorImage> {
    let img = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?;
    let t = img.resize_to_fill(320, 200, image::imageops::FilterType::Triangle).to_rgba8();
    Some(ColorImage::from_rgba_unmultiplied([t.width() as usize, t.height() as usize], &t))
}

fn pick_wallpapers() -> Result<Vec<PathBuf>, String> {
    let start_dir = sys::output("xdg-user-dir", &["PICTURES"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && Path::new(s).is_dir())
        .unwrap_or_else(|| home().to_string_lossy().into_owned());

    let out = if super::network::which("kdialog") {
        sys::output_localized(
            "kdialog",
            &[
                "--getopenfilename",
                &start_dir,
                "Images (*.png *.jpg *.jpeg *.webp)",
                "--multiple",
                "--separate-output",
            ],
        )
    } else if super::network::which("zenity") {
        sys::output_localized(
            "zenity",
            &[
                "--file-selection",
                "--multiple",
                "--separator=\n",
                "--title=Add Wallpaper",
                &format!("--filename={start_dir}/"),
                "--file-filter=Images | *.png *.jpg *.jpeg *.webp",
            ],
        )
    } else {
        return Err("No file picker found. Install kdialog or zenity, or drag and drop image files here.".into());
    };

    let Some(stdout) = out else {
        // User cancelled the dialog.
        return Ok(Vec::new());
    };
    let paths: Vec<PathBuf> = stdout
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    Ok(paths)
}

fn wallpaper_name(path: &Path) -> String {
    // KDE wallpaper packages: <dir>/<Name>/contents/images/<WxH>.<ext>
    if let Some(parent) = path.parent() {
        if parent.file_name().and_then(|f| f.to_str()) == Some("images") {
            if let Some(contents) = parent.parent() {
                if contents.file_name().and_then(|f| f.to_str()) == Some("contents") {
                    if let Some(pkg) = contents.parent().and_then(|p| p.file_name()) {
                        return pkg.to_string_lossy().into_owned();
                    }
                }
            }
        }
    }
    path.file_stem()
        .or_else(|| path.file_name())
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn wallpaper_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |p: PathBuf| {
        if seen.insert(p.clone()) {
            out.push(p);
        }
    };
    // wallpaper packages: <dir>/<Name>/contents/images/<WxH>.<ext>
    for base in [PathBuf::from("/usr/share/wallpapers"), home().join(".local/share/wallpapers")] {
        let Ok(rd) = std::fs::read_dir(&base) else { continue };
        let mut pkgs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        pkgs.sort();
        for pkg in pkgs {
            if is_wallpaper_file(&pkg) {
                push(pkg);
                continue;
            }
            let imgs = pkg.join("contents/images");
            let Ok(rd) = std::fs::read_dir(&imgs) else { continue };
            let best = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| is_wallpaper_file(p))
                .max_by_key(|p| {
                    p.file_stem()
                        .and_then(|s| s.to_str())
                        .and_then(|s| s.split_once('x'))
                        .and_then(|(w, h)| Some(w.parse::<u64>().ok()? * h.parse::<u64>().ok()?))
                        .unwrap_or(0)
                });
            if let Some(b) = best {
                push(b);
            }
        }
    }
    let pictures = sys::output("xdg-user-dir", &["PICTURES"])
        .map(|s| PathBuf::from(s.trim()))
        .unwrap_or_else(|| home().join("Pictures"));
    for dir in [PathBuf::from("/usr/share/backgrounds"), pictures.join("Wallpapers"), pictures] {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut v: Vec<PathBuf> = Vec::new();
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if let Ok(sub) = std::fs::read_dir(&p) {
                    v.extend(sub.flatten().map(|e| e.path()).filter(|p| is_wallpaper_file(p)));
                }
            } else if is_wallpaper_file(&p) {
                v.push(p);
            }
        }
        v.sort();
        for p in v {
            push(p);
        }
    }
    out.truncate(60);
    out
}

fn add_wallpapers(cx: &mut Cx, paths: Vec<PathBuf>) {
    let mut bad = Vec::new();
    let mut last_added: Option<String> = None;
    for path in paths {
        let valid = image::ImageReader::open(&path)
            .ok()
            .and_then(|r| r.with_guessed_format().ok())
            .and_then(|r| r.into_dimensions().ok())
            .is_some();
        if !valid {
            bad.push(wallpaper_name(&path));
            continue;
        }
        let s = path.to_string_lossy().into_owned();
        if !cx.s.desktop.wallpapers.contains(&s) {
            cx.s.desktop.wallpapers.push(s.clone());
        }
        // Clear any stale failed thumbnail state so it loads fresh.
        cx.st.desktop.wallpaper.thumbs.remove(&path);
        last_added = Some(s);
    }
    if let Some(sel) = last_added {
        cx.s.desktop.wallpaper = Some(sel);
    }
    cx.st.desktop.wallpaper.err = if bad.is_empty() {
        None
    } else {
        Some(format!("Could not read as an image: {}", bad.join(", ")))
    };
}

fn paint_rounded_image(ui: &Ui, tex: &TextureHandle, r: Rect, radius: f32) {
    egui::Image::from_texture((tex.id(), r.size()))
        .corner_radius(CornerRadius::same(radius.round() as u8))
        .paint_at(ui, r);
}

enum TileAction {
    None,
    Select,
    Remove,
}

fn wallpaper_tile(
    ui: &mut Ui,
    ctx: &egui::Context,
    ws: &mut WallpaperState,
    path: &Path,
    size: egui::Vec2,
    selected: bool,
    removable: bool,
) -> TileAction {
    let p = pal();
    let (r, resp) = ui.allocate_exact_size(size, Sense::click());
    let hovered = resp.hovered() || ui.rect_contains_pointer(r);

    match ws.thumb(ctx, path) {
        Thumb::Ready(tex) => {
            paint_rounded_image(ui, tex, r, 6.0);
        }
        Thumb::Loading => {
            ui.painter().rect_filled(r, 6, p.group);
            ui.painter().rect_stroke(r, 6, Stroke::new(1.0_f32, p.group_border), StrokeKind::Inside);
            let spin_r = Rect::from_center_size(r.center(), vec2(16.0, 16.0));
            ui.put(spin_r, egui::Spinner::new().size(14.0));
        }
        Thumb::Failed => {
            ui.painter().rect_filled(r, 6, p.group);
            ui.painter().rect_stroke(r, 6, Stroke::new(1.0_f32, p.group_border), StrokeKind::Inside);
            let label = if path.is_file() { "Unsupported" } else { "Missing" };
            ui.painter().text(
                r.center(),
                Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(11.0),
                p.text2,
            );
        }
    }

    if selected {
        ui.painter().rect_stroke(r.expand(2.0), 8, Stroke::new(3.0_f32, p.accent), StrokeKind::Middle);
    } else if hovered {
        ui.painter().rect_stroke(r, 6, Stroke::new(1.5_f32, p.text3), StrokeKind::Inside);
    } else {
        ui.painter().rect_stroke(r, 6, Stroke::new(0.8_f32, p.group_border), StrokeKind::Inside);
    }

    let mut action = TileAction::None;
    if removable && hovered {
        let btn = Rect::from_center_size(pos2(r.right() - 12.0, r.top() + 12.0), vec2(18.0, 18.0));
        let bresp = ui.interact(btn, ui.id().with(("rm-wp", path)), Sense::click());
        let bg = if bresp.hovered() {
            Color32::from_rgba_unmultiplied(0xFF, 0x45, 0x3A, 235)
        } else {
            Color32::from_black_alpha(170)
        };
        ui.painter().circle_filled(btn.center(), 8.5, bg);
        let c = btn.center();
        let x_stroke = Stroke::new(1.5_f32, Color32::WHITE);
        ui.painter().line_segment([pos2(c.x - 3.2, c.y - 3.2), pos2(c.x + 3.2, c.y + 3.2)], x_stroke);
        ui.painter().line_segment([pos2(c.x + 3.2, c.y - 3.2), pos2(c.x - 3.2, c.y + 3.2)], x_stroke);
        if bresp.on_hover_text("Remove wallpaper").clicked() {
            action = TileAction::Remove;
        }
    }

    let tip = if path.is_file() {
        wallpaper_name(path)
    } else {
        format!("{} (file not found)", wallpaper_name(path))
    };
    let resp = resp.on_hover_text(tip);
    if matches!(action, TileAction::None) && resp.clicked() {
        action = TileAction::Select;
    }
    action
}

pub fn wallpaper(ui: &mut Ui, cx: &mut Cx) {
    let ctx = ui.ctx().clone();
    cx.st.desktop.wallpaper.poll_thumbs(&ctx);
    cx.st.desktop.wallpaper.system.every(&ctx, 300.0, wallpaper_candidates);

    // Handle async file picker completion.
    cx.st.desktop.wallpaper.picker.poll();
    if let Some(res) = cx.st.desktop.wallpaper.picker.value.take() {
        match res {
            Ok(paths) if !paths.is_empty() => add_wallpapers(cx, paths),
            Ok(_) => {}
            Err(e) => cx.st.desktop.wallpaper.err = Some(e),
        }
    }

    // Support drag-and-drop of image files onto the window.
    let dropped: Vec<PathBuf> = ctx.input(|i| {
        i.raw
            .dropped_files
            .iter()
            .filter_map(|f| f.path.clone())
            .collect()
    });
    if !dropped.is_empty() {
        add_wallpapers(cx, dropped);
    }

    let p = pal();
    let current = cx.s.desktop.wallpaper.clone();
    let picking = cx.st.desktop.wallpaper.picker.busy();
    let mut open_picker = false;

    // Top preview card
    group(ui, |g| {
        g.custom(|ui| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(160.0, 100.0), Sense::hover());
                let ws = &mut cx.st.desktop.wallpaper;
                match current.as_deref().map(Path::new) {
                    Some(path) => match ws.thumb(&ctx, path) {
                        Thumb::Ready(tex) => paint_rounded_image(ui, tex, r, 6.0),
                        Thumb::Loading => {
                            ui.painter().rect_filled(r, 6, p.content);
                            ui.put(Rect::from_center_size(r.center(), vec2(18.0, 18.0)), egui::Spinner::new().size(16.0));
                        }
                        Thumb::Failed => {
                            let grad = ws.gradient_tex(&ctx).clone();
                            paint_rounded_image(ui, &grad, r, 6.0);
                        }
                    },
                    None => {
                        let grad = ws.gradient_tex(&ctx).clone();
                        paint_rounded_image(ui, &grad, r, 6.0);
                    }
                }
                ui.painter().rect_stroke(r, 6, Stroke::new(1.0_f32, p.group_border), StrokeKind::Outside);
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.add_space(8.0);
                    let name = match &current {
                        Some(c) => wallpaper_name(Path::new(c)),
                        None => "Orchard Gradient".into(),
                    };
                    ui.label(egui::RichText::new(name).font(bold(13.5)));
                    secondary(ui, "Orchard only. Other desktops keep their own wallpaper.");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let label = if picking { "Choosing…" } else { "Add Wallpaper…" };
                        if button_enabled(ui, label, !picking).clicked() {
                            open_picker = true;
                        }
                        if current.is_some() && button(ui, "Use Default").clicked() {
                            cx.s.desktop.wallpaper = None;
                        }
                    });
                });
            });
        });
    });

    super::error_line(ui, cx.st.desktop.wallpaper.err.clone());

    let cols = 4usize;
    let gap = 10.0;
    let w = (ui.available_width() - gap * (cols as f32 - 1.0)) / cols as f32;
    let h = w * 0.625;
    let tile_size = vec2(w, h);

    // Your Wallpapers (user-added grid + "+ Add Wallpaper" tile)
    header(ui, "Your Wallpapers");
    ui.add_space(4.0);
    let added = cx.s.desktop.wallpapers.clone();
    let total_added_cells = added.len() + 1; // +1 for the Add tile
    let mut remove_path: Option<String> = None;
    let mut select_path: Option<Option<String>> = None;

    for row_start in (0..total_added_cells).step_by(cols) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for idx in row_start..(row_start + cols).min(total_added_cells) {
                if idx < added.len() {
                    let path_s = &added[idx];
                    let selected = current.as_deref() == Some(path_s.as_str());
                    match wallpaper_tile(ui, &ctx, &mut cx.st.desktop.wallpaper, Path::new(path_s), tile_size, selected, true) {
                        TileAction::Select => select_path = Some(Some(path_s.clone())),
                        TileAction::Remove => remove_path = Some(path_s.clone()),
                        TileAction::None => {}
                    }
                } else {
                    // "+ Add Wallpaper" tile
                    let sense = if picking { Sense::hover() } else { Sense::click() };
                    let (r, resp) = ui.allocate_exact_size(tile_size, sense);
                    let hovered = !picking && resp.hovered();
                    ui.painter().rect_filled(r, 6, if hovered { p.hover } else { p.group });
                    ui.painter().rect_stroke(
                        r,
                        6,
                        Stroke::new(1.0_f32, if hovered { p.accent } else { p.group_border }),
                        StrokeKind::Inside,
                    );
                    let c = pos2(r.center().x, r.center().y - 8.0);
                    let plus_col = if picking { p.text3 } else if hovered { p.accent } else { p.text2 };
                    let s = Stroke::new(1.8_f32, plus_col);
                    ui.painter().line_segment([pos2(c.x - 7.0, c.y), pos2(c.x + 7.0, c.y)], s);
                    ui.painter().line_segment([pos2(c.x, c.y - 7.0), pos2(c.x, c.y + 7.0)], s);
                    ui.painter().text(
                        pos2(r.center().x, r.center().y + 13.0),
                        Align2::CENTER_CENTER,
                        if picking { "Choosing…" } else { "Add Wallpaper" },
                        egui::FontId::proportional(11.5),
                        plus_col,
                    );
                    if resp.on_hover_text("Add image files (or drag and drop)").clicked() {
                        open_picker = true;
                    }
                }
            }
        });
        ui.add_space(gap - 4.0);
    }

    if let Some(rm) = remove_path {
        cx.s.desktop.wallpapers.retain(|p| p != &rm);
        if cx.s.desktop.wallpaper.as_deref() == Some(rm.as_str()) {
            cx.s.desktop.wallpaper = None;
        }
    }
    if let Some(sel) = select_path {
        cx.s.desktop.wallpaper = sel;
    }
    if open_picker {
        cx.st.desktop.wallpaper.start_picker(&ctx);
    }

    ui.add_space(6.0);

    // Built-in & Pictures grid
    header(ui, "Built-in & Pictures");
    ui.add_space(4.0);
    let added_set: HashSet<&str> = cx.s.desktop.wallpapers.iter().map(String::as_str).collect();
    let system_paths: Vec<PathBuf> = cx
        .st
        .desktop
        .wallpaper
        .system
        .value
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter(|p| !added_set.contains(p.to_string_lossy().as_ref()))
        .cloned()
        .collect();

    // Cell 0 is always the built-in Orchard Gradient; subsequent cells are discovered pictures.
    let total_sys_cells = 1 + system_paths.len();
    let current = cx.s.desktop.wallpaper.clone();
    for row_start in (0..total_sys_cells).step_by(cols) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for idx in row_start..(row_start + cols).min(total_sys_cells) {
                if idx == 0 {
                    let (r, resp) = ui.allocate_exact_size(tile_size, Sense::click());
                    let grad = cx.st.desktop.wallpaper.gradient_tex(&ctx).clone();
                    paint_rounded_image(ui, &grad, r, 6.0);
                    let selected = current.is_none();
                    if selected {
                        ui.painter().rect_stroke(r.expand(2.0), 8, Stroke::new(3.0_f32, p.accent), StrokeKind::Middle);
                    } else if resp.hovered() {
                        ui.painter().rect_stroke(r, 6, Stroke::new(1.5_f32, p.text3), StrokeKind::Inside);
                    } else {
                        ui.painter().rect_stroke(r, 6, Stroke::new(0.8_f32, p.group_border), StrokeKind::Inside);
                    }
                    if resp.on_hover_text("Orchard Gradient").clicked() {
                        cx.s.desktop.wallpaper = None;
                    }
                } else {
                    let path = &system_paths[idx - 1];
                    let path_s = path.to_string_lossy().into_owned();
                    let selected = current.as_deref() == Some(path_s.as_str());
                    if matches!(
                        wallpaper_tile(ui, &ctx, &mut cx.st.desktop.wallpaper, path, tile_size, selected, false),
                        TileAction::Select
                    ) {
                        cx.s.desktop.wallpaper = Some(path_s);
                    }
                }
            }
        });
        ui.add_space(gap - 4.0);
    }
    if cx.st.desktop.wallpaper.system.busy() && system_paths.is_empty() {
        ui.spinner();
    }
}

// ---------------------------------------------------------------- Lock screen

pub fn lock_screen(ui: &mut Ui, cx: &mut Cx) {
    hero(ui, cx, Page::LockScreen, "Lock your screen when you step away, so only you can unlock it.");
    group(ui, |g| {
        let mut m = cx.s.lock.lock_after_minutes;
        let opts: Vec<(u32, String)> = [0u32, 1, 2, 5, 10, 15, 20, 30, 60]
            .iter()
            .map(|m| (*m, if *m == 0 { "Never".to_string() } else if *m == 60 { "1 hour".to_string() } else { format!("{m} minutes") }))
            .map(|(m, s)| (m, if m == 1 { "1 minute".to_string() } else { s }))
            .collect();
        g.row("Lock screen when inactive for", |ui| {
            if popup(ui, "lock-after", &mut m, &opts) {
                cx.s.lock.lock_after_minutes = m;
            }
        });
        g.row_sub("Lock command", "Runs on ⌃⌘Q, hot corners and after inactivity", |ui| {
            text_field(ui, &mut cx.s.lock.locker, 220.0, "swaylock -f");
        });
    });
    let bin = cx.s.lock.locker.split_whitespace().next().unwrap_or_default().to_string();
    if !bin.is_empty() && !super::network::which(&bin) {
        super::error_line(ui, Some(format!("“{bin}” is not installed. Install it with: sudo dnf install {bin}")));
    }
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        if button(ui, "Lock Screen Now").clicked() {
            let r = orchard_config::ipc_request(&serde_json::json!({"request": "action", "name": "lock"}));
            cx.st.desktop.lock_msg = match r {
                Ok(v) if v["event"] == "ok" => None,
                Ok(v) => Some(v["message"].as_str().unwrap_or("Orchard refused").to_string()),
                Err(e) => Some(e.to_string()),
            };
        }
    });
    super::error_line(ui, cx.st.desktop.lock_msg.clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wallpaper_name_handles_files_and_kde_packages() {
        assert_eq!(wallpaper_name(Path::new("/home/user/Pictures/sunset.jpg")), "sunset");
        assert_eq!(
            wallpaper_name(Path::new("/usr/share/wallpapers/Next/contents/images/3840x2160.png")),
            "Next"
        );
        assert_eq!(
            wallpaper_name(Path::new("/home/user/Downloads/my.cool.photo.webp")),
            "my.cool.photo"
        );
    }
}
