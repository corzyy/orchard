//! Appearance (theme, accent, corners, icon theme), Accessibility (motion,
//! transparency, pointer) and Wallpaper (preview, add/remove, system &
//! Pictures grid).

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

use iced::alignment::Horizontal;
use iced::widget::image::Handle;
use iced::widget::{button, column, container, hover, row, text, Image, Space};
use iced::{Alignment, Color, ContentFit, Length, Theme};
use orchard_config::{Accent, Settings, ThemeMode};
use orchard_ui::icons::{self, Symbol};
use orchard_ui::palette::{from_rgb, mix};
use orchard_ui::text::truncate;
use orchard_ui::{choices, controls, fonts, form, images, layout, palette, style, Palette};

use crate::{
    pages::{act, art, edit, some, El},
    sys::{self, Bg, Wake},
    Message,
};

/// Height of a wallpaper tile in the grids.
const TILE_H: f32 = 88.0;
/// Displayed corner radius of wallpaper thumbnails.
const THUMB_RADIUS: f32 = 6.0;
/// Width thumbnails are roughly displayed at (for baking the corner radius).
const THUMB_DISPLAY_W: f32 = 150.0;

#[derive(Clone, Default)]
struct ThemesInfo {
    icons: Vec<String>,
    cursors: Vec<String>,
}

/// A decoded image: a GPU handle with baked rounded corners, plus the raw
/// pixels for accent extraction.
struct Pixels {
    handle: Handle,
    rgba: Vec<u8>,
}

impl Pixels {
    fn new(w: u32, h: u32, rgba: Vec<u8>) -> Self {
        let radius = THUMB_RADIUS * w as f32 / THUMB_DISPLAY_W;
        Self { handle: images::rounded_handle(rgba.clone(), w, h, radius), rgba }
    }
}

enum Thumb {
    Loading,
    Ready(Pixels),
    Failed,
}

type Decoded = Option<(u32, u32, Vec<u8>)>;

#[derive(Default)]
pub struct AppearanceState {
    themes: Bg<ThemesInfo>,
    system_wp: Bg<Vec<PathBuf>>,
    picker: Bg<Result<Vec<PathBuf>, String>>,
    wp_accent: Bg<(Option<String>, [u8; 3])>,
    thumbs: HashMap<PathBuf, Thumb>,
    thumb_tx: Option<mpsc::Sender<PathBuf>>,
    thumb_rx: Option<mpsc::Receiver<(PathBuf, Decoded)>>,
    gradient: Option<Pixels>,
    wp_err: Option<String>,
    /// Wallpaper seen by the last `poll` (`None` before the first one), to
    /// re-derive the wallpaper accent when it changes.
    seen_wallpaper: Option<Option<String>>,
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

// ---------------------------------------------------------------- Appearance

/// A round colour swatch; a white dot marks the selected one.
fn swatch(c: Color, selected: bool) -> El {
    let dot: El = if selected {
        container(Space::new(5.6, 5.6))
            .style(|_| container::Style {
                background: Some(Color::WHITE.into()),
                border: style::no_border(2.8),
                ..Default::default()
            })
            .into()
    } else {
        Space::new(0, 0).into()
    };
    container(dot)
        .center(15)
        .style(move |_| container::Style {
            background: Some(c.into()),
            border: style::border(mix(c, Color::BLACK, 0.22), 0.8, 7.5),
            ..Default::default()
        })
        .into()
}

pub fn appearance(s: &Settings, st: &mut AppearanceState, wake: &Wake) -> El {
    st.themes.ensure(wake, Duration::from_secs(120), load_themes);
    let p = palette();

    let modes = [
        (ThemeMode::Auto, "Auto", None),
        (ThemeMode::Light, "Light", Some(false)),
        (ThemeMode::Dark, "Dark", Some(true)),
    ];
    let mut thumbs = row![].spacing(12);
    for (mode, label, dark) in modes {
        let sel = s.appearance.mode == mode;
        let thumb = container(art::mini_desktop(dark, 74.0, 48.0)).padding(4).style(style::selection_ring(sel));
        let caption = text(label)
            .size(11.5)
            .font(if sel { fonts::bold() } else { fonts::text() })
            .color(if sel { p.text } else { p.text_secondary });
        thumbs = thumbs.push(
            button(column![thumb, caption].spacing(3).align_x(Alignment::Center))
                .padding(0)
                .style(style::bare)
                .on_press(edit(move |s| s.appearance.mode = mode)),
        );
    }
    let mode_row = form::custom_row(
        row![text("Appearance").size(13).color(p.text).width(Length::Fill), thumbs].align_y(Alignment::Center),
    );

    // Accent swatches, a separator, then the "from wallpaper" palette.
    let wp_sel = s.appearance.accent == Accent::Wallpaper;
    let mut accents = row![].spacing(4).align_y(Alignment::Center);
    for a in Accent::ALL {
        accents = accents.push(
            button(swatch(from_rgb(a.rgb()), s.appearance.accent == a))
                .padding(2.5)
                .style(style::bare)
                .on_press(edit(move |s| s.appearance.accent = a)),
        );
    }
    accents = accents.push(container(controls::vline()).height(14).padding([0, 6]));
    accents = accents.push(
        button(art::palette_glyph(wp_sel, from_rgb(s.appearance.accent_rgb())))
            .padding(1)
            .style(style::bare)
            .on_press(act(|s, st, wake| {
                s.appearance.accent = Accent::Wallpaper;
                st.appearance.refresh_accent(s, wake);
            })),
    );
    let accent_row = form::row("Accent colour", wp_sel.then(|| "From wallpaper".to_string()), accents);

    let themes = st.themes.value.clone().unwrap_or_default();
    let def_icon = orchard_config::DEFAULT_ICON_THEME;
    let mut icon_opts = vec![(String::new(), format!("{def_icon} (default)"))];
    icon_opts.extend(themes.icons.iter().filter(|t| *t != def_icon).map(|t| (t.clone(), t.clone())));
    let cur_icon = s.appearance.icon_theme.clone().unwrap_or_default();

    column![
        form::group([mode_row, accent_row]),
        form::section("Windows & Icons"),
        form::group([
            form::slider_row(
                "Window corners",
                None,
                0.0..=16.0,
                s.appearance.corner_radius as f64,
                2.0,
                Some(("Square", "Round")),
                |v| edit(move |s| s.appearance.corner_radius = (v / 2.0).round() as f32 * 2.0),
            ),
            form::popup_row("Icon theme", None, choices(icon_opts), &cur_icon, |v| {
                edit(move |s| s.appearance.icon_theme = (!v.is_empty()).then(|| v.clone()))
            }),
        ]),
    ]
    .into()
}

pub fn accessibility(s: &Settings, st: &mut AppearanceState, wake: &Wake) -> El {
    st.themes.ensure(wake, Duration::from_secs(120), load_themes);
    let a = &s.appearance;

    let themes = st.themes.value.clone().unwrap_or_default();
    let def_cur = orchard_config::DEFAULT_CURSOR_THEME;
    let mut cur_opts = vec![(String::new(), format!("{def_cur} (default)"))];
    cur_opts.extend(themes.cursors.iter().filter(|t| *t != def_cur).map(|t| (t.clone(), t.clone())));
    let cur_ptr = a.cursor_theme.clone().unwrap_or_default();
    let sizes = [24u32, 32, 36, 48, 64].map(|n| (n.to_string(), format!("{n} px")));

    column![
        form::section("Display"),
        form::group([
            form::toggle_row(
                "Reduce motion",
                some("Replace genie, spring and slide animations with quick fades"),
                a.reduce_motion,
                |v| edit(move |s| s.appearance.reduce_motion = v),
            ),
            form::toggle_row(
                "Reduce transparency",
                some("Use solid backgrounds instead of frosted glass"),
                a.reduce_transparency,
                |v| edit(move |s| s.appearance.reduce_transparency = v),
            ),
        ]),
        form::section("Pointer"),
        form::group([
            form::popup_row("Pointer theme", None, choices(cur_opts), &cur_ptr, |v| {
                edit(move |s| s.appearance.cursor_theme = (!v.is_empty()).then(|| v.clone()))
            }),
            form::popup_row("Pointer size", None, choices(sizes), &a.cursor_size().to_string(), |v| {
                edit(move |s| {
                    if let Ok(n) = v.parse() {
                        s.appearance.cursor_size = n;
                    }
                })
            }),
            form::toggle_row(
                "Shake mouse pointer to locate",
                some("Quickly move the mouse back and forth to make the pointer bigger for a moment"),
                s.pointer.shake_to_locate,
                |v| edit(move |s| s.pointer.shake_to_locate = v),
            ),
        ]),
    ]
    .into()
}

// ---------------------------------------------------------------- Wallpaper state

impl AppearanceState {
    /// Re-derives the wallpaper accent for the current wallpaper: immediately
    /// when its pixels are at hand, otherwise on a worker (see [`poll`]).
    fn refresh_accent(&mut self, s: &mut Settings, wake: &Wake) {
        let wallpaper = s.desktop.wallpaper.clone();
        match effective_wallpaper_path(wallpaper.as_deref()) {
            None => s.appearance.wallpaper_accent = Some(extract_accent_from_rgba(&self.grad().rgba)),
            Some(p) => {
                if let Some(Thumb::Ready(px)) = self.thumbs.get(&p) {
                    s.appearance.wallpaper_accent = Some(extract_accent_from_rgba(&px.rgba));
                    return;
                }
                self.wp_accent.value = None;
                self.wp_accent.start(wake, move || {
                    let c = extract_accent_from_wallpaper(wallpaper.as_deref());
                    (wallpaper, c)
                });
            }
        }
    }

    fn ensure_thumb_worker(&mut self, wake: &Wake) {
        if self.thumb_tx.is_some() {
            return;
        }
        let (req_tx, req_rx) = mpsc::channel::<PathBuf>();
        let (res_tx, res_rx) = mpsc::channel::<(PathBuf, Decoded)>();
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
            let t = match decoded {
                Some((w, h, rgba)) => Thumb::Ready(Pixels::new(w, h, rgba)),
                None => Thumb::Failed,
            };
            self.thumbs.insert(path, t);
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

    fn grad(&mut self) -> &Pixels {
        self.gradient.get_or_insert_with(|| {
            let (w, h, rgba) = gradient_rgba();
            Pixels::new(w, h, rgba)
        })
    }

    /// Adds the files the picker returned and selects the last good one.
    fn add_picked(&mut self, s: &mut Settings, res: Result<Vec<PathBuf>, String>) {
        let paths = match res {
            Ok(paths) => paths,
            Err(e) => {
                self.wp_err = Some(e);
                return;
            }
        };
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
            self.thumbs.remove(&p);
            s.desktop.wallpaper = Some(ps);
        }
        self.wp_err = (!bad.is_empty()).then(|| format!("Not a readable image: {}", bad.join(", ")));
    }
}

/// Folds finished background work (thumbnails, picker, wallpaper accent)
/// into the state and settings. Runs on every update.
pub fn poll(s: &mut Settings, st: &mut AppearanceState, wake: &Wake) {
    st.poll_thumbs();

    st.wp_accent.poll();
    if let Some((key, rgb)) = st.wp_accent.value.take() {
        if s.appearance.accent == Accent::Wallpaper && s.desktop.wallpaper == key {
            s.appearance.wallpaper_accent = Some(rgb);
        }
    }

    st.picker.poll();
    if let Some(res) = st.picker.value.take() {
        st.add_picked(s, res);
    }

    let current = s.desktop.wallpaper.clone();
    if st.seen_wallpaper.as_ref() != Some(&current) {
        let changed = st.seen_wallpaper.is_some();
        st.seen_wallpaper = Some(current);
        if changed && s.appearance.accent == Accent::Wallpaper {
            st.refresh_accent(s, wake);
        }
    }
}

// ---------------------------------------------------------------- Wallpaper view

fn open_picker() -> Message {
    act(|_, st, wake| {
        let a = &mut st.appearance;
        a.wp_err = None;
        a.picker.value = None;
        a.picker.start(wake, pick_wallpapers);
    })
}

fn set_wallpaper(v: Option<String>) -> Message {
    edit(move |s| s.desktop.wallpaper = v.clone())
}

fn thumb_image(handle: Handle, h: f32) -> El {
    Image::new(handle).width(Length::Fill).height(h).content_fit(ContentFit::Fill).into()
}

fn placeholder(label: &'static str, h: f32, p: Palette) -> El {
    container(text(label).size(11).color(p.text_secondary))
        .center_x(Length::Fill)
        .center_y(h)
        .style(move |_| container::Style {
            background: Some(p.group.into()),
            border: style::border(p.group_border, 1.0, THUMB_RADIUS),
            ..Default::default()
        })
        .into()
}

fn tile_image(t: Option<&Thumb>, failed: &'static str, p: Palette) -> El {
    match t {
        Some(Thumb::Ready(px)) => thumb_image(px.handle.clone(), TILE_H),
        Some(Thumb::Failed) => placeholder(failed, TILE_H, p),
        _ => placeholder("", TILE_H, p),
    }
}

/// A clickable thumbnail with an accent ring when selected.
fn select_tile(content: El, selected: bool, on_press: Message) -> El {
    button(container(content).padding(3).style(style::selection_ring(selected)))
        .padding(0)
        .width(Length::Fill)
        .style(style::bare)
        .on_press(on_press)
        .into()
}

fn remove_style(_: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => palette().danger,
        _ => Color::from_rgba8(0, 0, 0, 0.69),
    };
    button::Style {
        background: Some(background.into()),
        text_color: Color::WHITE,
        border: style::no_border(9.0),
        ..Default::default()
    }
}

pub fn wallpaper(s: &Settings, st: &mut AppearanceState, wake: &Wake) -> El {
    let p = palette();
    st.system_wp.ensure(wake, Duration::from_secs(300), wallpaper_candidates);
    let current = s.desktop.wallpaper.clone();
    let picking = st.picker.busy();
    if let Some(c) = &current {
        st.request_thumb(wake, Path::new(c));
    }
    let gradient = st.grad().handle.clone();

    // Preview card.
    let preview_img = match current.as_deref().and_then(|c| st.thumbs.get(Path::new(c))) {
        Some(Thumb::Ready(px)) => thumb_image(px.handle.clone(), 96.0),
        Some(Thumb::Loading) => placeholder("Loading…", 96.0, p),
        _ => thumb_image(gradient.clone(), 96.0),
    };
    let name = current
        .as_deref()
        .map(|c| wallpaper_name(Path::new(c)))
        .unwrap_or_else(|| "Orchard Gradient".into());
    let mut buttons = row![controls::push_button(
        if picking { "Choosing…" } else { "Add Wallpaper…" },
        (!picking).then(open_picker),
    )]
    .spacing(8);
    if current.is_some() {
        buttons = buttons.push(controls::push_button("Use Default", Some(set_wallpaper(None))));
    }
    let preview = form::group([form::custom_row(
        row![
            container(preview_img).width(154),
            column![
                text(truncate(&name, 40)).size(14).font(fonts::bold()).color(p.text),
                text("Orchard desktop wallpaper").size(11.5).color(p.text_secondary),
                Space::with_height(8),
                buttons,
            ]
            .spacing(4),
        ]
        .spacing(14)
        .align_y(Alignment::Center),
    )]);

    // Your wallpapers: removable on hover, followed by an "Add" tile.
    let mut yours: Vec<El> = Vec::new();
    for ps in &s.desktop.wallpapers {
        let path = Path::new(ps);
        st.request_thumb(wake, path);
        let sel = current.as_deref() == Some(ps.as_str());
        let tile = select_tile(tile_image(st.thumbs.get(path), "Missing", p), sel, set_wallpaper(Some(ps.clone())));
        let rm = ps.clone();
        let remove = button(container(icons::symbol(Symbol::Close, 9.0, Color::WHITE)).center(18))
            .padding(0)
            .style(remove_style)
            .on_press(edit(move |s| {
                s.desktop.wallpapers.retain(|p| p != &rm);
                if s.desktop.wallpaper.as_deref() == Some(rm.as_str()) {
                    s.desktop.wallpaper = None;
                }
            }));
        yours.push(hover(tile, container(remove).width(Length::Fill).align_x(Horizontal::Right).padding(8)));
    }
    let fg = if picking { p.text_tertiary } else { p.text_secondary };
    yours.push(
        container(
            button(
                container(
                    column![
                        icons::symbol(Symbol::Plus, 16.0, fg),
                        text(if picking { "Choosing…" } else { "Add Wallpaper" }).size(11.5).color(fg),
                    ]
                    .spacing(6)
                    .align_x(Alignment::Center),
                )
                .center_x(Length::Fill)
                .center_y(TILE_H),
            )
            .padding(0)
            .width(Length::Fill)
            .style(style::card_button(THUMB_RADIUS))
            .on_press_maybe((!picking).then(open_picker)),
        )
        .padding(3)
        .into(),
    );

    // Built-in & Pictures: the Orchard gradient first, then everything found
    // on disk that isn't already in "Your Wallpapers".
    let added: HashSet<&str> = s.desktop.wallpapers.iter().map(String::as_str).collect();
    let sys_paths: Vec<PathBuf> = st
        .system_wp
        .value
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter(|p| !added.contains(p.to_string_lossy().as_ref()))
        .cloned()
        .collect();
    let mut builtin = vec![select_tile(thumb_image(gradient, TILE_H), current.is_none(), set_wallpaper(None))];
    for path in sys_paths {
        st.request_thumb(wake, &path);
        let ps = path.to_string_lossy().into_owned();
        let sel = current.as_deref() == Some(ps.as_str());
        builtin.push(select_tile(tile_image(st.thumbs.get(&path), "Unsupported", p), sel, set_wallpaper(Some(ps))));
    }

    column![
        preview,
        form::error(st.wp_err.clone()),
        form::section("Your Wallpapers"),
        layout::grid(yours, 4, 7.0, 7.0),
        Space::with_height(16),
        form::section("Built-in & Pictures"),
        layout::grid(builtin, 4, 7.0, 7.0),
    ]
    .into()
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
    let out = if sys::which("zenity") {
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
    } else if sys::which("yad") {
        sys::output_localized(
            "yad",
            &[
                "--file",
                "--multiple",
                "--separator=\n",
                "--title=Add Wallpaper",
                &format!("--filename={start_dir}/"),
                "--file-filter=Images | *.png *.jpg *.jpeg *.webp",
            ],
        )
    } else {
        return Err("No file picker found (install zenity or yad).".into());
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

fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) * 0.5;
    let d = max - min;
    if d < 1e-5 {
        return (0.0, 0.0, l);
    }
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let mut h = if (max - r).abs() < 1e-5 {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if (max - g).abs() < 1e-5 {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    h *= 60.0;
    (h.rem_euclid(360.0), s.clamp(0.0, 1.0), l.clamp(0.0, 1.0))
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [u8; 3] {
    if s <= 1e-5 {
        let v = (l.clamp(0.0, 1.0) * 255.0).round() as u8;
        return [v, v, v];
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let hue2rgb = |mut t: f32| {
        t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    let hk = h.rem_euclid(360.0) / 360.0;
    [
        (hue2rgb(hk + 1.0 / 3.0).clamp(0.0, 1.0) * 255.0).round() as u8,
        (hue2rgb(hk).clamp(0.0, 1.0) * 255.0).round() as u8,
        (hue2rgb(hk - 1.0 / 3.0).clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

/// Pull a vibrant, UI-legible accent colour from an RGBA8 pixel buffer.
fn extract_accent_from_rgba(rgba: &[u8]) -> [u8; 3] {
    const BINS: usize = 36;
    #[derive(Clone, Copy, Default)]
    struct Bin {
        w: f32,
        r: f32,
        g: f32,
        b: f32,
    }
    let mut bins = [Bin::default(); BINS];
    let mut total_w = 0.0f32;

    for px in rgba.chunks_exact(4) {
        if px[3] < 128 {
            continue;
        }
        let r = px[0] as f32 / 255.0;
        let g = px[1] as f32 / 255.0;
        let b = px[2] as f32 / 255.0;
        let (h, s, l) = rgb_to_hsl(r, g, b);
        if s < 0.12 || !(0.12..=0.90).contains(&l) {
            continue;
        }
        // Prefer saturated, mid-lightness colours that read well as UI accents.
        let lum_factor = (1.0 - (2.0 * l - 1.0).abs()).max(0.1);
        let w = s * s * lum_factor;
        let idx = ((h / 360.0) * BINS as f32).floor() as usize % BINS;
        bins[idx].w += w;
        bins[idx].r += r * w;
        bins[idx].g += g * w;
        bins[idx].b += b * w;
        total_w += w;
    }

    if total_w <= 1e-4 {
        return Accent::Graphite.rgb();
    }

    // Smooth across immediate hue neighbours so colours near a bin edge aren't split.
    let best = (0..BINS)
        .max_by(|&a, &b| {
            let score = |i: usize| {
                let prev = (i + BINS - 1) % BINS;
                let next = (i + 1) % BINS;
                bins[i].w + 0.25 * (bins[prev].w + bins[next].w)
            };
            score(a).total_cmp(&score(b))
        })
        .unwrap_or(0);

    let bin = bins[best];
    if bin.w <= 1e-5 {
        return Accent::Graphite.rgb();
    }
    let (h, s, l) = rgb_to_hsl(bin.r / bin.w, bin.g / bin.w, bin.b / bin.w);
    hsl_to_rgb(h, s.clamp(0.45, 0.88), l.clamp(0.38, 0.56))
}

fn effective_wallpaper_path(setting: Option<&str>) -> Option<PathBuf> {
    std::env::var_os("ORCHARD_WALLPAPER")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .or_else(|| {
            setting
                .map(|p| PathBuf::from(p.trim_start_matches("file://")))
                .filter(|p| p.is_file())
        })
}

fn extract_accent_from_wallpaper(setting: Option<&str>) -> [u8; 3] {
    if let Some(path) = effective_wallpaper_path(setting) {
        if let Ok(reader) = image::ImageReader::open(&path) {
            if let Ok(reader) = reader.with_guessed_format() {
                if let Ok(img) = reader.decode() {
                    let small = img.resize_to_fill(64, 64, image::imageops::FilterType::Triangle).to_rgba8();
                    return extract_accent_from_rgba(small.as_raw());
                }
            }
        }
    }
    let (_, _, rgba) = gradient_rgba();
    extract_accent_from_rgba(&rgba)
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

    #[test]
    fn extract_accent_picks_dominant_vibrant_colour() {
        // Mostly dark background with a vibrant orange highlight region
        let mut rgba = Vec::new();
        for _ in 0..80 {
            rgba.extend_from_slice(&[18, 20, 24, 255]);
        }
        for _ in 0..20 {
            rgba.extend_from_slice(&[240, 120, 25, 255]);
        }
        let [r, g, b] = extract_accent_from_rgba(&rgba);
        assert!(r > g && g > b, "expected warm orange, got [{r}, {g}, {b}]");
    }

    #[test]
    fn extract_accent_falls_back_to_graphite_for_monochrome() {
        let mut rgba = Vec::new();
        for v in [20u8, 80, 140, 200, 240] {
            for _ in 0..10 {
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
        }
        assert_eq!(extract_accent_from_rgba(&rgba), Accent::Graphite.rgb());
    }

    #[test]
    fn extract_accent_handles_builtin_gradient() {
        let (_, _, rgba) = gradient_rgba();
        let [r, g, b] = extract_accent_from_rgba(&rgba);
        assert!(b > g && b > r, "expected blue/indigo from default gradient, got [{r}, {g}, {b}]");
    }
}
