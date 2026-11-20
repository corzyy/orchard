//! Appearance (theme, accent, corners, transparency, motion, icon/cursor themes)
//! and Wallpaper (preview, add/remove, system & Pictures grid).

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

use orchard_config::{Accent, Settings, ThemeMode};

use crate::{
    sys::{self, Bg, Wake},
    theme::{mix, rgb, with_alpha, Weight},
    ui::{hash_pair, Rect, Ui},
};

#[derive(Clone, Default)]
struct ThemesInfo {
    icons: Vec<String>,
    cursors: Vec<String>,
}

enum Thumb {
    Loading,
    Ready { w: u32, h: u32, rgba: Vec<u8> },
    Failed,
}

#[derive(Default)]
pub struct AppearanceState {
    themes: Bg<ThemesInfo>,
    system_wp: Bg<Vec<PathBuf>>,
    picker: Bg<Result<Vec<PathBuf>, String>>,
    thumbs: HashMap<PathBuf, Thumb>,
    thumb_tx: Option<mpsc::Sender<PathBuf>>,
    thumb_rx: Option<mpsc::Receiver<(PathBuf, Option<(u32, u32, Vec<u8>)>)>>,
    gradient: Option<(u32, u32, Vec<u8>)>,
    wp_err: Option<String>,
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

fn installed_themes(cursors: bool) -> Vec<String> {
    let mut dirs = vec![home().join(".local/share/icons"), home().join(".icons")];
    let data = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(data.split(':').filter(|d| !d.is_empty()).map(|d| Path::new(d).join("icons")));
    let mut out = Vec::new();
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

fn load_themes() -> ThemesInfo {
    ThemesInfo {
        icons: installed_themes(false),
        cursors: installed_themes(true),
    }
}

pub fn draw_mini_desktop(ui: &mut Ui, r: Rect, dark: Option<bool>) {
    ui.fill_rect(r, 6.0, rgb([0x4A, 0x7B, 0xD8]));
    ui.fill_rect(
        Rect::new(r.x, r.cy(), r.w, r.h * 0.5),
        4.0,
        rgb([0x8A, 0x5C, 0xC8]),
    );
    let w = Rect::new(
        r.x + r.w * 0.16,
        r.y + r.h * 0.2,
        r.w * 0.68,
        r.h * 0.62,
    );
    match dark {
        Some(is_dark) => {
            let (body, bar) = if is_dark {
                (rgb([0x2A, 0x2A, 0x2C]), rgb([0x3A, 0x3A, 0x3C]))
            } else {
                ([255, 255, 255, 255], rgb([0xE8, 0xE6, 0xE8]))
            };
            ui.fill_rect(w, 4.0, body);
            ui.fill_rect(Rect::new(w.x, w.y, w.w, 8.0), 3.0, bar);
            let line_c = if is_dark { [255, 255, 255, 42] } else { [0, 0, 0, 30] };
            for i in 0..3 {
                let y = w.y + 13.0 + i as f32 * 5.5;
                ui.fill_rect(Rect::new(w.x + 6.0, y, w.w * (0.7 - i as f32 * 0.15), 2.2), 1.0, line_c);
            }
        }
        None => {
            // Split Light (left) / Dark (right) for Auto
            let half = w.w * 0.5;
            ui.fill_rect(Rect::new(w.x, w.y, half, w.h), 3.0, [255, 255, 255, 255]);
            ui.fill_rect(Rect::new(w.x + half, w.y, half, w.h), 3.0, rgb([0x2A, 0x2A, 0x2C]));
            ui.fill_rect(Rect::new(w.x, w.y, half, 8.0), 2.0, rgb([0xE8, 0xE6, 0xE8]));
            ui.fill_rect(Rect::new(w.x + half, w.y, half, 8.0), 2.0, rgb([0x3A, 0x3A, 0x3C]));
            for i in 0..3 {
                let y = w.y + 13.0 + i as f32 * 5.5;
                ui.fill_rect(Rect::new(w.x + 6.0, y, (half - 8.0).max(4.0), 2.2), 1.0, [0, 0, 0, 30]);
                ui.fill_rect(Rect::new(w.x + half + 3.0, y, (half - 9.0).max(4.0), 2.2), 1.0, [255, 255, 255, 42]);
            }
        }
    }
    for (i, c) in [[0xFF, 0x5F, 0x57], [0xFE, 0xBC, 0x2E], [0x28, 0xC8, 0x40]].iter().enumerate() {
        ui.circle_filled(w.x + 5.0 + i as f32 * 5.0, w.y + 4.0, 1.5, rgb(*c));
    }
}

pub fn appearance(ui: &mut Ui, s: &mut Settings, st: &mut AppearanceState, wake: &Wake) {
    st.themes.ensure(wake, Duration::from_secs(120), load_themes);

    ui.group("app-theme", |ui| {
        let r = ui.custom_row(88.0);
        ui.text("Appearance", r.x + 14.0, r.y + 38.0, 13.0, Weight::Regular, ui.pal.text);

        let tw = 74.0;
        let th = 48.0;
        let gap = 12.0;
        let total = 3.0 * tw + 2.0 * gap;
        let modes = [
            (ThemeMode::Auto, "Auto", None),
            (ThemeMode::Light, "Light", Some(false)),
            (ThemeMode::Dark, "Dark", Some(true)),
        ];
        for (i, (mode, label, dark)) in modes.into_iter().enumerate() {
            let x = r.right() - 14.0 - total + i as f32 * (tw + gap);
            let tr = Rect::new(x, r.y + 12.0, tw, th);
            draw_mini_desktop(ui, tr, dark);
            let sel = s.appearance.mode == mode;
            if sel {
                ui.stroke_rect(
                    Rect::new(tr.x - 2.5, tr.y - 2.5, tr.w + 5.0, tr.h + 5.0),
                    8.0,
                    2.5,
                    ui.pal.accent,
                );
            }
            if ui.clicked(tr) {
                s.appearance.mode = mode;
            }
            ui.text_center(
                label,
                tr.cx(),
                tr.bottom() + 13.0,
                11.5,
                if sel { Weight::Bold } else { Weight::Regular },
                if sel { ui.pal.text } else { ui.pal.text2 },
            );
        }

        // Accent swatches
        let ar = ui.row_base("Accent colour", None, None);
        let spacing = 24.0;
        let total_w = Accent::ALL.len() as f32 * spacing - 8.0;
        let sx = ar.right() - 14.0 - total_w;
        for (i, a) in Accent::ALL.iter().enumerate() {
            let cx = sx + i as f32 * spacing + 8.0;
            let cy = ar.cy();
            let hit = Rect::new(cx - 10.0, cy - 10.0, 20.0, 20.0);
            let c = rgb(a.rgb());
            ui.circle_filled(cx, cy, 7.5, c);
            ui.circle_stroke(cx, cy, 7.5, 0.8, mix(c, [0, 0, 0, 255], 0.22));
            if s.appearance.accent == *a {
                ui.circle_filled(cx, cy, 2.8, [255, 255, 255, 255]);
            }
            if ui.clicked(hit) {
                s.appearance.accent = *a;
            }
        }
    });

    ui.section("Windows & Effects");
    ui.group("app-windows", |ui| {
        let mut cr = s.appearance.corner_radius as f64;
        if ui.row_slider("corner-rad", "Window corners", None, &mut cr, 0.0, 16.0, "Square", "Round") {
            s.appearance.corner_radius = (cr / 2.0).round() as f32 * 2.0;
        }
        ui.row_toggle(
            "Reduce transparency",
            Some("Use solid backgrounds instead of frosted glass"),
            &mut s.appearance.reduce_transparency,
        );
        ui.row_toggle(
            "Reduce motion",
            Some("Replace genie, spring and slide animations with quick fades"),
            &mut s.appearance.reduce_motion,
        );
    });

    let themes = st.themes.value.clone().unwrap_or_default();
    ui.section("Icons & Pointer");
    ui.group("app-themes", |ui| {
        let def_icon = orchard_config::DEFAULT_ICON_THEME;
        let mut icon_opts = vec![(String::new(), format!("{def_icon} (default)"))];
        for t in &themes.icons {
            if t != def_icon {
                icon_opts.push((t.clone(), t.clone()));
            }
        }
        let cur_icon = s.appearance.icon_theme.clone().unwrap_or_default();
        if let Some(v) = ui.row_popup("icon-theme", "Icon theme", None, &cur_icon, &icon_opts) {
            s.appearance.icon_theme = if v.is_empty() { None } else { Some(v) };
        }

        let def_cur = orchard_config::DEFAULT_CURSOR_THEME;
        let mut cur_opts = vec![(String::new(), format!("{def_cur} (default)"))];
        for t in &themes.cursors {
            if t != def_cur {
                cur_opts.push((t.clone(), t.clone()));
            }
        }
        let cur_ptr = s.appearance.cursor_theme.clone().unwrap_or_default();
        if let Some(v) = ui.row_popup("cursor-theme", "Pointer theme", None, &cur_ptr, &cur_opts) {
            s.appearance.cursor_theme = if v.is_empty() { None } else { Some(v) };
        }

        let sz_opts: Vec<(String, String)> = [24u32, 32, 36, 48, 64]
            .iter()
            .map(|n| (n.to_string(), format!("{n} px")))
            .collect();
        let cur_sz = s.appearance.cursor_size().to_string();
        if let Some(v) = ui.row_popup("cursor-size", "Pointer size", None, &cur_sz, &sz_opts) {
            if let Ok(n) = v.parse() {
                s.appearance.cursor_size = n;
            }
        }
    });
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

pub fn wallpaper_name(path: &Path) -> String {
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

fn decode_thumb(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?;
    let t = img.resize_to_fill(280, 175, image::imageops::FilterType::Triangle).to_rgba8();
    Some((t.width(), t.height(), t.into_raw()))
}

fn gradient_rgba() -> (u32, u32, Vec<u8>) {
    let (w, h) = (160u32, 100u32);
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let t = (x as f32 / w as f32 * 0.6 + y as f32 / h as f32 * 0.4).clamp(0.0, 1.0);
            let a = [64.0, 92.0, 160.0];
            let b = [168.0, 112.0, 176.0];
            out.push((a[0] + (b[0] - a[0]) * t) as u8);
            out.push((a[1] + (b[1] - a[1]) * t) as u8);
            out.push((a[2] + (b[2] - a[2]) * t) as u8);
            out.push(255);
        }
    }
    (w, h, out)
}

fn wallpaper_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |p: PathBuf| {
        if seen.insert(p.clone()) {
            out.push(p);
        }
    };
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
        let mut v = Vec::new();
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
    out.truncate(48);
    out
}

fn pick_wallpapers() -> Result<Vec<PathBuf>, String> {
    let start_dir = sys::output("xdg-user-dir", &["PICTURES"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && Path::new(s).is_dir())
        .unwrap_or_else(|| home().to_string_lossy().into_owned());
    let out = if sys::which("kdialog") {
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
    } else if sys::which("zenity") {
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
        return Err("No file picker found (install kdialog or zenity).".into());
    };
    let Some(stdout) = out else {
        return Ok(Vec::new());
    };
    Ok(stdout
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect())
}

impl AppearanceState {
    fn ensure_thumb_worker(&mut self, wake: &Wake) {
        if self.thumb_tx.is_some() {
            return;
        }
        let (req_tx, req_rx) = mpsc::channel::<PathBuf>();
        let (res_tx, res_rx) = mpsc::channel::<(PathBuf, Option<(u32, u32, Vec<u8>)>)>();
        let wake = wake.clone();
        std::thread::spawn(move || {
            while let Ok(path) = req_rx.recv() {
                let decoded = decode_thumb(&path);
                if res_tx.send((path, decoded)).is_err() {
                    break;
                }
                wake.notify();
            }
        });
        self.thumb_tx = Some(req_tx);
        self.thumb_rx = Some(res_rx);
    }

    fn poll_thumbs(&mut self) {
        let Some(rx) = &self.thumb_rx else { return };
        while let Ok((path, decoded)) = rx.try_recv() {
            let st = match decoded {
                Some((w, h, rgba)) => Thumb::Ready { w, h, rgba },
                None => Thumb::Failed,
            };
            self.thumbs.insert(path, st);
        }
    }

    fn request_thumb(&mut self, wake: &Wake, path: &Path) {
        if !self.thumbs.contains_key(path) {
            self.ensure_thumb_worker(wake);
            self.thumbs.insert(path.to_path_buf(), Thumb::Loading);
            if let Some(tx) = &self.thumb_tx {
                let _ = tx.send(path.to_path_buf());
            }
        }
    }

    fn grad(&mut self) -> &(u32, u32, Vec<u8>) {
        self.gradient.get_or_insert_with(gradient_rgba)
    }
}

pub fn wallpaper(ui: &mut Ui, s: &mut Settings, st: &mut AppearanceState, wake: &Wake) {
    st.poll_thumbs();
    st.system_wp.ensure(wake, Duration::from_secs(300), wallpaper_candidates);
    st.picker.poll();
    if let Some(res) = st.picker.value.take() {
        match res {
            Ok(paths) => {
                let mut bad = Vec::new();
                for p in paths {
                    let valid = image::ImageReader::open(&p)
                        .ok()
                        .and_then(|r| r.with_guessed_format().ok())
                        .and_then(|r| r.into_dimensions().ok())
                        .is_some();
                    if !valid {
                        bad.push(wallpaper_name(&p));
                        continue;
                    }
                    let ps = p.to_string_lossy().into_owned();
                    if !s.desktop.wallpapers.contains(&ps) {
                        s.desktop.wallpapers.push(ps.clone());
                    }
                    st.thumbs.remove(&p);
                    s.desktop.wallpaper = Some(ps);
                }
                st.wp_err = if bad.is_empty() {
                    None
                } else {
                    Some(format!("Not a readable image: {}", bad.join(", ")))
                };
            }
            Err(e) => st.wp_err = Some(e),
        }
    }

    let current = s.desktop.wallpaper.clone();
    let picking = st.picker.busy();
    let mut open_picker = false;

    if let Some(ref cur) = current {
        st.request_thumb(wake, Path::new(cur));
    }

    // Top preview card
    ui.group("wp-preview", |ui| {
        let r = ui.custom_row(120.0);
        let pr = Rect::new(r.x + 14.0, r.y + 12.0, 154.0, 96.0);
        let drew = current
            .as_deref()
            .and_then(|c| st.thumbs.get(Path::new(c)))
            .map(|t| match t {
                Thumb::Ready { w, h, rgba } => {
                    ui.rounded_image(pr, rgba, *w, *h, 6.0);
                    true
                }
                Thumb::Loading => {
                    ui.fill_rect(pr, 6.0, ui.pal.content);
                    ui.text_center("Loading…", pr.cx(), pr.cy(), 11.0, Weight::Regular, ui.pal.text2);
                    true
                }
                Thumb::Failed => false,
            })
            .unwrap_or(false);
        if !drew {
            let (gw, gh, ref grgba) = *st.grad();
            ui.rounded_image(pr, grgba, gw, gh, 6.0);
        }
        ui.stroke_rect(pr, 6.0, 1.0, ui.pal.group_border);

        let tx = pr.right() + 14.0;
        let name = current
            .as_deref()
            .map(|c| wallpaper_name(Path::new(c)))
            .unwrap_or_else(|| "Orchard Gradient".into());
        ui.text_elided(&name, tx, r.y + 34.0, r.right() - tx - 14.0, 14.0, Weight::Bold, ui.pal.text);
        ui.text("Orchard desktop wallpaper", tx, r.y + 54.0, 11.5, Weight::Regular, ui.pal.text2);

        let b1 = Rect::new(tx, r.y + 74.0, 118.0, 26.0);
        if ui.button_at(b1, if picking { "Choosing…" } else { "Add Wallpaper…" }, false, !picking) {
            open_picker = true;
        }
        if current.is_some() {
            let b2 = Rect::new(b1.right() + 8.0, r.y + 74.0, 96.0, 26.0);
            if ui.button_at(b2, "Use Default", false, true) {
                s.desktop.wallpaper = None;
            }
        }
    });

    ui.error_line(st.wp_err.as_deref());

    let cols = 4usize;
    let gap = 10.0;
    let tile_w = (ui.w - gap * (cols as f32 - 1.0)) / cols as f32;
    let tile_h = (tile_w * 0.625).round();

    // User wallpapers
    ui.section("Your Wallpapers");
    let added = s.desktop.wallpapers.clone();
    let total_added = added.len() + 1;
    let mut remove_path: Option<String> = None;

    for row in (0..total_added).step_by(cols) {
        let y = ui.y;
        for col in 0..cols {
            let idx = row + col;
            if idx >= total_added {
                break;
            }
            let tr = Rect::new(ui.x + col as f32 * (tile_w + gap), y, tile_w, tile_h);
            if idx < added.len() {
                let ps = &added[idx];
                let path = Path::new(ps);
                st.request_thumb(wake, path);
                let sel = current.as_deref() == Some(ps.as_str());
                let hov = ui.hovered(tr);
                match st.thumbs.get(path) {
                    Some(Thumb::Ready { w, h, rgba }) => ui.rounded_image(tr, rgba, *w, *h, 6.0),
                    Some(Thumb::Failed) => {
                        ui.fill_rect(tr, 6.0, ui.pal.group);
                        ui.text_center("Missing", tr.cx(), tr.cy(), 11.0, Weight::Regular, ui.pal.text2);
                    }
                    _ => ui.fill_rect(tr, 6.0, ui.pal.group),
                }
                if sel {
                    ui.stroke_rect(Rect::new(tr.x - 2.0, tr.y - 2.0, tr.w + 4.0, tr.h + 4.0), 8.0, 2.5, ui.pal.accent);
                } else {
                    ui.stroke_rect(tr, 6.0, 1.0, if hov { ui.pal.text3 } else { ui.pal.group_border });
                }

                // Remove button on hover
                let mut removed_click = false;
                if hov {
                    let rm = Rect::new(tr.right() - 22.0, tr.y + 4.0, 18.0, 18.0);
                    let rm_hov = ui.hovered(rm);
                    ui.circle_filled(
                        rm.cx(),
                        rm.cy(),
                        8.5,
                        if rm_hov { ui.pal.danger } else { [0, 0, 0, 175] },
                    );
                    let c = rm.cx();
                    let cy = rm.cy();
                    ui.line(c - 3.0, cy - 3.0, c + 3.0, cy + 3.0, 1.5, [255, 255, 255, 255]);
                    ui.line(c + 3.0, cy - 3.0, c - 3.0, cy + 3.0, 1.5, [255, 255, 255, 255]);
                    if ui.clicked(rm) {
                        remove_path = Some(ps.clone());
                        removed_click = true;
                    }
                }
                if !removed_click && ui.clicked(tr) {
                    s.desktop.wallpaper = Some(ps.clone());
                }
            } else {
                // "+ Add" tile
                let hov = !picking && ui.hovered(tr);
                ui.fill_rect(tr, 6.0, if hov { ui.pal.hover } else { ui.pal.group });
                ui.stroke_rect(tr, 6.0, 1.0, if hov { ui.pal.accent } else { ui.pal.group_border });
                let col_fg = if picking {
                    ui.pal.text3
                } else if hov {
                    ui.pal.accent
                } else {
                    ui.pal.text2
                };
                let cx = tr.cx();
                let cy = tr.cy() - 8.0;
                ui.line(cx - 7.0, cy, cx + 7.0, cy, 1.8, col_fg);
                ui.line(cx, cy - 7.0, cx, cy + 7.0, 1.8, col_fg);
                ui.text_center(
                    if picking { "Choosing…" } else { "Add Wallpaper" },
                    tr.cx(),
                    tr.cy() + 13.0,
                    11.5,
                    Weight::Regular,
                    col_fg,
                );
                if !picking && ui.clicked(tr) {
                    open_picker = true;
                }
            }
        }
        ui.y += tile_h + gap;
    }

    if let Some(rm) = remove_path {
        s.desktop.wallpapers.retain(|p| p != &rm);
        if s.desktop.wallpaper.as_deref() == Some(rm.as_str()) {
            s.desktop.wallpaper = None;
        }
    }
    if open_picker {
        st.wp_err = None;
        st.picker.value = None;
        st.picker.start(wake, pick_wallpapers);
    }

    // Built-in & Pictures
    ui.space(6.0);
    ui.section("Built-in & Pictures");
    let added_set: HashSet<&str> = s.desktop.wallpapers.iter().map(String::as_str).collect();
    let sys_paths: Vec<PathBuf> = st
        .system_wp
        .value
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter(|p| !added_set.contains(p.to_string_lossy().as_ref()))
        .cloned()
        .collect();

    let total_sys = 1 + sys_paths.len();
    let current = s.desktop.wallpaper.clone();
    let _ = hash_pair("wp", "sys");

    for row in (0..total_sys).step_by(cols) {
        let y = ui.y;
        for col in 0..cols {
            let idx = row + col;
            if idx >= total_sys {
                break;
            }
            let tr = Rect::new(ui.x + col as f32 * (tile_w + gap), y, tile_w, tile_h);
            if idx == 0 {
                let (gw, gh, ref grgba) = *st.grad();
                ui.rounded_image(tr, grgba, gw, gh, 6.0);
                let sel = current.is_none();
                if sel {
                    ui.stroke_rect(Rect::new(tr.x - 2.0, tr.y - 2.0, tr.w + 4.0, tr.h + 4.0), 8.0, 2.5, ui.pal.accent);
                } else {
                    ui.stroke_rect(tr, 6.0, 1.0, if ui.hovered(tr) { ui.pal.text3 } else { ui.pal.group_border });
                }
                if ui.clicked(tr) {
                    s.desktop.wallpaper = None;
                }
            } else {
                let path = &sys_paths[idx - 1];
                let ps = path.to_string_lossy().into_owned();
                if ui.visible(tr) {
                    st.request_thumb(wake, path);
                }
                let sel = current.as_deref() == Some(ps.as_str());
                match st.thumbs.get(path) {
                    Some(Thumb::Ready { w, h, rgba }) => ui.rounded_image(tr, rgba, *w, *h, 6.0),
                    Some(Thumb::Failed) => {
                        ui.fill_rect(tr, 6.0, ui.pal.group);
                        ui.text_center("Unsupported", tr.cx(), tr.cy(), 10.5, Weight::Regular, ui.pal.text2);
                    }
                    _ => ui.fill_rect(tr, 6.0, with_alpha(ui.pal.group, 220)),
                }
                if sel {
                    ui.stroke_rect(Rect::new(tr.x - 2.0, tr.y - 2.0, tr.w + 4.0, tr.h + 4.0), 8.0, 2.5, ui.pal.accent);
                } else {
                    ui.stroke_rect(tr, 6.0, 1.0, if ui.hovered(tr) { ui.pal.text3 } else { ui.pal.group_border });
                }
                if ui.clicked(tr) {
                    s.desktop.wallpaper = Some(ps);
                }
            }
        }
        ui.y += tile_h + gap;
    }
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
    }
}
