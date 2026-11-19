//! Desktop entries (.desktop files) and icon theme lookup.

use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
};

use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};

#[derive(Debug, Clone)]
pub struct DesktopEntry {
    /// file stem, e.g. "org.kde.konsole"
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    pub exec: Option<String>,
    pub wm_class: Option<String>,
    pub no_display: bool,
}

impl DesktopEntry {
    /// Exec line with field codes (%f, %U, …) removed.
    pub fn command(&self) -> Option<String> {
        let exec = self.exec.as_ref()?;
        let cleaned: Vec<&str> = exec
            .split_whitespace()
            .filter(|t| !(t.starts_with('%') && t.len() == 2))
            .collect();
        Some(cleaned.join(" "))
    }
}

fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.push(
            std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local/share")),
        );
        dirs.push(home.join(".local/share/flatpak/exports/share"));
    }
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share"));
    let sys = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(sys.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    dirs
}

fn locale_keys() -> Vec<String> {
    let lang = std::env::var("LC_MESSAGES")
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default();
    let base = lang.split('.').next().unwrap_or_default().to_string();
    let short = base.split('_').next().unwrap_or_default().to_string();
    let mut v = Vec::new();
    if !base.is_empty() {
        v.push(format!("Name[{base}]"));
    }
    if !short.is_empty() {
        v.push(format!("Name[{short}]"));
    }
    v
}

fn parse_entry(path: &Path, id: String, locale: &[String]) -> Option<DesktopEntry> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut in_main = false;
    let mut map: HashMap<String, String> = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_main = line == "[Desktop Entry]";
            continue;
        }
        if !in_main {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            map.entry(k.trim().to_string()).or_insert_with(|| v.trim().to_string());
        }
    }
    if map.get("Type").map(|t| t != "Application").unwrap_or(true) {
        return None;
    }
    let name = locale
        .iter()
        .find_map(|k| map.get(k).cloned())
        .or_else(|| map.get("Name").cloned())?;
    Some(DesktopEntry {
        id,
        name,
        icon: map.get("Icon").cloned(),
        exec: map.get("Exec").cloned(),
        wm_class: map.get("StartupWMClass").cloned(),
        no_display: map.get("NoDisplay").map(|v| v == "true").unwrap_or(false)
            || map.get("Hidden").map(|v| v == "true").unwrap_or(false),
    })
}

pub struct Apps {
    pub entries: Vec<DesktopEntry>,
    icons: IconTheme,
    icon_theme: Option<String>,
    icon_cache: RefCell<HashMap<(String, u32), Option<Rc<Pixmap>>>>,
}

impl Apps {
    pub fn load(icon_theme: Option<String>) -> Apps {
        let locale = locale_keys();
        let mut seen = std::collections::HashSet::new();
        let mut entries = Vec::new();
        for dir in data_dirs() {
            let Ok(rd) = std::fs::read_dir(dir.join("applications")) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("desktop") {
                    continue;
                }
                let id = p.file_stem().unwrap().to_string_lossy().to_string();
                if !seen.insert(id.clone()) {
                    continue; // earlier dirs take precedence
                }
                if let Some(entry) = parse_entry(&p, id, &locale) {
                    entries.push(entry);
                }
            }
        }
        Apps {
            entries,
            icons: IconTheme::new(icon_theme.as_deref()),
            icon_theme,
            icon_cache: RefCell::new(HashMap::new()),
        }
    }

    /// Switch the icon theme (`None` = Orchard's default). Returns true if it changed.
    pub fn set_icon_theme(&mut self, theme: Option<String>) -> bool {
        if theme == self.icon_theme {
            return false;
        }
        tracing::info!("icon theme -> {}", theme.as_deref().unwrap_or(orchard_config::DEFAULT_ICON_THEME));
        self.icons = IconTheme::new(theme.as_deref());
        self.icon_theme = theme;
        self.icon_cache.borrow_mut().clear();
        true
    }

    pub fn by_id(&self, id: &str) -> Option<&DesktopEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Find the desktop entry for a Wayland app_id / X11 class.
    pub fn for_app_id(&self, app_id: &str) -> Option<&DesktopEntry> {
        if app_id.is_empty() {
            return None;
        }
        let lower = app_id.to_lowercase();
        let last = lower.rsplit('.').next().unwrap_or(&lower).to_string();
        self.entries
            .iter()
            .find(|e| e.id == app_id)
            .or_else(|| self.entries.iter().find(|e| e.id.to_lowercase() == lower))
            .or_else(|| {
                self.entries
                    .iter()
                    .find(|e| e.wm_class.as_deref().map(|c| c.to_lowercase()) == Some(lower.clone()))
            })
            .or_else(|| {
                self.entries
                    .iter()
                    .find(|e| e.id.to_lowercase().rsplit('.').next() == Some(last.as_str()))
            })
    }

    /// Icon for an app, rendered at `size` physical px.
    pub fn icon(&self, icon_name: &str, size: u32) -> Option<Rc<Pixmap>> {
        let key = (icon_name.to_string(), size);
        if let Some(v) = self.icon_cache.borrow().get(&key) {
            return v.clone();
        }
        let pm = self.icons.find(icon_name, size).and_then(|p| render_icon(&p, size)).map(Rc::new);
        self.icon_cache.borrow_mut().insert(key, pm.clone());
        pm
    }
}

/// Index of app icons across the active theme + fallbacks.
struct IconTheme {
    /// theme name -> icon name -> candidate paths (with nominal size, 0 = scalable)
    index: Vec<HashMap<String, Vec<(u32, PathBuf)>>>,
}

fn size_from_path(p: &Path) -> u32 {
    for comp in p.components().rev() {
        let s = comp.as_os_str().to_string_lossy();
        if s == "scalable" {
            return 0;
        }
        if let Some((a, _)) = s.split_once('x') {
            if let Ok(n) = a.parse() {
                return n;
            }
        }
        if let Ok(n) = s.parse::<u32>() {
            return n;
        }
    }
    48
}

fn index_dir(dir: &Path, map: &mut HashMap<String, Vec<(u32, PathBuf)>>, depth: u32) {
    if depth > 4 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let ft = match e.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_dir() || (ft.is_symlink() && p.is_dir()) {
            index_dir(&p, map, depth + 1);
        } else {
            let ext = p.extension().and_then(|x| x.to_str()).unwrap_or_default();
            if ext != "svg" && ext != "png" {
                continue;
            }
            // application icons plus places (Trash, folders)
            let wanted = p.components().any(|c| {
                matches!(
                    c.as_os_str().to_str(),
                    Some("apps") | Some("applications") | Some("places")
                )
            });
            if !wanted {
                continue;
            }
            let name = p.file_stem().unwrap().to_string_lossy().to_string();
            map.entry(name).or_default().push((size_from_path(&p), p));
        }
    }
}

impl IconTheme {
    fn new(preferred: Option<&str>) -> IconTheme {
        let mut themes: Vec<String> = Vec::new();
        if let Ok(t) = std::env::var("ORCHARD_ICON_THEME") {
            themes.push(t);
        }
        themes.push(preferred.filter(|t| !t.is_empty()).unwrap_or(orchard_config::DEFAULT_ICON_THEME).to_string());
        themes.extend(["Papirus", "breeze", "Adwaita", "hicolor"].iter().map(|s| s.to_string()));
        let mut seen = std::collections::HashSet::new();
        themes.retain(|t| seen.insert(t.clone()));
        let mut index = Vec::new();
        for theme in &themes {
            let mut map = HashMap::new();
            for dir in data_dirs() {
                index_dir(&dir.join("icons").join(theme), &mut map, 0);
            }
            if let Some(home) = std::env::var_os("HOME") {
                index_dir(&PathBuf::from(home).join(".icons").join(theme), &mut map, 0);
            }
            if !map.is_empty() {
                index.push(map);
            }
        }
        IconTheme { index }
    }

    fn find(&self, name: &str, size: u32) -> Option<PathBuf> {
        let p = Path::new(name);
        if p.is_absolute() && p.is_file() {
            return Some(p.to_path_buf());
        }
        for map in &self.index {
            if let Some(cands) = map.get(name) {
                // prefer scalable, else the smallest size >= requested, else largest
                if let Some((_, p)) = cands.iter().find(|(s, _)| *s == 0) {
                    return Some(p.clone());
                }
                let mut sorted = cands.clone();
                sorted.sort_by_key(|(s, _)| *s);
                if let Some((_, p)) = sorted.iter().find(|(s, _)| *s >= size) {
                    return Some(p.clone());
                }
                if let Some((_, p)) = sorted.last() {
                    return Some(p.clone());
                }
            }
        }
        let pixmap = PathBuf::from(format!("/usr/share/pixmaps/{name}.png"));
        pixmap.is_file().then_some(pixmap)
    }
}

pub fn render_icon(path: &Path, size: u32) -> Option<Pixmap> {
    let data = std::fs::read(path).ok()?;
    let mut pm = Pixmap::new(size, size)?;
    if path.extension().and_then(|x| x.to_str()) == Some("svg") {
        let tree = resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default()).ok()?;
        let s = tree.size();
        let k = size as f32 / s.width().max(s.height());
        let dx = (size as f32 - s.width() * k) / 2.0;
        let dy = (size as f32 - s.height() * k) / 2.0;
        resvg::render(&tree, Transform::from_row(k, 0.0, 0.0, k, dx, dy), &mut pm.as_mut());
    } else {
        let img = image::load_from_memory(&data).ok()?.to_rgba8();
        let (w, h) = img.dimensions();
        let mut src = Pixmap::new(w, h)?;
        for (i, px) in img.pixels().enumerate() {
            let a = px[3] as u32;
            let d = &mut src.data_mut()[i * 4..i * 4 + 4];
            d[0] = (px[0] as u32 * a / 255) as u8;
            d[1] = (px[1] as u32 * a / 255) as u8;
            d[2] = (px[2] as u32 * a / 255) as u8;
            d[3] = a as u8;
        }
        let k = size as f32 / w.max(h) as f32;
        let paint = PixmapPaint {
            quality: FilterQuality::Bicubic,
            ..Default::default()
        };
        pm.draw_pixmap(0, 0, src.as_ref(), &paint, Transform::from_scale(k, k), None);
    }
    Some(pm)
}
