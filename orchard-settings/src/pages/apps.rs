//! Login Items (XDG autostart) and Default Apps (xdg-mime).

use std::collections::HashMap;

use eframe::egui::Ui;
use orchard_config::desktop::{self, AutostartItem, DesktopEntry};

use super::{error_line, hero, Cx};
use crate::{
    sys::{self, output, Bg, Job},
    widgets::{self, button, group, header, popup, toggle},
    Page,
};

#[derive(Default)]
pub struct AppsState {
    autostart: Bg<Vec<AutostartItem>>,
    apps: Bg<Vec<DesktopEntry>>,
    defaults: Bg<HashMap<String, String>>,
    job: Job,
    adding: bool,
    add_choice: String,
    message: Option<String>,
}

fn set_hidden(item: &AutostartItem, hidden: bool) -> std::io::Result<()> {
    let dir = desktop::user_autostart_dir();
    std::fs::create_dir_all(&dir)?;
    let target = dir.join(&item.entry.id);
    if !hidden && item.in_user_dir && item.in_system_dir {
        // the user file only existed to hide the system one
        let only_hides = item.entry.keys.iter().any(|(k, v)| k == "Hidden" && v == "true");
        if only_hides {
            return std::fs::remove_file(target);
        }
    }
    let src = std::fs::read_to_string(&item.entry.path)?;
    let mut out = String::new();
    let mut in_main = false;
    let mut wrote = false;
    for line in src.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            if in_main && !wrote {
                out.push_str(&format!("Hidden={hidden}\n"));
                wrote = true;
            }
            in_main = l == "[Desktop Entry]";
        } else if in_main && (l.starts_with("Hidden=") || l.starts_with("X-GNOME-Autostart-enabled=")) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !wrote {
        out.push_str(&format!("Hidden={hidden}\n"));
    }
    std::fs::write(target, out)
}

pub fn login_items(ui: &mut Ui, cx: &mut Cx) {
    let ctx = ui.ctx().clone();
    let a = &mut cx.st.apps;
    a.autostart.every(&ctx, 30.0, desktop::autostart_items);
    a.apps.every(&ctx, 600.0, desktop::applications);
    hero(ui, cx, Page::LoginItems, "These items open automatically when you log in to Orchard.");
    let a = &mut cx.st.apps;
    group(ui, |g| {
        let mut on = cx.s.apps.run_autostart;
        g.row_sub("Open login items at login", "Runs entries from ~/.config/autostart and /etc/xdg/autostart", |ui| {
            if toggle(ui, &mut on).changed() {
                cx.s.apps.run_autostart = on;
            }
        });
    });

    let Some(items) = a.autostart.value.clone() else {
        ui.spinner();
        return;
    };
    header(ui, "Open at Login");
    let mut changed = false;
    group(ui, |g| {
        let visible: Vec<&AutostartItem> = items
            .iter()
            .filter(|i| {
                // entries for other desktops that Orchard never runs are just noise
                let mut probe = i.entry.clone();
                probe.keys.retain(|(k, _)| k != "Hidden" && k != "X-GNOME-Autostart-enabled");
                probe.autostarts_in_orchard()
            })
            .collect();
        if visible.is_empty() {
            g.row("No login items", |_| {});
        }
        for item in visible {
            let mut on = !item.entry.hidden();
            let sub = item.entry.command().unwrap_or_default();
            let sub: String = sub.chars().take(70).collect();
            g.row_sub(&item.entry.name(), &sub, |ui| {
                if toggle(ui, &mut on).changed() {
                    if let Err(e) = set_hidden(item, !on) {
                        a.message = Some(e.to_string());
                    }
                    changed = true;
                }
                if item.in_user_dir && !item.in_system_dir && button(ui, "Remove").clicked() {
                    let _ = std::fs::remove_file(&item.entry.path);
                    changed = true;
                }
            });
        }
    });
    if changed {
        a.autostart.invalidate();
    }
    error_line(ui, a.message.take());

    // add an application
    let apps: Vec<(String, String)> = a
        .apps
        .value
        .as_ref()
        .map(|v| v.iter().filter(|e| !e.no_display()).map(|e| (e.id.clone(), e.name())).collect())
        .unwrap_or_default();
    ui.horizontal(|ui| {
        if a.adding {
            if a.add_choice.is_empty() {
                if let Some(first) = apps.first() {
                    a.add_choice = first.0.clone();
                }
            }
            popup(ui, "add-login", &mut a.add_choice, &apps);
            if widgets::primary_button(ui, "Add").clicked() {
                if let Some(e) = a.apps.value.as_ref().and_then(|v| v.iter().find(|e| e.id == a.add_choice)) {
                    let dir = desktop::user_autostart_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    if let Err(err) = std::fs::copy(&e.path, dir.join(&e.id)) {
                        a.message = Some(err.to_string());
                    }
                }
                a.adding = false;
                a.autostart.invalidate();
            }
            if button(ui, "Cancel").clicked() {
                a.adding = false;
            }
        } else if button(ui, "+  Add Application…").clicked() {
            a.adding = true;
        }
    });
    ui.add_space(8.0);
    widgets::footnote(ui, "Changes apply the next time you log in.");
}

// ---------------------------------------------------------------- Default Apps

struct Kind {
    label: &'static str,
    /// first one is queried; all are set
    mimes: &'static [&'static str],
}

const KINDS: &[Kind] = &[
    Kind {
        label: "Web browser",
        mimes: &["x-scheme-handler/http", "x-scheme-handler/https", "text/html", "application/xhtml+xml"],
    },
    Kind {
        label: "Email",
        mimes: &["x-scheme-handler/mailto"],
    },
    Kind {
        label: "File manager",
        mimes: &["inode/directory"],
    },
    Kind {
        label: "Text editor",
        mimes: &["text/plain"],
    },
    Kind {
        label: "Images",
        mimes: &["image/png", "image/jpeg", "image/webp", "image/gif"],
    },
    Kind {
        label: "Video",
        mimes: &["video/mp4", "video/x-matroska", "video/webm"],
    },
    Kind {
        label: "Music",
        mimes: &["audio/mpeg", "audio/flac", "audio/ogg"],
    },
    Kind {
        label: "PDF documents",
        mimes: &["application/pdf"],
    },
];

fn load_defaults() -> HashMap<String, String> {
    KINDS
        .iter()
        .map(|k| {
            let v = output("xdg-mime", &["query", "default", k.mimes[0]]).unwrap_or_default().trim().to_string();
            (k.mimes[0].to_string(), v)
        })
        .collect()
}

pub fn default_apps(ui: &mut Ui, cx: &mut Cx) {
    let ctx = ui.ctx().clone();
    let a = &mut cx.st.apps;
    a.apps.every(&ctx, 600.0, desktop::applications);
    a.defaults.every(&ctx, 30.0, load_defaults);
    if a.job.take_done() {
        a.defaults.invalidate();
    }
    hero(ui, cx, Page::DefaultApps, "Choose which apps open links, files and folders.");
    let a = &mut cx.st.apps;
    let (Some(apps), Some(defaults)) = (a.apps.value.clone(), a.defaults.value.clone()) else {
        ui.spinner();
        return;
    };
    group(ui, |g| {
        for k in KINDS {
            let mut opts: Vec<(String, String)> = apps
                .iter()
                .filter(|e| e.mime_types().iter().any(|m| m == k.mimes[0]))
                .map(|e| (e.id.clone(), e.name()))
                .collect();
            let mut cur = defaults.get(k.mimes[0]).cloned().unwrap_or_default();
            if cur.is_empty() {
                opts.insert(0, (String::new(), "None".into()));
            } else if !opts.iter().any(|o| o.0 == cur) {
                opts.insert(0, (cur.clone(), cur.trim_end_matches(".desktop").to_string()));
            }
            g.row(k.label, |ui| {
                if popup(ui, k.label, &mut cur, &opts) && !cur.is_empty() {
                    let id = cur.clone();
                    let mimes = k.mimes;
                    a.job.start(ui.ctx(), move || {
                        let mut args = vec!["default", id.as_str()];
                        args.extend_from_slice(mimes);
                        let r = sys::run("xdg-mime", &args);
                        if mimes[0] == "x-scheme-handler/http" {
                            let _ = sys::run("xdg-settings", &["set", "default-web-browser", &id]);
                        }
                        r
                    });
                }
            });
        }
    });
    error_line(ui, a.job.error());

    header(ui, "Orchard");
    group(ui, |g| {
        let terms: Vec<(String, String)> = apps
            .iter()
            .filter(|e| e.categories().iter().any(|c| c == "TerminalEmulator"))
            .filter_map(|e| {
                let cmd = e.command()?;
                Some((cmd.split_whitespace().next()?.to_string(), e.name()))
            })
            .collect();
        let mut t = cx.s.apps.terminal.clone();
        let mut opts = terms;
        if !opts.iter().any(|o| o.0 == t) {
            opts.insert(0, (t.clone(), t.clone()));
        }
        g.row_sub("Terminal", "Opened with ⌘Return", |ui| {
            if popup(ui, "terminal", &mut t, &opts) {
                cx.s.apps.terminal = t.clone();
            }
        });
    });
}
