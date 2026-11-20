//! macOS-style desktop icons rendered on a `Layer::Bottom` surface above the
//! wallpaper and below all windows.
//!
//! Displays non-hidden files and folders from the user's XDG Desktop directory,
//! arranged in vertical columns starting from the top-right corner.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Instant, SystemTime},
};

use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};

use crate::{
    apps::Apps,
    draw::{fill_rounded, stroke_rounded},
    text::{Fonts, Rgba, Weight},
    theme::Theme,
};

const CELL_W: f32 = 88.0;
const CELL_H: f32 = 96.0;
const ICON_SIZE: f32 = 48.0;
const MARGIN_TOP: f32 = 38.0;
const MARGIN_RIGHT: f32 = 16.0;
const MARGIN_BOTTOM: f32 = 92.0;
const DOUBLE_CLICK_MS: u128 = 420;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortBy {
    Name,
    Date,
    Kind,
}

#[derive(Debug, Clone)]
pub struct DesktopEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub exec: Option<String>,
    pub icon_name: String,
    pub modified: SystemTime,
}

pub struct Desktop {
    pub dir: PathBuf,
    pub entries: Vec<DesktopEntry>,
    pub selected: Option<usize>,
    pub sort: SortBy,
    pub enabled: bool,
    /// set by helper threads (a failed trash) to rescan on the next poll
    pub force_rescan: std::sync::Arc<std::sync::atomic::AtomicBool>,
    last_mtime: Option<SystemTime>,
    last_click: Option<(usize, Instant)>,
    icon_cache: HashMap<String, Option<Rc<Pixmap>>>,
    /// Cached cell rects `(x, y, w, h)` in logical coordinates for hit testing.
    rects: Vec<(f32, f32, f32, f32)>,
}

impl Desktop {
    pub fn new(enabled: bool) -> Self {
        let dir = resolve_desktop_dir();
        let mut d = Self {
            dir,
            entries: Vec::new(),
            selected: None,
            sort: SortBy::Name,
            enabled,
            force_rescan: Default::default(),
            last_mtime: None,
            last_click: None,
            icon_cache: HashMap::new(),
            rects: Vec::new(),
        };
        if enabled {
            d.rescan();
        }
        d
    }

    /// Check if the Desktop directory mtime changed; rescan if so. Returns true if changed.
    pub fn check_refresh(&mut self) -> bool {
        if !self.enabled {
            return false;
        }
        let mtime = fs::metadata(&self.dir).and_then(|m| m.modified()).ok();
        let forced = self.force_rescan.swap(false, std::sync::atomic::Ordering::Relaxed);
        if forced || mtime != self.last_mtime {
            self.rescan();
            return true;
        }
        false
    }

    pub fn rescan(&mut self) {
        self.last_mtime = fs::metadata(&self.dir).and_then(|m| m.modified()).ok();
        let mut entries = Vec::new();
        if let Ok(rd) = fs::read_dir(&self.dir) {
            for ent in rd.flatten() {
                let path = ent.path();
                let Some(fname) = path.file_name().and_then(|n| n.to_str()) else { continue };
                if fname.starts_with('.') {
                    continue;
                }
                let meta = match fs::metadata(&path) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let is_dir = meta.is_dir();
                let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                if !is_dir && fname.ends_with(".desktop") {
                    let (name, icon, exec) = parse_desktop_file(&path, fname);
                    entries.push(DesktopEntry {
                        path,
                        name,
                        is_dir: false,
                        exec,
                        icon_name: icon,
                        modified,
                    });
                } else {
                    let icon_name = if is_dir {
                        "folder".to_string()
                    } else {
                        guess_icon_name(fname).to_string()
                    };
                    let name = fname.to_string();
                    entries.push(DesktopEntry {
                        path,
                        name,
                        is_dir,
                        exec: None,
                        icon_name,
                        modified,
                    });
                }
            }
        }
        self.entries = entries;
        self.apply_sort();
        self.selected = None;
    }

    pub fn set_sort(&mut self, sort: SortBy) {
        self.sort = sort;
        self.apply_sort();
        self.selected = None;
    }

    fn apply_sort(&mut self) {
        match self.sort {
            SortBy::Name => self.entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
            SortBy::Date => self.entries.sort_by(|a, b| b.modified.cmp(&a.modified)),
            SortBy::Kind => self.entries.sort_by(|a, b| {
                b.is_dir
                    .cmp(&a.is_dir)
                    .then_with(|| a.icon_name.cmp(&b.icon_name))
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            }),
        }
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        self.rects
            .iter()
            .position(|&(rx, ry, rw, rh)| x >= rx && x <= rx + rw && y >= ry && y <= ry + rh)
    }

    /// Handle a left click at `(x, y)`. Returns `Some(entry_index)` if a double-click should open it.
    pub fn left_click(&mut self, x: f32, y: f32) -> (bool, Option<usize>) {
        let hit = self.hit(x, y);
        let prev = self.selected;
        self.selected = hit;
        let mut open = None;
        if let Some(idx) = hit {
            let now = Instant::now();
            if let Some((last_idx, at)) = self.last_click {
                if last_idx == idx && now.duration_since(at).as_millis() <= DOUBLE_CLICK_MS {
                    open = Some(idx);
                    self.last_click = None;
                } else {
                    self.last_click = Some((idx, now));
                }
            } else {
                self.last_click = Some((idx, now));
            }
        } else {
            self.last_click = None;
        }
        (prev != self.selected, open)
    }

    pub fn clear_icon_cache(&mut self) {
        self.icon_cache.clear();
    }

    pub fn paint(&mut self, pm: &mut Pixmap, scale: f32, fonts: &Fonts, apps: &mut Apps, theme: &Theme) {
        self.rects.clear();
        if !self.enabled {
            return;
        }
        let s = scale;
        let sw = pm.width() as f32 / s;
        let sh = pm.height() as f32 / s;
        let usable_h = (sh - MARGIN_TOP - MARGIN_BOTTOM).max(CELL_H);
        let rows_per_col = ((usable_h / CELL_H).floor() as usize).max(1);

        let accent = theme.menu_selection;
        let label_px = 11.5 * s;

        for (i, entry) in self.entries.iter().enumerate() {
            let col = i / rows_per_col;
            let row = i % rows_per_col;
            let cx = sw - MARGIN_RIGHT - (col as f32 + 1.0) * CELL_W;
            let cy = MARGIN_TOP + row as f32 * CELL_H;
            if cx < 8.0 {
                break;
            }
            self.rects.push((cx, cy, CELL_W, CELL_H));

            let selected = self.selected == Some(i);
            let icon_x = cx + (CELL_W - ICON_SIZE) * 0.5;
            let icon_y = cy + 6.0;

            if selected {
                fill_rounded(
                    pm,
                    (icon_x - 6.0) * s,
                    (icon_y - 4.0) * s,
                    (ICON_SIZE + 12.0) * s,
                    (ICON_SIZE + 8.0) * s,
                    8.0 * s,
                    [255, 255, 255, 42],
                );
                stroke_rounded(
                    pm,
                    (icon_x - 6.0) * s,
                    (icon_y - 4.0) * s,
                    (ICON_SIZE + 12.0) * s,
                    (ICON_SIZE + 8.0) * s,
                    8.0 * s,
                    1.0 * s,
                    [255, 255, 255, 85],
                );
            }

            // Icon (from theme cache or image preview)
            let cache_key = if is_image_file(&entry.path) {
                format!("img:{}", entry.path.display())
            } else {
                format!("ico:{}", entry.icon_name)
            };
            let pix = self
                .icon_cache
                .entry(cache_key)
                .or_insert_with(|| {
                    if is_image_file(&entry.path) {
                        load_image_thumb(&entry.path, (ICON_SIZE * s).round() as u32)
                            .or_else(|| apps.icon_any(&entry.icon_name, (ICON_SIZE * s).round() as u32))
                    } else {
                        apps.icon_any(&entry.icon_name, (ICON_SIZE * s).round() as u32)
                            .or_else(|| apps.icon_any("text-x-generic", (ICON_SIZE * s).round() as u32))
                    }
                })
                .clone();

            if let Some(icon) = pix {
                let target = ICON_SIZE * s;
                let iw = icon.width() as f32;
                let ih = icon.height() as f32;
                let sc = (target / iw.max(ih)).min(target / iw).min(target / ih);
                let dx = icon_x * s + (target - iw * sc) * 0.5;
                let dy = icon_y * s + (target - ih * sc) * 0.5;
                let paint = PixmapPaint {
                    opacity: 1.0,
                    quality: FilterQuality::Bilinear,
                    ..Default::default()
                };
                pm.draw_pixmap(0, 0, icon.as_ref().as_ref(), &paint, Transform::from_scale(sc, sc).post_translate(dx, dy), None);
            } else {
                // Clean fallback document/folder tile
                let col_bg: Rgba = if entry.is_dir { [88, 174, 245, 235] } else { [235, 235, 240, 230] };
                fill_rounded(pm, icon_x * s, icon_y * s, ICON_SIZE * s, ICON_SIZE * s, 9.0 * s, col_bg);
            }

            // Label (up to 2 lines, centered, macOS drop shadow or accent pill when selected)
            let max_lw = (CELL_W - 8.0) * s;
            let lines = wrap_label(fonts, &entry.name, label_px, max_lw);
            let mut ly = icon_y + ICON_SIZE + 15.0;
            for line in &lines {
                let tw = fonts.measure(line, label_px, Weight::Regular);
                let tx = (cx + CELL_W * 0.5) * s - tw * 0.5;
                if selected {
                    fill_rounded(
                        pm,
                        tx - 5.0 * s,
                        (ly - 11.0) * s,
                        tw + 10.0 * s,
                        15.0 * s,
                        4.0 * s,
                        accent,
                    );
                } else {
                    // Subtle dark shadow for legibility on bright wallpapers
                    fonts.draw(pm, line, tx, (ly + 1.0) * s, label_px, Weight::Regular, [0, 0, 0, 175]);
                }
                fonts.draw(pm, line, tx, ly * s, label_px, Weight::Regular, [255, 255, 255, 255]);
                ly += 14.5;
            }
        }
    }
}

fn wrap_label(fonts: &Fonts, name: &str, px: f32, max_w: f32) -> Vec<String> {
    if fonts.measure(name, px, Weight::Regular) <= max_w {
        return vec![name.to_string()];
    }
    // Split into 2 lines with ellipsis on the second
    let mut split_at = name.len();
    for (idx, _) in name.char_indices() {
        if idx > 0 && fonts.measure(&name[..idx], px, Weight::Regular) > max_w {
            break;
        }
        split_at = idx;
    }
    let first = name[..split_at].trim_end().to_string();
    let rest = name[split_at..].trim_start();
    if rest.is_empty() {
        vec![fonts.elide(name, px, Weight::Regular, max_w)]
    } else {
        vec![first, fonts.elide(rest, px, Weight::Regular, max_w)]
    }
}

pub fn resolve_desktop_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"));
    let cfg = home.join(".config/user-dirs.dirs");
    if let Ok(text) = fs::read_to_string(&cfg) {
        for line in text.lines() {
            let l = line.trim();
            if let Some(rest) = l.strip_prefix("XDG_DESKTOP_DIR=") {
                let val = rest.trim().trim_matches('"');
                let p = if let Some(sub) = val.strip_prefix("$HOME/") {
                    home.join(sub)
                } else {
                    PathBuf::from(val)
                };
                if p.is_dir() {
                    return p;
                }
            }
        }
    }
    for name in ["Desktop", "Schreibtisch"] {
        let p = home.join(name);
        if p.is_dir() {
            return p;
        }
    }
    home.join("Desktop")
}

fn parse_desktop_file(path: &Path, fallback_name: &str) -> (String, String, Option<String>) {
    let mut name = fallback_name.trim_end_matches(".desktop").to_string();
    let mut icon = "application-x-executable".to_string();
    let mut exec = None;
    if let Ok(content) = fs::read_to_string(path) {
        let mut in_entry = false;
        for line in content.lines() {
            let l = line.trim();
            if l.starts_with('[') {
                in_entry = l == "[Desktop Entry]";
                continue;
            }
            if !in_entry {
                continue;
            }
            if let Some(v) = l.strip_prefix("Name=") {
                if !v.is_empty() {
                    name = v.to_string();
                }
            } else if let Some(v) = l.strip_prefix("Icon=") {
                if !v.is_empty() {
                    icon = v.to_string();
                }
            } else if let Some(v) = l.strip_prefix("Exec=") {
                let cleaned: String = v
                    .split_whitespace()
                    .filter(|t| !t.starts_with('%'))
                    .collect::<Vec<_>>()
                    .join(" ");
                if !cleaned.is_empty() {
                    exec = Some(cleaned);
                }
            }
        }
    }
    (name, icon, exec)
}

fn is_image_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(|s| s.to_ascii_lowercase()).as_deref(),
        Some("png" | "jpg" | "jpeg" | "webp")
    )
}

fn load_image_thumb(path: &Path, max_px: u32) -> Option<Rc<Pixmap>> {
    let img = image::open(path).ok()?;
    let thumb = img.thumbnail(max_px.max(32), max_px.max(32)).to_rgba8();
    let (w, h) = thumb.dimensions();
    let mut pm = Pixmap::new(w, h)?;
    let dst = pm.data_mut();
    for (src, chunk) in thumb.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
        let a = src[3] as u32;
        chunk[0] = ((src[0] as u32 * a + 127) / 255) as u8;
        chunk[1] = ((src[1] as u32 * a + 127) / 255) as u8;
        chunk[2] = ((src[2] as u32 * a + 127) / 255) as u8;
        chunk[3] = a as u8;
    }
    Some(Rc::new(pm))
}

fn guess_icon_name(fname: &str) -> &'static str {
    let ext = fname.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "svg" => "image-x-generic",
        "mp4" | "mkv" | "webm" | "mov" | "avi" => "video-x-generic",
        "mp3" | "flac" | "ogg" | "wav" | "opus" => "audio-x-generic",
        "pdf" => "application-pdf",
        "zip" | "tar" | "gz" | "xz" | "7z" | "rar" | "zst" => "package-x-generic",
        "rs" | "py" | "js" | "ts" | "c" | "cpp" | "h" | "sh" | "json" | "toml" | "yaml" => "text-x-script",
        "md" | "txt" | "log" | "doc" | "docx" | "odt" => "text-x-generic",
        _ => "text-x-generic",
    }
}
