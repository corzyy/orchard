//! Notification server (`org.freedesktop.Notifications`) with macOS-style banners.
//!
//! The D-Bus side runs on zbus' own executor thread and forwards everything to
//! the shell's event loop through a calloop channel. Banners stack in the top
//! right corner under the menu bar, slide in, expire after a few seconds (not
//! while hovered) and support the default action (click), extra action buttons
//! and a close button on hover.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use smithay_client_toolkit::reexports::calloop::channel::Sender;
use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};
use zbus::zvariant::{OwnedValue, Value};

use crate::{
    apps::Apps,
    draw::{fill_circle, fill_rounded, stroke_rounded},
    text::{Fonts, Rgba, Weight},
    theme::Theme,
};

pub const WIDTH: f32 = 344.0;
/// transparent margin around the banners (room for the slide-in)
pub const PAD: f32 = 10.0;
const GAP: f32 = 10.0;
const INNER: f32 = 14.0;
const ICON: f32 = 36.0;
const TITLE_PX: f32 = 13.0;
const BODY_PX: f32 = 13.0;
const LINE: f32 = 17.0;
const BUTTON_H: f32 = 26.0;
const MAX_VISIBLE: usize = 4;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(6);
pub const SLIDE: Duration = Duration::from_millis(280);

/// Messages from the D-Bus thread.
pub enum Msg {
    Notify(Box<Incoming>),
    Close(u32),
}

#[derive(Clone)]
pub struct Incoming {
    pub id: u32,
    pub app_name: String,
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    pub actions: Vec<(String, String)>,
    pub desktop_entry: Option<String>,
    pub image: Option<Pixmap>,
    pub image_path: Option<String>,
    pub critical: bool,
    pub resident: bool,
    /// None = default, Some(0) = never expires, Some(ms)
    pub timeout_ms: Option<u32>,
}

pub struct Banner {
    pub n: Incoming,
    pub shown: Instant,
    pub expires: Option<Instant>,
}

/// A notification kept for the Notification Center.
#[derive(Clone)]
pub struct Past {
    pub n: Incoming,
    pub received: Instant,
}

/// Why a notification closed (spec values).
#[derive(Clone, Copy)]
pub enum Reason {
    Expired = 1,
    Dismissed = 2,
    Closed = 3,
}

/// What a click on a notification card hit.
#[derive(Debug, Clone, PartialEq)]
pub enum Hit {
    Body(u32),
    Close(u32),
    Action(u32, String),
}

const HISTORY: usize = 50;

#[derive(Default)]
pub struct Center {
    pub banners: Vec<Banner>,
    /// newest first; what the Notification Center lists
    pub history: Vec<Past>,
    /// pointer position (surface-local) while over the banner surface
    pub hover: Option<(f32, f32)>,
    pub do_not_disturb: bool,
    /// Game Mode holds banners back (without touching the saved DND switch)
    pub game_quiet: bool,
}

/// Text layout of one notification card.
pub struct CardLayout {
    pub h: f32,
    lines: Vec<String>,
    title: String,
    /// (action key, x offset in the card, width)
    buttons: Vec<(String, f32, f32)>,
}

/// Where the clickable parts of a painted card are (surface coordinates).
#[derive(Clone, Debug)]
pub struct CardHit {
    pub id: u32,
    pub rect: (f32, f32, f32, f32),
    pub close: (f32, f32, f32),
    pub buttons: Vec<(String, (f32, f32, f32, f32))>,
}

impl CardHit {
    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        let (bx, by, bw, bh) = self.rect;
        let (cx, cy, r) = self.close;
        let on_close = (x - cx).powi(2) + (y - cy).powi(2) <= (r + 3.0).powi(2);
        if !on_close && !(x >= bx && x < bx + bw && y >= by && y < by + bh) {
            return None;
        }
        if on_close {
            return Some(Hit::Close(self.id));
        }
        for (key, (ax, ay, aw, ah)) in &self.buttons {
            if x >= *ax && x < ax + aw && y >= *ay && y < ay + ah {
                return Some(Hit::Action(self.id, key.clone()));
            }
        }
        Some(Hit::Body(self.id))
    }
}

pub fn card_layout(fonts: &Fonts, n: &Incoming, width: f32) -> CardLayout {
    let text_w = width - INNER * 3.0 - ICON;
    let title_src = if n.summary.is_empty() { &n.app_name } else { &n.summary };
    let title = fonts.elide(title_src, TITLE_PX, Weight::Bold, text_w - 40.0);
    let lines = fonts.wrap(&n.body, BODY_PX, Weight::Regular, text_w, 4);
    let mut h = INNER * 2.0 + LINE * (1 + lines.len()) as f32;
    h = h.max(INNER * 2.0 + ICON);
    let mut buttons = Vec::new();
    let extra: Vec<&(String, String)> = n.actions.iter().filter(|(k, _)| k != "default").collect();
    if !extra.is_empty() {
        let mut x = INNER * 2.0 + ICON;
        for (key, label) in extra.into_iter().take(3) {
            let label = fonts.elide(label, 12.0, Weight::Regular, 120.0);
            let w = fonts.measure(&label, 12.0, Weight::Regular) + 24.0;
            buttons.push((key.clone(), x, w));
            x += w + 8.0;
        }
        h += BUTTON_H + 6.0;
    }
    CardLayout { h, lines, title, buttons }
}

/// Paint one notification card at (x, y) (logical, surface coordinates).
#[allow(clippy::too_many_arguments)]
pub fn paint_card(
    pm: &mut Pixmap,
    s: f32,
    x: f32,
    y: f32,
    width: f32,
    n: &Incoming,
    l: &CardLayout,
    age: Duration,
    show_close: bool,
    apps: &Apps,
    fonts: &Fonts,
    theme: &Theme,
) -> CardHit {
    let dark = theme.bar_text[0] > 128;
    let solid = theme.menu_bg[3] == 255;
    let alpha = if solid { 255 } else { 205 };
    let bg: Rgba = if dark { [44, 44, 46, alpha] } else { [246, 246, 246, alpha] };
    let border: Rgba = if dark { [255, 255, 255, 30] } else { [0, 0, 0, 22] };
    let title_c = theme.menu_text;
    let body_c: Rgba = if dark { [220, 220, 220, 255] } else { [45, 45, 45, 255] };
    let button_bg: Rgba = if dark { [255, 255, 255, 28] } else { [0, 0, 0, 18] };
    fill_rounded(pm, x * s, y * s, width * s, l.h * s, 16.0 * s, bg);
    stroke_rounded(pm, x * s, y * s, width * s, l.h * s, 16.0 * s, 1.0 * s, border);

    // icon: image hint, image path, app icon, desktop entry icon
    let px = (ICON * s).round() as u32;
    let icon = n
        .image
        .clone()
        .map(std::rc::Rc::new)
        .or_else(|| n.image_path.as_deref().and_then(|p| icon_by_name_or_path(apps, p, px)))
        .or_else(|| icon_by_name_or_path(apps, &n.app_icon, px))
        .or_else(|| {
            let entry = n.desktop_entry.as_deref().and_then(|d| apps.for_app_id(d));
            entry.and_then(|e| e.icon.clone()).and_then(|i| apps.icon(&i, px))
        })
        .or_else(|| apps.icon("preferences-desktop-notification", px))
        .or_else(|| apps.icon("dialog-information", px));
    if let Some(icon) = icon {
        // fit (keep the aspect ratio, e.g. screenshot thumbnails), centered in the icon box
        let k = (ICON * s) / icon.width().max(icon.height()) as f32;
        let ox = (ICON * s - icon.width() as f32 * k) / 2.0;
        let oy = (ICON * s - icon.height() as f32 * k) / 2.0;
        let paint = PixmapPaint {
            quality: FilterQuality::Bicubic,
            ..Default::default()
        };
        pm.draw_pixmap(
            0,
            0,
            icon.as_ref().as_ref(),
            &paint,
            Transform::from_row(k, 0.0, 0.0, k, (x + INNER) * s + ox, (y + INNER) * s + oy),
            None,
        );
    }

    // text
    let tx = x + INNER * 2.0 + ICON;
    let mut line_top = y + INNER;
    let base = fonts.baseline_centered(line_top * s, LINE * s, TITLE_PX * s, Weight::Bold);
    fonts.draw(pm, &l.title, tx * s, base, TITLE_PX * s, Weight::Bold, title_c);
    let age = age_label(age);
    let aw = fonts.measure(&age, 11.0 * s, Weight::Regular);
    let abase = fonts.baseline_centered(line_top * s, LINE * s, 11.0 * s, Weight::Regular);
    fonts.draw(pm, &age, (x + width - INNER) * s - aw, abase, 11.0 * s, Weight::Regular, theme.menu_text_disabled);
    for line in &l.lines {
        line_top += LINE;
        let base = fonts.baseline_centered(line_top * s, LINE * s, BODY_PX * s, Weight::Regular);
        fonts.draw(pm, line, tx * s, base, BODY_PX * s, Weight::Regular, body_c);
    }

    // action buttons
    let mut buttons = Vec::new();
    let by = y + l.h - INNER - BUTTON_H + 4.0;
    for (key, bx, bw) in &l.buttons {
        let label = n
            .actions
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| fonts.elide(v, 12.0, Weight::Regular, 120.0))
            .unwrap_or_default();
        let ax = x + bx;
        fill_rounded(pm, ax * s, by * s, bw * s, BUTTON_H * s, 7.0 * s, button_bg);
        let base = fonts.baseline_centered(by * s, BUTTON_H * s, 12.0 * s, Weight::Regular);
        fonts.draw(pm, &label, (ax + 12.0) * s, base, 12.0 * s, Weight::Regular, title_c);
        buttons.push((key.clone(), (ax, by, *bw, BUTTON_H)));
    }

    // close button on hover (top-left, like macOS)
    let close = (x + 2.0, y + 2.0, 9.0);
    if show_close {
        let ring: Rgba = if dark { [90, 90, 92, 255] } else { [255, 255, 255, 255] };
        fill_circle(pm, close.0 * s, close.1 * s, (close.2 + 1.0) * s, border);
        fill_circle(pm, close.0 * s, close.1 * s, close.2 * s, ring);
        draw_x(pm, close.0 * s, close.1 * s, 3.5 * s, title_c);
    }
    CardHit {
        id: n.id,
        rect: (x, y, width, l.h),
        close,
        buttons,
    }
}

impl Center {
    pub fn add(&mut self, n: Incoming) {
        let timeout = match n.timeout_ms {
            Some(0) => None,
            _ if n.critical || n.resident => None,
            Some(ms) => Some(Duration::from_millis(ms as u64).max(Duration::from_secs(2))),
            None => Some(DEFAULT_TIMEOUT),
        };
        let now = Instant::now();
        let expires = timeout.map(|t| now + t);
        // history (the Notification Center); replacements update in place
        match self.history.iter_mut().find(|p| p.n.id == n.id) {
            Some(p) => p.n = n.clone(),
            None => {
                self.history.insert(0, Past { n: n.clone(), received: now });
                self.history.truncate(HISTORY);
            }
        }
        if let Some(b) = self.banners.iter_mut().find(|b| b.n.id == n.id) {
            // replacement: update in place without sliding in again
            b.n = n;
            b.expires = expires;
            return;
        }
        if (self.do_not_disturb || self.game_quiet) && !n.critical {
            return;
        }
        self.banners.insert(0, Banner { n, shown: now, expires });
    }

    /// Gone for good (dismissed, clicked, closed by the app). True if it was showing.
    pub fn remove(&mut self, id: u32) -> bool {
        self.history.retain(|p| p.n.id != id);
        let before = self.banners.len();
        self.banners.retain(|b| b.n.id != id);
        self.banners.len() != before
    }

    pub fn clear_history(&mut self) -> Vec<u32> {
        let ids = self.history.iter().map(|p| p.n.id).collect();
        self.history.clear();
        ids
    }

    /// Remove expired banners (except the hovered one); returns their ids.
    /// They stay in the history.
    pub fn expire(&mut self, now: Instant) -> Vec<u32> {
        let hovered = self.hover.and_then(|(x, y)| self.banner_at(x, y));
        let mut gone = Vec::new();
        self.banners.retain(|b| {
            let keep = b.expires.map(|e| e > now).unwrap_or(true) || Some(b.n.id) == hovered;
            if !keep {
                gone.push(b.n.id);
            }
            keep
        });
        gone
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.banners.iter().filter_map(|b| b.expires).min()
    }

    pub fn is_animating(&self) -> bool {
        self.banners.iter().take(MAX_VISIBLE).any(|b| b.shown.elapsed() < SLIDE)
    }

    fn visible(&self) -> impl Iterator<Item = &Banner> {
        self.banners.iter().take(MAX_VISIBLE)
    }

    /// (y, layout) of the visible banners.
    fn layouts(&self, fonts: &Fonts) -> Vec<(f32, CardLayout)> {
        let mut y = PAD;
        let mut out = Vec::new();
        for b in self.visible() {
            let l = card_layout(fonts, &b.n, WIDTH);
            let h = l.h;
            out.push((y, l));
            y += h + GAP;
        }
        out
    }

    /// Surface size for the current banners (logical px).
    pub fn size(&self, fonts: &Fonts) -> (u32, u32) {
        let ls = self.layouts(fonts);
        let h = ls.last().map(|(y, l)| y + l.h + PAD).unwrap_or(0.0);
        ((WIDTH + 2.0 * PAD).ceil() as u32, h.ceil() as u32)
    }

    /// Banner rects (x, y, w, h) in surface coordinates, including the slide offset.
    pub fn rects(&self, fonts: &Fonts) -> Vec<(f32, f32, f32, f32)> {
        self.layouts(fonts)
            .iter()
            .zip(self.visible())
            .map(|((y, l), b)| (PAD + slide_offset(b), *y, WIDTH, l.h))
            .collect()
    }

    fn banner_at(&self, x: f32, y: f32) -> Option<u32> {
        // hover uses the cached rects from the last paint
        HIT_CACHE.with(|c| {
            c.borrow()
                .iter()
                .find(|h| {
                    let (bx, by, bw, bh) = h.rect;
                    x >= bx && x < bx + bw && y >= by && y < by + bh
                })
                .map(|h| h.id)
        })
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        HIT_CACHE.with(|c| c.borrow().iter().find_map(|h| h.hit(x, y)))
    }

    pub fn paint(&self, pm: &mut Pixmap, scale: f32, apps: &Apps, fonts: &Fonts, theme: &Theme) {
        let hover = self.hover.and_then(|(x, y)| self.banner_at(x, y));
        let mut cache = Vec::new();
        for ((y, l), b) in self.layouts(fonts).into_iter().zip(self.visible()) {
            let x = PAD + slide_offset(b);
            let hit = paint_card(
                pm,
                scale,
                x,
                y,
                WIDTH,
                &b.n,
                &l,
                b.shown.elapsed(),
                hover == Some(b.n.id),
                apps,
                fonts,
                theme,
            );
            cache.push(hit);
        }
        HIT_CACHE.with(|c| *c.borrow_mut() = cache);
    }
}

thread_local! {
    static HIT_CACHE: std::cell::RefCell<Vec<CardHit>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// `name` is a path / file:// URI or an icon theme name (libnotify sends both).
fn icon_by_name_or_path(apps: &Apps, name: &str, px: u32) -> Option<std::rc::Rc<Pixmap>> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let path = name.strip_prefix("file://").unwrap_or(name);
    if path.starts_with('/') {
        return apps.icon_by_path(path, px);
    }
    apps.icon(name, px)
}

fn draw_x(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    let mut pb = tiny_skia::PathBuilder::new();
    pb.move_to(cx - r, cy - r);
    pb.line_to(cx + r, cy + r);
    pb.move_to(cx + r, cy - r);
    pb.line_to(cx - r, cy + r);
    if let Some(p) = pb.finish() {
        let st = tiny_skia::Stroke {
            width: r * 0.45,
            line_cap: tiny_skia::LineCap::Round,
            ..Default::default()
        };
        pm.stroke_path(&p, &crate::draw::paint(c), &st, Transform::identity(), None);
    }
}

fn slide_offset(b: &Banner) -> f32 {
    let t = (b.shown.elapsed().as_secs_f32() / SLIDE.as_secs_f32()).clamp(0.0, 1.0);
    let ease = 1.0 - (1.0 - t).powi(3);
    (1.0 - ease) * (WIDTH + PAD)
}

fn age_label(d: Duration) -> String {
    let german = std::env::var("LANG").map(|l| l.starts_with("de")).unwrap_or(false);
    let m = d.as_secs() / 60;
    let h = m / 60;
    match (m, h, german) {
        (0, _, false) => "now".into(),
        (0, _, true) => "jetzt".into(),
        (m, 0, false) => format!("{m}m ago"),
        (m, 0, true) => format!("vor {m} Min."),
        (_, h, false) => format!("{h}h ago"),
        (_, h, true) => format!("vor {h} Std."),
    }
}

// ---- markup ------------------------------------------------------------

/// Strip the small HTML subset notifications may use and decode entities.
pub fn strip_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    let mut tag = String::new();
    for c in s.chars() {
        match (in_tag, c) {
            (false, '<') => {
                in_tag = true;
                tag.clear();
            }
            (true, '>') => {
                in_tag = false;
                let t = tag.trim().to_ascii_lowercase();
                if t == "br" || t == "br/" || t == "br /" || t == "/p" {
                    out.push('\n');
                }
            }
            (true, c) => tag.push(c),
            (false, c) => out.push(c),
        }
    }
    if in_tag {
        // a lone "<" (e.g. "a < b"): keep the text
        out.push('<');
        out.push_str(&tag);
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

// ---- D-Bus ---------------------------------------------------------------

struct Server {
    tx: Sender<Msg>,
    next: Arc<AtomicU32>,
}

fn hint_str(hints: &HashMap<String, OwnedValue>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| match hints.get(*k).map(|v| &**v) {
        Some(Value::Str(s)) => Some(s.to_string()),
        _ => None,
    })
}

fn hint_bool(hints: &HashMap<String, OwnedValue>, key: &str) -> bool {
    matches!(hints.get(key).map(|v| &**v), Some(Value::Bool(true)))
}

fn hint_u8(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<u8> {
    match hints.get(key).map(|v| &**v) {
        Some(Value::U8(b)) => Some(*b),
        Some(Value::U32(b)) => Some(*b as u8),
        Some(Value::I32(b)) => Some(*b as u8),
        _ => None,
    }
}

/// `image-data` (iiibiiay) -> premultiplied pixmap, downscaled later when drawn.
fn hint_image(hints: &HashMap<String, OwnedValue>) -> Option<Pixmap> {
    let v = ["image-data", "image_data", "icon_data"].iter().find_map(|k| hints.get(*k))?;
    let Value::Structure(st) = &**v else { return None };
    let f = st.fields();
    if f.len() != 7 {
        return None;
    }
    let int = |v: &Value| match v {
        Value::I32(i) => Some(*i),
        _ => None,
    };
    let (w, h, stride) = (int(&f[0])?, int(&f[1])?, int(&f[2])?);
    let alpha = matches!(f[3], Value::Bool(true));
    let channels = int(&f[5])?;
    let Value::Array(arr) = &f[6] else { return None };
    let bytes: Vec<u8> = arr
        .iter()
        .filter_map(|b| match b {
            Value::U8(b) => Some(*b),
            _ => None,
        })
        .collect();
    if w <= 0 || h <= 0 || w > 1024 || h > 1024 || channels < 3 {
        return None;
    }
    let mut pm = Pixmap::new(w as u32, h as u32)?;
    let data = pm.data_mut();
    for y in 0..h as usize {
        for x in 0..w as usize {
            let src = y * stride as usize + x * channels as usize;
            if src + channels as usize > bytes.len() {
                return None;
            }
            let a = if alpha && channels >= 4 { bytes[src + 3] as u32 } else { 255 };
            let dst = (y * w as usize + x) * 4;
            for k in 0..3 {
                data[dst + k] = (bytes[src + k] as u32 * a / 255) as u8;
            }
            data[dst + 3] = a as u8;
        }
    }
    // Downscale immediately so notification history never retains multi-MB bitmaps
    const MAX_DIM: u32 = 128;
    let (uw, uh) = (w as u32, h as u32);
    if uw > MAX_DIM || uh > MAX_DIM {
        let k = MAX_DIM as f32 / uw.max(uh) as f32;
        let nw = (uw as f32 * k).round().max(1.0) as u32;
        let nh = (uh as f32 * k).round().max(1.0) as u32;
        if let Some(mut small) = Pixmap::new(nw, nh) {
            let paint = tiny_skia::PixmapPaint {
                quality: tiny_skia::FilterQuality::Bicubic,
                ..Default::default()
            };
            small.draw_pixmap(0, 0, pm.as_ref(), &paint, Transform::from_scale(nw as f32 / uw as f32, nh as f32 / uh as f32), None);
            return Some(small);
        }
    }
    Some(pm)
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Server {
    fn get_capabilities(&self) -> Vec<&'static str> {
        vec!["actions", "body", "body-markup", "icon-static"]
    }

    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let id = if replaces_id != 0 {
            replaces_id
        } else {
            self.next.fetch_add(1, Ordering::Relaxed)
        };
        let actions = actions
            .chunks(2)
            .filter(|c| c.len() == 2)
            .map(|c| (c[0].clone(), c[1].clone()))
            .collect();
        let n = Incoming {
            id,
            app_name,
            app_icon,
            summary: strip_markup(&summary),
            body: strip_markup(&body),
            actions,
            desktop_entry: hint_str(&hints, &["desktop-entry"]),
            image: hint_image(&hints),
            image_path: hint_str(&hints, &["image-path", "image_path"]),
            critical: hint_u8(&hints, "urgency") == Some(2),
            resident: hint_bool(&hints, "resident"),
            timeout_ms: (expire_timeout >= 0).then_some(expire_timeout as u32),
        };
        let _ = self.tx.send(Msg::Notify(Box::new(n)));
        id
    }

    fn close_notification(&self, id: u32) {
        let _ = self.tx.send(Msg::Close(id));
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        ("Orchard".into(), "Orchard".into(), env!("CARGO_PKG_VERSION").into(), "1.2".into())
    }
}

pub const PATH: &str = "/org/freedesktop/Notifications";
pub const IFACE: &str = "org.freedesktop.Notifications";

/// Register the server. Returns None (and logs) if another server owns the name.
pub fn start(tx: Sender<Msg>) -> Option<zbus::blocking::Connection> {
    if std::env::var_os("ORCHARD_NO_NOTIFICATIONS").is_some() {
        return None;
    }
    let server = Server {
        tx,
        next: Arc::new(AtomicU32::new(1)),
    };
    let conn = match zbus::blocking::connection::Builder::session()
        .and_then(|b| b.serve_at(PATH, server))
        .and_then(|b| b.build())
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("notifications: no session bus: {e}");
            return None;
        }
    };
    use zbus::fdo::{RequestNameFlags, RequestNameReply};
    match conn.request_name_with_flags(IFACE, RequestNameFlags::DoNotQueue.into()) {
        Ok(RequestNameReply::PrimaryOwner) | Ok(RequestNameReply::AlreadyOwner) => {
            tracing::info!("notifications: serving {IFACE}");
            Some(conn)
        }
        Ok(other) => {
            tracing::info!("notifications: {IFACE} already owned ({other:?}), not serving");
            None
        }
        Err(e) => {
            tracing::info!("notifications: {IFACE} not acquired: {e}");
            None
        }
    }
}

pub fn emit_closed(conn: &zbus::blocking::Connection, id: u32, reason: Reason) {
    if let Err(e) = conn.emit_signal(None::<&str>, PATH, IFACE, "NotificationClosed", &(id, reason as u32)) {
        tracing::debug!("NotificationClosed: {e}");
    }
}

pub fn emit_action(conn: &zbus::blocking::Connection, id: u32, key: &str) {
    if let Err(e) = conn.emit_signal(None::<&str>, PATH, IFACE, "ActionInvoked", &(id, key)) {
        tracing::debug!("ActionInvoked: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::strip_markup;

    #[test]
    fn markup() {
        assert_eq!(strip_markup("<b>Hi</b> &amp; bye"), "Hi & bye");
        assert_eq!(strip_markup("a<br/>b"), "a\nb");
        assert_eq!(strip_markup("<a href=\"x\">link</a>"), "link");
        assert_eq!(strip_markup("1 &lt; 2"), "1 < 2");
    }
}
