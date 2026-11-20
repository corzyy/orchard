//! macOS-style confirmation dialogs for Shut Down, Restart, Log Out, and Sleep.

use std::time::{Duration, Instant};

use tiny_skia::{FillRule, LineCap, PathBuilder, Pixmap, Stroke, Transform};

use crate::{
    draw::{fill_circle, fill_rounded, paint, stroke_rounded},
    text::{Fonts, Rgba, Weight},
    theme::Theme,
};

const WIDTH: f32 = 340.0;
const HEIGHT: f32 = 216.0;
const RADIUS: f32 = 16.0;
const COUNTDOWN_SECS: u64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogKind {
    ShutDown,
    Restart,
    LogOut,
    Sleep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogButton {
    Cancel,
    Confirm,
}

pub struct ConfirmDialog {
    pub kind: DialogKind,
    pub opened: Instant,
    pub hovered: Option<DialogButton>,
    pub rect: (f32, f32, f32, f32),
    cancel_rect: (f32, f32, f32, f32),
    confirm_rect: (f32, f32, f32, f32),
}

impl ConfirmDialog {
    pub fn new(kind: DialogKind) -> Self {
        Self {
            kind,
            opened: Instant::now(),
            hovered: None,
            rect: (0.0, 0.0, WIDTH, HEIGHT),
            cancel_rect: (0.0, 0.0, 0.0, 0.0),
            confirm_rect: (0.0, 0.0, 0.0, 0.0),
        }
    }

    pub fn remaining_secs(&self) -> u64 {
        let elapsed = self.opened.elapsed().as_secs();
        COUNTDOWN_SECS.saturating_sub(elapsed)
    }

    pub fn expired(&self) -> bool {
        self.opened.elapsed() >= Duration::from_secs(COUNTDOWN_SECS)
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<DialogButton> {
        let in_r = |r: (f32, f32, f32, f32)| x >= r.0 && x <= r.0 + r.2 && y >= r.1 && y <= r.1 + r.3;
        if in_r(self.confirm_rect) {
            Some(DialogButton::Confirm)
        } else if in_r(self.cancel_rect) {
            Some(DialogButton::Cancel)
        } else {
            None
        }
    }

    pub fn in_dialog(&self, x: f32, y: f32) -> bool {
        let r = self.rect;
        x >= r.0 && x <= r.0 + r.2 && y >= r.1 && y <= r.1 + r.3
    }

    pub fn paint(&mut self, pm: &mut Pixmap, scale: f32, fonts: &Fonts, theme: &Theme, german: bool) {
        let s = scale;
        let sw = pm.width() as f32 / s;
        let sh = pm.height() as f32 / s;

        // Subtle screen dim behind the sheet
        fill_rounded(pm, 0.0, 0.0, pm.width() as f32, pm.height() as f32, 0.0, [0, 0, 0, 56]);

        let x0 = ((sw - WIDTH) * 0.5).round();
        let y0 = ((sh - HEIGHT) * 0.44).round();
        self.rect = (x0, y0, WIDTH, HEIGHT);

        let dark = theme.menu_text[0] > 128;
        let bg: Rgba = if dark { [40, 40, 44, 224] } else { [248, 248, 252, 232] };
        let border: Rgba = if dark { [255, 255, 255, 44] } else { [0, 0, 0, 32] };
        let text = theme.menu_text;
        let sub: Rgba = if dark { [220, 220, 226, 175] } else { [40, 40, 46, 165] };
        let accent = theme.menu_selection;
        let cancel_bg: Rgba = if dark { [255, 255, 255, 28] } else { [0, 0, 0, 18] };
        let cancel_hov: Rgba = if dark { [255, 255, 255, 44] } else { [0, 0, 0, 30] };

        // Soft drop shadow
        fill_rounded(pm, (x0 - 6.0) * s, (y0 + 10.0) * s, (WIDTH + 12.0) * s, (HEIGHT + 12.0) * s, (RADIUS + 6.0) * s, [0, 0, 0, 48]);
        // Frosted sheet background + hairline border
        fill_rounded(pm, x0 * s, y0 * s, WIDTH * s, HEIGHT * s, RADIUS * s, bg);
        stroke_rounded(pm, x0 * s, y0 * s, WIDTH * s, HEIGHT * s, RADIUS * s, 1.0 * s, border);

        // Centered emblem circle at top
        let cx = x0 + WIDTH * 0.5;
        let cy = y0 + 38.0;
        let badge_col: Rgba = match self.kind {
            DialogKind::ShutDown => [255, 69, 58, 230],
            DialogKind::Restart => [255, 159, 10, 230],
            DialogKind::LogOut => accent,
            DialogKind::Sleep => [94, 92, 230, 230],
        };
        fill_circle(pm, cx * s, cy * s, 20.0 * s, badge_col);
        draw_icon(pm, self.kind, cx * s, cy * s, s, [255, 255, 255, 255]);

        let secs = self.remaining_secs();
        let (title, body, confirm_label) = match (self.kind, german) {
            (DialogKind::ShutDown, false) => (
                "Are you sure you want to shut down your computer now?",
                format!("If you do nothing, your computer will shut down automatically in {secs} seconds."),
                "Shut Down",
            ),
            (DialogKind::ShutDown, true) => (
                "Möchtest du deinen Computer jetzt wirklich ausschalten?",
                format!("Wenn du nichts tust, wird der Computer in {secs} Sekunden automatisch ausgeschaltet."),
                "Ausschalten",
            ),
            (DialogKind::Restart, false) => (
                "Are you sure you want to restart your computer now?",
                format!("If you do nothing, your computer will restart automatically in {secs} seconds."),
                "Restart",
            ),
            (DialogKind::Restart, true) => (
                "Möchtest du deinen Computer jetzt wirklich neu starten?",
                format!("Wenn du nichts tust, wird der Computer in {secs} Sekunden automatisch neu gestartet."),
                "Neustart",
            ),
            (DialogKind::LogOut, false) => (
                "Are you sure you want to quit all applications and log out now?",
                format!("If you do nothing, you will be logged out automatically in {secs} seconds."),
                "Log Out",
            ),
            (DialogKind::LogOut, true) => (
                "Möchtest du wirklich alle Programme beenden und dich abmelden?",
                format!("Wenn du nichts tust, wirst du in {secs} Sekunden automatisch abgemeldet."),
                "Abmelden",
            ),
            (DialogKind::Sleep, false) => (
                "Put your computer to sleep now?",
                format!("Your computer will go to sleep automatically in {secs} seconds."),
                "Sleep",
            ),
            (DialogKind::Sleep, true) => (
                "Computer jetzt in den Ruhezustand versetzen?",
                format!("Der Computer wechselt in {secs} Sekunden automatisch in den Ruhezustand."),
                "Ruhezustand",
            ),
        };
        let cancel_label = if german { "Abbrechen" } else { "Cancel" };

        // Wrapped centered title (up to 2 lines)
        let max_w = (WIDTH - 44.0) * s;
        let title_px = 13.5 * s;
        let title_lines = wrap_text(fonts, title, title_px, Weight::Bold, max_w, 2);
        let mut ty = y0 + 74.0;
        for line in &title_lines {
            let lw = fonts.measure(line, title_px, Weight::Bold);
            fonts.draw(pm, line, cx * s - lw * 0.5, ty * s, title_px, Weight::Bold, text);
            ty += 18.0;
        }

        // Wrapped centered subtitle (up to 2 lines)
        let body_px = 11.5 * s;
        let body_lines = wrap_text(fonts, &body, body_px, Weight::Regular, max_w, 2);
        let mut by = ty + 6.0;
        for line in &body_lines {
            let lw = fonts.measure(line, body_px, Weight::Regular);
            fonts.draw(pm, line, cx * s - lw * 0.5, by * s, body_px, Weight::Regular, sub);
            by += 15.5;
        }

        // Buttons at the bottom
        let btn_h = 30.0;
        let btn_gap = 10.0;
        let btn_w = (WIDTH - 40.0 - btn_gap) * 0.5;
        let btn_y = y0 + HEIGHT - 18.0 - btn_h;
        let cancel_x = x0 + 20.0;
        let confirm_x = cancel_x + btn_w + btn_gap;
        self.cancel_rect = (cancel_x, btn_y, btn_w, btn_h);
        self.confirm_rect = (confirm_x, btn_y, btn_w, btn_h);

        // Cancel button
        let c_fill = if self.hovered == Some(DialogButton::Cancel) { cancel_hov } else { cancel_bg };
        fill_rounded(pm, cancel_x * s, btn_y * s, btn_w * s, btn_h * s, 8.0 * s, c_fill);
        let btn_px = 13.0 * s;
        let base = fonts.baseline_centered(btn_y * s, btn_h * s, btn_px, Weight::Regular);
        let cw = fonts.measure(cancel_label, btn_px, Weight::Regular);
        fonts.draw(pm, cancel_label, (cancel_x + btn_w * 0.5) * s - cw * 0.5, base, btn_px, Weight::Regular, text);

        // Confirm button
        let confirm_fill = if self.hovered == Some(DialogButton::Confirm) {
            [
                accent[0].saturating_add(16),
                accent[1].saturating_add(16),
                accent[2].saturating_add(16),
                255,
            ]
        } else {
            accent
        };
        fill_rounded(pm, confirm_x * s, btn_y * s, btn_w * s, btn_h * s, 8.0 * s, confirm_fill);
        let ok_w = fonts.measure(confirm_label, btn_px, Weight::Bold);
        fonts.draw(
            pm,
            confirm_label,
            (confirm_x + btn_w * 0.5) * s - ok_w * 0.5,
            base,
            btn_px,
            Weight::Bold,
            [255, 255, 255, 255],
        );
    }
}

fn wrap_text(fonts: &Fonts, text: &str, px: f32, weight: Weight, max_w: f32, max_lines: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let cand = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if fonts.measure(&cand, px, weight) <= max_w || cur.is_empty() {
            cur = cand;
        } else {
            lines.push(cur);
            cur = word.to_string();
            if lines.len() + 1 == max_lines {
                break;
            }
        }
    }
    if !cur.is_empty() && lines.len() < max_lines {
        lines.push(cur);
    }
    lines
}

fn draw_icon(pm: &mut Pixmap, kind: DialogKind, cx: f32, cy: f32, s: f32, col: Rgba) {
    let mut st = Stroke::default();
    st.width = 2.0 * s;
    st.line_cap = LineCap::Round;
    let p = paint(col);
    match kind {
        DialogKind::ShutDown => {
            // Power symbol: vertical stem + open circle arc
            let r = 8.0 * s;
            let mut pb = PathBuilder::new();
            pb.move_to(cx, cy - r - 1.0 * s);
            pb.line_to(cx, cy - 1.0 * s);
            for i in 0..=20 {
                let a = -0.75 + (i as f32 / 20.0) * (std::f32::consts::TAU - 1.6);
                let px = cx + r * a.sin();
                let py = cy - r * a.cos();
                if i == 0 { pb.move_to(px, py); } else { pb.line_to(px, py); }
            }
            if let Some(path) = pb.finish() {
                pm.stroke_path(&path, &p, &st, Transform::identity(), None);
            }
        }
        DialogKind::Restart => {
            // Circular arrow
            let r = 7.5 * s;
            let mut pb = PathBuilder::new();
            for i in 0..=20 {
                let a = 0.5 + (i as f32 / 20.0) * (std::f32::consts::TAU - 1.3);
                let px = cx + r * a.cos();
                let py = cy + r * a.sin();
                if i == 0 { pb.move_to(px, py); } else { pb.line_to(px, py); }
            }
            if let Some(path) = pb.finish() {
                pm.stroke_path(&path, &p, &st, Transform::identity(), None);
            }
            // Arrowhead at top-right
            let mut ah = PathBuilder::new();
            ah.move_to(cx + 3.0 * s, cy - 7.5 * s);
            ah.line_to(cx + 8.5 * s, cy - 7.5 * s);
            ah.line_to(cx + 8.5 * s, cy - 2.0 * s);
            if let Some(path) = ah.finish() {
                pm.stroke_path(&path, &p, &st, Transform::identity(), None);
            }
        }
        DialogKind::LogOut => {
            // Door + arrow pointing right
            let mut pb = PathBuilder::new();
            pb.move_to(cx - 1.0 * s, cy - 7.5 * s);
            pb.line_to(cx - 7.5 * s, cy - 7.5 * s);
            pb.line_to(cx - 7.5 * s, cy + 7.5 * s);
            pb.line_to(cx - 1.0 * s, cy + 7.5 * s);
            pb.move_to(cx - 2.0 * s, cy);
            pb.line_to(cx + 8.0 * s, cy);
            pb.move_to(cx + 4.5 * s, cy - 4.0 * s);
            pb.line_to(cx + 8.5 * s, cy);
            pb.line_to(cx + 4.5 * s, cy + 4.0 * s);
            if let Some(path) = pb.finish() {
                pm.stroke_path(&path, &p, &st, Transform::identity(), None);
            }
        }
        DialogKind::Sleep => {
            // Crescent moon
            let mut pb = PathBuilder::new();
            let r = 8.0 * s;
            pb.move_to(cx + 2.0 * s, cy - r);
            pb.cubic_to(cx - 6.0 * s, cy - r, cx - 9.0 * s, cy - 2.0 * s, cx - 5.0 * s, cy + 5.0 * s);
            pb.cubic_to(cx - 1.0 * s, cy + 10.0 * s, cx + 6.5 * s, cy + 8.0 * s, cx + 8.0 * s, cy + 2.5 * s);
            pb.cubic_to(cx + 2.5 * s, cy + 4.5 * s, cx - 2.0 * s, cy + 1.0 * s, cx - 1.5 * s, cy - 3.5 * s);
            pb.cubic_to(cx - 1.2 * s, cy - 6.0 * s, cx + 0.5 * s, cy - 7.5 * s, cx + 2.0 * s, cy - r);
            pb.close();
            if let Some(path) = pb.finish() {
                pm.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
            }
        }
    }
}
