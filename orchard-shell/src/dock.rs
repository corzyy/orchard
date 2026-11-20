//! The Dock: pinned + running apps, Launchpad, minimized windows (with real
//! thumbnails), Trash, magnification. Items grow in and shrink out.

use std::{collections::HashMap, rc::Rc, time::Instant};

use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};

use crate::{
    apps::Apps,
    draw::{fill_circle, fill_rounded, stroke_rounded},
    ipc::WindowInfo,
    text::{Fonts, Weight},
    theme::Theme,
};

use orchard_config::{DockAutohide, DockPosition, DockSettings};

const GAP: f32 = 6.0;
const PAD_X: f32 = 8.0;
const PAD_Y: f32 = 6.0;
const DOT_SPACE: f32 = 6.0;
pub const MARGIN_BOTTOM: f32 = 4.0;
const TOOLTIP_H: f32 = 26.0;
/// room beside a side Dock for the hovered item's name
const SIDE_TOOLTIP_ROOM: f32 = 260.0;
/// magnification falloff radius in units of base tile width
const FALLOFF_TILES: f32 = 2.6;
/// how fast items grow in / shrink out (1/s); about as long as the genie
const APPEAR_RATE: f32 = 8.0;
/// a new minimized window's picture shows once the genie has arrived
const THUMB_REVEAL_AT: f32 = 0.45;
const THUMB_REVEAL_FADE: f32 = 0.12;

/// Height of the strip that reveals an auto-hidden Dock.
pub const REVEAL_STRIP: f32 = 3.0;

#[derive(Debug, Clone, PartialEq)]
pub enum ItemKind {
    App { desktop_id: Option<String>, app_id: String, pinned: bool },
    Launchpad,
    Minimized { window_id: u64, app_id: String, thumb: Option<String> },
    Separator,
    Trash,
}

impl ItemKind {
    /// Identity across rebuilds.
    fn key(&self) -> String {
        match self {
            ItemKind::App { desktop_id, app_id, .. } => format!("app:{}", desktop_id.as_deref().unwrap_or(app_id)),
            ItemKind::Launchpad => "launchpad".into(),
            ItemKind::Minimized { window_id, .. } => format!("min:{window_id}"),
            ItemKind::Separator => "sep".into(),
            ItemKind::Trash => "trash".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Item {
    pub kind: ItemKind,
    pub name: String,
    pub icon_name: Option<String>,
    pub running: bool,
    pub bounce_start: Option<Instant>,
    /// 0..1: width share while growing in / shrinking out
    pub appear: f32,
    /// gone from the Dock; shrinking out
    pub leaving: bool,
    pub born: Instant,
}

impl Item {
    fn new(kind: ItemKind, name: String, icon_name: Option<String>, running: bool) -> Item {
        Item {
            kind,
            name,
            icon_name,
            running,
            bounce_start: None,
            appear: 1.0,
            leaving: false,
            born: Instant::now(),
        }
    }

    fn is_separator(&self) -> bool {
        matches!(self.kind, ItemKind::Separator)
    }
}

/// Layout positions are computed for a Dock at the bottom ("canonical":
/// `x` along the Dock, `y` towards the screen edge) and mapped onto the
/// surface for the chosen edge. Icons stay upright.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed {
    /// left edge of the icon (logical, surface coords)
    pub x: f32,
    /// icon size after magnification
    pub size: f32,
    /// bottom of the icon (logical, surface coords)
    pub bottom: f32,
}

/// A rect (x, y, w, h) in surface coordinates.
pub type RectF = (f32, f32, f32, f32);

/// Pointer travel (logical px) before a press on an icon becomes a drag.
pub const DRAG_THRESHOLD: f32 = 6.0;
/// How far from the Dock (towards the screen centre) an icon must be dragged
/// to be removed on release.
const REMOVE_DISTANCE: f32 = 64.0;

/// An app icon being dragged (canonical coordinates).
#[derive(Debug, Clone)]
pub struct DockDrag {
    /// `ItemKind::key()` of the dragged item
    pub key: String,
    pub start: (f32, f32),
    pub pos: (f32, f32),
    pub active: bool,
}

/// What a finished drag asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum DragOutcome {
    /// the new list of kept apps, in Dock order
    Pinned(Vec<String>),
    /// stop keeping this app in the Dock
    Remove(String),
}

pub struct Dock {
    pub items: Vec<Item>,
    pub pinned: Vec<String>,
    pub hover_x: Option<f32>,
    /// where the pointer left: magnification fades out around it
    pub last_hover_x: Option<f32>,
    pub hovered: Option<usize>,
    pub pressed: Option<usize>,
    /// 0..1 magnification strength (animated)
    pub mag: f32,
    /// Settings ▸ Desktop & Dock
    pub cfg: DockSettings,
    /// 0 = shown, 1 = hidden (animated; with autohide / avoid-windows)
    pub hidden: f32,
    /// keep an auto-hidden Dock visible (pointer on it, menu open)
    pub reveal: bool,
    /// a visible window overlaps the Dock's resting rect (for AvoidWindows)
    pub obstructed: bool,
    /// minimized-window pictures by path
    thumbs: HashMap<String, Rc<Pixmap>>,
    /// the first rebuild shows everything at once (no grow-in)
    built: bool,
    /// an app icon being dragged to reorder, keep or remove it
    pub drag: Option<DockDrag>,
}

/// Where a minimized window's picture goes inside its tile (aspect-fit,
/// centered, small inset). The compositor's genie aims for exactly this rect
/// (`thumb_rect_in_tile` in orchard/src/wm.rs).
pub fn thumb_rect(x: f32, y: f32, size: f32, w: f32, h: f32) -> RectF {
    let inset = size * 0.06;
    let a = size - 2.0 * inset;
    let k = (a / w.max(1.0)).min(a / h.max(1.0));
    let (fw, fh) = (w * k, h * k);
    (x + (size - fw) / 2.0, y + (size - fh) / 2.0, fw, fh)
}

fn config_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))?;
    Some(base.join("orchard/dock.json"))
}

pub fn load_pinned(apps: &Apps) -> Vec<String> {
    if let Some(p) = config_path() {
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                if let Some(arr) = v["pinned"].as_array() {
                    return arr.iter().filter_map(|x| x.as_str().map(String::from)).collect();
                }
            }
        }
    }
    let categories: &[&[&str]] = &[
        &["org.gnome.Nautilus", "thunar", "nemo", "pcmanfm", "com.system76.CosmicFiles"],
        &["firefox", "org.mozilla.firefox", "chromium", "com.brave.Browser", "org.gnome.Epiphany"],
        &[
            "com.mitchellh.ghostty",
            "kitty",
            "Alacritty",
            "foot",
            "org.wezfurlong.wezterm",
            "org.gnome.Ptyxis",
            "org.gnome.Terminal",
            "xfce4-terminal",
        ],
        &["org.gnome.TextEditor", "mousepad", "geany", "code"],
        &["org.gnome.Calculator", "galculator"],
        &["org.orchard.settings"],
    ];
    categories
        .iter()
        .filter_map(|group| group.iter().find(|id| apps.by_id(id).is_some()))
        .map(|s| s.to_string())
        .collect()
}

pub fn save_pinned(pinned: &[String]) {
    if let Some(p) = config_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, serde_json::json!({ "pinned": pinned }).to_string());
    }
}

impl Dock {
    pub fn new(pinned: Vec<String>) -> Dock {
        Dock {
            items: Vec::new(),
            pinned,
            hover_x: None,
            last_hover_x: None,
            hovered: None,
            pressed: None,
            mag: 0.0,
            cfg: DockSettings::default(),
            hidden: 0.0,
            reveal: false,
            obstructed: false,
            thumbs: HashMap::new(),
            built: false,
            drag: None,
        }
    }

    /// Base tile size (logical px).
    pub fn icon(&self) -> f32 {
        self.cfg.icon_size.clamp(24.0, 128.0)
    }

    /// Tile size under the pointer.
    pub fn icon_max(&self) -> f32 {
        if self.cfg.magnification {
            self.cfg.magnified_size.clamp(self.icon(), 160.0)
        } else {
            self.icon()
        }
    }

    pub fn radius(&self) -> f32 {
        (self.icon() * 0.38).clamp(12.0, 28.0)
    }

    pub fn bar_height(&self) -> f32 {
        self.icon() + 2.0 * PAD_Y + DOT_SPACE
    }

    /// Depth of the dock layer surface away from its screen edge (room for
    /// magnified icons + tooltip; beside the icons for a side Dock).
    pub fn surface_height(&self) -> f32 {
        let tooltip = if self.vertical() { SIDE_TOOLTIP_ROOM } else { TOOLTIP_H + 16.0 };
        MARGIN_BOTTOM + self.bar_height() + (self.icon_max() - self.icon()) + tooltip
    }

    pub fn edge(&self) -> DockPosition {
        self.cfg.position
    }

    /// Left or right Dock.
    pub fn vertical(&self) -> bool {
        self.cfg.position != DockPosition::Bottom
    }

    /// (length along the Dock, depth towards the screen edge) of a surface.
    pub fn axes(&self, sw: f32, sh: f32) -> (f32, f32) {
        if self.vertical() {
            (sh, sw)
        } else {
            (sw, sh)
        }
    }

    /// Canonical rect -> surface rect.
    pub fn map_rect(&self, r: RectF, sw: f32, _sh: f32) -> RectF {
        let (u, v, w, h) = r;
        match self.edge() {
            DockPosition::Bottom => r,
            DockPosition::Left => (sw - v - h, u, h, w),
            DockPosition::Right => (v, u, h, w),
        }
    }

    /// Canonical point -> surface point.
    pub fn map_point(&self, u: f32, v: f32, sw: f32, _sh: f32) -> (f32, f32) {
        match self.edge() {
            DockPosition::Bottom => (u, v),
            DockPosition::Left => (sw - v, u),
            DockPosition::Right => (v, u),
        }
    }

    /// Surface point -> canonical point.
    pub fn unmap_point(&self, x: f32, y: f32, sw: f32, _sh: f32) -> (f32, f32) {
        match self.edge() {
            DockPosition::Bottom => (x, y),
            DockPosition::Left => (y, sw - x),
            DockPosition::Right => (y, x),
        }
    }

    /// Background of the Dock in surface coordinates.
    pub fn surface_bg(&self, sw: f32, sh: f32) -> RectF {
        let (_, bg) = self.layout(sw, sh);
        self.map_rect(bg, sw, sh)
    }

    /// Settled (unhidden, unmagnified) Dock rect extended to the screen edge,
    /// in surface coordinates (used for "avoid windows" overlap detection).
    pub fn resting_rect(&self, sw: f32, sh: f32) -> RectF {
        let tmp = self.settled(false);
        let (_, bg) = tmp.layout(sw, sh);
        self.map_rect((bg.0, bg.1, bg.2, bg.3 + MARGIN_BOTTOM), sw, sh)
    }

    /// Icon square of a placed item in surface coordinates.
    pub fn tile_rect(&self, p: &Placed, sw: f32, sh: f32) -> RectF {
        self.map_rect((p.x, p.bottom - p.size, p.size, p.size), sw, sh)
    }

    /// Exclusive zone (what windows must not cover); none when auto-hiding or avoiding windows.
    pub fn exclusive_zone(&self) -> f32 {
        if self.cfg.autohide != DockAutohide::Never {
            0.0
        } else {
            MARGIN_BOTTOM + self.bar_height() + 4.0
        }
    }

    fn hidden_target(&self) -> f32 {
        let should_hide = match self.cfg.autohide {
            DockAutohide::Never => false,
            DockAutohide::Always => true,
            DockAutohide::AvoidWindows => self.obstructed,
        };
        if should_hide && !self.reveal && self.hover_x.is_none() {
            1.0
        } else {
            0.0
        }
    }

    /// Fully hidden (input only on the reveal strip).
    pub fn is_hidden(&self) -> bool {
        self.cfg.autohide != DockAutohide::Never && self.hidden > 0.99
    }

    /// Rebuild items from pinned apps + the compositor's window list.
    pub fn rebuild(&mut self, apps: &Apps, windows: &[WindowInfo]) {
        let old = std::mem::take(&mut self.items);
        let running_ids: Vec<String> = windows.iter().map(|w| w.app_id.clone()).collect();
        let entry_matches = |desktop_id: &str, app_id: &str| -> bool {
            if desktop_id == app_id {
                return true;
            }
            apps.for_app_id(app_id).map(|e| e.id == desktop_id).unwrap_or(false)
        };

        let mut items = Vec::new();
        for id in &self.pinned {
            let entry = apps.by_id(id);
            let running = running_ids.iter().any(|a| entry_matches(id, a));
            let app_id = running_ids
                .iter()
                .find(|a| entry_matches(id, a))
                .cloned()
                .unwrap_or_else(|| id.clone());
            items.push(Item::new(
                ItemKind::App {
                    desktop_id: Some(id.clone()),
                    app_id,
                    pinned: true,
                },
                entry.map(|e| e.name.clone()).unwrap_or_else(|| id.clone()),
                entry.and_then(|e| e.icon.clone()),
                running,
            ));
        }
        // Launchpad right after the first app (Finder's place on macOS)
        items.insert(items.len().min(1), Item::new(ItemKind::Launchpad, "Launchpad".into(), None, false));
        // running apps that are not pinned, in first-seen order
        let mut seen: Vec<String> = Vec::new();
        for w in windows {
            if seen.contains(&w.app_id) {
                continue;
            }
            seen.push(w.app_id.clone());
            let pinned = self.pinned.iter().any(|id| entry_matches(id, &w.app_id));
            if pinned {
                continue;
            }
            let entry = apps.for_app_id(&w.app_id);
            items.push(Item::new(
                ItemKind::App {
                    desktop_id: entry.map(|e| e.id.clone()),
                    app_id: w.app_id.clone(),
                    pinned: false,
                },
                entry.map(|e| e.name.clone()).unwrap_or_else(|| {
                    if w.title.is_empty() {
                        w.app_id.clone()
                    } else {
                        w.title.clone()
                    }
                }),
                entry.and_then(|e| e.icon.clone()).or_else(|| Some(w.app_id.clone())),
                true,
            ));
        }
        items.push(Item::new(ItemKind::Separator, String::new(), None, false));
        for w in windows.iter().filter(|w| w.minimized) {
            let entry = apps.for_app_id(&w.app_id);
            items.push(Item::new(
                ItemKind::Minimized {
                    window_id: w.id,
                    app_id: w.app_id.clone(),
                    thumb: w.thumb.clone(),
                },
                if w.title.is_empty() { w.app_id.clone() } else { w.title.clone() },
                entry.and_then(|e| e.icon.clone()).or_else(|| Some(w.app_id.clone())),
                false,
            ));
        }
        static GERMAN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let trash_name = if *GERMAN.get_or_init(|| std::env::var("LANG").map(|l| l.starts_with("de")).unwrap_or(false)) {
            "Papierkorb"
        } else {
            "Trash"
        };
        static TRASH_CACHE: std::sync::Mutex<Option<(Instant, bool)>> = std::sync::Mutex::new(None);
        let full = {
            let mut guard = TRASH_CACHE.lock().unwrap();
            match *guard {
                Some((at, v)) if at.elapsed() < std::time::Duration::from_secs(5) => v,
                _ => {
                    let v = trash_full();
                    *guard = Some((Instant::now(), v));
                    v
                }
            }
        };
        items.push(Item::new(
            ItemKind::Trash,
            trash_name.into(),
            Some(if full { "user-trash-full" } else { "user-trash" }.into()),
            false,
        ));

        // carry animation state over; new apps and minimized windows grow in
        for it in items.iter_mut() {
            let key = it.kind.key();
            match old.iter().find(|o| !o.leaving && o.kind.key() == key) {
                Some(o) => {
                    it.appear = o.appear;
                    it.born = o.born;
                    it.bounce_start = if it.running { None } else { o.bounce_start };
                }
                None if self.built && matches!(it.kind, ItemKind::App { .. } | ItemKind::Minimized { .. }) => {
                    it.appear = 0.0;
                }
                None => {
                    // present at startup: no reveal delay
                    if !self.built {
                        it.born = Instant::now().checked_sub(std::time::Duration::from_secs(10)).unwrap_or(it.born);
                    }
                }
            }
        }
        // items that went away shrink out where they were
        for (i, o) in old.iter().enumerate() {
            if !matches!(o.kind, ItemKind::App { .. } | ItemKind::Minimized { .. }) || o.appear < 0.02 {
                continue;
            }
            let key = o.kind.key();
            if items.iter().any(|it| !it.leaving && it.kind.key() == key) {
                continue;
            }
            let mut gone = o.clone();
            gone.leaving = true;
            let at = i.min(items.len().saturating_sub(1));
            items.insert(at, gone);
        }
        // forget pictures of windows that are no longer minimized
        let live: Vec<String> = items
            .iter()
            .filter_map(|i| match &i.kind {
                ItemKind::Minimized { thumb: Some(t), .. } => Some(t.clone()),
                _ => None,
            })
            .collect();
        self.thumbs.retain(|k, _| live.contains(k));
        self.items = items;
        self.hovered = None;
        self.built = true;
        self.load_thumbs();
    }

    /// Load minimized-window pictures the compositor has written by now.
    fn load_thumbs(&mut self) {
        for it in &self.items {
            if let ItemKind::Minimized { thumb: Some(path), .. } = &it.kind {
                if !self.thumbs.contains_key(path) {
                    if let Ok(pm) = Pixmap::load_png(path) {
                        self.thumbs.insert(path.clone(), Rc::new(pm));
                    }
                }
            }
        }
    }

    fn missing_thumbs(&self) -> bool {
        self.items.iter().any(|it| match &it.kind {
            ItemKind::Minimized { thumb: Some(p), .. } => {
                !self.thumbs.contains_key(p) && it.born.elapsed().as_secs_f32() < 3.0
            }
            _ => false,
        })
    }

    fn base_width(&self, item: &Item) -> f32 {
        if item.is_separator() {
            1.0
        } else {
            self.icon()
        }
    }

    /// Lay out items for the current pointer position. Returns placements
    /// and the background rect (x, y, w, h) in surface coordinates.
    pub fn layout(&self, sw: f32, sh: f32) -> (Vec<Placed>, RectF) {
        let (surface_w, surface_h) = self.axes(sw, sh);
        let n = self.items.len();
        // gap before item i (i > 0) scales with how far it has grown in
        let gap = |i: usize| if i == 0 { 0.0 } else { GAP * self.items[i].appear };
        // unmagnified centers
        let total_base: f32 = self
            .items
            .iter()
            .enumerate()
            .map(|(i, it)| self.base_width(it) * it.appear + gap(i))
            .sum();
        let mut centers = Vec::with_capacity(n);
        let mut x = (surface_w - total_base) / 2.0;
        for (i, it) in self.items.iter().enumerate() {
            x += gap(i);
            let w = self.base_width(it) * it.appear;
            centers.push(x + w / 2.0);
            x += w;
        }
        let (icon, icon_max) = (self.icon(), self.icon_max());
        let falloff = FALLOFF_TILES * (icon + GAP);
        // after the pointer left, magnification fades out around where it was
        let hx = self.hover_x.or(self.last_hover_x);
        let sizes: Vec<f32> = self
            .items
            .iter()
            .zip(&centers)
            .map(|(it, c)| {
                if it.is_separator() {
                    return it.appear;
                }
                let f = match hx {
                    Some(px) => {
                        let d = ((px - c).abs() / falloff).min(1.0);
                        let k = (d * std::f32::consts::FRAC_PI_2).cos();
                        k * k
                    }
                    None => 0.0,
                };
                (icon + (icon_max - icon) * f * self.mag) * it.appear
            })
            .collect();
        let total: f32 = sizes.iter().sum::<f32>() + (0..n).map(gap).sum::<f32>();
        let start = (surface_w - total) / 2.0;
        // auto-hide slides the whole Dock below the screen edge
        let slide = self.hidden * (self.bar_height() + MARGIN_BOTTOM + 8.0);
        let bar_bottom = surface_h - MARGIN_BOTTOM + slide;
        let icon_bottom = bar_bottom - PAD_Y - DOT_SPACE;
        let mut placed = Vec::with_capacity(n);
        let mut x = start;
        for (i, s) in sizes.iter().enumerate() {
            x += gap(i);
            placed.push(Placed {
                x,
                size: *s,
                bottom: icon_bottom,
            });
            x += s;
        }
        let bg = (
            start - PAD_X,
            bar_bottom - self.bar_height(),
            total + 2.0 * PAD_X,
            self.bar_height(),
        );
        (placed, bg)
    }

    /// Start tracking a press on item `idx` (surface point): becomes a drag
    /// once the pointer moves far enough. Only app icons can be dragged.
    pub fn press_for_drag(&mut self, idx: usize, x: f32, y: f32, sw: f32, sh: f32) {
        self.drag = self.items.get(idx).filter(|it| matches!(it.kind, ItemKind::App { .. }) && !it.leaving).map(|it| {
            let p = self.unmap_point(x, y, sw, sh);
            DockDrag { key: it.kind.key(), start: p, pos: p, active: false }
        });
    }

    /// The dragged icon is far enough from the Dock to be removed.
    pub fn drag_outside(&self, sw: f32, sh: f32) -> bool {
        let Some(d) = self.drag.as_ref().filter(|d| d.active) else { return false };
        let (_, bg) = self.layout(sw, sh);
        d.pos.1 < bg.1 - REMOVE_DISTANCE
    }

    /// Pointer moved while pressed (surface point). Returns true while a drag
    /// is active (the caller redraws and skips hover handling).
    pub fn drag_motion(&mut self, x: f32, y: f32, sw: f32, sh: f32) -> bool {
        let p = self.unmap_point(x, y, sw, sh);
        let Some(d) = self.drag.as_mut() else { return false };
        d.pos = p;
        if !d.active {
            let (dx, dy) = (p.0 - d.start.0, p.1 - d.start.1);
            if (dx * dx + dy * dy).sqrt() < DRAG_THRESHOLD {
                return false;
            }
            d.active = true;
            self.pressed = None;
        }
        if !self.drag_outside(sw, sh) {
            self.reorder_for_drag(sw, sh);
        }
        true
    }

    /// Move the dragged app between the other apps, by where the pointer is.
    fn reorder_for_drag(&mut self, sw: f32, sh: f32) {
        let Some(d) = self.drag.clone() else { return };
        let Some(sep) = self.items.iter().position(|i| i.is_separator()) else { return };
        let (placed, _) = self.layout(sw, sh);
        let is_app = |it: &Item| matches!(it.kind, ItemKind::App { .. }) && !it.leaving;
        let Some(di) = self.items[..sep].iter().position(|it| it.kind.key() == d.key) else { return };
        // rank among the other apps = how many of them lie before the pointer
        let others: Vec<usize> = (0..sep).filter(|&i| i != di && is_app(&self.items[i])).collect();
        let rank = others.iter().filter(|&&i| placed[i].x + placed[i].size / 2.0 < d.pos.0).count();
        let current = (0..di).filter(|&i| is_app(&self.items[i])).count();
        if rank == current {
            return;
        }
        let rest = self.items.split_off(sep);
        let head = std::mem::take(&mut self.items);
        let mut apps: Vec<Item> = Vec::new();
        let mut dragged = None;
        let mut launchpad = None;
        for it in head {
            match &it.kind {
                ItemKind::Launchpad => launchpad = Some(it),
                _ if it.kind.key() == d.key => dragged = Some(it),
                _ if is_app(&it) => apps.push(it),
                _ => {} // shrinking out anyway
            }
        }
        if let Some(it) = dragged {
            apps.insert(rank.min(apps.len()), it);
        }
        // Launchpad stays right after the first app
        if let Some(l) = launchpad {
            apps.insert(apps.len().min(1), l);
        }
        apps.extend(rest);
        self.items = apps;
        self.hovered = None;
    }

    /// Pointer released after a drag: what to do (None: nothing changes).
    pub fn finish_drag(&mut self, sw: f32, sh: f32) -> Option<DragOutcome> {
        let outside = self.drag_outside(sw, sh);
        let d = self.drag.take().filter(|d| d.active)?;
        let id_of = |it: &Item| match &it.kind {
            ItemKind::App { desktop_id, app_id, pinned } => Some((desktop_id.clone().unwrap_or_else(|| app_id.clone()), *pinned)),
            _ => None,
        };
        let it = self.items.iter().find(|it| it.kind.key() == d.key)?;
        let (id, pinned) = id_of(it)?;
        if outside {
            return pinned.then_some(DragOutcome::Remove(id));
        }
        let sep = self.items.iter().position(|i| i.is_separator()).unwrap_or(self.items.len());
        let order: Vec<String> = self.items[..sep]
            .iter()
            .filter(|i| !i.leaving)
            .filter_map(|i| {
                let (id, pinned) = id_of(i)?;
                (pinned || i.kind.key() == d.key).then_some(id)
            })
            .collect();
        Some(DragOutcome::Pinned(order))
    }

    /// Settled (unmagnified, fully grown) layout of the items that stay.
    fn settled(&self, extra_minimized: bool) -> Dock {
        let mut items: Vec<Item> = self.items.iter().filter(|i| !i.leaving).cloned().collect();
        for it in items.iter_mut() {
            it.appear = 1.0;
        }
        if extra_minimized {
            let at = items.len().saturating_sub(1); // before the Trash
            items.insert(
                at,
                Item::new(
                    ItemKind::Minimized { window_id: u64::MAX, app_id: String::new(), thumb: None },
                    String::new(),
                    None,
                    false,
                ),
            );
        }
        Dock {
            items,
            pinned: Vec::new(),
            hover_x: None,
            last_hover_x: None,
            hovered: None,
            pressed: None,
            mag: 0.0,
            cfg: self.cfg.clone(),
            hidden: 0.0,
            reveal: false,
            obstructed: false,
            thumbs: HashMap::new(),
            built: true,
            drag: None,
        }
    }

    /// Genie targets: every tile once the Dock has settled, plus the tile a
    /// window minimized next would get (surface coords).
    pub fn genie_tiles(&self, surface_w: f32, surface_h: f32) -> (Vec<(ItemKind, RectF)>, Option<RectF>) {
        let tmp = self.settled(false);
        let (placed, _) = tmp.layout(surface_w, surface_h);
        let tiles = tmp
            .items
            .iter()
            .zip(&placed)
            .filter(|(i, _)| !i.is_separator())
            .map(|(i, p)| (i.kind.clone(), self.tile_rect(p, surface_w, surface_h)))
            .collect();
        let next = self.settled(true);
        let (placed, _) = next.layout(surface_w, surface_h);
        let at = next.items.len().saturating_sub(2);
        let slot = placed.get(at).map(|p| self.tile_rect(p, surface_w, surface_h));
        (tiles, slot)
    }

    pub fn hit(&self, x: f32, y: f32, surface_w: f32, surface_h: f32) -> Option<usize> {
        if self.is_hidden() {
            return None;
        }
        let (x, y) = self.unmap_point(x, y, surface_w, surface_h);
        let (placed, bg) = self.layout(surface_w, surface_h);
        if y < bg.1 - (self.icon_max() - self.icon()) - 4.0 || y > bg.1 + bg.3 + MARGIN_BOTTOM + 1.0 {
            return None;
        }
        placed.iter().enumerate().find_map(|(i, p)| {
            let it = &self.items[i];
            let in_x = x >= p.x - GAP / 2.0 && x < p.x + p.size + GAP / 2.0;
            (in_x && !it.is_separator() && !it.leaving && it.appear > 0.5).then_some(i)
        })
    }

    pub fn is_animating(&self, target_mag: f32) -> bool {
        (self.mag - target_mag).abs() > 0.001
            || (self.hidden - self.hidden_target()).abs() > 0.001
            || (self.cfg.animate_launch && self.items.iter().any(|i| i.bounce_start.is_some()))
            || self.items.iter().any(|i| i.leaving || i.appear < 1.0)
            || self.missing_thumbs()
            || self.items.iter().any(|i| {
                matches!(i.kind, ItemKind::Minimized { .. })
                    && i.born.elapsed().as_secs_f32() < THUMB_REVEAL_AT + THUMB_REVEAL_FADE
            })
    }

    /// Advance animations by `dt` seconds.
    pub fn step(&mut self, dt: f32) {
        let target = if self.hover_x.is_some() { 1.0 } else { 0.0 };
        let k = 1.0 - (-dt * 14.0).exp();
        self.mag += (target - self.mag) * k;
        if (self.mag - target).abs() < 0.002 {
            self.mag = target;
        }
        if self.hover_x.is_some() {
            self.last_hover_x = self.hover_x;
        } else if self.mag == 0.0 {
            self.last_hover_x = None;
        }
        let ht = self.hidden_target();
        // hide a little slower than revealing, like macOS
        let kh = 1.0 - (-dt * if ht > self.hidden { 9.0 } else { 20.0 }).exp();
        self.hidden += (ht - self.hidden) * kh;
        if (self.hidden - ht).abs() < 0.002 {
            self.hidden = ht;
        }
        let ka = 1.0 - (-dt * APPEAR_RATE).exp();
        for it in self.items.iter_mut() {
            let to = if it.leaving { 0.0 } else { 1.0 };
            it.appear += (to - it.appear) * ka;
            if (it.appear - to).abs() < 0.004 {
                it.appear = to;
            }
            if let Some(s) = it.bounce_start {
                if it.running || s.elapsed().as_secs_f32() > 6.0 {
                    it.bounce_start = None;
                }
            }
        }
        let before = self.items.len();
        self.items.retain(|i| !(i.leaving && i.appear <= 0.0));
        if self.items.len() != before {
            self.hovered = None;
            self.pressed = None;
        }
        if self.missing_thumbs() {
            self.load_thumbs();
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn paint(&self, pm: &mut Pixmap, scale: f32, apps: &Apps, fonts: &Fonts, theme: &Theme) {
        let sw = pm.width() as f32 / scale;
        let sh = pm.height() as f32 / scale;
        let (placed, cbg) = self.layout(sw, sh);
        let bg = self.map_rect(cbg, sw, sh);
        let s = scale;
        // background (the compositor draws the frosted blur behind it)
        let radius = self.radius();
        fill_rounded(pm, bg.0 * s, bg.1 * s, bg.2 * s, bg.3 * s, radius * s, theme.dock_tint);
        stroke_rounded(
            pm,
            bg.0 * s + 0.5,
            bg.1 * s + 0.5,
            bg.2 * s - 1.0,
            bg.3 * s - 1.0,
            radius * s,
            1.0,
            theme.dock_border,
        );

        for (i, (it, p)) in self.items.iter().zip(&placed).enumerate() {
            if it.is_separator() {
                let (x, y, w, h) = self.map_rect((p.x, cbg.1 + 10.0, 1.0, cbg.3 - 20.0), sw, sh);
                fill_rounded(pm, x * s, y * s, w * s, h * s, 0.5 * s, theme.dock_separator);
                continue;
            }
            if p.size < 2.0 {
                continue;
            }
            // the dragged icon is drawn under the pointer instead (its slot stays free)
            if self.drag.as_ref().map(|d| d.active && d.key == it.kind.key()).unwrap_or(false) {
                continue;
            }
            let bounce = it
                .bounce_start
                .filter(|_| self.cfg.animate_launch)
                .map(|t| {
                    let ph = t.elapsed().as_secs_f32() / 0.7 * std::f32::consts::PI;
                    ph.sin().abs() * 18.0
                })
                .unwrap_or(0.0);
            let size_px = p.size * s;
            // Rasterize icons once at max magnified size for this scale and let draw_icon
            // scale smoothly with bilinear filtering (avoids caching 100+ sizes per icon).
            let raster_px = (self.icon_max() * s).round().max(1.0) as u32;
            let badge_raster_px = (self.icon_max() * 0.42 * s).round().max(1.0) as u32;
            // icon square on the surface (bounces away from the screen edge)
            let (ix, iy, _, _) = self.map_rect((p.x, p.bottom - p.size - bounce, p.size, p.size), sw, sh);
            let (ix, top) = (ix * s, iy * s);
            let pressed = i == self.pressed.unwrap_or(usize::MAX);
            let app_icon = |n: u32| {
                it.icon_name
                    .as_deref()
                    .and_then(|name| apps.icon(name, n.max(1)))
                    .or_else(|| apps.icon("application-x-executable", n.max(1)))
            };
            match &it.kind {
                ItemKind::Launchpad => draw_launchpad_icon(pm, ix, top, size_px, pressed),
                ItemKind::Minimized { thumb: Some(path), .. } if self.thumbs.contains_key(path) => {
                    // the genie is still flying into this tile: keep it empty
                    let reveal = ((it.born.elapsed().as_secs_f32() - THUMB_REVEAL_AT) / THUMB_REVEAL_FADE).clamp(0.0, 1.0);
                    if reveal <= 0.0 || it.leaving {
                        continue;
                    }
                    let shade = |a: u8| (a as f32 * reveal) as u8;
                    let pic = &self.thumbs[path];
                    let (tx, ty, tw, th) = thumb_rect(ix, top, size_px, pic.width() as f32, pic.height() as f32);
                    // soft shadow under the window picture
                    for (grow, a) in [(3.0, 18u8), (1.5, 30)] {
                        let g = grow * s;
                        fill_rounded(pm, tx - g, ty - g + 1.5 * s, tw + 2.0 * g, th + 2.0 * g, 6.0 * s + g, [0, 0, 0, shade(a)]);
                    }
                    let k = tw / pic.width() as f32;
                    let paint = PixmapPaint {
                        quality: FilterQuality::Bicubic,
                        opacity: reveal,
                        ..Default::default()
                    };
                    pm.draw_pixmap(0, 0, pic.as_ref().as_ref(), &paint, Transform::from_row(k, 0.0, 0.0, k, tx, ty), None);
                    // the app's icon as a badge in the corner
                    let b = (p.size * 0.42 * s).round();
                    if let Some(icon) = app_icon(badge_raster_px) {
                        draw_icon(pm, &icon, ix + size_px - b, top + size_px - b, b, pressed);
                    }
                }
                _ => match app_icon(raster_px) {
                    Some(icon) => draw_icon(pm, &icon, ix, top, size_px, pressed),
                    None => fill_rounded(pm, ix, top, size_px, size_px, p.size * 0.22 * s, [128, 128, 128, 200]),
                },
            }
            if it.running && self.cfg.show_indicators && !it.leaving {
                let (dx, dy) = self.map_point(p.x + p.size / 2.0, p.bottom + DOT_SPACE * 0.55, sw, sh);
                fill_circle(pm, dx * s, dy * s, 2.0 * s * it.appear, theme.dock_dot);
            }
        }

        // the dragged icon follows the pointer; away from the Dock it fades and says "Remove"
        if let Some((d, it)) = self
            .drag
            .as_ref()
            .filter(|d| d.active)
            .and_then(|d| Some((d, self.items.iter().find(|it| it.kind.key() == d.key)?)))
        {
            let size = self.icon_max().max(self.icon());
            let (_, depth) = self.axes(sw, sh);
            let v = (d.pos.1 - size / 2.0).clamp(0.0, (depth - size).max(0.0));
            let (x, y, _, _) = self.map_rect((d.pos.0 - size / 2.0, v, size, size), sw, sh);
            let outside = self.drag_outside(sw, sh);
            let alpha = if outside { 0.55 } else { 1.0 };
            if let Some(icon) = it
                .icon_name
                .as_deref()
                .and_then(|n| apps.icon(n, (size * s).round() as u32))
                .or_else(|| apps.icon("application-x-executable", (size * s).round() as u32))
            {
                draw_icon_alpha(pm, &icon, x * s, y * s, size * s, alpha);
            }
            let pinned = matches!(it.kind, ItemKind::App { pinned: true, .. });
            if outside && pinned {
                let de = std::env::var("LANG").map(|l| l.starts_with("de")).unwrap_or(false);
                let label = if de { "Entfernen" } else { "Remove" };
                let px = 13.0 * s;
                let tw = fonts.measure(label, px, Weight::Regular) + 20.0 * s;
                let th = 22.0 * s;
                let (tx, ty) = match self.edge() {
                    DockPosition::Bottom => ((x + size / 2.0) * s - tw / 2.0, y * s - th - 6.0 * s),
                    DockPosition::Left => ((x + size + 8.0) * s, (y + size / 2.0) * s - th / 2.0),
                    DockPosition::Right => ((x - 8.0) * s - tw, (y + size / 2.0) * s - th / 2.0),
                };
                let tx = crate::draw::clamp_span(tx, 2.0, pm.width() as f32 - tw - 2.0);
                let ty = ty.max(2.0);
                fill_rounded(pm, tx, ty, tw, th, 6.0 * s, theme.tooltip_bg);
                let baseline = fonts.baseline_centered(ty, th, px, Weight::Regular);
                fonts.draw(pm, label, tx + 10.0 * s, baseline, px, Weight::Regular, theme.tooltip_text);
            }
            return;
        }

        // tooltip above hovered item
        if let Some(i) = self.hovered {
            if let (Some(it), Some(p)) = (self.items.get(i), placed.get(i)) {
                if !it.name.is_empty() && self.mag > 0.5 {
                    let px = 13.0 * s;
                    let tw = fonts.measure(&it.name, px, Weight::Regular) + 20.0 * s;
                    let th = 22.0 * s;
                    let (ix, iy, iw, ih) = self.tile_rect(p, sw, sh);
                    let (tx, ty) = match self.edge() {
                        DockPosition::Bottom => {
                            let cx = (ix + iw / 2.0) * s;
                            (
                                crate::draw::clamp_span(cx - tw / 2.0, 2.0, pm.width() as f32 - tw - 2.0),
                                (iy - 12.0) * s - th,
                            )
                        }
                        // beside a side Dock, vertically centred on the icon
                        DockPosition::Left => ((ix + iw + 12.0) * s, (iy + ih / 2.0) * s - th / 2.0),
                        DockPosition::Right => ((ix - 12.0) * s - tw, (iy + ih / 2.0) * s - th / 2.0),
                    };
                    fill_rounded(pm, tx, ty, tw, th, 6.0 * s, theme.tooltip_bg);
                    let baseline = fonts.baseline_centered(ty, th, px, Weight::Regular);
                    fonts.draw(pm, &it.name, tx + 10.0 * s, baseline, px, Weight::Regular, theme.tooltip_text);
                }
            }
        }
    }
}

/// Launchpad's tile: a dark rounded square with a 3×3 grid of coloured tiles.
pub fn draw_launchpad_icon(pm: &mut Pixmap, x: f32, y: f32, size: f32, pressed: bool) {
    use tiny_skia::{Color, GradientStop, LinearGradient, Paint, Point, SpreadMode};
    let inset = size * 0.08;
    let (bx, by, bs) = (x + inset, y + inset, size - 2.0 * inset);
    let r = bs * 0.225;
    // drop shadow
    fill_rounded(pm, bx, by + bs * 0.02, bs, bs, r, [0, 0, 0, 40]);
    if let Some(path) = crate::draw::rounded_rect(bx, by, bs, bs, r) {
        let mut paint = Paint::default();
        paint.anti_alias = true;
        paint.shader = LinearGradient::new(
            Point::from_xy(bx, by),
            Point::from_xy(bx, by + bs),
            vec![
                GradientStop::new(0.0, Color::from_rgba8(78, 78, 84, 255)),
                GradientStop::new(1.0, Color::from_rgba8(32, 32, 36, 255)),
            ],
            SpreadMode::Pad,
            Transform::identity(),
        )
        .unwrap_or(paint.shader);
        pm.fill_path(&path, &paint, tiny_skia::FillRule::Winding, Transform::identity(), None);
    }
    stroke_rounded(pm, bx + 0.5, by + 0.5, bs - 1.0, bs - 1.0, r, 1.0, [255, 255, 255, 30]);
    const COLORS: [[u8; 4]; 9] = [
        [255, 95, 87, 255],
        [255, 189, 46, 255],
        [40, 200, 64, 255],
        [10, 132, 255, 255],
        [191, 90, 242, 255],
        [255, 159, 10, 255],
        [100, 210, 255, 255],
        [255, 55, 95, 255],
        [48, 209, 88, 255],
    ];
    let cell = bs * 0.19;
    let gap = bs * 0.075;
    let grid = 3.0 * cell + 2.0 * gap;
    let (gx, gy) = (bx + (bs - grid) / 2.0, by + (bs - grid) / 2.0);
    for (i, c) in COLORS.iter().enumerate() {
        let (cx, cy) = ((i % 3) as f32, (i / 3) as f32);
        fill_rounded(pm, gx + cx * (cell + gap), gy + cy * (cell + gap), cell, cell, cell * 0.28, *c);
    }
    if pressed {
        fill_rounded(pm, bx, by, bs, bs, r, [0, 0, 0, 90]);
    }
}

fn draw_icon_alpha(pm: &mut Pixmap, icon: &Rc<Pixmap>, x: f32, y: f32, size: f32, alpha: f32) {
    let k = size / icon.width() as f32;
    let paint = PixmapPaint {
        quality: FilterQuality::Bilinear,
        opacity: alpha,
        ..Default::default()
    };
    pm.draw_pixmap(0, 0, icon.as_ref().as_ref(), &paint, Transform::from_row(k, 0.0, 0.0, k, x, y), None);
}

fn draw_icon(pm: &mut Pixmap, icon: &Rc<Pixmap>, x: f32, y: f32, size: f32, pressed: bool) {
    let k = size / icon.width() as f32;
    let paint = PixmapPaint {
        quality: FilterQuality::Bilinear,
        ..Default::default()
    };
    pm.draw_pixmap(0, 0, icon.as_ref().as_ref(), &paint, Transform::from_row(k, 0.0, 0.0, k, x, y), None);
    if pressed {
        // darken while pressed, like macOS
        let mut dark = icon.as_ref().clone();
        for px in dark.data_mut().chunks_exact_mut(4) {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
            px[3] = (px[3] as u32 * 90 / 255) as u8;
        }
        pm.draw_pixmap(0, 0, dark.as_ref(), &paint, Transform::from_row(k, 0.0, 0.0, k, x, y), None);
    }
}

fn trash_full() -> bool {
    let Some(home) = std::env::var_os("HOME") else { return false };
    let dir = std::path::PathBuf::from(home).join(".local/share/Trash/files");
    std::fs::read_dir(dir).map(|mut d| d.next().is_some()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dock_with(n: usize) -> Dock {
        let mut d = Dock::new(vec![]);
        for i in 0..n {
            d.items.push(Item::new(
                ItemKind::App {
                    desktop_id: None,
                    app_id: format!("app{i}"),
                    pinned: true,
                },
                format!("App {i}"),
                None,
                false,
            ));
        }
        d
    }

    #[test]
    fn layout_is_centered_and_magnifies() {
        let mut d = dock_with(5);
        let (p, bg) = d.layout(1000.0, 200.0);
        assert!((bg.0 + bg.2 / 2.0 - 500.0).abs() < 0.5);
        let (icon, icon_max) = (d.icon(), d.icon_max());
        assert!(p.iter().all(|p| (p.size - icon).abs() < 1e-3));
        d.hover_x = Some(p[2].x + icon / 2.0);
        d.mag = 1.0;
        let (p2, _) = d.layout(1000.0, 200.0);
        assert!((p2[2].size - icon_max).abs() < 0.5);
        assert!(p2[1].size > icon && p2[1].size < icon_max);
        assert!((p2[0].size - p2[4].size).abs() < 0.5, "symmetric");
    }

    #[test]
    fn hit_test_finds_items() {
        let d = dock_with(3);
        let (p, bg) = d.layout(800.0, 200.0);
        let y = bg.1 + bg.3 / 2.0;
        assert_eq!(d.hit(p[1].x + 5.0, y, 800.0, 200.0), Some(1));
        assert_eq!(d.hit(5.0, y, 800.0, 200.0), None);
    }

    #[test]
    fn leaving_pointer_fades_magnification_out() {
        let mut d = dock_with(5);
        let (p, _) = d.layout(1000.0, 200.0);
        d.hover_x = Some(p[2].x + d.icon() / 2.0);
        d.mag = 1.0;
        d.step(0.0);
        d.hover_x = None;
        let (p2, _) = d.layout(1000.0, 200.0);
        assert!(p2[2].size > d.icon() + 1.0, "no jump back to the base size");
        for _ in 0..200 {
            d.step(0.01);
        }
        let (p3, _) = d.layout(1000.0, 200.0);
        assert!((p3[2].size - d.icon()).abs() < 0.1);
    }

    #[test]
    fn next_minimized_slot_is_where_the_item_settles() {
        let mut d = dock_with(3);
        d.items.push(Item::new(ItemKind::Separator, String::new(), None, false));
        d.items.push(Item::new(ItemKind::Trash, "Trash".into(), None, false));
        let (_, slot) = d.genie_tiles(1000.0, 200.0);
        let slot = slot.unwrap();
        // the window gets minimized: its item grows in before the Trash
        let mut it = Item::new(ItemKind::Minimized { window_id: 7, app_id: "a".into(), thumb: None }, "w".into(), None, false);
        it.appear = 0.0;
        let at = d.items.len() - 1;
        d.items.insert(at, it);
        for _ in 0..300 {
            d.step(0.01);
        }
        let (p, _) = d.layout(1000.0, 200.0);
        assert!((p[at].x - slot.0).abs() < 0.5 && (p[at].size - slot.2).abs() < 0.5);
    }

    fn app(id: &str, pinned: bool) -> Item {
        Item::new(
            ItemKind::App { desktop_id: Some(id.into()), app_id: id.into(), pinned },
            id.into(),
            None,
            !pinned,
        )
    }

    fn real_dock() -> Dock {
        let mut d = Dock::new(vec![]);
        d.items = vec![
            app("a", true),
            Item::new(ItemKind::Launchpad, "Launchpad".into(), None, false),
            app("b", true),
            app("c", true),
            app("r", false),
            Item::new(ItemKind::Separator, String::new(), None, false),
            Item::new(ItemKind::Trash, "Trash".into(), None, false),
        ];
        d
    }

    fn center_of(d: &Dock, key: &str) -> (f32, f32) {
        let (placed, bg) = d.layout(1000.0, 200.0);
        let i = d.items.iter().position(|it| it.kind.key() == key).unwrap();
        (placed[i].x + placed[i].size / 2.0, bg.1 + bg.3 / 2.0)
    }

    #[test]
    fn drag_reorders_and_keeps_a_running_app() {
        let mut d = real_dock();
        // drag "c" to the far left
        let (x, y) = center_of(&d, "app:c");
        d.press_for_drag(3, x, y, 1000.0, 200.0);
        assert!(!d.drag_motion(x + 2.0, y, 1000.0, 200.0), "below the threshold");
        let (ax, _) = center_of(&d, "app:a");
        assert!(d.drag_motion(ax - 30.0, y, 1000.0, 200.0));
        assert_eq!(d.items[0].kind.key(), "app:c");
        assert_eq!(d.items[1].kind.key(), "launchpad", "Launchpad stays after the first app");
        assert_eq!(d.finish_drag(1000.0, 200.0), Some(DragOutcome::Pinned(vec!["c".into(), "a".into(), "b".into()])));

        // a running app dropped between kept ones gets kept there
        let mut d = real_dock();
        let (x, y) = center_of(&d, "app:r");
        d.press_for_drag(4, x, y, 1000.0, 200.0);
        let (bx, _) = center_of(&d, "app:b");
        d.drag_motion(bx - 5.0, y, 1000.0, 200.0);
        d.drag_motion(bx - 10.0, y, 1000.0, 200.0);
        assert_eq!(
            d.finish_drag(1000.0, 200.0),
            Some(DragOutcome::Pinned(vec!["a".into(), "r".into(), "b".into(), "c".into()]))
        );
    }

    #[test]
    fn drag_away_removes_only_kept_apps() {
        let mut d = real_dock();
        let (x, y) = center_of(&d, "app:b");
        d.press_for_drag(2, x, y, 1000.0, 200.0);
        d.drag_motion(x, y - 120.0, 1000.0, 200.0);
        assert!(d.drag_outside(1000.0, 200.0));
        assert_eq!(d.finish_drag(1000.0, 200.0), Some(DragOutcome::Remove("b".into())));
        let mut d = real_dock();
        let (x, y) = center_of(&d, "app:r");
        d.press_for_drag(4, x, y, 1000.0, 200.0);
        d.drag_motion(x, y - 120.0, 1000.0, 200.0);
        assert_eq!(d.finish_drag(1000.0, 200.0), None);
        // the Trash cannot be dragged
        let mut d = real_dock();
        d.press_for_drag(6, 10.0, 10.0, 1000.0, 200.0);
        assert!(d.drag.is_none());
    }

    #[test]
    fn side_docks_are_vertical_and_hit_testable() {
        for pos in [DockPosition::Left, DockPosition::Right] {
            let mut d = dock_with(4);
            d.cfg.position = pos;
            let (sw, sh) = (d.surface_height(), 900.0);
            let bg = d.surface_bg(sw, sh);
            // taller than wide, vertically centred, against its edge
            assert!(bg.3 > bg.2);
            assert!((bg.1 + bg.3 / 2.0 - 450.0).abs() < 0.5);
            match pos {
                DockPosition::Left => assert!((bg.0 - MARGIN_BOTTOM).abs() < 0.5),
                _ => assert!((bg.0 + bg.2 - (sw - MARGIN_BOTTOM)).abs() < 0.5),
            }
            let (placed, _) = d.layout(sw, sh);
            let (x, y, w, h) = d.tile_rect(&placed[2], sw, sh);
            assert_eq!(d.hit(x + w / 2.0, y + h / 2.0, sw, sh), Some(2));
            // icons go down the Dock in order
            let (_, y1, _, _) = d.tile_rect(&placed[1], sw, sh);
            assert!(y1 < y);
        }
    }

    #[test]
    fn settings_change_sizes() {
        let mut d = dock_with(3);
        d.cfg.icon_size = 40.0;
        d.cfg.magnification = false;
        assert_eq!(d.icon_max(), 40.0);
        let (p, _) = d.layout(800.0, 200.0);
        assert!(p.iter().all(|p| (p.size - 40.0).abs() < 1e-3));
        d.cfg.autohide = DockAutohide::Always;
        assert_eq!(d.exclusive_zone(), 0.0);
        d.step(1.0);
        assert!(d.is_hidden());
        let (_, bg) = d.layout(800.0, d.surface_height());
        assert!(bg.1 >= d.surface_height(), "hidden dock is below the edge");

        // AvoidWindows: no exclusive zone, hides only when obstructed (and reveals on hover)
        d.cfg.autohide = DockAutohide::AvoidWindows;
        assert_eq!(d.exclusive_zone(), 0.0);
        d.obstructed = false;
        d.step(1.0);
        assert!(!d.is_hidden());
        d.obstructed = true;
        d.step(1.0);
        assert!(d.is_hidden());
        d.hover_x = Some(400.0);
        d.step(1.0);
        assert!(!d.is_hidden());
        // hitting the bottom monitor edge pixel still hits the icon above it when shown,
        // and does not hit when hidden
        let (p, _) = d.layout(800.0, d.surface_height());
        assert_eq!(d.hit(p[1].x + 5.0, d.surface_height() - 1.0, 800.0, d.surface_height()), Some(1));
        d.hover_x = None;
        d.step(1.0);
        assert!(d.is_hidden());
        assert_eq!(d.hit(p[1].x + 5.0, d.surface_height() - 1.0, 800.0, d.surface_height()), None);
    }
}
