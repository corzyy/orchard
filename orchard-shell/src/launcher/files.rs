//! Spotlight's file index: the files and folders in your home directory,
//! their types (shared-mime-info globs), recently used files (GTK/KDE's
//! recently-used.xbel) and freedesktop thumbnails.
//!
//! The index is built on a helper thread and handed to the shell as an
//! `Arc<FileIndex>`; searching it takes a few milliseconds per keystroke.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Instant, SystemTime},
};

/// Hard limits so a giant home directory cannot eat the shell's memory.
const MAX_ENTRIES: usize = 400_000;
const MAX_DEPTH: usize = 14;

/// Directory names that are never worth searching.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "__pycache__",
    "site-packages",
    "venv",
    "Trash",
    "lost+found",
    "go",
    "snap",
    "target",
    "build",
    "_build",
    "CMakeFiles",
    "dist",
];

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct FileEntry {
    pub path: Box<str>,
    /// lowercase file name, for matching
    pub name_lc: Box<str>,
    pub is_dir: bool,
    /// modification time, seconds since the epoch
    pub mtime: i64,
    /// directory depth below $HOME (0 = directly in it)
    pub depth: u8,
}

impl FileEntry {
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

/// Compact fixed-size entry referencing `FileIndex`'s contiguous string arenas
/// (zero per-entry heap allocations).
#[derive(Debug, Clone, Copy)]
struct CompactEntry {
    path_off: u32,
    path_len: u16,
    name_off: u32,
    name_len: u16,
    mtime: i64,
    is_dir: bool,
    depth: u8,
}

pub struct FileIndex {
    home_prefix: String,
    paths: String,
    names_lc: String,
    entries: Vec<CompactEntry>,
    /// path → last time it was opened (seconds), from recently-used.xbel
    pub recent: HashMap<String, i64>,
    pub mime: MimeDb,
    pub built: Instant,
}

impl FileIndex {
    pub fn build() -> FileIndex {
        let t = Instant::now();
        let home = home();
        let mut home_prefix = home.to_string_lossy().into_owned();
        if !home_prefix.ends_with('/') {
            home_prefix.push('/');
        }
        let mut paths = String::new();
        let mut names_lc = String::new();
        let mut entries = Vec::new();
        walk(&home, &home_prefix, 0, &mut paths, &mut names_lc, &mut entries);
        paths.shrink_to_fit();
        names_lc.shrink_to_fit();
        entries.shrink_to_fit();
        let recent = read_recent();
        let mime = MimeDb::load();
        tracing::info!("spotlight: indexed {} files in {:?}", entries.len(), t.elapsed());
        FileIndex {
            home_prefix,
            paths,
            names_lc,
            entries,
            recent,
            mime,
            built: Instant::now(),
        }
    }

    fn rel_path(&self, e: &CompactEntry) -> &str {
        let start = e.path_off as usize;
        &self.paths[start..start + e.path_len as usize]
    }

    fn name_lc(&self, e: &CompactEntry) -> &str {
        let start = e.name_off as usize;
        &self.names_lc[start..start + e.name_len as usize]
    }

    fn full_path(&self, e: &CompactEntry) -> String {
        let rel = self.rel_path(e);
        let mut out = String::with_capacity(self.home_prefix.len() + rel.len());
        out.push_str(&self.home_prefix);
        out.push_str(rel);
        out
    }
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn walk(
    dir: &Path,
    home_prefix: &str,
    depth: usize,
    paths: &mut String,
    names_lc: &mut String,
    out: &mut Vec<CompactEntry>,
) {
    if depth > MAX_DEPTH || out.len() >= MAX_ENTRIES {
        return;
    }
    // build output, caches: CACHEDIR.TAG marks a directory as disposable
    if depth > 0 && dir.join("CACHEDIR.TAG").exists() {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut subdirs = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with('.') || name.ends_with('~') {
            continue;
        }
        let Ok(ft) = e.file_type() else { continue };
        // symlinks are listed but never followed (no loops, no huge mounts)
        let is_dir = ft.is_dir();
        if is_dir && SKIP_DIRS.contains(&name) {
            continue;
        }
        let path = e.path();
        let mtime = e
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let Some(p) = path.to_str() else { continue };
        let rel = p.strip_prefix(home_prefix).unwrap_or(p);
        let name_lower = name.to_lowercase();
        let (Ok(path_off), Ok(path_len), Ok(name_off), Ok(name_len)) = (
            u32::try_from(paths.len()),
            u16::try_from(rel.len()),
            u32::try_from(names_lc.len()),
            u16::try_from(name_lower.len()),
        ) else {
            continue;
        };
        paths.push_str(rel);
        names_lc.push_str(&name_lower);
        out.push(CompactEntry {
            path_off,
            path_len,
            name_off,
            name_len,
            is_dir,
            mtime,
            depth: depth.min(255) as u8,
        });
        if out.len() >= MAX_ENTRIES {
            return;
        }
        if is_dir {
            subdirs.push(path);
        }
    }
    for d in subdirs {
        walk(&d, home_prefix, depth + 1, paths, names_lc, out);
    }
}

/// Recently used files: `~/.local/share/recently-used.xbel` (GTK, KDE).
fn read_recent() -> HashMap<String, i64> {
    let mut out = HashMap::new();
    let path = home().join(".local/share/recently-used.xbel");
    let Ok(text) = std::fs::read_to_string(path) else { return out };
    for chunk in text.split("<bookmark ").skip(1) {
        let attr = |name: &str| -> Option<&str> {
            let key = format!("{name}=\"");
            let start = chunk.find(&key)? + key.len();
            let end = chunk[start..].find('"')? + start;
            Some(&chunk[start..end])
        };
        let Some(href) = attr("href") else { continue };
        let Some(p) = href.strip_prefix("file://") else { continue };
        let p = percent_decode(p);
        let when = attr("visited").or_else(|| attr("modified")).and_then(parse_iso8601).unwrap_or(0);
        let e = out.entry(p).or_insert(0);
        *e = (*e).max(when);
    }
    out
}

/// "2025-03-01T12:34:56.123Z" → seconds since the epoch (UTC).
fn parse_iso8601(s: &str) -> Option<i64> {
    let dt = chrono::DateTime::parse_from_rfc3339(s).ok()?;
    Some(dt.timestamp())
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A path as a file:// URI the way thumbnailers write it.
pub fn file_uri(path: &str) -> String {
    let mut out = String::from("file://");
    for &b in path.as_bytes() {
        let keep = b.is_ascii_alphanumeric() || b"/-_.~!$&'()*+,;=:@".contains(&b);
        if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

// ---- file types --------------------------------------------------------------

/// What Spotlight groups results by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FileClass {
    Folder,
    Document,
    Pdf,
    Image,
    Movie,
    Music,
    Code,
    Archive,
    Other,
}

pub struct MimeDb {
    /// lowercase extension (without the dot, may contain dots: "tar.gz") → mime type
    ext: HashMap<String, String>,
    /// mime type → generic icon name
    generic: HashMap<String, String>,
    /// mime type → its description ("Markdown document"), read on demand
    comments: std::sync::Mutex<HashMap<String, Option<String>>>,
    /// language for descriptions ("de")
    lang: String,
}

impl MimeDb {
    fn load() -> MimeDb {
        let mut ext = HashMap::new();
        let mut generic = HashMap::new();
        let dirs = [home().join(".local/share/mime"), PathBuf::from("/usr/local/share/mime"), PathBuf::from("/usr/share/mime")];
        for d in &dirs {
            if let Ok(text) = std::fs::read_to_string(d.join("globs2")) {
                // weight:mime/type:*.ext[:flags]; higher weights first in the file
                for line in text.lines() {
                    if line.starts_with('#') {
                        continue;
                    }
                    let mut parts = line.split(':');
                    let (Some(_w), Some(mime), Some(glob)) = (parts.next(), parts.next(), parts.next()) else { continue };
                    let Some(e) = glob.strip_prefix("*.") else { continue };
                    if e.contains(['*', '?', '[']) {
                        continue;
                    }
                    ext.entry(e.to_lowercase()).or_insert_with(|| mime.to_string());
                }
            }
            if let Ok(text) = std::fs::read_to_string(d.join("generic-icons")) {
                for line in text.lines() {
                    if let Some((m, icon)) = line.split_once(':') {
                        generic.entry(m.to_string()).or_insert_with(|| icon.to_string());
                    }
                }
            }
        }
        let lang = std::env::var("LC_MESSAGES")
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| std::env::var("LANG").ok())
            .unwrap_or_default();
        let lang = lang.split(['_', '.', '@']).next().unwrap_or_default().to_string();
        MimeDb {
            ext,
            generic,
            comments: std::sync::Mutex::new(HashMap::new()),
            lang,
        }
    }

    /// Mime type of a file name ("photo.JPG" → "image/jpeg").
    pub fn mime_of(&self, name: &str) -> Option<&str> {
        let lc = name.to_lowercase();
        // longest extension first: "a.tar.gz" → "tar.gz" before "gz"
        let mut rest = lc.as_str();
        while let Some((_, e)) = rest.split_once('.') {
            if let Some(m) = self.ext.get(e) {
                return Some(m);
            }
            rest = e;
        }
        None
    }

    /// Icon names to try for a file, best first.
    pub fn icons_for(&self, name: &str, is_dir: bool) -> Vec<String> {
        if is_dir {
            return vec![folder_icon(name).into(), "folder".into(), "inode-directory".into()];
        }
        let mut v = Vec::new();
        if let Some(m) = self.mime_of(name) {
            v.push(m.replace('/', "-"));
            if let Some(g) = self.generic.get(m) {
                v.push(g.clone());
            }
            let major = m.split('/').next().unwrap_or_default();
            v.push(format!("{major}-x-generic"));
        }
        v.push("text-x-generic".into());
        v.push("unknown".into());
        v
    }

    pub fn class_of(&self, name: &str, is_dir: bool) -> FileClass {
        if is_dir {
            return FileClass::Folder;
        }
        let Some(m) = self.mime_of(name) else { return FileClass::Other };
        classify(m)
    }

    /// The type's description from shared-mime-info, in the user's language
    /// ("Markdown-Dokument", "PNG-Bild").
    fn comment(&self, mime: &str) -> Option<String> {
        let mut cache = self.comments.lock().ok()?;
        if let Some(c) = cache.get(mime) {
            return c.clone();
        }
        let c = read_mime_comment(mime, &self.lang);
        cache.insert(mime.to_string(), c.clone());
        c
    }

    /// "PNG image", "PDF document", "Rust source code" …
    pub fn kind_label(&self, name: &str, is_dir: bool, german: bool) -> String {
        let t = |en: &str, de: &str| if german { de.to_string() } else { en.to_string() };
        if is_dir {
            return t("Folder", "Ordner");
        }
        if let Some(c) = self.mime_of(name).and_then(|m| self.comment(m)) {
            let mut ch = c.chars();
            return ch.next().map(|f| f.to_uppercase().collect::<String>() + ch.as_str()).unwrap_or(c);
        }
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_uppercase()).unwrap_or_default();
        let m = self.mime_of(name).unwrap_or("");
        match classify(m) {
            FileClass::Image => format!("{ext} {}", t("image", "Bild")).trim().to_string(),
            FileClass::Movie => format!("{ext} {}", t("movie", "Film")).trim().to_string(),
            FileClass::Music => format!("{ext} {}", t("audio", "Audio")).trim().to_string(),
            FileClass::Pdf => t("PDF document", "PDF-Dokument"),
            FileClass::Archive => format!("{ext} {}", t("archive", "Archiv")).trim().to_string(),
            FileClass::Code => format!("{ext} {}", t("source code", "Quelltext")).trim().to_string(),
            FileClass::Document if m == "text/plain" => t("Plain text", "Reiner Text"),
            FileClass::Document => format!("{ext} {}", t("document", "Dokument")).trim().to_string(),
            _ if ext.is_empty() => t("Document", "Dokument"),
            _ => format!("{ext} {}", t("file", "Datei")),
        }
    }
}

/// `<comment xml:lang="de">…</comment>` (or the untranslated one) from
/// /usr/share/mime/<type>.xml.
fn read_mime_comment(mime: &str, lang: &str) -> Option<String> {
    let dirs = [home().join(".local/share/mime"), PathBuf::from("/usr/local/share/mime"), PathBuf::from("/usr/share/mime")];
    let text = dirs.iter().find_map(|d| std::fs::read_to_string(d.join(format!("{mime}.xml"))).ok())?;
    let mut plain = None;
    let mut localized = None;
    for part in text.split("<comment").skip(1) {
        let Some((attrs, rest)) = part.split_once('>') else { continue };
        let Some((body, _)) = rest.split_once("</comment>") else { continue };
        let body = body.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'");
        if attrs.trim().is_empty() {
            plain.get_or_insert(body);
        } else if !lang.is_empty() && attrs.contains(&format!("xml:lang=\"{lang}\"")) {
            localized.get_or_insert(body);
        }
    }
    localized.or(plain)
}

fn classify(m: &str) -> FileClass {
    let (major, minor) = m.split_once('/').unwrap_or((m, ""));
    match major {
        "image" => FileClass::Image,
        "video" => FileClass::Movie,
        "audio" => FileClass::Music,
        _ if m == "application/pdf" => FileClass::Pdf,
        "text" if matches!(minor, "plain" | "markdown" | "x-markdown" | "rtf" | "csv" | "x-log" | "org") => FileClass::Document,
        "text" => FileClass::Code,
        "application"
            if minor.starts_with("vnd.oasis.opendocument")
                || minor.starts_with("vnd.openxmlformats")
                || minor.starts_with("vnd.ms-")
                || matches!(minor, "msword" | "rtf" | "epub+zip" | "x-mobipocket-ebook") =>
        {
            FileClass::Document
        }
        "application"
            if matches!(
                minor,
                "zip" | "x-tar" | "gzip" | "x-7z-compressed" | "x-rar" | "vnd.rar" | "x-bzip2" | "x-xz" | "zstd"
                    | "x-compressed-tar" | "x-xz-compressed-tar" | "x-bzip2-compressed-tar" | "x-zstd-compressed-tar"
                    | "x-iso9660-image" | "x-rpm" | "vnd.debian.binary-package" | "vnd.flatpak"
            ) =>
        {
            FileClass::Archive
        }
        "application"
            if matches!(
                minor,
                "json" | "xml" | "javascript" | "x-shellscript" | "toml" | "x-yaml" | "yaml" | "sql" | "x-python"
                    | "x-perl" | "x-ruby" | "x-php" | "x-desktop" | "graphql" | "x-sh"
            ) =>
        {
            FileClass::Code
        }
        _ => FileClass::Other,
    }
}

/// Coloured folder icons for the well-known XDG folders.
fn folder_icon(name: &str) -> &'static str {
    match name {
        "Desktop" | "Schreibtisch" => "user-desktop",
        "Documents" | "Dokumente" => "folder-documents",
        "Downloads" => "folder-download",
        "Music" | "Musik" => "folder-music",
        "Pictures" | "Bilder" => "folder-pictures",
        "Videos" => "folder-videos",
        "Public" | "Öffentlich" => "folder-publicshare",
        "Templates" | "Vorlagen" => "folder-templates",
        _ => "folder",
    }
}

// ---- search --------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct FileHit {
    pub entry: FileEntry,
    pub score: i64,
    pub class: FileClass,
}

/// Search file names. Every query word has to match the name (or, for words
/// after the first, the parent folder names), so "notes rust" finds
/// `~/rust/notes.md`.
pub fn search(index: &FileIndex, query: &str, limit: usize) -> Vec<FileHit> {
    let q = query.trim().to_lowercase();
    if q.chars().count() < 2 {
        return Vec::new();
    }
    let words: Vec<&str> = q.split_whitespace().collect();
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let mut scored: Vec<(i64, usize)> = Vec::new();
    let mut full_buf = String::new();
    for (idx, e) in index.entries.iter().enumerate() {
        let name = index.name_lc(e);
        let rel = index.rel_path(e);
        let mut score: i64 = 0;
        let mut ok = true;
        for (i, w) in words.iter().enumerate() {
            let s = if let Some(pos) = name.find(w) {
                let at_word = pos == 0 || !name.as_bytes()[pos - 1].is_ascii_alphanumeric();
                let whole = name.len() == w.len()
                    || name.strip_prefix(w).is_some_and(|rest| rest.starts_with('.') && !rest[1..].contains('.'));
                if whole {
                    1000
                } else if pos == 0 {
                    700
                } else if at_word {
                    500
                } else {
                    250
                }
            } else if i > 0 && rel.to_lowercase().contains(w) {
                120
            } else {
                ok = false;
                break;
            };
            score += s;
        }
        if !ok {
            continue;
        }
        // shallower, recently modified and recently opened files first
        score -= e.depth as i64 * 25;
        score -= (name.len() as i64 - q.len() as i64).max(0) * 2;
        let age_days = ((now - e.mtime).max(0) / 86_400).min(3650);
        score += match age_days {
            0..=1 => 120,
            2..=7 => 80,
            8..=30 => 40,
            31..=365 => 10,
            _ => 0,
        };
        if !index.recent.is_empty() {
            full_buf.clear();
            full_buf.push_str(&index.home_prefix);
            full_buf.push_str(rel);
            if let Some(when) = index.recent.get(&full_buf) {
                let days = ((now - when).max(0) / 86_400).min(365);
                score += 400 - days;
            }
        }
        scored.push((score, idx));
    }
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| index.entries[a.1].path_len.cmp(&index.entries[b.1].path_len))
    });
    scored.truncate(limit);
    scored
        .into_iter()
        .map(|(score, idx)| {
            let e = &index.entries[idx];
            let name = index.name_lc(e);
            FileHit {
                class: index.mime.class_of(name, e.is_dir),
                entry: FileEntry {
                    path: index.full_path(e).into_boxed_str(),
                    name_lc: name.into(),
                    is_dir: e.is_dir,
                    mtime: e.mtime,
                    depth: e.depth,
                },
                score,
            }
        })
        .collect()
}

// ---- thumbnails --------------------------------------------------------------

/// The freedesktop thumbnail of a file, if a file manager already made one.
pub fn cached_thumbnail(path: &str) -> Option<PathBuf> {
    let hash = md5_hex(file_uri(path).as_bytes());
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".cache"))
        .join("thumbnails");
    let src_mtime = std::fs::metadata(path).ok()?.modified().ok()?;
    for size in ["xx-large", "x-large", "large", "normal"] {
        let p = base.join(size).join(format!("{hash}.png"));
        if let Ok(m) = std::fs::metadata(&p) {
            // a thumbnail older than the file is stale
            if m.modified().map(|t| t >= src_mtime).unwrap_or(true) {
                return Some(p);
            }
        }
    }
    None
}

/// Data for the preview pane, loaded off the main thread.
pub enum PreviewData {
    /// premultiplied RGBA
    Image { w: u32, h: u32, rgba: Vec<u8> },
    Text(String),
    Folder { items: usize },
    Nothing,
}

/// Load what the preview pane shows for a file. `max` is the largest edge in px.
pub fn load_preview(path: &str, is_dir: bool, class: FileClass, max: u32) -> PreviewData {
    if is_dir {
        let items = std::fs::read_dir(path).map(|rd| rd.take(100_000).count()).unwrap_or(0);
        return PreviewData::Folder { items };
    }
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let try_image = |p: &Path| -> Option<PreviewData> {
        let img = image::open(p).ok()?;
        let img = if img.width() > max || img.height() > max { img.thumbnail(max, max) } else { img };
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        let mut data = rgba.into_raw();
        for px in data.chunks_exact_mut(4) {
            let a = px[3] as u32;
            px[0] = (px[0] as u32 * a / 255) as u8;
            px[1] = (px[1] as u32 * a / 255) as u8;
            px[2] = (px[2] as u32 * a / 255) as u8;
        }
        Some(PreviewData::Image { w, h, rgba: data })
    };
    if let Some(t) = cached_thumbnail(path) {
        if let Some(p) = try_image(&t) {
            return p;
        }
    }
    let lc = path.to_lowercase();
    if class == FileClass::Image && size < 40_000_000 && [".png", ".jpg", ".jpeg", ".webp"].iter().any(|e| lc.ends_with(e)) {
        if let Some(p) = try_image(Path::new(path)) {
            return p;
        }
    }
    if matches!(class, FileClass::Document | FileClass::Code) || size < 64 * 1024 {
        if let Some(text) = read_text_head(path) {
            return PreviewData::Text(text);
        }
    }
    PreviewData::Nothing
}

/// The start of a text file, or `None` if it looks binary.
fn read_text_head(path: &str) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 6 * 1024];
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    if buf.contains(&0) {
        return None;
    }
    // cut a partial UTF-8 sequence at the end
    let text = match std::str::from_utf8(&buf) {
        Ok(t) => t.to_string(),
        Err(e) if e.valid_up_to() + 4 >= buf.len() => String::from_utf8_lossy(&buf[..e.valid_up_to()]).into_owned(),
        Err(_) => return None,
    };
    let text = text.replace('\t', "    ").replace('\r', "");
    (!text.trim().is_empty()).then_some(text)
}

/// Item count / size text helpers.
pub fn human_size(bytes: u64, german: bool) -> String {
    // Finder uses decimal units
    let units = ["bytes", "KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} {}", if german { "Byte" } else { "bytes" });
    }
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u < units.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    let s = if v < 10.0 { format!("{v:.1}") } else { format!("{v:.0}") };
    let s = if german { s.replace('.', ",") } else { s };
    format!("{s} {}", units[u])
}

// ---- md5 (thumbnail file names) -------------------------------------------------

pub fn md5_hex(input: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14,
        20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6,
        10, 15, 21,
    ];
    let k: Vec<u32> = (0..64).map(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32).collect();
    let (mut a0, mut b0, mut c0, mut d0) = (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());
    for chunk in msg.chunks_exact(64) {
        let m: Vec<u32> = chunk.chunks_exact(4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])).collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    [a0, b0, c0, d0].iter().flat_map(|v| v.to_le_bytes()).map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_known_values() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"The quick brown fox jumps over the lazy dog"), "9e107d9d372bb6826bd81d3542a419d6");
        assert_eq!(md5_hex(b"file:///home/jakob/a.png"), md5_hex(b"file:///home/jakob/a.png"));
    }

    #[test]
    fn uri_and_decode() {
        assert_eq!(file_uri("/home/a b/ä.png"), "file:///home/a%20b/%C3%A4.png");
        assert_eq!(percent_decode("/home/a%20b/%C3%A4.png"), "/home/a b/ä.png");
    }

    #[test]
    fn mime_comments() {
        if std::path::Path::new("/usr/share/mime/text/markdown.xml").exists() {
            assert_eq!(read_mime_comment("text/markdown", "").as_deref(), Some("Markdown document"));
            assert!(read_mime_comment("text/markdown", "de").is_some());
        }
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(512, false), "512 bytes");
        assert_eq!(human_size(1_500_000, false), "1.5 MB");
        assert_eq!(human_size(25_000_000, true), "25 MB");
    }
}
