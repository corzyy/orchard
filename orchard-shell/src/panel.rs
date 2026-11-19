//! Notification Center (click the clock) and Control Center (the switches icon
//! in the menu bar). Both live on one full-screen, transparent overlay surface,
//! so a click anywhere outside the panel closes it, like on macOS.

use std::time::{Duration, Instant};

use tiny_skia::{FillRule, PathBuilder, Pixmap, PixmapPaint, Stroke, Transform};

use crate::{
    apps::Apps,
    draw::{fill_circle, fill_rounded, paint, stroke_rounded},
    notify::{self, CardHit, Center, Hit},
    status::Network,
    text::{Fonts, Rgba, Weight},
    theme::Theme,
};

pub const NC_WIDTH: f32 = 356.0;
pub const CC_WIDTH: f32 = 320.0;
const MARGIN: f32 = 10.0;
const SLIDE: Duration = Duration::from_millis(260);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Notifications,
    Control,
}

/// What the Control Center shows (queried on a helper thread).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ControlState {
    /// Wi-Fi radio on, connected network
    pub wifi: Option<(bool, Option<String>)>,
    pub bluetooth: Option<bool>,
    /// 0..1, muted
    pub volume: Option<(f32, bool)>,
    /// 0..1 (only with a backlight)
    pub brightness: Option<f32>,
    pub media: Option<Media>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Media {
    pub title: String,
    pub artist: String,
    pub playing: bool,
}

/// Interactive parts of the panel (panel-surface coordinates).
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Card(Hit),
    ClearAll,
    Dnd,
    Wifi,
    Bluetooth,
    /// slider: x range of the track
    Volume(f32, f32),
    Brightness(f32, f32),
    MediaPrev,
    MediaPlay,
    MediaNext,
    /// inside the panel, nothing interactive
    Panel,
}

pub struct Panel {
    pub kind: Kind,
    pub opened: Instant,
    pub scroll: f32,
    pub hover: Option<(f32, f32)>,
    /// x of the right edge of the Control Center icon (Control Center anchors there)
    pub anchor_right: f32,
    pub control: ControlState,
    /// slider being dragged
    pub dragging: Option<Target>,
    // filled by paint, used for hit tests
    targets: Vec<((f32, f32, f32, f32), Target)>,
    cards: Vec<CardHit>,
    panel_rect: (f32, f32, f32, f32),
}

impl Panel {
    pub fn new(kind: Kind, anchor_right: f32) -> Panel {
        Panel {
            kind,
            opened: Instant::now(),
            scroll: 0.0,
            hover: None,
            anchor_right,
            control: ControlState::default(),
            dragging: None,
            targets: Vec::new(),
            cards: Vec::new(),
            panel_rect: (0.0, 0.0, 0.0, 0.0),
        }
    }

    pub fn is_animating(&self) -> bool {
        self.opened.elapsed() < SLIDE
    }

    fn progress(&self) -> f32 {
        let t = (self.opened.elapsed().as_secs_f32() / SLIDE.as_secs_f32()).clamp(0.0, 1.0);
        1.0 - (1.0 - t).powi(3)
    }

    /// The panel's rect (for the frosted backdrop), surface coordinates.
    pub fn rect(&self) -> (f32, f32, f32, f32) {
        self.panel_rect
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<Target> {
        for c in &self.cards {
            if let Some(h) = c.hit(x, y) {
                return Some(Target::Card(h));
            }
        }
        for ((rx, ry, rw, rh), t) in &self.targets {
            if x >= *rx && x < rx + rw && y >= *ry && y < ry + rh {
                return Some(t.clone());
            }
        }
        let (px, py, pw, ph) = self.panel_rect;
        (x >= px && x < px + pw && y >= py && y < py + ph).then_some(Target::Panel)
    }

    /// Slider value for pointer x.
    pub fn slider_value(target: &Target, x: f32) -> Option<f32> {
        match target {
            Target::Volume(a, b) | Target::Brightness(a, b) => Some(((x - a) / (b - a)).clamp(0.0, 1.0)),
            _ => None,
        }
    }

    pub fn scroll_by(&mut self, dy: f32) {
        self.scroll = (self.scroll + dy).max(0.0);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn paint(&mut self, pm: &mut Pixmap, s: f32, w: f32, h: f32, center: &Center, apps: &Apps, fonts: &Fonts, theme: &Theme, german: bool) {
        self.targets.clear();
        self.cards.clear();
        match self.kind {
            Kind::Notifications => self.paint_nc(pm, s, w, h, center, apps, fonts, theme, german),
            Kind::Control => self.paint_cc(pm, s, w, center, fonts, theme, german),
        }
    }

    // ---- Notification Center -------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn paint_nc(&mut self, pm: &mut Pixmap, s: f32, w: f32, h: f32, center: &Center, apps: &Apps, fonts: &Fonts, theme: &Theme, german: bool) {
        let t = |en: &'static str, de: &'static str| if german { de } else { en };
        let slide = (1.0 - self.progress()) * (NC_WIDTH + MARGIN);
        let x0 = w - NC_WIDTH - MARGIN + slide;
        let (top, bottom) = (MARGIN, h - MARGIN);
        self.panel_rect = (x0, top, NC_WIDTH, bottom - top);
        let dark = theme.bar_text[0] > 128;
        let tint: Rgba = if dark { [30, 30, 32, 150] } else { [240, 240, 242, 150] };
        let card: Rgba = if dark { [255, 255, 255, 22] } else { [255, 255, 255, 150] };
        fill_rounded(pm, x0 * s, top * s, NC_WIDTH * s, (bottom - top) * s, 18.0 * s, tint);

        let inner_x = x0 + 12.0;
        let inner_w = NC_WIDTH - 24.0;
        // date
        let date = date_label(german);
        let base = fonts.baseline_centered((top + 12.0) * s, 28.0 * s, 18.0 * s, Weight::Bold);
        fonts.draw(pm, &date, inner_x * s, base, 18.0 * s, Weight::Bold, theme.menu_text);

        // Do Not Disturb row
        let row_y = top + 50.0;
        fill_rounded(pm, inner_x * s, row_y * s, inner_w * s, 44.0 * s, 14.0 * s, card);
        let accent = theme.menu_selection;
        let moon_bg = if center.do_not_disturb { [94, 92, 230, 255] } else { [128, 128, 128, 90] };
        fill_circle(pm, (inner_x + 24.0) * s, (row_y + 22.0) * s, 14.0 * s, moon_bg);
        moon(pm, (inner_x + 24.0) * s, (row_y + 22.0) * s, 7.0 * s, [255, 255, 255, 255]);
        let base = fonts.baseline_centered(row_y * s, 44.0 * s, 13.0 * s, Weight::Bold);
        fonts.draw(pm, t("Do Not Disturb", "Nicht stören"), (inner_x + 48.0) * s, base, 13.0 * s, Weight::Bold, theme.menu_text);
        switch(pm, s, inner_x + inner_w - 52.0, row_y + 11.0, center.do_not_disturb, accent);
        self.targets.push(((inner_x, row_y, inner_w, 44.0), Target::Dnd));

        // section header
        let head_y = row_y + 58.0;
        let base = fonts.baseline_centered(head_y * s, 22.0 * s, 13.0 * s, Weight::Bold);
        fonts.draw(pm, t("Notifications", "Mitteilungen"), inner_x * s, base, 13.0 * s, Weight::Bold, theme.menu_text_disabled);
        if !center.history.is_empty() {
            let label = t("Clear All", "Alle löschen");
            let lw = fonts.measure(label, 12.0 * s, Weight::Regular) / s;
            let lx = inner_x + inner_w - lw - 4.0;
            fonts.draw(pm, label, lx * s, base, 12.0 * s, Weight::Regular, accent);
            self.targets.push(((lx - 6.0, head_y, lw + 12.0, 22.0), Target::ClearAll));
        }

        // the list, clipped to its area by drawing into a separate pixmap
        let list_y = head_y + 30.0;
        let list_h = (bottom - 8.0 - list_y).max(0.0);
        if center.history.is_empty() {
            let msg = t("No Notifications", "Keine Mitteilungen");
            let mw = fonts.measure(msg, 13.0 * s, Weight::Regular) / s;
            let base = fonts.baseline_centered((list_y + 40.0) * s, 20.0 * s, 13.0 * s, Weight::Regular);
            fonts.draw(pm, msg, (inner_x + (inner_w - mw) / 2.0) * s, base, 13.0 * s, Weight::Regular, theme.menu_text_disabled);
            return;
        }
        let layouts: Vec<_> = center.history.iter().map(|p| notify::card_layout(fonts, &p.n, inner_w)).collect();
        let total: f32 = layouts.iter().map(|l| l.h + 10.0).sum();
        self.scroll = self.scroll.min((total - list_h).max(0.0));
        let (Some(mut sub), true) = (Pixmap::new((inner_w * s).ceil().max(1.0) as u32 + 2, (list_h * s).ceil().max(1.0) as u32), list_h > 1.0) else {
            return;
        };
        let hover_id = self.hover.and_then(|(hx, hy)| {
            self.cards_prev_hover(hx, hy)
        });
        let mut y = -self.scroll + 10.0;
        let mut cards = Vec::new();
        for (p, l) in center.history.iter().zip(&layouts) {
            if y + l.h >= 0.0 && y <= list_h {
                let hit = notify::paint_card(&mut sub, s, 1.0, y, inner_w - 2.0, &p.n, l, p.received.elapsed(), hover_id == Some(p.n.id), apps, fonts, theme);
                // back to surface coordinates; cards outside the visible list don't count
                let mut hit = hit;
                hit.rect = (hit.rect.0 + inner_x - 1.0, hit.rect.1 + list_y, hit.rect.2, hit.rect.3);
                hit.close = (hit.close.0 + inner_x - 1.0, hit.close.1 + list_y, hit.close.2);
                for (_, r) in hit.buttons.iter_mut() {
                    r.0 += inner_x - 1.0;
                    r.1 += list_y;
                }
                cards.push(hit);
            }
            y += l.h + 10.0;
        }
        pm.draw_pixmap(
            ((inner_x - 1.0) * s) as i32,
            (list_y * s) as i32,
            sub.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
        // only the visible part of a card is clickable
        for c in cards.iter_mut() {
            let (x, y, w, h) = c.rect;
            let y2 = (y + h).min(list_y + list_h);
            let y1 = y.max(list_y);
            c.rect = (x, y1, w, (y2 - y1).max(0.0));
            if c.close.1 < list_y {
                c.close.2 = -100.0;
            }
        }
        self.cards = cards;
    }

    /// The card under the pointer, from the last paint.
    fn cards_prev_hover(&self, x: f32, y: f32) -> Option<u32> {
        PREV_CARDS.with(|c| {
            c.borrow().iter().find(|(_, (cx, cy, cw, ch))| x >= *cx && x < cx + cw && y >= *cy && y < cy + ch).map(|(id, _)| *id)
        })
    }

    pub fn remember_cards(&self) {
        PREV_CARDS.with(|c| *c.borrow_mut() = self.cards.iter().map(|h| (h.id, h.rect)).collect());
    }

    // ---- Control Center ------------------------------------------------------

    fn paint_cc(&mut self, pm: &mut Pixmap, s: f32, w: f32, center: &Center, fonts: &Fonts, theme: &Theme, german: bool) {
        let t = |en: &'static str, de: &'static str| if german { de } else { en };
        let st = &self.control;
        let dark = theme.bar_text[0] > 128;
        let tint: Rgba = if dark { [30, 30, 32, 150] } else { [240, 240, 242, 150] };
        let card: Rgba = if dark { [255, 255, 255, 22] } else { [255, 255, 255, 150] };
        let off: Rgba = if dark { [255, 255, 255, 40] } else { [0, 0, 0, 30] };
        let accent = theme.menu_selection;
        let text = theme.menu_text;
        let sub = theme.menu_text_disabled;

        let pad = 10.0;
        let x0 = (self.anchor_right - CC_WIDTH).clamp(MARGIN, (w - CC_WIDTH - MARGIN).max(MARGIN));
        let y0 = 6.0 - (1.0 - self.progress()) * 12.0;
        let inner_x = x0 + pad;
        let inner_w = CC_WIDTH - 2.0 * pad;
        let half = (inner_w - pad) / 2.0;

        // compute height first
        let mut height = pad + 110.0 + pad;
        if st.brightness.is_some() {
            height += 64.0 + pad;
        }
        height += 64.0 + pad; // sound
        if st.media.is_some() {
            height += 64.0 + pad;
        }
        self.panel_rect = (x0, y0, CC_WIDTH, height);
        fill_rounded(pm, x0 * s, y0 * s, CC_WIDTH * s, height * s, 18.0 * s, tint);

        // connectivity card: Wi-Fi + Bluetooth
        let y = y0 + pad;
        fill_rounded(pm, inner_x * s, y * s, half * s, 110.0 * s, 14.0 * s, card);
        let (wifi_on, ssid) = st.wifi.clone().unwrap_or((false, None));
        let rows = [
            (
                Target::Wifi,
                "Wi-Fi".to_string(),
                if st.wifi.is_none() {
                    t("Unavailable", "Nicht verfügbar").to_string()
                } else if !wifi_on {
                    t("Off", "Aus").to_string()
                } else {
                    ssid.unwrap_or_else(|| t("Not Connected", "Nicht verbunden").into())
                },
                wifi_on,
                st.wifi.is_some(),
            ),
            (
                Target::Bluetooth,
                "Bluetooth".to_string(),
                match st.bluetooth {
                    Some(true) => t("On", "Ein").to_string(),
                    Some(false) => t("Off", "Aus").to_string(),
                    None => t("Unavailable", "Nicht verfügbar").to_string(),
                },
                st.bluetooth == Some(true),
                st.bluetooth.is_some(),
            ),
        ];
        for (i, (target, title, status, on, available)) in rows.into_iter().enumerate() {
            let ry = y + 10.0 + i as f32 * 48.0;
            let (cx, cy) = (inner_x + 24.0, ry + 20.0);
            fill_circle(pm, cx * s, cy * s, 15.0 * s, if on { accent } else { off });
            let glyph = if on { [255, 255, 255, 255] } else { text };
            match target {
                Target::Wifi => crate::menubar::draw_network(pm, cx * s, (cy - 1.0) * s, s * 0.8, Network::Wifi(3), glyph),
                _ => bluetooth(pm, cx * s, cy * s, 7.0 * s, glyph),
            }
            let tx = inner_x + 46.0;
            let maxw = half - 50.0;
            let b1 = fonts.baseline_centered((ry + 4.0) * s, 16.0 * s, 12.0 * s, Weight::Bold);
            fonts.draw(pm, &fonts.elide(&title, 12.0 * s, Weight::Bold, maxw * s), tx * s, b1, 12.0 * s, Weight::Bold, text);
            let b2 = fonts.baseline_centered((ry + 20.0) * s, 16.0 * s, 11.0 * s, Weight::Regular);
            fonts.draw(pm, &fonts.elide(&status, 11.0 * s, Weight::Regular, maxw * s), tx * s, b2, 11.0 * s, Weight::Regular, sub);
            if available {
                self.targets.push(((inner_x, ry, half, 44.0), target));
            }
        }

        // Do Not Disturb card
        let fx = inner_x + half + pad;
        fill_rounded(pm, fx * s, y * s, half * s, 110.0 * s, 14.0 * s, card);
        let dnd = center.do_not_disturb;
        let (cx, cy) = (fx + 24.0, y + 30.0);
        fill_circle(pm, cx * s, cy * s, 15.0 * s, if dnd { [94, 92, 230, 255] } else { off });
        moon(pm, cx * s, cy * s, 7.0 * s, if dnd { [255, 255, 255, 255] } else { text });
        let b1 = fonts.baseline_centered((y + 54.0) * s, 18.0 * s, 12.0 * s, Weight::Bold);
        fonts.draw(pm, t("Do Not Disturb", "Nicht stören"), (fx + 12.0) * s, b1, 12.0 * s, Weight::Bold, text);
        let b2 = fonts.baseline_centered((y + 72.0) * s, 16.0 * s, 11.0 * s, Weight::Regular);
        fonts.draw(pm, if dnd { t("On", "Ein") } else { t("Off", "Aus") }, (fx + 12.0) * s, b2, 11.0 * s, Weight::Regular, sub);
        self.targets.push(((fx, y, half, 110.0), Target::Dnd));

        // sliders
        let mut y = y + 110.0 + pad;
        let slider = |pm: &mut Pixmap, targets: &mut Vec<_>, y: f32, label: &str, value: f32, icon: u8, target: fn(f32, f32) -> Target| {
            fill_rounded(pm, inner_x * s, y * s, inner_w * s, 64.0 * s, 14.0 * s, card);
            let b = fonts.baseline_centered((y + 6.0) * s, 18.0 * s, 12.0 * s, Weight::Bold);
            fonts.draw(pm, label, (inner_x + 12.0) * s, b, 12.0 * s, Weight::Bold, text);
            let (tx, ty, tw, th) = (inner_x + 12.0, y + 32.0, inner_w - 24.0, 22.0);
            fill_rounded(pm, tx * s, ty * s, tw * s, th * s, 11.0 * s, off);
            let fill_w = (th + (tw - th) * value.clamp(0.0, 1.0)).max(th);
            fill_rounded(pm, tx * s, ty * s, fill_w * s, th * s, 11.0 * s, [255, 255, 255, 235]);
            let knob_x = tx + fill_w - th / 2.0;
            fill_circle(pm, knob_x * s, (ty + th / 2.0) * s, (th / 2.0 - 1.0) * s, [255, 255, 255, 255]);
            stroke_rounded(pm, (knob_x - th / 2.0 + 1.0) * s, (ty + 1.0) * s, (th - 2.0) * s, (th - 2.0) * s, (th / 2.0 - 1.0) * s, 1.0 * s, [0, 0, 0, 30]);
            let glyph: Rgba = [90, 90, 92, 255];
            match icon {
                0 => crate::osd::sun(pm, (tx + th / 2.0) * s, (ty + th / 2.0) * s, 8.0 * s, value, glyph),
                _ => crate::osd::speaker(pm, (tx + th / 2.0 + 1.0) * s, (ty + th / 2.0) * s, 7.0 * s, value, value <= 0.001, glyph),
            }
            targets.push(((tx - 4.0, ty - 6.0, tw + 8.0, th + 12.0), target(tx + th / 2.0, tx + tw - th / 2.0)));
        };
        if let Some(b) = st.brightness {
            slider(pm, &mut self.targets, y, t("Display", "Display"), b, 0, Target::Brightness);
            y += 64.0 + pad;
        }
        let (vol, muted) = st.volume.unwrap_or((0.0, true));
        slider(pm, &mut self.targets, y, t("Sound", "Ton"), if muted { 0.0 } else { vol.min(1.0) }, 1, Target::Volume);
        y += 64.0 + pad;

        // now playing
        if let Some(m) = &st.media {
            fill_rounded(pm, inner_x * s, y * s, inner_w * s, 64.0 * s, 14.0 * s, card);
            let maxw = inner_w - 130.0;
            let b1 = fonts.baseline_centered((y + 12.0) * s, 18.0 * s, 12.0 * s, Weight::Bold);
            let title = if m.title.is_empty() { t("Not Playing", "Keine Wiedergabe") } else { &m.title };
            fonts.draw(pm, &fonts.elide(title, 12.0 * s, Weight::Bold, maxw * s), (inner_x + 12.0) * s, b1, 12.0 * s, Weight::Bold, text);
            let b2 = fonts.baseline_centered((y + 32.0) * s, 16.0 * s, 11.0 * s, Weight::Regular);
            fonts.draw(pm, &fonts.elide(&m.artist, 11.0 * s, Weight::Regular, maxw * s), (inner_x + 12.0) * s, b2, 11.0 * s, Weight::Regular, sub);
            let cy = y + 32.0;
            let bx = inner_x + inner_w - 112.0;
            for (i, target) in [Target::MediaPrev, Target::MediaPlay, Target::MediaNext].into_iter().enumerate() {
                let cx = bx + 18.0 + i as f32 * 36.0;
                match target {
                    Target::MediaPrev => transport(pm, cx * s, cy * s, 6.0 * s, -1, text),
                    Target::MediaNext => transport(pm, cx * s, cy * s, 6.0 * s, 1, text),
                    _ if m.playing => pause(pm, cx * s, cy * s, 7.0 * s, text),
                    _ => play(pm, cx * s, cy * s, 8.0 * s, text),
                }
                self.targets.push(((cx - 16.0, cy - 16.0, 32.0, 32.0), target));
            }
        }
    }
}

thread_local! {
    static PREV_CARDS: std::cell::RefCell<Vec<(u32, (f32, f32, f32, f32))>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// "Thursday, 19 November" / "Donnerstag, 19. November"
fn date_label(german: bool) -> String {
    use chrono::Datelike;
    let now = chrono::Local::now();
    let wd = now.weekday().num_days_from_monday() as usize;
    let m = now.month0() as usize;
    if german {
        const D: [&str; 7] = ["Montag", "Dienstag", "Mittwoch", "Donnerstag", "Freitag", "Samstag", "Sonntag"];
        const M: [&str; 12] = [
            "Januar", "Februar", "März", "April", "Mai", "Juni", "Juli", "August", "September", "Oktober", "November", "Dezember",
        ];
        format!("{}, {}. {}", D[wd], now.day(), M[m])
    } else {
        const D: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
        const M: [&str; 12] = [
            "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December",
        ];
        format!("{}, {} {}", D[wd], now.day(), M[m])
    }
}

// ---- glyphs --------------------------------------------------------------------

/// A crescent moon (Do Not Disturb / Focus).
pub fn moon(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    let mut pb = PathBuilder::new();
    let n = 24;
    // outer arc of the full circle, then the inner arc of the cut-out circle back
    for k in 0..=n {
        let a = (50.0 + 260.0 * k as f32 / n as f32).to_radians();
        let (x, y) = (cx + r * a.cos(), cy + r * a.sin());
        if k == 0 {
            pb.move_to(x, y);
        } else {
            pb.line_to(x, y);
        }
    }
    let (ox, oy, rr) = (cx + r * 0.55, cy - r * 0.45, r * 0.8);
    for k in 0..=n {
        let a = (200.0 - 200.0 * k as f32 / n as f32 + 30.0).to_radians();
        pb.line_to(ox + rr * a.cos(), oy + rr * a.sin());
    }
    pb.close();
    if let Some(p) = pb.finish() {
        pm.fill_path(&p, &paint(c), FillRule::EvenOdd, Transform::identity(), None);
    }
}

fn bluetooth(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    let mut pb = PathBuilder::new();
    let (x, h) = (cx - r * 0.1, r);
    pb.move_to(cx - r * 0.55, cy - h * 0.45);
    pb.line_to(cx + r * 0.5, cy + h * 0.5);
    pb.line_to(x, cy + h);
    pb.line_to(x, cy - h);
    pb.line_to(cx + r * 0.5, cy - h * 0.5);
    pb.line_to(cx - r * 0.55, cy + h * 0.45);
    if let Some(p) = pb.finish() {
        let st = Stroke {
            width: r * 0.22,
            line_cap: tiny_skia::LineCap::Round,
            line_join: tiny_skia::LineJoin::Round,
            ..Default::default()
        };
        pm.stroke_path(&p, &paint(c), &st, Transform::identity(), None);
    }
}

fn play(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    let mut pb = PathBuilder::new();
    pb.move_to(cx - r * 0.6, cy - r);
    pb.line_to(cx + r, cy);
    pb.line_to(cx - r * 0.6, cy + r);
    pb.close();
    if let Some(p) = pb.finish() {
        pm.fill_path(&p, &paint(c), FillRule::Winding, Transform::identity(), None);
    }
}

fn pause(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, c: Rgba) {
    fill_rounded(pm, cx - r * 0.8, cy - r, r * 0.55, r * 2.0, r * 0.15, c);
    fill_rounded(pm, cx + r * 0.25, cy - r, r * 0.55, r * 2.0, r * 0.15, c);
}

/// ⏮ / ⏭: two triangles pointing in `dir`.
fn transport(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, dir: i32, c: Rgba) {
    let d = dir as f32;
    for off in [-r * 0.9, r * 0.1] {
        let base = cx + off * d;
        let mut pb = PathBuilder::new();
        pb.move_to(base, cy - r * 0.8);
        pb.line_to(base + r * 0.9 * d, cy);
        pb.line_to(base, cy + r * 0.8);
        pb.close();
        if let Some(p) = pb.finish() {
            pm.fill_path(&p, &paint(c), FillRule::Winding, Transform::identity(), None);
        }
    }
}

/// iOS-style switch at (x, y), 40×22.
fn switch(pm: &mut Pixmap, s: f32, x: f32, y: f32, on: bool, accent: Rgba) {
    let bg = if on { accent } else { [120, 120, 128, 90] };
    fill_rounded(pm, x * s, y * s, 40.0 * s, 22.0 * s, 11.0 * s, bg);
    let kx = if on { x + 29.0 } else { x + 11.0 };
    fill_circle(pm, kx * s, (y + 11.0) * s, 9.5 * s, [255, 255, 255, 255]);
}

// ---- system state (helper thread) ---------------------------------------------

fn sh(cmd: &str) -> Option<String> {
    // a hung daemon (bluetoothd, NetworkManager …) must not freeze the panel
    let out = std::process::Command::new("timeout").arg("3").arg("sh").arg("-c").arg(cmd).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn query_control() -> ControlState {
    let wifi = sh("nmcli -t radio wifi").map(|r| {
        let on = r.trim() == "enabled";
        let ssid = if on {
            sh("nmcli -t -f active,ssid dev wifi").and_then(|o| {
                o.lines().find_map(|l| l.strip_prefix("yes:").map(|s| s.replace("\\:", ":"))).filter(|s| !s.is_empty())
            })
        } else {
            None
        };
        (on, ssid)
    });
    // no Wi-Fi hardware: nmcli still answers, but there is no wifi device
    let has_wifi = sh("nmcli -t -f type dev").map(|o| o.lines().any(|l| l == "wifi")).unwrap_or(false);
    let bluetooth = sh("bluetoothctl show").and_then(|o| {
        if o.contains("Powered: yes") {
            Some(true)
        } else if o.contains("Powered: no") {
            Some(false)
        } else {
            None
        }
    });
    let volume = sh("wpctl get-volume @DEFAULT_AUDIO_SINK@").and_then(|o| crate::osd::parse_wpctl(&o));
    let brightness = std::fs::read_dir("/sys/class/backlight")
        .ok()
        .and_then(|mut d| d.next())
        .and_then(|_| sh("brightnessctl -m"))
        .and_then(|o| crate::osd::parse_brightnessctl(&o));
    let media = sh("playerctl metadata --format '{{status}}\t{{title}}\t{{artist}}'").and_then(|o| {
        let mut parts = o.trim_end_matches('\n').splitn(3, '\t');
        let status = parts.next()?;
        Some(Media {
            playing: status == "Playing",
            title: parts.next().unwrap_or_default().to_string(),
            artist: parts.next().unwrap_or_default().to_string(),
        })
    });
    ControlState {
        wifi: wifi.filter(|_| has_wifi),
        bluetooth,
        volume,
        brightness,
        media,
    }
}

/// Run a Control Center action; returns the new state.
pub fn run_control(target: &Target, value: Option<f32>, current: &ControlState) -> ControlState {
    match target {
        Target::Wifi => {
            let on = current.wifi.as_ref().map(|w| w.0).unwrap_or(false);
            sh(if on { "nmcli radio wifi off" } else { "nmcli radio wifi on" });
        }
        Target::Bluetooth => {
            let on = current.bluetooth == Some(true);
            // a soft-blocked adapter has to be unblocked before it can power on
            sh(if on { "bluetoothctl power off" } else { "rfkill unblock bluetooth; bluetoothctl power on" });
        }
        Target::Volume(..) => {
            if let Some(v) = value {
                sh(&format!("wpctl set-mute @DEFAULT_AUDIO_SINK@ 0; wpctl set-volume @DEFAULT_AUDIO_SINK@ {:.3}", v));
            }
        }
        Target::Brightness(..) => {
            if let Some(v) = value {
                sh(&format!("brightnessctl -q set {}%", (v * 100.0).round().max(1.0)));
            }
        }
        Target::MediaPrev => {
            sh("playerctl previous");
        }
        Target::MediaPlay => {
            sh("playerctl play-pause");
        }
        Target::MediaNext => {
            sh("playerctl next");
        }
        _ => {}
    }
    // players update their metadata a moment later
    if matches!(target, Target::MediaPrev | Target::MediaNext | Target::MediaPlay) {
        std::thread::sleep(Duration::from_millis(250));
    }
    query_control()
}
