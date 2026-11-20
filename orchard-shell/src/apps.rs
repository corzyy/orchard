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
    /// "Web Browser"
    pub generic_name: Option<String>,
    /// one-line description
    pub comment: Option<String>,
    /// search keywords (localized and English)
    pub keywords: Vec<String>,
    /// freedesktop categories ("Utility", "Development" …)
    pub categories: Vec<String>,
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
    dirs.push(PathBuf::from("/var/lib/snapd/desktop"));
    let sys = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(sys.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    dirs
}

fn dir_mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

fn scan_desktop_dir(
    dir: &Path,
    prefix: &str,
    depth: u32,
    locale: &[String],
    seen: &mut std::collections::HashSet<String>,
    entries: &mut Vec<DesktopEntry>,
    mtimes: &mut Vec<(PathBuf, Option<std::time::SystemTime>)>,
) {
    mtimes.push((dir.to_path_buf(), dir_mtime(dir)));
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if depth < 3 && p.is_dir() {
            if let Some(sub) = p.file_name().and_then(|n| n.to_str()) {
                let next_prefix = if prefix.is_empty() {
                    sub.to_string()
                } else {
                    format!("{prefix}-{sub}")
                };
                scan_desktop_dir(&p, &next_prefix, depth + 1, locale, seen, entries, mtimes);
            }
            continue;
        }
        if p.extension().and_then(|x| x.to_str()) != Some("desktop") {
            continue;
        }
        let Some(stem) = p.file_stem().map(|s| s.to_string_lossy()) else { continue };
        let id = if prefix.is_empty() {
            stem.into_owned()
        } else {
            format!("{prefix}-{stem}")
        };
        if !seen.insert(id.clone()) {
            continue; // earlier dirs take precedence
        }
        if let Some(entry) = parse_entry(&p, id, locale) {
            entries.push(entry);
        }
    }
}

/// Localized value of `key` ("Comment[de_DE]", "Comment[de]", "Comment").
fn localized(map: &HashMap<String, String>, key: &str, locale: &[String]) -> Option<String> {
    locale
        .iter()
        .find_map(|l| map.get(&l.replacen("Name", key, 1)).cloned())
        .or_else(|| map.get(key).cloned())
        .filter(|v| !v.is_empty())
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
    let list = |v: Option<String>| -> Vec<String> {
        v.map(|v| v.split(';').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()).unwrap_or_default()
    };
    let mut keywords = list(localized(&map, "Keywords", locale));
    for k in list(map.get("Keywords").cloned()) {
        if !keywords.contains(&k) {
            keywords.push(k);
        }
    }
    Some(DesktopEntry {
        generic_name: localized(&map, "GenericName", locale),
        comment: localized(&map, "Comment", locale),
        keywords,
        categories: list(map.get("Categories").cloned()),
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
    /// O(1) lookup indices into `entries` (exact id, lowercase id, lowercase wm_class, lowercase last segment)
    idx_id: HashMap<String, usize>,
    idx_id_lower: HashMap<String, usize>,
    idx_wm_class: HashMap<String, usize>,
    idx_last: HashMap<String, usize>,
    /// Snapshot of watched application/icon directories to detect installs and removals.
    dir_mtimes: Vec<(PathBuf, Option<std::time::SystemTime>)>,
    icons: IconTheme,
    /// everything else (file types, devices, symbolic icons) for Spotlight;
    /// separate so app icon lookup stays exactly as it was
    extra_icons: RefCell<Option<IconTheme>>,
    icon_theme: Option<String>,
    /// size -> name -> pixmap
    icon_cache: RefCell<HashMap<u32, HashMap<String, Option<Rc<Pixmap>>>>>,
}

type ScannedApps = (
    Vec<DesktopEntry>,
    HashMap<String, usize>,
    HashMap<String, usize>,
    HashMap<String, usize>,
    HashMap<String, usize>,
    Vec<(PathBuf, Option<std::time::SystemTime>)>,
);

impl Apps {
    fn scan_entries() -> ScannedApps {
        let locale = locale_keys();
        let mut seen = std::collections::HashSet::new();
        let mut entries = Vec::new();
        let mut dir_mtimes = Vec::new();
        let dirs = data_dirs();
        for dir in &dirs {
            scan_desktop_dir(&dir.join("applications"), "", 0, &locale, &mut seen, &mut entries, &mut dir_mtimes);
            let hicolor = dir.join("icons/hicolor");
            dir_mtimes.push((hicolor.clone(), dir_mtime(&hicolor)));
            let scalable = hicolor.join("scalable/apps");
            dir_mtimes.push((scalable.clone(), dir_mtime(&scalable)));
        }
        let pixmaps = PathBuf::from("/usr/share/pixmaps");
        dir_mtimes.push((pixmaps.clone(), dir_mtime(&pixmaps)));

        let mut idx_id = HashMap::with_capacity(entries.len());
        let mut idx_id_lower = HashMap::with_capacity(entries.len());
        let mut idx_wm_class = HashMap::new();
        let mut idx_last = HashMap::with_capacity(entries.len());
        for (i, e) in entries.iter().enumerate() {
            idx_id.entry(e.id.clone()).or_insert(i);
            let lower = e.id.to_lowercase();
            if let Some(last) = lower.rsplit('.').next() {
                idx_last.entry(last.to_string()).or_insert(i);
            }
            idx_id_lower.entry(lower).or_insert(i);
            if let Some(wm) = &e.wm_class {
                idx_wm_class.entry(wm.to_lowercase()).or_insert(i);
            }
        }
        (entries, idx_id, idx_id_lower, idx_wm_class, idx_last, dir_mtimes)
    }

    pub fn load(icon_theme: Option<String>) -> Apps {
        let (entries, idx_id, idx_id_lower, idx_wm_class, idx_last, dir_mtimes) = Self::scan_entries();
        Apps {
            entries,
            idx_id,
            idx_id_lower,
            idx_wm_class,
            idx_last,
            dir_mtimes,
            icons: IconTheme::new(icon_theme.as_deref()),
            extra_icons: RefCell::new(None),
            icon_theme,
            icon_cache: RefCell::new(HashMap::new()),
        }
    }

    /// Re-scan desktop entries and app icons if any application/icon directory changed.
    /// Returns `true` if entries were reloaded.
    pub fn refresh(&mut self) -> bool {
        if !self.dir_mtimes.iter().any(|(p, m)| dir_mtime(p) != *m) {
            return false;
        }
        let (entries, idx_id, idx_id_lower, idx_wm_class, idx_last, dir_mtimes) = Self::scan_entries();
        tracing::info!("reloaded {} desktop entries", entries.len());
        self.entries = entries;
        self.idx_id = idx_id;
        self.idx_id_lower = idx_id_lower;
        self.idx_wm_class = idx_wm_class;
        self.idx_last = idx_last;
        self.dir_mtimes = dir_mtimes;
        self.icons = IconTheme::new(self.icon_theme.as_deref());
        self.icon_cache.borrow_mut().clear();
        true
    }

    /// Switch the icon theme (`None` = Orchard's default). Returns true if it changed.
    pub fn set_icon_theme(&mut self, theme: Option<String>) -> bool {
        if theme == self.icon_theme {
            return false;
        }
        tracing::info!("icon theme -> {}", theme.as_deref().unwrap_or(orchard_config::DEFAULT_ICON_THEME));
        self.icons = IconTheme::new(theme.as_deref());
        *self.extra_icons.borrow_mut() = None;
        self.icon_theme = theme;
        self.icon_cache.borrow_mut().clear();
        true
    }

    pub fn by_id(&self, id: &str) -> Option<&DesktopEntry> {
        self.idx_id.get(id).and_then(|&i| self.entries.get(i))
    }

    /// Find the desktop entry for a Wayland app_id / X11 class.
    pub fn for_app_id(&self, app_id: &str) -> Option<&DesktopEntry> {
        if app_id.is_empty() {
            return None;
        }
        if let Some(&i) = self.idx_id.get(app_id) {
            return self.entries.get(i);
        }
        let lower = app_id.to_lowercase();
        let idx = self
            .idx_id_lower
            .get(&lower)
            .or_else(|| self.idx_wm_class.get(&lower))
            .or_else(|| lower.rsplit('.').next().and_then(|last| self.idx_last.get(last)));
        idx.and_then(|&i| self.entries.get(i))
    }

    /// Icon for an app, rendered at `size` physical px.
    pub fn icon(&self, icon_name: &str, size: u32) -> Option<Rc<Pixmap>> {
        if let Some(v) = self.icon_cache.borrow().get(&size).and_then(|m| m.get(icon_name)) {
            return v.clone();
        }
        let pm = self.icons.find(icon_name, size).and_then(|p| render_icon(&p, size)).map(Rc::new);
        let mut cache = self.icon_cache.borrow_mut();
        let map = cache.entry(size).or_default();
        if map.len() >= 512 {
            map.clear();
        }
        map.insert(icon_name.to_string(), pm.clone());
        pm
    }

    /// Icon from an explicit filesystem path, cached in `icon_cache`.
    pub fn icon_by_path(&self, path: &str, size: u32) -> Option<Rc<Pixmap>> {
        let key = format!("\x01{path}");
        if let Some(v) = self.icon_cache.borrow().get(&size).and_then(|m| m.get(&key)) {
            return v.clone();
        }
        let pm = render_icon(Path::new(path), size).map(Rc::new);
        let mut cache = self.icon_cache.borrow_mut();
        let map = cache.entry(size).or_default();
        if map.len() >= 512 {
            map.clear();
        }
        map.insert(key, pm.clone());
        pm
    }
}

impl Apps {
    /// Any themed icon (apps, then file types, devices, actions …), at `size` px.
    /// The extra index is built the first time it is needed.
    pub fn icon_any(&self, icon_name: &str, size: u32) -> Option<Rc<Pixmap>> {
        if let Some(pm) = self.icon(icon_name, size) {
            return Some(pm);
        }
        let key = format!("\0{icon_name}");
        if let Some(v) = self.icon_cache.borrow().get(&size).and_then(|m| m.get(&key)) {
            return v.clone();
        }
        if self.extra_icons.borrow().is_none() {
            *self.extra_icons.borrow_mut() = Some(IconTheme::with_dirs(self.icon_theme.as_deref(), EXTRA_DIRS));
        }
        let pm = self
            .extra_icons
            .borrow()
            .as_ref()
            .and_then(|t| t.find(icon_name, size))
            .and_then(|p| render_icon(&p, size))
            .map(Rc::new);
        let mut cache = self.icon_cache.borrow_mut();
        let map = cache.entry(size).or_default();
        if map.len() >= 512 {
            map.clear();
        }
        map.insert(key, pm.clone());
        pm
    }
}

/// The extra icon index, built off the main thread (it scans a lot of files).
pub struct ExtraIcons(IconTheme);

impl ExtraIcons {
    pub fn build(theme: Option<&str>) -> ExtraIcons {
        ExtraIcons(IconTheme::with_dirs(theme, EXTRA_DIRS))
    }
}

impl Apps {
    pub fn icon_theme_name(&self) -> Option<String> {
        self.icon_theme.clone()
    }

    /// Install a prebuilt extra index (ignored if the theme changed meanwhile).
    pub fn set_extra_icons(&self, theme: Option<String>, icons: ExtraIcons) {
        if theme == self.icon_theme {
            *self.extra_icons.borrow_mut() = Some(icons.0);
            for map in self.icon_cache.borrow_mut().values_mut() {
                map.retain(|k, _| !k.starts_with('\0'));
            }
        }
    }

    pub fn has_extra_icons(&self) -> bool {
        self.extra_icons.borrow().is_some()
    }
}

const APP_DIRS: &[&str] = &["apps", "applications", "places"];
const EXTRA_DIRS: &[&str] = &["mimetypes", "devices", "status", "actions", "categories", "legacy", "preferences"];

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

fn index_dir(dir: &Path, map: &mut HashMap<String, Vec<(u32, PathBuf)>>, depth: u32, kinds: &[&str]) {
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
            index_dir(&p, map, depth + 1, kinds);
        } else {
            let ext = p.extension().and_then(|x| x.to_str()).unwrap_or_default();
            if ext != "svg" && ext != "png" {
                continue;
            }
            // application icons plus places (Trash, folders) by default
            let wanted = p.components().any(|c| c.as_os_str().to_str().is_some_and(|c| kinds.contains(&c)));
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
        IconTheme::with_dirs(preferred, APP_DIRS)
    }

    fn with_dirs(preferred: Option<&str>, kinds: &[&str]) -> IconTheme {
        let mut themes: Vec<String> = Vec::new();
        if let Ok(t) = std::env::var("ORCHARD_ICON_THEME") {
            themes.push(t);
        }
        themes.push(preferred.filter(|t| !t.is_empty()).unwrap_or(orchard_config::DEFAULT_ICON_THEME).to_string());
        themes.extend(["Papirus", "Adwaita", "hicolor"].iter().map(|s| s.to_string()));
        if kinds != APP_DIRS {
            themes.push("AdwaitaLegacy".into());
        }
        let mut seen = std::collections::HashSet::new();
        themes.retain(|t| seen.insert(t.clone()));
        let mut index = Vec::new();
        for theme in &themes {
            let mut map = HashMap::new();
            for dir in data_dirs() {
                index_dir(&dir.join("icons").join(theme), &mut map, 0, kinds);
            }
            if let Some(home) = std::env::var_os("HOME") {
                index_dir(&PathBuf::from(home).join(".icons").join(theme), &mut map, 0, kinds);
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
        let stem = name
            .strip_suffix(".png")
            .or_else(|| name.strip_suffix(".svg"))
            .unwrap_or(name);
        for map in &self.index {
            if let Some(cands) = map.get(stem) {
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
        for ext in ["svg", "png"] {
            let pixmap = PathBuf::from(format!("/usr/share/pixmaps/{stem}.{ext}"));
            if pixmap.is_file() {
                return Some(pixmap);
            }
        }
        None
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_tracks_new_dirs_files_and_subdirs() {
        let base = std::env::temp_dir().join(format!("orchard-apps-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let apps_dir = base.join("applications");

        // Before the directory exists, its mtime is None and is still recorded.
        let mut seen = std::collections::HashSet::new();
        let mut entries = Vec::new();
        let mut mtimes = Vec::new();
        scan_desktop_dir(&apps_dir, "", 0, &[], &mut seen, &mut entries, &mut mtimes);
        assert!(entries.is_empty());
        assert_eq!(mtimes, vec![(apps_dir.clone(), None)]);

        // Creating the directory and a .desktop file changes the recorded mtime.
        std::fs::create_dir_all(apps_dir.join("sub")).unwrap();
        std::fs::write(
            apps_dir.join("foo.desktop"),
            "[Desktop Entry]\nType=Application\nName=Foo\nExec=foo\n",
        )
        .unwrap();
        std::fs::write(
            apps_dir.join("sub/bar.desktop"),
            "[Desktop Entry]\nType=Application\nName=Bar\nExec=bar\n",
        )
        .unwrap();
        assert!(mtimes.iter().any(|(p, m)| dir_mtime(p) != *m));

        seen.clear();
        entries.clear();
        mtimes.clear();
        scan_desktop_dir(&apps_dir, "", 0, &[], &mut seen, &mut entries, &mut mtimes);
        let mut ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["foo", "sub-bar"]);
        assert!(!mtimes.iter().any(|(p, m)| dir_mtime(p) != *m));

        let _ = std::fs::remove_dir_all(&base);
    }
}
