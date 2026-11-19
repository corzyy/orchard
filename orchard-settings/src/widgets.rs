//! macOS-style controls drawn with egui's painter.

use std::{hash::Hash, ops::RangeInclusive};

use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, CornerRadius, FontId, Layout, Margin, Pos2, Rect, Response, Sense,
    Stroke, StrokeKind, Ui, UiBuilder,
};

use crate::{
    icons::Icons,
    style::{bold, pal},
};

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}

pub fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

// ---------------------------------------------------------------- groups

pub struct Group<'a> {
    pub ui: &'a mut Ui,
    n: usize,
}

/// A rounded "inset grouped" box, like the forms in System Settings.
pub fn group<R>(ui: &mut Ui, f: impl FnOnce(&mut Group) -> R) -> R {
    let p = pal();
    let r = egui::Frame::new()
        .fill(p.group)
        .stroke(Stroke::new(1.0_f32, p.group_border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(0, 1))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            let mut g = Group { ui, n: 0 };
            f(&mut g)
        })
        .inner;
    ui.add_space(14.0);
    r
}

/// Bold caption above a group.
pub fn header(ui: &mut Ui, text: &str) {
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(text).font(bold(13.0)).color(pal().text));
    });
    ui.add_space(2.0);
}

/// Small grey explanatory text below a group.
pub fn footnote(ui: &mut Ui, text: &str) {
    ui.add_space(-8.0);
    ui.horizontal_wrapped(|ui| {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(text).size(11.5).color(pal().text2));
    });
    ui.add_space(12.0);
}

pub struct RowOpts<'a> {
    pub icon: Option<(&'a str, Color32)>,
    pub sub: Option<&'a str>,
    pub height: Option<f32>,
    pub clickable: bool,
    pub chevron: bool,
}

impl Default for RowOpts<'_> {
    fn default() -> Self {
        RowOpts {
            icon: None,
            sub: None,
            height: None,
            clickable: false,
            chevron: false,
        }
    }
}

impl Group<'_> {
    fn separator(&mut self) {
        if self.n > 0 {
            let r = self.ui.max_rect();
            let y = self.ui.cursor().top();
            self.ui.painter().hline(
                (r.left() + 12.0)..=(r.right() - 12.0),
                y,
                Stroke::new(1.0_f32, pal().separator),
            );
        }
        self.n += 1;
    }

    /// Label on the left, `control` right-aligned.
    pub fn row(&mut self, label: &str, control: impl FnOnce(&mut Ui)) -> Response {
        self.row_with(label, RowOpts::default(), None, control)
    }

    pub fn row_sub(&mut self, label: &str, sub: &str, control: impl FnOnce(&mut Ui)) -> Response {
        self.row_with(
            label,
            RowOpts {
                sub: Some(sub),
                ..Default::default()
            },
            None,
            control,
        )
    }

    /// A clickable row with a ">" chevron (navigates to a sub page).
    pub fn nav(&mut self, icons: &mut Icons, label: &str, icon: Option<(&str, Color32)>, value: &str) -> Response {
        let value = value.to_string();
        self.row_with(
            label,
            RowOpts {
                icon,
                clickable: true,
                chevron: true,
                ..Default::default()
            },
            Some(icons),
            |ui| {
                if !value.is_empty() {
                    ui.label(egui::RichText::new(value).color(pal().text2));
                }
            },
        )
    }

    pub fn row_with(
        &mut self,
        label: &str,
        o: RowOpts,
        icons: Option<&mut Icons>,
        control: impl FnOnce(&mut Ui),
    ) -> Response {
        self.separator();
        let p = pal();
        let ui = &mut *self.ui;
        let w = ui.available_width();
        let h = o.height.unwrap_or(if o.sub.is_some() { 48.0 } else { 38.0 });
        let sense = if o.clickable { Sense::click() } else { Sense::hover() };
        let (rect, resp) = ui.allocate_exact_size(vec2(w, h), sense);
        if o.clickable && resp.is_pointer_button_down_on() {
            ui.painter().rect_filled(rect.shrink2(vec2(1.0, 0.0)), 6, p.hover);
        }
        // Label (and optional icon / subtitle) are painted directly so they
        // stay vertically centred; only the control gets a child layout.
        let inner = rect.shrink2(vec2(12.0, 0.0));
        let cy = inner.center().y;
        let mut x = inner.left();
        if let Some((name, color)) = o.icon {
            let r = Rect::from_center_size(pos2(x + 11.0, cy), vec2(22.0, 22.0));
            if let Some(icons) = icons {
                tile(ui, icons, r, color, name);
            }
            x += 32.0;
        }
        let body = egui::TextStyle::Body.resolve(ui.style());
        let painter = ui.painter();
        match o.sub {
            Some(sub) => {
                painter.text(pos2(x, cy + 0.5), Align2::LEFT_BOTTOM, label, body, p.text);
                painter.text(pos2(x, cy + 2.5), Align2::LEFT_TOP, sub, egui::FontId::proportional(11.5), p.text2);
            }
            None => {
                painter.text(pos2(x, cy), Align2::LEFT_CENTER, label, body, p.text);
            }
        }
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(inner)
                .layout(Layout::right_to_left(Align::Center)),
        );
        child.spacing_mut().item_spacing.x = 8.0;
        if o.chevron {
            let (r, _) = child.allocate_exact_size(vec2(8.0, 12.0), Sense::hover());
            chevron_right(&child, r.center(), p.text3);
        }
        control(&mut child);
        resp
    }

    /// Free-form content with padding.
    pub fn custom<R>(&mut self, f: impl FnOnce(&mut Ui) -> R) -> R {
        self.separator();
        let ui = &mut *self.ui;
        egui::Frame::new()
            .inner_margin(Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 6.0;
                f(ui)
            })
            .inner
    }
}

// ---------------------------------------------------------------- glyphs

pub fn chevron_right(ui: &Ui, c: Pos2, color: Color32) {
    let s = Stroke::new(1.6_f32, color);
    ui.painter().line_segment([pos2(c.x - 2.0, c.y - 4.5), pos2(c.x + 2.5, c.y)], s);
    ui.painter().line_segment([pos2(c.x + 2.5, c.y), pos2(c.x - 2.0, c.y + 4.5)], s);
}

pub fn chevron_left(ui: &Ui, c: Pos2, color: Color32) {
    let s = Stroke::new(1.8_f32, color);
    ui.painter().line_segment([pos2(c.x + 2.0, c.y - 5.0), pos2(c.x - 3.0, c.y)], s);
    ui.painter().line_segment([pos2(c.x - 3.0, c.y), pos2(c.x + 2.0, c.y + 5.0)], s);
}

fn updown_chevrons(ui: &Ui, c: Pos2, color: Color32) {
    let s = Stroke::new(1.3_f32, color);
    let p = ui.painter();
    p.line_segment([pos2(c.x - 3.0, c.y - 2.0), pos2(c.x, c.y - 5.0)], s);
    p.line_segment([pos2(c.x, c.y - 5.0), pos2(c.x + 3.0, c.y - 2.0)], s);
    p.line_segment([pos2(c.x - 3.0, c.y + 2.0), pos2(c.x, c.y + 5.0)], s);
    p.line_segment([pos2(c.x, c.y + 5.0), pos2(c.x + 3.0, c.y + 2.0)], s);
}

pub fn checkmark(ui: &Ui, c: Pos2, color: Color32) {
    let s = Stroke::new(1.6_f32, color);
    ui.painter().line_segment([pos2(c.x - 4.0, c.y), pos2(c.x - 1.0, c.y + 3.5)], s);
    ui.painter().line_segment([pos2(c.x - 1.0, c.y + 3.5), pos2(c.x + 4.5, c.y - 4.0)], s);
}

/// Rounded-square coloured icon, like the sidebar icons in System Settings.
pub fn tile(ui: &Ui, icons: &mut Icons, rect: Rect, color: Color32, icon: &str) {
    let r = (rect.width() * 0.24).round() as u8;
    let painter = ui.painter();
    painter.rect_filled(rect, r, color);
    // subtle top highlight
    let top = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.center().y));
    painter.rect_filled(
        top,
        CornerRadius {
            nw: r,
            ne: r,
            sw: 0,
            se: 0,
        },
        Color32::from_white_alpha(18),
    );
    let inner = rect.shrink(rect.width() * 0.19);
    if !icons.paint(ui, icon, inner, Color32::WHITE) {
        let letter = icon.chars().next().unwrap_or('?').to_ascii_uppercase();
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            letter,
            bold(rect.height() * 0.55),
            Color32::WHITE,
        );
    }
}

// ---------------------------------------------------------------- controls

pub fn toggle(ui: &mut Ui, on: &mut bool) -> Response {
    let p = pal();
    let (rect, mut resp) = ui.allocate_exact_size(vec2(32.0, 18.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_with_time(resp.id, *on, 0.14);
    let painter = ui.painter();
    painter.rect_filled(rect, 9, mix(p.toggle_off, p.accent, t));
    let r = rect.height() / 2.0 - 1.5;
    let cx = egui::lerp((rect.left() + r + 1.5)..=(rect.right() - r - 1.5), t);
    let c = pos2(cx, rect.center().y);
    painter.circle_filled(c + vec2(0.0, 0.6), r + 0.4, Color32::from_black_alpha(45));
    painter.circle_filled(c, r, p.knob);
    resp
}

pub struct SliderOpts<'a> {
    pub width: f32,
    pub ticks: usize,
    pub snap: bool,
    pub left: &'a str,
    pub right: &'a str,
}

/// macOS slider: thin track, accent fill, white knob, optional ticks and
/// end labels.
pub fn slider(ui: &mut Ui, value: &mut f64, range: RangeInclusive<f64>, o: SliderOpts) -> Response {
    let p = pal();
    let labels = !o.left.is_empty() || !o.right.is_empty();
    let h = if labels { 34.0 } else { 22.0 };
    let (rect, mut resp) = ui.allocate_exact_size(vec2(o.width, h), Sense::click_and_drag());
    let knob_r = 8.5;
    let cy = rect.top() + 11.0;
    let (x0, x1) = (rect.left() + knob_r, rect.right() - knob_r);
    let (lo, hi) = (*range.start(), *range.end());
    if let Some(pos) = resp.interact_pointer_pos() {
        if resp.dragged() || resp.clicked() || resp.is_pointer_button_down_on() {
            let mut t = ((pos.x - x0) / (x1 - x0)).clamp(0.0, 1.0) as f64;
            if o.snap && o.ticks > 1 {
                let n = (o.ticks - 1) as f64;
                t = (t * n).round() / n;
            }
            let v = lo + (hi - lo) * t;
            if (v - *value).abs() > 1e-9 {
                *value = v;
                resp.mark_changed();
            }
        }
    }
    let t = (((*value - lo) / (hi - lo)) as f32).clamp(0.0, 1.0);
    let kx = x0 + (x1 - x0) * t;
    let painter = ui.painter();
    let track = Rect::from_x_y_ranges(x0..=x1, (cy - 2.0)..=(cy + 2.0));
    painter.rect_filled(track, 2, p.toggle_off);
    painter.rect_filled(Rect::from_x_y_ranges(x0..=kx, track.y_range()), 2, p.accent);
    if o.ticks > 1 {
        for i in 0..o.ticks {
            let x = x0 + (x1 - x0) * i as f32 / (o.ticks - 1) as f32;
            painter.line_segment([pos2(x, cy + 6.0), pos2(x, cy + 9.0)], Stroke::new(1.0_f32, p.text3));
        }
    }
    let kc = pos2(kx, cy);
    painter.circle_filled(kc + vec2(0.0, 0.8), knob_r + 0.5, Color32::from_black_alpha(40));
    painter.circle_filled(kc, knob_r, p.knob);
    painter.circle_stroke(kc, knob_r, Stroke::new(0.6_f32, p.control_border));
    if labels {
        let f = FontId::proportional(10.5);
        painter.text(pos2(rect.left(), rect.bottom()), Align2::LEFT_BOTTOM, o.left, f.clone(), p.text2);
        painter.text(pos2(rect.right(), rect.bottom()), Align2::RIGHT_BOTTOM, o.right, f, p.text2);
    }
    resp
}

fn menu_item(ui: &mut Ui, label: &str, selected: bool, min_w: f32) -> Response {
    let p = pal();
    let font = FontId::proportional(13.0);
    let galley = ui.painter().layout_no_wrap(label.to_string(), font.clone(), p.text);
    let w = (galley.size().x + 40.0).max(min_w);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 22.0), Sense::click());
    let hovered = resp.hovered();
    let fg = if hovered { Color32::WHITE } else { p.text };
    if hovered {
        ui.painter().rect_filled(rect, 4, p.accent);
    }
    if selected {
        checkmark(ui, pos2(rect.left() + 11.0, rect.center().y), fg);
    }
    ui.painter()
        .text(pos2(rect.left() + 22.0, rect.center().y), Align2::LEFT_CENTER, label, font, fg);
    resp
}

/// NSPopUpButton-like selector. Returns true when the value changed.
pub fn popup<T: PartialEq + Clone>(ui: &mut Ui, id: impl Hash, value: &mut T, options: &[(T, String)]) -> bool {
    ui.push_id(id, |ui| {
        let p = pal();
        let current = options
            .iter()
            .find(|(v, _)| v == value)
            .map(|(_, l)| l.clone())
            .unwrap_or_else(|| "—".into());
        let font = FontId::proportional(13.0);
        let galley = ui.painter().layout_no_wrap(current, font, p.text);
        let w = galley.size().x + 30.0;
        let (rect, resp) = ui.allocate_exact_size(vec2(w, 22.0), Sense::click());
        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        if resp.hovered() || resp.is_pointer_button_down_on() {
            ui.painter().rect_filled(rect, 5, p.hover);
        }
        ui.painter()
            .galley(pos2(rect.left() + 7.0, rect.center().y - galley.size().y / 2.0), galley, p.text);
        updown_chevrons(ui, pos2(rect.right() - 10.0, rect.center().y), p.text2);
        let mut changed = false;
        let min_w = rect.width().max(140.0);
        egui::Popup::menu(&resp).show(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                for (v, l) in options {
                    if menu_item(ui, l, v == value, min_w).clicked() {
                        *value = v.clone();
                        changed = true;
                        ui.close();
                    }
                }
            });
        });
        changed
    })
    .inner
}

/// Convenience for string-valued popups.
pub fn popup_str(ui: &mut Ui, id: impl Hash, value: &mut String, options: &[String]) -> bool {
    let opts: Vec<(String, String)> = options.iter().map(|o| (o.clone(), o.clone())).collect();
    popup(ui, id, value, &opts)
}

/// Segmented control.
#[allow(dead_code)] // kept as a building block for new pages
pub fn segmented<T: PartialEq + Clone>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let p = pal();
    let font = FontId::proportional(12.5);
    let widths: Vec<f32> = options
        .iter()
        .map(|(_, l)| ui.painter().layout_no_wrap(l.to_string(), font.clone(), p.text).size().x + 22.0)
        .collect();
    let seg_w = widths.iter().cloned().fold(0.0, f32::max);
    let (rect, _) = ui.allocate_exact_size(vec2(seg_w * options.len() as f32 + 4.0, 24.0), Sense::hover());
    ui.painter().rect_filled(rect, 7, p.toggle_off.gamma_multiply(0.8));
    let mut changed = false;
    for (i, (v, l)) in options.iter().enumerate() {
        let r = Rect::from_min_size(pos2(rect.left() + 2.0 + seg_w * i as f32, rect.top() + 2.0), vec2(seg_w, 20.0));
        let resp = ui.interact(r, ui.id().with(("seg", i, l)), Sense::click());
        let sel = v == value;
        if sel {
            ui.painter().rect_filled(r.translate(vec2(0.0, 0.5)), 5, Color32::from_black_alpha(25));
            ui.painter().rect_filled(r, 5, if p.dark { Color32::from_rgb(0x63, 0x62, 0x66) } else { Color32::WHITE });
        } else if i > 0 && options.get(i - 1).map(|(pv, _)| pv != value).unwrap_or(true) {
            ui.painter().vline(r.left(), (r.top() + 4.0)..=(r.bottom() - 4.0), Stroke::new(1.0_f32, p.separator));
        }
        ui.painter().text(r.center(), Align2::CENTER_CENTER, *l, font.clone(), p.text);
        if resp.clicked() && !sel {
            *value = v.clone();
            changed = true;
        }
    }
    changed
}

fn push_button(ui: &mut Ui, text: &str, primary: bool, enabled: bool) -> Response {
    let p = pal();
    let font = FontId::proportional(13.0);
    let fg = if primary { Color32::WHITE } else { p.text };
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, fg);
    let size = vec2(galley.size().x + 24.0, 22.0);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    let down = resp.is_pointer_button_down_on();
    let painter = ui.painter();
    painter.rect_filled(rect.translate(vec2(0.0, 0.5)), 6, Color32::from_black_alpha(if p.dark { 60 } else { 18 }));
    let mut fill = if primary { p.accent } else { p.control };
    if down {
        fill = mix(fill, Color32::BLACK, 0.12);
    }
    painter.rect_filled(rect, 6, fill);
    if !primary {
        painter.rect_stroke(rect, 6, Stroke::new(0.7_f32, p.control_border), StrokeKind::Inside);
    }
    let alpha = if enabled { 1.0 } else { 0.4 };
    painter.galley(rect.center() - galley.size() / 2.0, galley, fg.gamma_multiply(alpha));
    resp
}

pub fn button(ui: &mut Ui, text: &str) -> Response {
    push_button(ui, text, false, true)
}

#[allow(dead_code)] // kept as a building block for new pages
pub fn button_enabled(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    push_button(ui, text, false, enabled)
}

pub fn primary_button(ui: &mut Ui, text: &str) -> Response {
    push_button(ui, text, true, true)
}

pub fn text_field(ui: &mut Ui, value: &mut String, width: f32, hint: &str) -> Response {
    ui.add(
        egui::TextEdit::singleline(value)
            .desired_width(width)
            .hint_text(hint)
            .margin(Margin::symmetric(6, 3)),
    )
}

pub fn secondary(ui: &mut Ui, text: impl Into<String>) -> Response {
    ui.label(egui::RichText::new(text.into()).color(pal().text2))
}

/// Wi‑Fi style signal bars (0..=100).
pub fn signal_bars(ui: &mut Ui, strength: u8) {
    let p = pal();
    let (rect, _) = ui.allocate_exact_size(vec2(16.0, 12.0), Sense::hover());
    let lit = match strength {
        0..=25 => 1,
        26..=50 => 2,
        51..=75 => 3,
        _ => 4,
    };
    for i in 0..4 {
        let h = 3.0 + i as f32 * 3.0;
        let r = Rect::from_min_size(pos2(rect.left() + i as f32 * 4.0, rect.bottom() - h), vec2(2.6, h));
        ui.painter().rect_filled(r, 1, if i < lit { p.text } else { p.text3 });
    }
}

/// Circular avatar with initials.
pub fn avatar(ui: &mut Ui, size: f32, name: &str, tex: Option<&egui::TextureHandle>) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let c = rect.center();
    if let Some(tex) = tex {
        // egui can't clip images to circles; draw the image and mask the corners with a ring
        ui.painter()
            .image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
        ui.painter()
            .circle_stroke(c, size / 2.0 + size * 0.15, Stroke::new(size * 0.3, pal().sidebar));
    } else {
        ui.painter().circle_filled(c, size / 2.0, Color32::from_rgb(0x9A, 0x9A, 0xA2));
        let initials: String = name
            .split_whitespace()
            .filter_map(|w| w.chars().next())
            .take(2)
            .collect::<String>()
            .to_uppercase();
        ui.painter()
            .text(c, Align2::CENTER_CENTER, initials, bold(size * 0.4), Color32::WHITE);
    }
    resp
}
