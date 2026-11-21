//! Spotlight: one search field for apps, System Settings panes, system
//! commands, files and folders, a calculator, unit conversions and the web.
//!
//! A compact, Raycast-style command palette: one full-width result list with
//! section headings and a footer that shows what ⏎ will do. Opening and
//! closing are instant; only the result list grows and shrinks. Spotlight
//! learns: what you pick for a query ranks first the next time you type it.
//!
//! This module holds the model and the drawing; `glue.rs` wires it into the
//! shell (surface, keyboard, pointer, background work).

pub mod calc;
pub mod files;
mod glue;

pub use glue::{Msg, Spot};

use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::Arc,
    time::{Instant, SystemTime},
};

use serde::{Deserialize, Serialize};
use smithay_client_toolkit::seat::keyboard::{Keysym, Modifiers};
use tiny_skia::{BlendMode, FillRule, FilterQuality, Pixmap, PixmapPaint, Stroke, Transform};

use crate::{
    apps::Apps,
    draw::{fill_rounded, paint, rounded_rect, stroke_rounded},
    text::{Fonts, Rgba, Weight},
    theme::Theme,
};
use files::{FileClass, FileHit, FileIndex};
use orchard_config::SearchSettings;

// ---- geometry (logical px) ---------------------------------------------------

pub const WIDTH: f32 = 640.0;
pub const FIELD_H: f32 = 52.0;
/// Height of the result area (below the field, above the footer).
pub const BODY_H: f32 = 356.0;
/// Action hints at the bottom of the panel.
pub const FOOTER_H: f32 = 32.0;
pub const RADIUS: f32 = 14.0;
const HEADER_H: f32 = 24.0;
const ROW_H: f32 = 34.0;
const TOP_ROW_H: f32 = 44.0;
const LIST_PAD: f32 = 4.0;

/// Ranking for the launcher and Launchpad search: prefix match > word-prefix
/// match > substring > subsequence.
pub fn score(name: &str, query: &str) -> Option<u32> {
    let n = name.to_lowercase();
    let q = query.to_lowercase();
    if q.is_empty() {
        return None;
    }
    if n.starts_with(&q) {
        return Some(400 - n.len().min(100) as u32);
    }
    if n.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(&q)) {
        return Some(300 - n.len().min(100) as u32);
    }
    if n.contains(&q) {
        return Some(200 - n.len().min(100) as u32);
    }
    let mut it = n.chars();
    if q.chars().all(|c| it.any(|x| x == c)) {
        return Some(100 - n.len().min(99) as u32);
    }
    None
}

fn tr(german: bool, en: &'static str, de: &'static str) -> &'static str {
    if german {
        de
    } else {
        en
    }
}

// ---- static catalogues ----------------------------------------------------------

/// A System Settings pane (opened with `orchard-settings <title>`).
pub struct Pane {
    pub title: &'static str,
    pub de: &'static str,
    pub icon: &'static str,
    pub color: [u8; 3],
    pub keywords: &'static str,
}

const BLUE: [u8; 3] = [0x0A, 0x84, 0xFF];
const GRAY: [u8; 3] = [0x8E, 0x8E, 0x93];
const BLACK: [u8; 3] = [0x3A, 0x3A, 0x3C];

pub const PANES: &[Pane] = &[
    Pane { title: "Wi‑Fi", de: "WLAN", icon: "network-wireless", color: BLUE, keywords: "wifi wireless wlan internet network ssid" },
    Pane { title: "Bluetooth", de: "Bluetooth", icon: "bluetooth", color: BLUE, keywords: "devices headphones pair kopfhörer geräte" },
    Pane { title: "Network", de: "Netzwerk", icon: "network-workgroup", color: BLUE, keywords: "ethernet vpn ip proxy wired lan netzwerk" },
    Pane { title: "Sound", de: "Ton", icon: "audio-volume-high", color: [0xFF, 0x37, 0x5F], keywords: "volume audio output input microphone speakers mute lautstärke mikrofon" },
    Pane { title: "General", de: "Allgemein", icon: "emblem-system", color: GRAY, keywords: "about software system allgemein" },
    Pane { title: "Spotlight", de: "Spotlight", icon: "system-search", color: BLUE, keywords: "spotlight search launcher find files results web actions apps index" },
    Pane { title: "About", de: "Info", icon: "computer", color: GRAY, keywords: "system hardware cpu memory version hostname computer über" },
    Pane { title: "Date & Time", de: "Datum & Uhrzeit", icon: "preferences-system-time", color: BLUE, keywords: "clock time zone ntp 24-hour uhr zeitzone datum" },
    Pane { title: "Language & Region", de: "Sprache & Region", icon: "preferences-desktop-locale", color: BLUE, keywords: "locale region format sprache" },
    Pane { title: "Login Items", de: "Anmeldeobjekte", icon: "system-run", color: GRAY, keywords: "autostart startup anmeldung" },
    Pane { title: "Default Apps", de: "Standard-Apps", icon: "view-app-grid", color: [0xAF, 0x52, 0xDE], keywords: "browser mail web terminal file manager open with standard" },
    Pane { title: "Storage", de: "Speicher", icon: "drive-harddisk", color: GRAY, keywords: "disk space drive festplatte speicherplatz" },
    Pane { title: "Appearance", de: "Erscheinungsbild", icon: "preferences-desktop-appearance", color: BLACK, keywords: "dark light mode accent colour color theme dunkel hell akzentfarbe" },
    Pane { title: "Accessibility", de: "Bedienungshilfen", icon: "preferences-desktop-accessibility", color: BLUE, keywords: "reduce motion transparency animation" },
    Pane { title: "Menu Bar", de: "Menüleiste", icon: "open-menu", color: GRAY, keywords: "clock control center battery percentage date seconds menüleiste" },
    Pane { title: "Desktop & Dock", de: "Schreibtisch & Dock", icon: "view-app-grid", color: BLACK, keywords: "dock magnification minimize trash bin hot corners mission control windows title bar focus schreibtisch" },
    Pane { title: "Displays", de: "Displays", icon: "video-display", color: BLUE, keywords: "screen monitor resolution scale hidpi vrr freesync gsync adaptive sync bildschirm auflösung" },
    Pane { title: "Wallpaper", de: "Hintergrundbild", icon: "preferences-desktop-wallpaper", color: [0x32, 0xAD, 0xE6], keywords: "background desktop picture hintergrund" },
    Pane { title: "Lock Screen", de: "Sperrbildschirm", icon: "system-lock-screen", color: BLACK, keywords: "lock idle screensaver password sperren" },
    Pane { title: "Battery", de: "Batterie", icon: "battery-full", color: [0x30, 0xD1, 0x58], keywords: "power energy profile performance vrr latency scanout blur gaming akku energie leistung" },
    Pane { title: "Users & Groups", de: "Benutzer & Gruppen", icon: "system-users", color: BLUE, keywords: "account password user group benutzer konto" },
    Pane { title: "Keyboard", de: "Tastatur", icon: "input-keyboard", color: GRAY, keywords: "layout repeat shortcuts keys caps tastatur" },
    Pane { title: "Mouse", de: "Maus", icon: "input-mouse", color: GRAY, keywords: "pointer speed scroll natural acceleration maus zeiger" },
    Pane { title: "Trackpad", de: "Trackpad", icon: "input-touchpad", color: GRAY, keywords: "touchpad tap click gestures swipe gesten" },
];

pub enum CmdAction {
    Shell(&'static str),
    Wm(&'static str),
    LogOut,
}

/// A system command (Lock Screen, Restart …).
pub struct Cmd {
    pub id: &'static str,
    pub en: &'static str,
    pub de: &'static str,
    pub icon: &'static str,
    pub color: [u8; 3],
    pub keywords: &'static str,
    pub action: CmdAction,
}

pub const CMDS: &[Cmd] = &[
    Cmd {
        id: "lock",
        en: "Lock Screen",
        de: "Bildschirm sperren",
        icon: "system-lock-screen",
        color: BLACK,
        keywords: "lock sperren",
        action: CmdAction::Shell("${ORCHARD_LOCKER:-swaylock -f -c 1e1e1e}"),
    },
    Cmd {
        id: "sleep",
        en: "Sleep",
        de: "Ruhezustand",
        icon: "system-suspend",
        color: [0x5E, 0x5C, 0xE6],
        keywords: "sleep suspend standby ruhezustand",
        action: CmdAction::Shell("systemctl suspend"),
    },
    Cmd {
        id: "restart",
        en: "Restart",
        de: "Neustart",
        icon: "system-reboot",
        color: GRAY,
        keywords: "restart reboot neustart neu starten",
        action: CmdAction::Shell("systemctl reboot"),
    },
    Cmd {
        id: "shutdown",
        en: "Shut Down",
        de: "Ausschalten",
        icon: "system-shutdown",
        color: [0xFF, 0x45, 0x3A],
        keywords: "shut down shutdown power off poweroff ausschalten herunterfahren",
        action: CmdAction::Shell("systemctl poweroff"),
    },
    Cmd {
        id: "logout",
        en: "Log Out",
        de: "Abmelden",
        icon: "system-log-out",
        color: GRAY,
        keywords: "log out logout sign out abmelden",
        action: CmdAction::LogOut,
    },
    Cmd {
        id: "trash",
        en: "Empty Trash",
        de: "Papierkorb entleeren",
        icon: "user-trash",
        color: GRAY,
        keywords: "empty trash bin papierkorb leeren",
        action: CmdAction::Shell("gio trash --empty"),
    },
    Cmd {
        id: "screenshot",
        en: "Screenshot",
        de: "Bildschirmfoto",
        icon: "applets-screenshooter",
        color: GRAY,
        keywords: "screenshot capture screen bildschirmfoto foto",
        action: CmdAction::Wm("screenshot"),
    },
    Cmd {
        id: "mission-control",
        en: "Mission Control",
        de: "Mission Control",
        icon: "preferences-system-windows",
        color: [0x5A, 0xC8, 0xFA],
        keywords: "mission control windows spaces exposé fenster",
        action: CmdAction::Wm("mission-control"),
    },
];

// ---- usage memory ------------------------------------------------------------------

/// What Spotlight learned: how often and when things were opened, and what
/// was picked for a query.
#[derive(Default, Serialize, Deserialize)]
pub struct Usage {
    /// key → (count, last opened, seconds)
    #[serde(default)]
    launches: HashMap<String, (u32, i64)>,
    /// lowercase query → key picked for it
    #[serde(default)]
    picks: HashMap<String, String>,
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

impl Usage {
    fn path() -> std::path::PathBuf {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| files::home().join(".local/state"));
        base.join("orchard/spotlight.json")
    }

    pub fn load() -> Usage {
        std::fs::read_to_string(Self::path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    fn save(&self) {
        let p = Self::path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(t) = serde_json::to_string(self) {
            let tmp = p.with_extension("json.tmp");
            if std::fs::write(&tmp, t).is_ok() {
                let _ = std::fs::rename(tmp, p);
            }
        }
    }

    pub fn record(&mut self, query: &str, key: &str) {
        let now = now_secs();
        let e = self.launches.entry(key.to_string()).or_insert((0, now));
        e.0 = e.0.saturating_add(1);
        e.1 = now;
        let q = query.trim().to_lowercase();
        if !q.is_empty() {
            self.picks.insert(q, key.to_string());
        }
        // keep the file small
        if self.picks.len() > 600 {
            let drop: Vec<String> = self.picks.keys().take(100).cloned().collect();
            for k in drop {
                self.picks.remove(&k);
            }
        }
        if self.launches.len() > 1500 {
            let mut v: Vec<(String, i64)> = self.launches.iter().map(|(k, (_, t))| (k.clone(), *t)).collect();
            v.sort_by_key(|(_, t)| *t);
            for (k, _) in v.into_iter().take(300) {
                self.launches.remove(&k);
            }
        }
        self.save();
    }

    /// Extra rank for `key` when searching `query`.
    fn boost(&self, query: &str, key: &str) -> i64 {
        let q = query.trim().to_lowercase();
        let mut b = 0;
        if self.picks.get(&q).map(|k| k == key).unwrap_or(false) {
            b += 3000;
        } else if self.picks.iter().any(|(pq, k)| k == key && pq.starts_with(&q) && q.len() >= 1) {
            b += 700;
        }
        if let Some((count, last)) = self.launches.get(key) {
            b += (*count).min(40) as i64 * 8;
            let days = (now_secs() - last).max(0) / 86_400;
            b += match days {
                0 => 90,
                1..=7 => 50,
                8..=30 => 20,
                _ => 0,
            };
        }
        b
    }
}

// ---- results -----------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    Calc(&'static str),
    TopHit,
    Apps,
    Settings,
    Commands,
    Files(FileClass),
    Web,
}

impl Section {
    fn title(self, german: bool) -> String {
        let t = |en: &'static str, de: &'static str| tr(german, en, de).to_string();
        match self {
            Section::Calc("Calculator") => t("Calculator", "Rechner"),
            Section::Calc(kind) => format!("{} – {}", tr(german, "Conversion", "Umrechnung"), kind),
            Section::TopHit => t("Top Hit", "Toptreffer"),
            Section::Apps => t("Applications", "Programme"),
            Section::Settings => t("System Settings", "Systemeinstellungen"),
            Section::Commands => t("Actions", "Aktionen"),
            Section::Files(c) => match c {
                FileClass::Folder => t("Folders", "Ordner"),
                FileClass::Document => t("Documents", "Dokumente"),
                FileClass::Pdf => t("PDF Documents", "PDF-Dokumente"),
                FileClass::Image => t("Images", "Bilder"),
                FileClass::Movie => t("Movies", "Filme"),
                FileClass::Music => t("Music", "Musik"),
                FileClass::Code => t("Developer", "Entwickler"),
                FileClass::Archive => t("Archives", "Archive"),
                FileClass::Other => t("Other", "Andere"),
            },
            Section::Web => t("Web", "Web"),
        }
    }
}

#[derive(Clone)]
pub enum Target {
    App(usize),
    Setting(&'static Pane),
    Command(&'static Cmd),
    Calc(calc::Answer),
    File(FileHit),
    /// a typed path that exists ("~/Downloads")
    Path { path: String, is_dir: bool },
    Url(String),
    Web(String),
}

#[derive(Clone)]
pub struct Item {
    pub target: Target,
    pub title: String,
    /// kind shown next to the inline completion ("Application")
    pub kind: String,
    pub rank: i64,
    pub section: Section,
}

impl Item {
    /// Usage-memory key.
    fn key(&self, apps: &Apps) -> String {
        match &self.target {
            Target::App(i) => format!("app:{}", apps.entries.get(*i).map(|e| e.id.as_str()).unwrap_or("")),
            Target::Setting(p) => format!("pane:{}", p.title),
            Target::Command(c) => format!("cmd:{}", c.id),
            Target::Calc(_) => "calc".into(),
            Target::File(h) => format!("file:{}", h.entry.path),
            Target::Path { path, .. } => format!("file:{path}"),
            Target::Url(u) => format!("url:{u}"),
            Target::Web(_) => "web".into(),
        }
    }

    fn file(&self) -> Option<(&str, bool)> {
        match &self.target {
            Target::File(h) => Some((&h.entry.path, h.entry.is_dir)),
            Target::Path { path, is_dir } => Some((path, *is_dir)),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum RowKind {
    Header(usize),
    Item(usize),
}

#[derive(Clone, Copy, Debug)]
struct Row {
    kind: RowKind,
    y: f32,
    h: f32,
}

/// What the shell should do after a key press or click.
pub enum Act {
    App(usize),
    Spawn(String),
    Wm(&'static str),
    LogOut,
    Copy(String),
    Reveal(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Nothing,
    Redraw,
    /// the query changed: search again
    Changed,
    Close,
    Open { reveal: bool },
    Copy,
    Paste,
    WebSearch,
}

pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else if b == b' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn looks_like_url(q: &str) -> Option<String> {
    let q = q.trim();
    if q.contains(char::is_whitespace) || q.len() < 4 {
        return None;
    }
    if q.starts_with("http://") || q.starts_with("https://") || q.starts_with("file://") {
        return Some(q.to_string());
    }
    let host = q.split('/').next().unwrap_or(q);
    let tld = host.rsplit('.').next().unwrap_or("");
    let common = ["com", "org", "net", "de", "io", "dev", "app", "edu", "gov", "co", "uk", "eu", "info", "me", "at", "ch", "fr", "nl", "es", "tv", "gg", "xyz", "ly", "fm"];
    (host.contains('.') && !host.starts_with('.') && common.contains(&tld) && host.chars().all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == ':'))
        .then(|| format!("https://{q}"))
}

fn expand_path(q: &str) -> Option<String> {
    let q = q.trim();
    let p = if q == "~" {
        files::home().to_string_lossy().into_owned()
    } else if let Some(rest) = q.strip_prefix("~/") {
        files::home().join(rest).to_string_lossy().into_owned()
    } else if q.starts_with('/') {
        q.to_string()
    } else {
        return None;
    };
    std::path::Path::new(&p).exists().then_some(p)
}

// ---- the launcher --------------------------------------------------------------------

pub struct Launcher {
    pub query: String,
    /// byte offset of the caret in `query`
    cursor: usize,
    /// the whole query is selected (after reopening, ⌘A)
    sel_all: bool,
    pub items: Vec<Item>,
    rows: Vec<Row>,
    content_h: f32,
    pub selected: usize,
    hover: Option<usize>,
    scroll: f32,
    pub caret_on: bool,
    /// id of this opening (stale timers check it)
    pub gen: u64,
    shown_h: f32,
    last_step: Instant,
    last_click: Option<(usize, Instant)>,
    german: bool,
    /// recoloured symbolic icons
    tiles: RefCell<HashMap<(String, u32), Option<Rc<Pixmap>>>>,
}

impl Launcher {
    pub fn new(last_query: &str, gen: u64, german: bool) -> Launcher {
        Launcher {
            query: last_query.to_string(),
            cursor: last_query.len(),
            sel_all: !last_query.is_empty(),
            items: Vec::new(),
            rows: Vec::new(),
            content_h: 0.0,
            selected: 0,
            hover: None,
            scroll: 0.0,
            caret_on: true,
            gen,
            shown_h: FIELD_H + 1.0 + FOOTER_H,
            last_step: Instant::now(),
            last_click: None,
            german,
            tiles: RefCell::new(HashMap::new()),
        }
    }

    fn t(&self, en: &'static str, de: &'static str) -> &'static str {
        tr(self.german, en, de)
    }

    // ---- search ----------------------------------------------------------

    pub fn update(&mut self, apps: &Apps, index: Option<&Arc<FileIndex>>, usage: &Usage, cfg: &SearchSettings) {
        self.items = self.search(apps, index, usage, cfg);
        self.layout();
        // like macOS, every keystroke selects the (new) best result
        self.selected = 0;
        self.scroll = 0.0;
        self.hover = None;
    }

    fn search(&self, apps: &Apps, index: Option<&Arc<FileIndex>>, usage: &Usage, cfg: &SearchSettings) -> Vec<Item> {
        let q = self.query.trim();
        if q.is_empty() {
            return Vec::new();
        }
        let ql = q.to_lowercase();
        let german = self.german;
        let mut cands: Vec<Item> = Vec::new();

        // apps
        if cfg.show_apps {
            for (i, e) in apps.entries.iter().enumerate() {
                if e.no_display || e.exec.is_none() {
                    continue;
                }
                let mut best: Option<i64> = name_score(&e.name, q);
                let mut consider = |v: Option<i64>| {
                    if let Some(v) = v {
                        best = Some(best.map_or(v, |b| b.max(v)));
                    }
                };
                consider(e.generic_name.as_deref().and_then(|g| score(g, q)).filter(|s| *s >= 200).map(|s| s as i64 * 2));
                if ql.len() >= 2 {
                    consider(e.keywords.iter().any(|k| k.to_lowercase().starts_with(&ql)).then_some(560));
                    consider(score(&e.id, q).filter(|s| *s >= 300).map(|s| s as i64 * 2));
                }
                if ql.len() >= 3 {
                    consider(e.comment.as_deref().filter(|c| c.to_lowercase().contains(&ql)).map(|_| 260));
                }
                let Some(s) = best else { continue };
                let mut it = Item {
                    target: Target::App(i),
                    title: e.name.clone(),
                    kind: self.t("Application", "Programm").into(),
                    rank: s + 300,
                    section: Section::Apps,
                };
                it.rank += usage.boost(q, &it.key(apps));
                cands.push(it);
            }
        }

        // System Settings panes
        if cfg.show_settings {
            for p in PANES {
                let title = if german { p.de } else { p.title };
                let mut best = name_score(title, q).or_else(|| name_score(p.title, q));
                if ql.len() >= 2 && p.keywords.split_whitespace().any(|k| k.starts_with(&ql)) {
                    best = Some(best.unwrap_or(0).max(520));
                }
                let Some(s) = best.filter(|s| *s >= 300) else { continue };
                let mut it = Item {
                    target: Target::Setting(p),
                    title: title.to_string(),
                    kind: self.t("System Settings", "Systemeinstellungen").into(),
                    rank: s,
                    section: Section::Settings,
                };
                it.rank += usage.boost(q, &it.key(apps));
                cands.push(it);
            }
        }

        // commands
        if cfg.show_actions {
            for c in CMDS {
                let title = if german { c.de } else { c.en };
                let mut best = name_score(title, q).or_else(|| name_score(c.en, q));
                if ql.len() >= 2 && c.keywords.split_whitespace().any(|k| k.starts_with(&ql)) {
                    best = Some(best.unwrap_or(0).max(520));
                }
                let Some(s) = best.filter(|s| *s >= 600) else { continue };
                let mut it = Item {
                    target: Target::Command(c),
                    title: title.to_string(),
                    kind: self.t("Action", "Aktion").into(),
                    // "lock" means locking, not the Lock Screen settings
                    rank: s + 40,
                    section: Section::Commands,
                };
                it.rank += usage.boost(q, &it.key(apps));
                cands.push(it);
            }
        }

        // a typed path or URL
        if let Some(p) = expand_path(q) {
            let is_dir = std::path::Path::new(&p).is_dir();
            let name = std::path::Path::new(&p)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.clone());
            let class = index.map(|ix| ix.mime.class_of(&name, is_dir)).unwrap_or(if is_dir { FileClass::Folder } else { FileClass::Other });
            cands.push(Item {
                kind: if is_dir { self.t("Folder", "Ordner").into() } else { self.t("Document", "Dokument").into() },
                target: Target::Path { path: p, is_dir },
                title: name,
                rank: 9000,
                section: Section::Files(class),
            });
        } else if let Some(url) = looks_like_url(q) {
            cands.push(Item {
                target: Target::Url(url.clone()),
                title: q.to_string(),
                kind: self.t("Website", "Website").into(),
                rank: 8000,
                section: Section::Web,
            });
        }

        // files
        if cfg.show_files {
            if let Some(ix) = index {
                let per_class = 4;
                let mut counts: HashMap<FileClass, usize> = HashMap::new();
                for h in files::search(ix, q, 120) {
                    let c = counts.entry(h.class).or_insert(0);
                    if *c >= per_class {
                        continue;
                    }
                    *c += 1;
                    let name = h.entry.name().to_string();
                    let mut it = Item {
                        kind: ix.mime.kind_label(&name, h.entry.is_dir, german),
                        title: name,
                        rank: h.score,
                        section: Section::Files(h.class),
                        target: Target::File(h),
                    };
                    it.rank += usage.boost(q, &it.key(apps));
                    cands.push(it);
                }
            }
        }

        // order inside sections
        cands.sort_by(|a, b| b.rank.cmp(&a.rank).then_with(|| a.title.len().cmp(&b.title.len())));

        let mut out: Vec<Item> = Vec::new();

        // calculator / conversion first
        if let Some(ans) = calc::evaluate(q, german).or_else(|| calc::convert(q, german)) {
            out.push(Item {
                title: ans.value.clone(),
                kind: tr(german, "Calculator", "Rechner").into(),
                section: Section::Calc(ans.kind),
                target: Target::Calc(ans),
                rank: i64::MAX,
            });
        }

        // the Top Hit: the best result of any kind (URLs are their own thing)
        if let Some(pos) = cands.iter().position(|c| !matches!(c.target, Target::Url(_))) {
            if out.is_empty() || cands[pos].rank >= 1500 {
                let mut top = cands.remove(pos);
                top.section = Section::TopHit;
                out.push(top);
            }
        }
        // a URL goes right after the top hit
        if let Some(pos) = cands.iter().position(|c| matches!(c.target, Target::Url(_))) {
            let u = cands.remove(pos);
            out.push(u);
        }

        let take = |sec: Section, max: usize, cands: &mut Vec<Item>, out: &mut Vec<Item>| {
            let mut n = 0;
            let mut i = 0;
            while i < cands.len() {
                if cands[i].section == sec && n < max {
                    out.push(cands.remove(i));
                    n += 1;
                } else {
                    i += 1;
                }
            }
        };
        take(Section::Apps, 5, &mut cands, &mut out);
        take(Section::Settings, 3, &mut cands, &mut out);
        take(Section::Commands, 3, &mut cands, &mut out);
        // file sections, best section first
        let mut classes: Vec<(FileClass, i64)> = Vec::new();
        for c in &cands {
            if let Section::Files(fc) = c.section {
                if !classes.iter().any(|(x, _)| *x == fc) {
                    classes.push((fc, c.rank));
                }
            }
        }
        classes.sort_by(|a, b| b.1.cmp(&a.1));
        for (fc, _) in classes {
            take(Section::Files(fc), 4, &mut cands, &mut out);
        }

        if cfg.show_web {
            out.push(Item {
                target: Target::Web(q.to_string()),
                title: if german { format!("„{q}“ im Web suchen") } else { format!("Search the web for “{q}”") },
                kind: self.t("Web search", "Websuche").into(),
                rank: 0,
                section: Section::Web,
            });
        }
        // a lone URL item and the web search share the Web section; keep the
        // order: URL, then search
        out
    }

    fn layout(&mut self) {
        self.rows.clear();
        let mut y = LIST_PAD;
        let mut last: Option<Section> = None;
        for (i, it) in self.items.iter().enumerate() {
            if last != Some(it.section) {
                self.rows.push(Row { kind: RowKind::Header(i), y, h: HEADER_H });
                y += HEADER_H;
                last = Some(it.section);
            }
            let h = if it.section == Section::TopHit || matches!(it.section, Section::Calc(_)) { TOP_ROW_H } else { ROW_H };
            self.rows.push(Row { kind: RowKind::Item(i), y, h });
            y += h;
        }
        self.content_h = y + LIST_PAD;
    }

    /// Height of the result area right now: the content, up to the maximum.
    fn view_h(&self) -> f32 {
        self.content_h.min(BODY_H)
    }

    pub fn target_height(&self) -> f32 {
        if self.items.is_empty() {
            FIELD_H + 1.0 + FOOTER_H
        } else {
            FIELD_H + 1.0 + self.view_h() + FOOTER_H
        }
    }

    pub fn shown_height(&self) -> f32 {
        self.shown_h
    }

    pub fn selected_item(&self) -> Option<&Item> {
        self.items.get(self.selected)
    }

    /// Advance the result-list resize animation. Returns true while moving.
    pub fn step(&mut self) -> bool {
        let now = Instant::now();
        let dt = now.duration_since(self.last_step).as_secs_f32().min(0.05);
        self.last_step = now;
        let target = self.target_height();
        let k = 1.0 - (-dt * 42.0).exp();
        self.shown_h += (target - self.shown_h) * k;
        if (target - self.shown_h).abs() < 0.5 {
            self.shown_h = target;
        }
        self.shown_h != target
    }

    // ---- selection ----------------------------------------------------------

    fn row_of(&self, item: usize) -> Option<&Row> {
        self.rows.iter().find(|r| matches!(r.kind, RowKind::Item(i) if i == item))
    }

    fn ensure_visible(&mut self) {
        let body = BODY_H;
        let Some(r) = self.row_of(self.selected).copied() else { return };
        // include the section header when the item is the first in it
        let mut top = r.y;
        if let Some(h) = self.rows.iter().find(|h| matches!(h.kind, RowKind::Header(i) if i == self.selected)) {
            top = h.y;
        }
        if self.selected == 0 {
            top = 0.0;
        }
        if top < self.scroll {
            self.scroll = top;
        } else if r.y + r.h + LIST_PAD > self.scroll + body {
            self.scroll = r.y + r.h + LIST_PAD - body;
        }
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        self.scroll = self.scroll.clamp(0.0, (self.content_h - BODY_H).max(0.0));
    }

    pub fn select(&mut self, i: usize) {
        if i < self.items.len() {
            self.selected = i;
            self.ensure_visible();
        }
    }

    fn jump_section(&mut self, forward: bool) {
        if self.items.is_empty() {
            return;
        }
        let cur = self.items[self.selected].section;
        if forward {
            if let Some(i) = (self.selected + 1..self.items.len()).find(|&i| self.items[i].section != cur) {
                self.select(i);
            } else {
                self.select(self.items.len() - 1);
            }
        } else {
            // start of this section, or of the previous one if already there
            let start = (0..=self.selected).rev().take_while(|&i| self.items[i].section == cur).last().unwrap_or(0);
            if start < self.selected {
                self.select(start);
            } else if start > 0 {
                let prev = self.items[start - 1].section;
                let ps = (0..start).rev().take_while(|&i| self.items[i].section == prev).last().unwrap_or(0);
                self.select(ps);
            }
        }
    }

    pub fn scroll_by(&mut self, dy: f32) {
        self.scroll += dy;
        self.clamp_scroll();
    }

    // ---- text editing --------------------------------------------------------------

    fn prev_boundary(&self, i: usize) -> usize {
        self.query[..i].char_indices().last().map(|(p, _)| p).unwrap_or(0)
    }

    fn next_boundary(&self, i: usize) -> usize {
        self.query[i..].chars().next().map(|c| i + c.len_utf8()).unwrap_or(i)
    }

    fn word_left(&self, i: usize) -> usize {
        let s = &self.query[..i];
        let trimmed = s.trim_end_matches(|c: char| !c.is_alphanumeric());
        trimmed.rfind(|c: char| !c.is_alphanumeric()).map(|p| p + trimmed[p..].chars().next().map(|c| c.len_utf8()).unwrap_or(1)).unwrap_or(0)
    }

    fn word_right(&self, i: usize) -> usize {
        let s = &self.query[i..];
        let skip = s.len() - s.trim_start_matches(|c: char| !c.is_alphanumeric()).len();
        let rest = &s[skip..];
        i + skip + rest.find(|c: char| !c.is_alphanumeric()).unwrap_or(rest.len())
    }

    pub fn insert(&mut self, text: &str) {
        if self.sel_all {
            self.query.clear();
            self.cursor = 0;
            self.sel_all = false;
        }
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        self.query.insert_str(self.cursor, &text);
        self.cursor += text.len();
        self.caret_on = true;
    }

    /// The text the top hit would complete the query to (the rest of its name).
    pub fn completion(&self) -> Option<String> {
        if self.sel_all || self.cursor != self.query.len() || self.query.is_empty() || self.selected != self.first_hit()? {
            return None;
        }
        let it = self.items.get(self.selected)?;
        if !matches!(it.target, Target::App(_) | Target::Setting(_) | Target::Command(_) | Target::File(_)) {
            return None;
        }
        let ql = self.query.to_lowercase();
        let tl = it.title.to_lowercase();
        if tl.starts_with(&ql) && tl.len() > ql.len() {
            // the title's own letters after the typed prefix
            let n = self.query.chars().count();
            Some(it.title.chars().skip(n).collect())
        } else {
            None
        }
    }

    fn first_hit(&self) -> Option<usize> {
        self.items.iter().position(|i| i.section == Section::TopHit)
    }

    fn accept_completion(&mut self) -> bool {
        if let Some(rest) = self.completion() {
            self.query.push_str(&rest);
            self.cursor = self.query.len();
            true
        } else {
            false
        }
    }

    /// Handle a key. `cmd` is ⌘ or Ctrl (Linux habits work too).
    pub fn key(&mut self, sym: Keysym, utf8: Option<&str>, mods: &Modifiers) -> Outcome {
        let cmd = mods.logo || mods.ctrl;
        let word = mods.alt || mods.ctrl;
        self.caret_on = true;
        match sym {
            Keysym::Escape => {
                if self.query.is_empty() {
                    Outcome::Close
                } else {
                    self.query.clear();
                    self.cursor = 0;
                    self.sel_all = false;
                    Outcome::Changed
                }
            }
            Keysym::Return | Keysym::KP_Enter => Outcome::Open { reveal: cmd },
            Keysym::Down | Keysym::KP_Down => {
                if cmd {
                    self.jump_section(true);
                } else if self.selected + 1 < self.items.len() {
                    self.select(self.selected + 1);
                }
                Outcome::Redraw
            }
            Keysym::Up | Keysym::KP_Up => {
                if cmd {
                    self.jump_section(false);
                } else if self.selected > 0 {
                    self.select(self.selected - 1);
                }
                Outcome::Redraw
            }
            Keysym::Page_Down => {
                let n = self.items.len().saturating_sub(1);
                self.select((self.selected + 10).min(n));
                Outcome::Redraw
            }
            Keysym::Page_Up => {
                self.select(self.selected.saturating_sub(10));
                Outcome::Redraw
            }
            Keysym::Tab => {
                if self.accept_completion() {
                    Outcome::Changed
                } else {
                    // Tab moves through the results like ↓
                    if mods.shift {
                        self.select(self.selected.saturating_sub(1));
                    } else if self.selected + 1 < self.items.len() {
                        self.select(self.selected + 1);
                    }
                    Outcome::Redraw
                }
            }
            Keysym::ISO_Left_Tab => {
                self.select(self.selected.saturating_sub(1));
                Outcome::Redraw
            }
            Keysym::Left | Keysym::KP_Left => {
                if self.sel_all {
                    self.sel_all = false;
                    self.cursor = 0;
                } else if mods.logo {
                    self.cursor = 0;
                } else if word {
                    self.cursor = self.word_left(self.cursor);
                } else {
                    self.cursor = self.prev_boundary(self.cursor);
                }
                Outcome::Redraw
            }
            Keysym::Right | Keysym::KP_Right => {
                if self.sel_all {
                    self.sel_all = false;
                    self.cursor = self.query.len();
                    Outcome::Redraw
                } else if self.cursor == self.query.len() {
                    if self.accept_completion() {
                        Outcome::Changed
                    } else {
                        Outcome::Redraw
                    }
                } else {
                    self.cursor = if mods.logo {
                        self.query.len()
                    } else if word {
                        self.word_right(self.cursor)
                    } else {
                        self.next_boundary(self.cursor)
                    };
                    Outcome::Redraw
                }
            }
            Keysym::Home | Keysym::KP_Home => {
                self.sel_all = false;
                self.cursor = 0;
                Outcome::Redraw
            }
            Keysym::End | Keysym::KP_End => {
                self.sel_all = false;
                self.cursor = self.query.len();
                Outcome::Redraw
            }
            Keysym::BackSpace => {
                if self.sel_all {
                    self.query.clear();
                    self.cursor = 0;
                    self.sel_all = false;
                } else if self.cursor > 0 {
                    let from = if mods.logo {
                        0
                    } else if word {
                        self.word_left(self.cursor)
                    } else {
                        self.prev_boundary(self.cursor)
                    };
                    self.query.replace_range(from..self.cursor, "");
                    self.cursor = from;
                } else {
                    return Outcome::Nothing;
                }
                Outcome::Changed
            }
            Keysym::Delete | Keysym::KP_Delete => {
                if self.sel_all {
                    self.query.clear();
                    self.cursor = 0;
                    self.sel_all = false;
                } else if self.cursor < self.query.len() {
                    let to = if word { self.word_right(self.cursor) } else { self.next_boundary(self.cursor) };
                    self.query.replace_range(self.cursor..to, "");
                } else {
                    return Outcome::Nothing;
                }
                Outcome::Changed
            }
            _ if cmd => {
                let k = utf8.map(|s| s.to_lowercase()).unwrap_or_default();
                // Ctrl turns letters into control characters: use the keysym
                let name = sym.name().unwrap_or("").to_lowercase();
                let is = |c: &str| k == c || name == c;
                if is("a") {
                    self.sel_all = !self.query.is_empty();
                    self.cursor = self.query.len();
                    Outcome::Redraw
                } else if is("c") {
                    Outcome::Copy
                } else if is("v") {
                    Outcome::Paste
                } else if is("x") && self.sel_all {
                    Outcome::Copy
                } else if is("b") {
                    Outcome::WebSearch
                } else if is("u") {
                    self.query.replace_range(..self.cursor, "");
                    self.cursor = 0;
                    self.sel_all = false;
                    Outcome::Changed
                } else if is("w") && mods.ctrl {
                    let from = self.word_left(self.cursor);
                    self.query.replace_range(from..self.cursor, "");
                    self.cursor = from;
                    Outcome::Changed
                } else {
                    Outcome::Nothing
                }
            }
            _ => match utf8.filter(|t| !t.is_empty() && !t.chars().any(|c| c.is_control())) {
                Some(text) => {
                    self.insert(text);
                    Outcome::Changed
                }
                None => Outcome::Nothing,
            },
        }
    }

    /// The text ⌘C copies: the selected text, or the selected result.
    pub fn copy_text(&self, apps: &Apps) -> Option<String> {
        if self.sel_all && !self.query.is_empty() {
            return Some(self.query.clone());
        }
        let it = self.selected_item()?;
        Some(match &it.target {
            Target::Calc(a) => a.plain.clone(),
            Target::File(h) => h.entry.path.to_string(),
            Target::Path { path, .. } => path.clone(),
            Target::Url(u) => u.clone(),
            Target::App(i) => apps.entries.get(*i)?.name.clone(),
            Target::Web(q) => q.clone(),
            _ => it.title.clone(),
        })
    }

    /// What opening the selected result does, and its usage key.
    pub fn action(&self, apps: &Apps, reveal: bool) -> Option<(Act, String)> {
        let it = self.selected_item()?;
        let key = it.key(apps);
        let act = match &it.target {
            Target::App(i) => Act::App(*i),
            Target::Setting(p) => Act::Spawn(format!("orchard-settings {}", sh_quote(p.title))),
            Target::Command(c) => match c.action {
                CmdAction::Shell(s) => Act::Spawn(s.to_string()),
                CmdAction::Wm(w) => Act::Wm(w),
                CmdAction::LogOut => Act::LogOut,
            },
            Target::Calc(a) => Act::Copy(a.plain.clone()),
            Target::File(_) | Target::Path { .. } => {
                let (path, _) = it.file()?;
                if reveal {
                    Act::Reveal(path.to_string())
                } else {
                    Act::Spawn(format!("xdg-open {}", sh_quote(path)))
                }
            }
            Target::Url(u) => Act::Spawn(format!("xdg-open {}", sh_quote(u))),
            Target::Web(q) => Act::Spawn(web_search_cmd(q)),
        };
        Some((act, key))
    }

    // ---- pointer -------------------------------------------------------------------

    /// Item under a point in panel coordinates.
    pub fn item_at(&self, x: f32, y: f32) -> Option<usize> {
        let body_top = FIELD_H + 1.0;
        let body_h = (self.shown_h - body_top - FOOTER_H).max(0.0);
        if x < 0.0 || x > WIDTH || y < body_top || y > body_top + body_h {
            return None;
        }
        let ly = y - body_top + self.scroll;
        self.rows.iter().find_map(|r| match r.kind {
            RowKind::Item(i) if ly >= r.y && ly < r.y + r.h => Some(i),
            _ => None,
        })
    }

    pub fn set_hover(&mut self, h: Option<usize>) -> bool {
        let changed = self.hover != h;
        self.hover = h;
        changed
    }

    /// A click on a result: select it; a second click on the same result
    /// within the double-click time opens it.
    pub fn click(&mut self, i: usize) -> bool {
        let now = Instant::now();
        let double = matches!(self.last_click, Some((j, t)) if j == i && now.duration_since(t).as_millis() < 450);
        self.last_click = Some((i, now));
        self.select(i);
        double
    }

    pub fn click_field(&mut self) {
        self.sel_all = false;
        self.cursor = self.query.len();
        self.caret_on = true;
    }

    // ---- painting ---------------------------------------------------------------------

    /// Paint the panel with its top-left at (x, y) on a full-output surface.
    /// Drawing goes straight into the target: no intermediate panel buffer and
    /// no open/close fade, so a keystroke costs one small list canvas.
    pub fn paint(&self, pm: &mut Pixmap, x: f32, y: f32, scale: f32, apps: &Apps, fonts: &Fonts, theme: &Theme) {
        let s = scale;
        let h = self.shown_h.max(FIELD_H + 1.0 + FOOTER_H);
        let pal = Palette::new(theme);

        self.paint_shadow(pm, s, x, y, h, &pal);
        let (px, py) = (x * s, y * s);

        // glass + borders
        fill_rounded(pm, px, py, WIDTH * s, h * s, RADIUS * s, pal.bg);
        stroke_rounded(pm, px + 0.5 * s, py + 0.5 * s, (WIDTH - 1.0) * s, (h - 1.0) * s, (RADIUS - 0.5) * s, 1.0 * s, pal.inner);
        stroke_rounded(pm, px - 0.5 * s, py - 0.5 * s, (WIDTH + 1.0) * s, (h + 1.0) * s, (RADIUS + 0.5) * s, 1.0 * s, pal.outer);

        self.paint_field(pm, px, py, s, apps, fonts, &pal);
        fill_rounded(pm, px, py + FIELD_H * s, WIDTH * s, (1.0 * s).max(1.0), 0.0, pal.sep);

        let body_top = FIELD_H + 1.0;
        let body_h = (h - body_top - FOOTER_H).max(0.0);
        if body_h > 0.5 && !self.items.is_empty() {
            // the list is the only off-screen canvas: it clips rows while the
            // panel grows or shrinks
            if let Some(mut list) = Pixmap::new((WIDTH * s).ceil() as u32, (body_h * s).ceil() as u32) {
                self.paint_list(&mut list, s, apps, fonts, &pal);
                pm.draw_pixmap(px as i32, ((y + body_top) * s) as i32, list.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
            }
        }

        self.paint_footer(pm, x, y, h, s, fonts, &pal);
    }

    /// A soft, slightly lowered shadow (nine-slice of one blurred sprite),
    /// with the panel's own area cut out so the glass stays clear.
    fn paint_shadow(&self, pm: &mut Pixmap, s: f32, x: f32, y: f32, h: f32, pal: &Palette) {
        let blur = (22.0 * s).round().max(2.0) as usize;
        let r = (RADIUS * s).round() as usize;
        let key = (s * 1000.0) as u32 ^ ((pal.shadow as u32) << 20);
        let sprite = {
            thread_local! {
                static SHADOW_CACHE: RefCell<Option<(u32, Rc<Pixmap>)>> = const { RefCell::new(None) };
            }
            SHADOW_CACHE.with(|cell| {
                let mut cache = cell.borrow_mut();
                if cache.as_ref().map(|(k, _)| *k != key).unwrap_or(true) {
                    *cache = shadow_sprite(blur, r, pal.shadow).map(|p| (key, Rc::new(p)));
                }
                cache.as_ref().map(|(_, p)| p.clone())
            })
        };
        let Some(sprite) = sprite else { return };
        let c = blur + r + 1; // corner piece size
        let full = sprite.width() as usize; // 2c + 1 … middle column at c
        let x0 = (x * s) as i32 - blur as i32;
        let y0 = ((y + 9.0) * s) as i32 - blur as i32;
        let w = (WIDTH * s) as i32 + 2 * blur as i32;
        let hh = (h * s) as i32 + 2 * blur as i32;
        let cut = |x: usize, y: usize, cw: usize, ch: usize| -> Option<Pixmap> {
            sprite.clone_rect(tiny_skia::IntRect::from_xywh(x as i32, y as i32, cw as u32, ch as u32)?)
        };
        let paint_px = PixmapPaint {
            quality: FilterQuality::Nearest,
            ..Default::default()
        };
        let c32 = c as i32;
        if w < 2 * c32 || hh < 2 * c32 {
            return;
        }
        // corners
        for (sx, sy, dx, dy) in [(0, 0, x0, y0), (full - c, 0, x0 + w - c32, y0), (0, full - c, x0, y0 + hh - c32), (full - c, full - c, x0 + w - c32, y0 + hh - c32)] {
            if let Some(p) = cut(sx, sy, c, c) {
                pm.draw_pixmap(dx, dy, p.as_ref(), &paint_px, Transform::identity(), None);
            }
        }
        // edges, stretched from the 1px middle row / column
        let mid = c;
        let ew = (w - 2 * c32) as f32;
        let eh = (hh - 2 * c32) as f32;
        if let Some(p) = cut(mid, 0, 1, c) {
            pm.draw_pixmap(0, 0, p.as_ref(), &paint_px, Transform::from_row(ew, 0.0, 0.0, 1.0, (x0 + c32) as f32, y0 as f32), None);
        }
        if let Some(p) = cut(mid, full - c, 1, c) {
            pm.draw_pixmap(0, 0, p.as_ref(), &paint_px, Transform::from_row(ew, 0.0, 0.0, 1.0, (x0 + c32) as f32, (y0 + hh - c32) as f32), None);
        }
        if let Some(p) = cut(0, mid, c, 1) {
            pm.draw_pixmap(0, 0, p.as_ref(), &paint_px, Transform::from_row(1.0, 0.0, 0.0, eh, x0 as f32, (y0 + c32) as f32), None);
        }
        if let Some(p) = cut(full - c, mid, c, 1) {
            pm.draw_pixmap(0, 0, p.as_ref(), &paint_px, Transform::from_row(1.0, 0.0, 0.0, eh, (x0 + w - c32) as f32, (y0 + c32) as f32), None);
        }
        // the centre stays empty; cut the panel's shape out of the shadow
        if let Some(path) = rounded_rect(x * s, y * s, WIDTH * s, h * s, RADIUS * s) {
            let mut clear = paint([0, 0, 0, 255]);
            clear.blend_mode = BlendMode::Clear;
            pm.fill_path(&path, &clear, FillRule::Winding, Transform::identity(), None);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_field(&self, pm: &mut Pixmap, px: f32, py: f32, s: f32, _apps: &Apps, fonts: &Fonts, pal: &Palette) {
        // magnifying glass
        let gx = px + 21.0 * s;
        let gy = py + (FIELD_H / 2.0 - 1.0) * s;
        let st = Stroke {
            width: 1.9 * s,
            line_cap: tiny_skia::LineCap::Round,
            ..Default::default()
        };
        if let Some(circle) = tiny_skia::PathBuilder::from_circle(gx, gy, 6.4 * s) {
            pm.stroke_path(&circle, &paint(pal.tertiary), &st, Transform::identity(), None);
        }
        let mut pb = tiny_skia::PathBuilder::new();
        pb.move_to(gx + 4.8 * s, gy + 4.8 * s);
        pb.line_to(gx + 9.2 * s, gy + 9.2 * s);
        if let Some(p) = pb.finish() {
            pm.stroke_path(&p, &paint(pal.tertiary), &st, Transform::identity(), None);
        }

        // text, clipped to the field (scrolls left to keep the caret visible)
        let tx0 = 40.0;
        let avail = WIDTH - tx0 - 16.0;
        let fpx = 17.0 * s;
        let Some(mut fld) = Pixmap::new((avail * s).ceil() as u32, (FIELD_H * s).ceil() as u32) else { return };
        let base = fonts.baseline_centered(0.0, FIELD_H * s, fpx, Weight::Regular);
        if self.query.is_empty() {
            fonts.draw(&mut fld, self.t("Search apps, files and actions…", "Apps, Dateien und Aktionen suchen …"), 0.0, base, fpx, Weight::Regular, pal.tertiary);
        } else {
            let full_w = fonts.measure(&self.query, fpx, Weight::Regular);
            let caret_x = fonts.measure(&self.query[..self.cursor], fpx, Weight::Regular);
            let room = avail * s - 4.0 * s;
            let shift = if caret_x > room { caret_x - room } else { 0.0 };
            let tx = -shift;
            if self.sel_all {
                let sel = [pal.accent[0], pal.accent[1], pal.accent[2], 80];
                fill_rounded(&mut fld, tx - 1.0 * s, base - 15.0 * s, full_w + 2.0 * s, 21.0 * s, 3.0 * s, sel);
            }
            fonts.draw(&mut fld, &self.query, tx, base, fpx, Weight::Regular, pal.text);
            // inline completion: the rest of the top hit's name, dimmed
            if let Some(rest) = self.completion() {
                let after = tx + full_w;
                fonts.draw(&mut fld, &rest, after, base, fpx, Weight::Regular, pal.secondary);
            }
            if self.caret_on && !self.sel_all {
                fill_rounded(&mut fld, (tx + caret_x).round(), base - 14.5 * s, (2.0 * s).max(2.0), 20.0 * s, 1.0 * s, pal.accent);
            }
        }
        if self.caret_on && self.query.is_empty() {
            fill_rounded(&mut fld, 0.0, base - 14.5 * s, (2.0 * s).max(2.0), 20.0 * s, 1.0 * s, pal.accent);
        }
        pm.draw_pixmap((px + tx0 * s) as i32, py as i32, fld.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
    }

    fn paint_list(&self, pm: &mut Pixmap, s: f32, apps: &Apps, fonts: &Fonts, pal: &Palette) {
        let view_h = pm.height() as f32 / s;
        for r in &self.rows {
            let y = r.y - self.scroll;
            if y + r.h < 0.0 || y > view_h {
                continue;
            }
            match r.kind {
                RowKind::Header(i) => {
                    let title = self.items[i].section.title(self.german);
                    let hpx = 10.5 * s;
                    let bl = fonts.baseline_centered((y + 2.0) * s, (r.h - 2.0) * s, hpx, Weight::Bold);
                    fonts.draw(pm, &title, 15.0 * s, bl, hpx, Weight::Bold, pal.tertiary);
                }
                RowKind::Item(i) => {
                    let it = &self.items[i];
                    let sel = i == self.selected;
                    let rh = r.h - 2.0;
                    if sel {
                        fill_rounded(pm, 8.0 * s, (y + 1.0) * s, (WIDTH - 16.0) * s, rh * s, 6.0 * s, pal.sel_bg);
                        stroke_rounded(pm, 8.5 * s, (y + 1.5) * s, (WIDTH - 17.0) * s, (rh - 1.0) * s, 5.5 * s, 1.0 * s, pal.sel_edge);
                    } else if self.hover == Some(i) {
                        fill_rounded(pm, 8.0 * s, (y + 1.0) * s, (WIDTH - 16.0) * s, rh * s, 6.0 * s, pal.hover);
                    }
                    let big = r.h >= TOP_ROW_H;
                    let isz = if big { 24.0 } else { 18.0 };
                    let ix = 14.0;
                    self.draw_item_icon(pm, it, ix * s, (y + (r.h - isz) / 2.0) * s, isz, s, apps, fonts);
                    let tx = ix + isz + 10.0;
                    let mut right = WIDTH - 14.0;
                    if big {
                        let tpx = 13.5 * s;
                        let spx = 10.5 * s;
                        let maxw = (right - tx) * s;
                        let title = fonts.elide(&it.title, tpx, Weight::Regular, maxw);
                        fonts.draw(pm, &title, tx * s, (y + 18.0) * s, tpx, Weight::Regular, pal.text);
                        let sub = match &it.target {
                            Target::Calc(a) => a.expr.clone(),
                            _ => it.kind.clone(),
                        };
                        let sub = fonts.elide(&sub, spx, Weight::Regular, maxw);
                        fonts.draw(pm, &sub, tx * s, (y + 33.0) * s, spx, Weight::Regular, pal.secondary);
                    } else {
                        let tpx = 13.0 * s;
                        let bl = fonts.baseline_centered(y * s, r.h * s, tpx, Weight::Regular);
                        // a file's folder, right-aligned and dimmed
                        if let Some((path, _)) = it.file() {
                            let parent = parent_name(path, self.german);
                            if !parent.is_empty() {
                                let ppx = 10.5 * s;
                                let ptext = format!("— {parent}");
                                let pw = fonts.measure(&ptext, ppx, Weight::Regular);
                                if pw < (right - tx - 70.0) * s {
                                    right -= pw / s + 10.0;
                                    let p = fonts.elide(&ptext, ppx, Weight::Regular, pw + 2.0 * s);
                                    fonts.draw(pm, &p, right * s, bl, ppx, Weight::Regular, pal.tertiary);
                                }
                            }
                        }
                        let maxw = (right - tx) * s;
                        let title = fonts.elide(&it.title, tpx, Weight::Regular, maxw);
                        fonts.draw(pm, &title, tx * s, bl, tpx, Weight::Regular, pal.text);
                    }
                }
            }
        }
        // scroll indicator
        if self.content_h > view_h + 1.0 {
            let track = view_h - 12.0;
            let bar = (view_h / self.content_h * track).max(28.0);
            let pos = self.scroll / (self.content_h - view_h) * (track - bar);
            fill_rounded(pm, (WIDTH - 7.0) * s, (6.0 + pos) * s, 3.0 * s, bar * s, 1.5 * s, pal.scrollbar);
        }
    }

    /// Keycaps shown in the footer for the selected result.
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        let Some(it) = self.selected_item() else { return Vec::new() };
        let t = |en, de| self.t(en, de);
        match &it.target {
            Target::Calc(_) => vec![("⏎", t("Copy Result", "Ergebnis kopieren"))],
            Target::File(_) | Target::Path { .. } => vec![
                ("⏎", t("Open", "Öffnen")),
                ("⌘⏎", t("Show in Folder", "Im Ordner zeigen")),
                ("⌘C", t("Copy Path", "Pfad kopieren")),
            ],
            Target::App(_) => vec![("⏎", t("Open", "Öffnen"))],
            Target::Setting(_) => vec![("⏎", t("Open Settings", "Einstellungen öffnen"))],
            Target::Command(_) => vec![("⏎", t("Run", "Ausführen"))],
            Target::Url(_) => vec![("⏎", t("Open in Browser", "Im Browser öffnen"))],
            Target::Web(_) => vec![("⏎", t("Search the Web", "Im Web suchen"))],
        }
    }

    /// A quiet action strip at the bottom: keycaps on the left, Esc on the right.
    fn paint_footer(&self, pm: &mut Pixmap, x: f32, y: f32, h: f32, s: f32, fonts: &Fonts, pal: &Palette) {
        if h < FIELD_H + 1.0 + FOOTER_H {
            return;
        }
        let fy = y + h - FOOTER_H;
        fill_rounded(pm, x * s, fy * s, WIDTH * s, (1.0 * s).max(1.0), 0.0, pal.sep);
        let base = fonts.baseline_centered(fy * s, FOOTER_H * s, 10.5 * s, Weight::Regular);
        let hints = self.hints();
        let mut cx = x + 13.0;
        if hints.is_empty() {
            let label = if self.query.trim().is_empty() {
                self.t("Type to search", "Zum Suchen tippen")
            } else {
                self.t("No Results", "Keine Treffer")
            };
            fonts.draw(pm, label, cx * s, base, 11.0 * s, Weight::Regular, pal.tertiary);
        } else {
            for (key, label) in hints {
                cx = self.keycap(pm, fonts, pal, cx, fy, s, key);
                let lw = fonts.measure(label, 10.5 * s, Weight::Regular);
                fonts.draw(pm, label, cx * s, base, 10.5 * s, Weight::Regular, pal.secondary);
                cx += lw / s + 14.0;
            }
        }
        // Esc, right-aligned
        let esc = "esc";
        let label = self.t("Close", "Schließen");
        let kw = fonts.measure(esc, 9.5 * s, Weight::Bold) + 8.0 * s;
        let lw = fonts.measure(label, 10.5 * s, Weight::Regular);
        let sx = x + WIDTH - 13.0 - (kw + 5.0 * s + lw) / s;
        let kx = self.keycap(pm, fonts, pal, sx, fy, s, esc);
        fonts.draw(pm, label, kx * s, base, 10.5 * s, Weight::Regular, pal.secondary);
    }

    /// Draw one keycap at `cx`; returns the x after it (plus a small gap).
    fn keycap(&self, pm: &mut Pixmap, fonts: &Fonts, pal: &Palette, cx: f32, fy: f32, s: f32, key: &str) -> f32 {
        let kpx = 9.5 * s;
        let kw = fonts.measure(key, kpx, Weight::Bold) + 8.0 * s;
        let kh = 15.0 * s;
        let top = fy * s + (FOOTER_H * s - kh) / 2.0;
        fill_rounded(pm, cx * s, top, kw, kh, 3.5 * s, pal.keycap);
        let bl = fonts.baseline_centered(top, kh, kpx, Weight::Bold);
        fonts.draw(pm, key, cx * s + 4.0 * s, bl, kpx, Weight::Bold, pal.secondary);
        cx + (kw / s).round() + 5.0
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_item_icon(&self, pm: &mut Pixmap, it: &Item, x: f32, y: f32, size: f32, s: f32, apps: &Apps, fonts: &Fonts) {
        match &it.target {
            Target::App(i) => {
                let e = &apps.entries[*i];
                draw_named_icon(pm, apps, &[e.icon.clone().unwrap_or_default(), "application-x-executable".into()], x, y, size, s);
            }
            Target::Setting(p) => self.draw_tile(pm, apps, p.icon, p.color, x, y, size, s, fonts, &it.title),
            Target::Command(c) => self.draw_tile(pm, apps, c.icon, c.color, x, y, size, s, fonts, &it.title),
            Target::Calc(_) => {
                if !draw_named_icon(pm, apps, &["accessories-calculator".into(), "org.gnome.Calculator".into()], x, y, size, s) {
                    self.draw_tile(pm, apps, "accessories-calculator", [0xFF, 0x9F, 0x0A], x, y, size, s, fonts, "=");
                }
            }
            Target::File(h) => {
                let names = file_icons(&h.entry.path, h.entry.is_dir, None);
                draw_named_icon(pm, apps, &names, x, y, size, s);
            }
            Target::Path { path, is_dir } => {
                let names = file_icons(path, *is_dir, None);
                draw_named_icon(pm, apps, &names, x, y, size, s);
            }
            Target::Url(_) | Target::Web(_) => {
                draw_named_icon(pm, apps, &["web-browser".into(), "internet-web-browser".into(), "applications-internet".into(), "globe".into()], x, y, size, s);
            }
        }
    }

    /// A System Settings–style icon: a coloured rounded square with a white glyph.
    #[allow(clippy::too_many_arguments)]
    fn draw_tile(&self, pm: &mut Pixmap, apps: &Apps, icon: &str, color: [u8; 3], x: f32, y: f32, size: f32, s: f32, fonts: &Fonts, label: &str) {
        let px = size * s;
        let r = px * 0.225;
        // a gentle vertical gradient like macOS's tiles
        let top = [lighten(color[0]), lighten(color[1]), lighten(color[2]), 255];
        let bottom = [color[0], color[1], color[2], 255];
        if let Some(path) = rounded_rect(x, y, px, px, r) {
            let mut p = paint(bottom);
            if let Some(sh) = tiny_skia::LinearGradient::new(
                tiny_skia::Point::from_xy(0.0, y),
                tiny_skia::Point::from_xy(0.0, y + px),
                vec![
                    tiny_skia::GradientStop::new(0.0, tiny_skia::Color::from_rgba8(top[0], top[1], top[2], 255)),
                    tiny_skia::GradientStop::new(1.0, tiny_skia::Color::from_rgba8(bottom[0], bottom[1], bottom[2], 255)),
                ],
                tiny_skia::SpreadMode::Pad,
                Transform::identity(),
            ) {
                p.shader = sh;
            }
            pm.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
        }
        let gsz = (px * 0.62).round() as u32;
        let glyph = {
            let key = (icon.to_string(), gsz);
            let mut cache = self.tiles.borrow_mut();
            cache
                .entry(key)
                .or_insert_with(|| {
                    apps.icon_any(&format!("{icon}-symbolic"), gsz).map(|p| Rc::new(whiten(&p)))
                })
                .clone()
        };
        match glyph {
            Some(g) => {
                let off = (px - gsz as f32) / 2.0;
                pm.draw_pixmap((x + off).round() as i32, (y + off).round() as i32, g.as_ref().as_ref(), &PixmapPaint::default(), Transform::identity(), None);
            }
            None => {
                // no symbolic icon: the coloured one, else the first letter
                let inner = size * 0.7;
                let off = (size - inner) / 2.0 * s;
                if !draw_named_icon(pm, apps, &[icon.to_string()], x + off, y + off, inner, s) {
                    let lpx = px * 0.5;
                    let l: String = label.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
                    let lw = fonts.measure(&l, lpx, Weight::Bold);
                    let bl = fonts.baseline_centered(y, px, lpx, Weight::Bold);
                    fonts.draw(pm, &l, x + (px - lw) / 2.0, bl, lpx, Weight::Bold, [255, 255, 255, 255]);
                }
            }
        }
    }
}

/// Spotlight's name match: whole-name or word prefixes, initials ("vsc" →
/// Visual Studio Code) and, from three letters on, substrings. No loose
/// letter-by-letter matches.
fn name_score(name: &str, q: &str) -> Option<i64> {
    // score(): prefix 300–400, word prefix 200–300, substring 100–200
    let s = score(name, q)?;
    if s >= 200 {
        return Some(s as i64 * 3);
    }
    let ql = q.to_lowercase();
    let initials: String = name
        .split(|c: char| !c.is_alphanumeric())
        .filter_map(|w| w.chars().next())
        .flat_map(|c| c.to_lowercase())
        .collect();
    if ql.chars().count() >= 2 && initials.starts_with(&ql) {
        return Some(820);
    }
    if s >= 100 && ql.chars().count() >= 3 {
        return Some(s as i64 * 3);
    }
    None
}

fn web_search_cmd(q: &str) -> String {
    format!("xdg-open {}", sh_quote(&format!("https://duckduckgo.com/?q={}", url_encode(q))))
}

/// ⌘B: search the web for the query.
pub fn web_action(query: &str) -> Act {
    Act::Spawn(web_search_cmd(query.trim()))
}

fn lighten(c: u8) -> u8 {
    (c as u16 + (255 - c as u16) * 22 / 100) as u8
}

/// A symbolic icon in white (keeps its alpha).
fn whiten(src: &Pixmap) -> Pixmap {
    let mut out = src.clone();
    for px in out.data_mut().chunks_exact_mut(4) {
        let a = px[3];
        px[0] = a;
        px[1] = a;
        px[2] = a;
    }
    out
}

fn file_icons(path: &str, is_dir: bool, index: Option<&FileIndex>) -> Vec<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    match index {
        Some(ix) => ix.mime.icons_for(name, is_dir),
        None => {
            // the list is painted without the index at hand: use the shared copy
            match FILE_ICON_DB.with(|db| db.borrow().as_ref().map(|ix| ix.mime.icons_for(name, is_dir))) {
                Some(v) => v,
                None if is_dir => vec!["folder".into()],
                None => vec!["text-x-generic".into(), "unknown".into()],
            }
        }
    }
}

thread_local! {
    /// The current file index, for icon lookups while painting.
    pub static FILE_ICON_DB: RefCell<Option<Arc<FileIndex>>> = const { RefCell::new(None) };
}

/// Draw the first icon of `names` that exists. Returns whether one was drawn.
fn draw_named_icon(pm: &mut Pixmap, apps: &Apps, names: &[String], x: f32, y: f32, size: f32, s: f32) -> bool {
    let isz = (size * s).round() as u32;
    for n in names.iter().filter(|n| !n.is_empty()) {
        if let Some(icon) = apps.icon_any(n, isz) {
            let k = isz as f32 / icon.width() as f32;
            let p = PixmapPaint {
                quality: FilterQuality::Bilinear,
                ..Default::default()
            };
            pm.draw_pixmap(0, 0, icon.as_ref().as_ref(), &p, Transform::from_row(k, 0.0, 0.0, k, x.round(), y.round()), None);
            return true;
        }
    }
    false
}

/// One blurred rounded square: `blur` px of soft edge around a (2r+2)² box.
fn shadow_sprite(blur: usize, r: usize, alpha: u8) -> Option<Pixmap> {
    let c = blur + r + 1;
    let size = 2 * c + 1;
    let mut a = vec![0f32; size * size];
    // solid rounded square in the middle
    let mut tmp = Pixmap::new(size as u32, size as u32)?;
    fill_rounded(&mut tmp, blur as f32, blur as f32, (size - 2 * blur) as f32, (size - 2 * blur) as f32, r as f32, [0, 0, 0, 255]);
    for (i, px) in tmp.data().chunks_exact(4).enumerate() {
        a[i] = px[3] as f32;
    }
    // three box blurs ≈ gaussian
    let rad = (blur / 3).max(1);
    for _ in 0..3 {
        box_blur(&mut a, size, size, rad);
    }
    let mut out = Pixmap::new(size as u32, size as u32)?;
    for (i, px) in out.data_mut().chunks_exact_mut(4).enumerate() {
        let v = (a[i] * alpha as f32 / 255.0).round().clamp(0.0, 255.0) as u8;
        px[0] = 0;
        px[1] = 0;
        px[2] = 0;
        px[3] = v;
    }
    Some(out)
}

fn box_blur(a: &mut [f32], w: usize, h: usize, r: usize) {
    let mut tmp = vec![0f32; a.len()];
    let norm = 1.0 / (2 * r + 1) as f32;
    for y in 0..h {
        let row = &a[y * w..(y + 1) * w];
        let mut acc = 0.0;
        for i in 0..=r.min(w - 1) {
            acc += row[i];
        }
        for x in 0..w {
            tmp[y * w + x] = acc * norm;
            if x + r + 1 < w {
                acc += row[x + r + 1];
            }
            if x >= r {
                acc -= row[x - r];
            }
        }
    }
    for x in 0..w {
        let mut acc = 0.0;
        for i in 0..=r.min(h - 1) {
            acc += tmp[i * w + x];
        }
        for y in 0..h {
            a[y * w + x] = acc * norm;
            if y + r + 1 < h {
                acc += tmp[(y + r + 1) * w + x];
            }
            if y >= r {
                acc -= tmp[(y - r) * w + x];
            }
        }
    }
}

/// The name of the folder that holds `path` ("Downloads", "Home").
fn parent_name(path: &str, german: bool) -> String {
    let parent = std::path::Path::new(path).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    if parent == files::home().to_string_lossy() {
        return tr(german, "Home", "Benutzerordner").into();
    }
    parent.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or("/").to_string()
}

/// Colours for the panel, from the shell theme.
struct Palette {
    bg: Rgba,
    inner: Rgba,
    outer: Rgba,
    sep: Rgba,
    text: Rgba,
    secondary: Rgba,
    tertiary: Rgba,
    accent: Rgba,
    hover: Rgba,
    sel_bg: Rgba,
    sel_edge: Rgba,
    keycap: Rgba,
    scrollbar: Rgba,
    shadow: u8,
}

impl Palette {
    fn new(theme: &Theme) -> Palette {
        let dark = theme.menu_text[0] > 128;
        let solid = theme.menu_bg[3] == 255;
        let t = theme.menu_text;
        let with = |c: Rgba, a: u8| [c[0], c[1], c[2], a];
        if dark {
            Palette {
                bg: if solid { [38, 38, 40, 255] } else { [28, 28, 30, 224] },
                inner: [255, 255, 255, 30],
                outer: [0, 0, 0, 150],
                sep: [255, 255, 255, 24],
                text: t,
                secondary: with(t, 150),
                tertiary: with(t, 100),
                accent: theme.menu_selection,
                hover: [255, 255, 255, 14],
                sel_bg: [255, 255, 255, 24],
                sel_edge: [255, 255, 255, 34],
                keycap: [255, 255, 255, 20],
                scrollbar: [255, 255, 255, 60],
                shadow: 145,
            }
        } else {
            Palette {
                bg: if solid { [246, 246, 248, 255] } else { [250, 250, 252, 230] },
                inner: [255, 255, 255, 150],
                outer: [0, 0, 0, 38],
                sep: [0, 0, 0, 20],
                text: t,
                secondary: with(t, 145),
                tertiary: with(t, 95),
                accent: theme.menu_selection,
                hover: [0, 0, 0, 10],
                sel_bg: [0, 0, 0, 16],
                sel_edge: [0, 0, 0, 22],
                keycap: [0, 0, 0, 12],
                scrollbar: [0, 0, 0, 52],
                shadow: 90,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranking() {
        assert!(score("Firefox", "fi") > score("Dateien finden", "fi"));
        assert!(score("Konsole", "kons").is_some());
        assert!(score("System Settings", "set").is_some());
        assert!(score("Konsole", "xyz").is_none());
        assert!(score("Kate", "kt").is_some());
    }

    #[test]
    fn spotlight_name_matching() {
        assert!(name_score("Firefox", "fire").is_some());
        assert!(name_score("Visual Studio Code", "vsc").is_some());
        assert!(name_score("System Settings", "set").is_some());
        assert!(name_score("BALL PICKER - Clean Up the Golf Yard", "png").is_none());
        assert!(name_score("KDE-Partitionsverwaltung", "png").is_none());
        assert!(name_score("Fedora Media Writer", "fire").is_none());
        assert!(name_score("Firefox", "fi").unwrap() > name_score("Dateien finden", "fi").unwrap());
    }

    #[test]
    fn urls_and_quotes() {
        assert_eq!(looks_like_url("github.com").as_deref(), Some("https://github.com"));
        assert_eq!(looks_like_url("https://x.org/a?b").as_deref(), Some("https://x.org/a?b"));
        assert_eq!(looks_like_url("main.rs"), None);
        assert_eq!(looks_like_url("notes.txt"), None);
        assert_eq!(looks_like_url("hello world.com"), None);
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
        assert_eq!(url_encode("a b&c"), "a+b%26c");
    }

    fn launcher(q: &str) -> Launcher {
        let mut l = Launcher::new("", 0, false);
        l.insert(q);
        l
    }

    #[test]
    fn editing() {
        let m = Modifiers::default();
        let mut l = launcher("hello world");
        assert_eq!(l.key(Keysym::BackSpace, None, &Modifiers { ctrl: true, ..m }), Outcome::Changed);
        assert_eq!(l.query, "hello ");
        l.key(Keysym::Left, None, &m);
        l.key(Keysym::Left, None, &m);
        assert_eq!(l.cursor, 4);
        l.insert("ä");
        assert_eq!(l.query, "hello ".replacen("o", "äo", 1));
        l.key(Keysym::Home, None, &m);
        l.key(Keysym::Delete, None, &m);
        assert!(l.query.starts_with("ell"));
        // reopening selects the previous query; typing replaces it
        let mut l = Launcher::new("old", 0, false);
        l.insert("n");
        assert_eq!(l.query, "n");
        // Escape clears first, then closes
        let mut l = launcher("x");
        assert_eq!(l.key(Keysym::Escape, None, &m), Outcome::Changed);
        assert_eq!(l.key(Keysym::Escape, None, &m), Outcome::Close);
    }

    #[test]
    fn dates_and_paths() {
        let home = files::home().to_string_lossy().into_owned();
        assert_eq!(parent_name(&format!("{home}/a.txt"), false), "Home");
        assert_eq!(parent_name(&format!("{home}/Docs/x/a.txt"), false), "x");
        assert_eq!(parent_name("/etc/fstab", false), "etc");
    }
}

/// Renders Spotlight to PNGs for a visual check:
/// `SPOT_SHOTS=/tmp/dir cargo test -p orchard-shell spotlight_shots -- --ignored`
#[cfg(test)]
mod shots {
    use super::*;

    #[test]
    #[ignore]
    fn spotlight_shots() {
        let Ok(dir) = std::env::var("SPOT_SHOTS") else { return };
        let apps = Apps::load(None);
        let ix = Arc::new(FileIndex::build());
        FILE_ICON_DB.with(|db| *db.borrow_mut() = Some(ix.clone()));
        let fonts = Fonts::load();
        let usage = Usage::default();
        let scale = 2.0;
        for (mode, dark) in [("light", false), ("dark", true)] {
            let theme = Theme::new(if dark { crate::theme::Appearance::Dark } else { crate::theme::Appearance::Light });
            let queries: &[(&str, usize)] = &[("", 0), ("fire", 0), ("kons", 0), ("2+3*4^2", 0), ("10 km in mi", 0), ("wifi", 0), ("readme", 0), ("png", 1), ("lock", 0), ("rust", 3)];
            for (qi, (q, sel)) in queries.iter().enumerate() {
                let mut l = Launcher::new("", 1, std::env::var("LANG").map(|v| v.starts_with("de")).unwrap_or(false));
                l.insert(q);
                l.update(&apps, Some(&ix), &usage, &SearchSettings::default());
                l.select(*sel);
                l.shown_h = l.target_height();
                l.caret_on = true;
                let (w, h) = (840.0, 620.0);
                let mut pm = Pixmap::new((w * scale) as u32, (h * scale) as u32).unwrap();
                // a wallpaper-ish backdrop
                let (a, b) = if dark { ([30, 40, 70], [80, 40, 90]) } else { ([120, 170, 230], [240, 180, 200]) };
                for y in 0..pm.height() {
                    let t = y as f32 / pm.height() as f32;
                    let c = |i: usize| (a[i] as f32 * (1.0 - t) + b[i] as f32 * t) as u8;
                    let row = [c(0), c(1), c(2), 255];
                    let wpx = pm.width() as usize;
                    let d = pm.data_mut();
                    for x in 0..wpx {
                        let i = (y as usize * wpx + x) * 4;
                        d[i..i + 4].copy_from_slice(&row);
                    }
                }
                l.paint(&mut pm, (w - WIDTH) / 2.0, 60.0, scale, &apps, &fonts, &theme);
                pm.save_png(format!("{dir}/{mode}-{qi:02}.png")).unwrap();
            }
        }
    }
}
