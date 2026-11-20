//! Lightweight immediate-mode UI engine rendered with tiny-skia + fontdue.
//!
//! All coordinates are in logical pixels; physical scaling (`scale`) is applied
//! when rasterizing into the Wayland SHM `Pixmap`.

use std::collections::HashMap;

use tiny_skia::Pixmap;

use crate::{
    icons::Icons,
    theme::{
        blit_rounded_rgba, fill_circle, fill_rrect, fill_rrect_vgrad, line, mix, stroke_circle, stroke_rrect,
        with_alpha, Fonts, Palette, Rgba, Weight,
    },
};

#[derive(Clone, Copy, Debug, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(self) -> f32 {
        self.y + self.h
    }
    pub fn cx(self) -> f32 {
        self.x + self.w * 0.5
    }
    pub fn cy(self) -> f32 {
        self.y + self.h * 0.5
    }
    pub fn contains(self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
    pub fn inset(self, d: f32) -> Self {
        Self::new(self.x + d, self.y + d, (self.w - 2.0 * d).max(0.0), (self.h - 2.0 * d).max(0.0))
    }
    pub fn intersects_y(self, y0: f32, y1: f32) -> bool {
        self.y + self.h >= y0 && self.y <= y1
    }
}

#[derive(Clone, Debug)]
pub enum TextEvent {
    Char(char),
    Backspace,
    Enter,
    Escape,
}

#[derive(Clone, Debug)]
pub struct PopupMenu {
    pub id: u64,
    pub anchor: Rect,
    pub options: Vec<(String, String)>,
    pub current: String,
    pub scroll: f32,
}

/// Persistent interaction state across frames.
#[derive(Default)]
pub struct UiState {
    pub pointer: (f32, f32),
    pub pointer_in: bool,
    pub pointer_down: bool,
    pub pressed_at: Option<(f32, f32)>,
    pub clicked: bool,
    pub just_pressed: bool,
    pub active_slider: Option<u64>,
    pub focused_input: Option<u64>,
    pub text_events: Vec<TextEvent>,
    pub scroll_y: f32,
    pub max_scroll: f32,
    pub open_popup: Option<PopupMenu>,
    pub popup_result: Option<(u64, String)>,
    pub group_heights: HashMap<u64, f32>,
    pub hovered_pointer: bool,
}

impl UiState {
    pub fn reset_page(&mut self) {
        self.scroll_y = 0.0;
        self.max_scroll = 0.0;
        self.open_popup = None;
        self.popup_result = None;
        self.active_slider = None;
        self.focused_input = None;
    }

    pub fn take_popup_result(&mut self, id: u64) -> Option<String> {
        if self.popup_result.as_ref().map(|(pid, _)| *pid) == Some(id) {
            self.popup_result.take().map(|(_, v)| v)
        } else {
            None
        }
    }
}

pub fn hash_id(salt: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in salt.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub fn hash_pair(a: &str, b: &str) -> u64 {
    let mut h = hash_id(a);
    h ^= 0xff;
    h = h.wrapping_mul(0x100000001b3);
    for byte in b.as_bytes() {
        h ^= *byte as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Active frame painter & layout cursor.
pub struct Ui<'a> {
    pub pm: &'a mut Pixmap,
    pub fonts: &'a Fonts,
    pub icons: &'a mut Icons,
    pub pal: Palette,
    pub st: &'a mut UiState,
    pub scale: f32,
    pub win_w: f32,
    pub win_h: f32,
    /// Current vertical layout cursor in logical coords.
    pub x: f32,
    pub y: f32,
    pub w: f32,
    /// Clipping rectangle in logical coords (None = full window).
    pub clip: Option<Rect>,
    /// True during a layout-only pass before the real paint pass.
    pub dry_run: bool,
    /// Set if any group height changed so the caller can re-run before committing.
    pub needs_relayout: bool,
    /// True when a popup menu is open and should swallow background pointer clicks.
    pub block_input: bool,
    /// Row counter inside the active group.
    group_row: usize,
}

impl<'a> Ui<'a> {
    pub fn new(
        pm: &'a mut Pixmap,
        fonts: &'a Fonts,
        icons: &'a mut Icons,
        pal: Palette,
        st: &'a mut UiState,
        scale: f32,
        win_w: f32,
        win_h: f32,
        dry_run: bool,
    ) -> Self {
        let block_input = st.open_popup.is_some();
        Self {
            pm,
            fonts,
            icons,
            pal,
            st,
            scale,
            win_w,
            win_h,
            x: 0.0,
            y: 0.0,
            w: win_w,
            clip: None,
            dry_run,
            needs_relayout: false,
            block_input,
            group_row: 0,
        }
    }

    fn phys_clip(&self) -> Option<[i32; 4]> {
        self.clip.map(|c| {
            [
                (c.x * self.scale).floor() as i32,
                (c.y * self.scale).floor() as i32,
                (c.right() * self.scale).ceil() as i32,
                (c.bottom() * self.scale).ceil() as i32,
            ]
        })
    }

    pub fn visible(&self, r: Rect) -> bool {
        if self.dry_run {
            return false;
        }
        match self.clip {
            Some(c) => r.intersects_y(c.y, c.bottom()),
            None => r.intersects_y(0.0, self.win_h),
        }
    }

    pub fn hovered(&self, r: Rect) -> bool {
        if self.dry_run || self.block_input || !self.st.pointer_in {
            return false;
        }
        if let Some(c) = self.clip {
            if !c.contains(self.st.pointer.0, self.st.pointer.1) {
                return false;
            }
        }
        r.contains(self.st.pointer.0, self.st.pointer.1)
    }

    pub fn clicked(&mut self, r: Rect) -> bool {
        if !self.hovered(r) {
            return false;
        }
        self.st.hovered_pointer = true;
        if self.st.clicked {
            if let Some((px, py)) = self.st.pressed_at {
                return r.contains(px, py);
            }
            return true;
        }
        false
    }

    pub fn fill_rect(&mut self, r: Rect, radius: f32, color: Rgba) {
        if !self.visible(r) {
            return;
        }
        let s = self.scale;
        fill_rrect(self.pm, r.x * s, r.y * s, r.w * s, r.h * s, radius * s, color);
    }

    pub fn stroke_rect(&mut self, r: Rect, radius: f32, width: f32, color: Rgba) {
        if !self.visible(r) {
            return;
        }
        let s = self.scale;
        stroke_rrect(
            self.pm,
            r.x * s,
            r.y * s,
            r.w * s,
            r.h * s,
            radius * s,
            (width * s).max(1.0),
            color,
        );
    }

    pub fn circle_filled(&mut self, cx: f32, cy: f32, r: f32, color: Rgba) {
        if self.dry_run {
            return;
        }
        let s = self.scale;
        fill_circle(self.pm, cx * s, cy * s, r * s, color);
    }

    pub fn circle_stroke(&mut self, cx: f32, cy: f32, r: f32, width: f32, color: Rgba) {
        if self.dry_run {
            return;
        }
        let s = self.scale;
        stroke_circle(self.pm, cx * s, cy * s, r * s, (width * s).max(1.0), color);
    }

    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, color: Rgba) {
        if self.dry_run {
            return;
        }
        let s = self.scale;
        line(self.pm, x0 * s, y0 * s, x1 * s, y1 * s, (width * s).max(1.0), color);
    }

    pub fn text(&mut self, text: &str, x: f32, cy: f32, size: f32, weight: Weight, color: Rgba) {
        if self.dry_run || text.is_empty() {
            return;
        }
        let s = self.scale;
        let px = size * s;
        let baseline = (cy + size * 0.34) * s;
        let clip = self.phys_clip();
        self.fonts.draw(self.pm, text, x * s, baseline, px, weight, color, clip);
    }

    pub fn text_right(&mut self, text: &str, right_x: f32, cy: f32, size: f32, weight: Weight, color: Rgba) {
        if self.dry_run || text.is_empty() {
            return;
        }
        let w = self.fonts.measure(text, size, weight);
        self.text(text, right_x - w, cy, size, weight, color);
    }

    pub fn text_center(&mut self, text: &str, cx: f32, cy: f32, size: f32, weight: Weight, color: Rgba) {
        if self.dry_run || text.is_empty() {
            return;
        }
        let w = self.fonts.measure(text, size, weight);
        self.text(text, cx - w * 0.5, cy, size, weight, color);
    }

    pub fn text_elided(
        &mut self,
        text: &str,
        x: f32,
        cy: f32,
        max_w: f32,
        size: f32,
        weight: Weight,
        color: Rgba,
    ) {
        if self.dry_run || text.is_empty() || max_w <= 4.0 {
            return;
        }
        let s = self.fonts.elide(text, size, weight, max_w);
        self.text(&s, x, cy, size, weight, color);
    }

    pub fn icon(&mut self, name: &str, r: Rect, tint: Rgba) -> bool {
        if !self.visible(r) {
            return true;
        }
        let s = self.scale;
        let px = (r.w.min(r.h) * s).round().max(1.0) as u32;
        let dx = ((r.cx() * s) - px as f32 * 0.5).round() as i32;
        let dy = ((r.cy() * s) - px as f32 * 0.5).round() as i32;
        let clip = self.phys_clip();
        self.icons.draw(self.pm, name, dx, dy, px, tint, clip)
    }

    /// macOS-style app icon tile: rounded square with a soft top-to-bottom
    /// gradient and a white symbolic glyph.
    pub fn icon_tile(&mut self, r: Rect, bg: Rgba, icon: &str) {
        if !self.visible(r) {
            return;
        }
        let radius = (r.w * 0.225).round().max(4.0);
        let s = self.scale;
        let top = mix(bg, [255, 255, 255, 255], 0.16);
        let bottom = mix(bg, [0, 0, 0, 255], 0.08);
        fill_rrect_vgrad(self.pm, r.x * s, r.y * s, r.w * s, r.h * s, radius * s, top, bottom);
        self.stroke_rect(r, radius, 0.5, [0, 0, 0, 30]);
        let inner = r.inset(r.w * 0.2);
        if !self.icon(icon, inner, [255, 255, 255, 255]) {
            let ch = icon.chars().next().unwrap_or('?').to_ascii_uppercase().to_string();
            self.text_center(&ch, r.cx(), r.cy(), r.h * 0.52, Weight::Bold, [255, 255, 255, 255]);
        }
    }

    /// Wrapped paragraph centred on `cx`; returns the height used.
    pub fn paragraph_center(&mut self, text: &str, cx: f32, y: f32, max_w: f32, size: f32, color: Rgba) -> f32 {
        let line_h = (size * 1.36).round();
        let lines = self.fonts.wrap(text, size, Weight::Regular, max_w);
        for (i, l) in lines.iter().enumerate() {
            self.text_center(l, cx, y + line_h * (i as f32 + 0.5), size, Weight::Regular, color);
        }
        line_h * lines.len() as f32
    }

    /// The header card at the top of each pane (big icon, title, blurb),
    /// as in macOS System Settings.
    pub fn hero(&mut self, id_key: &str, icon: &str, color: Rgba, title: &str, desc: &str) {
        let key = format!("hero-{id_key}");
        self.group(&key, |ui| {
            let y0 = ui.y;
            let cx = ui.x + ui.w * 0.5;
            let tile = Rect::new(cx - 27.0, y0 + 18.0, 54.0, 54.0);
            ui.icon_tile(tile, color, icon);
            ui.text_center(title, cx, tile.bottom() + 18.0, 15.0, Weight::Bold, ui.pal.text);
            let max_w = (ui.w - 64.0).min(420.0);
            let h = ui.paragraph_center(desc, cx, tile.bottom() + 30.0, max_w, 11.5, ui.pal.text2);
            ui.y = tile.bottom() + 30.0 + h + 16.0;
        });
        self.y += 6.0;
    }

    pub fn rounded_image(&mut self, r: Rect, rgba: &[u8], iw: u32, ih: u32, radius: f32) {
        if !self.visible(r) {
            return;
        }
        let s = self.scale;
        let clip = self.phys_clip();
        blit_rounded_rgba(
            self.pm,
            rgba,
            iw,
            ih,
            (r.x * s).round() as i32,
            (r.y * s).round() as i32,
            (r.w * s).round() as i32,
            (r.h * s).round() as i32,
            radius * s,
            clip,
        );
    }

    // ---------------------------------------------------------------- Layout containers

    pub fn space(&mut self, dy: f32) {
        self.y += dy;
    }

    pub fn section(&mut self, title: &str) {
        self.y += 4.0;
        self.text(title, self.x + 4.0, self.y + 10.0, 12.5, Weight::Bold, self.pal.text);
        self.y += 24.0;
    }

    pub fn error_line(&mut self, err: Option<&str>) {
        let Some(e) = err else { return };
        self.y -= 6.0;
        self.text_elided(e, self.x + 4.0, self.y + 8.0, self.w - 8.0, 11.5, Weight::Regular, self.pal.danger);
        self.y += 22.0;
    }

    /// Rounded inset card grouping rows with 1px separators.
    pub fn group(&mut self, id_key: &str, f: impl FnOnce(&mut Ui)) {
        let id = hash_id(id_key);
        let y0 = self.y;
        let known_h = self.st.group_heights.get(&id).copied();
        if let Some(h) = known_h {
            let box_r = Rect::new(self.x, y0, self.w, h);
            self.fill_rect(box_r, 10.0, self.pal.group);
            self.stroke_rect(box_r, 10.0, 1.0, self.pal.group_border);
        }
        let prev_row = self.group_row;
        self.group_row = 0;
        f(self);
        self.group_row = prev_row;
        let actual_h = (self.y - y0).max(0.0);
        if known_h.map(|h| (h - actual_h).abs() > 0.5).unwrap_or(true) {
            self.st.group_heights.insert(id, actual_h);
            self.needs_relayout = true;
        }
        self.y += 14.0;
    }

    fn row_sep(&mut self) {
        if self.group_row > 0 {
            let y = self.y;
            self.line(self.x + 14.0, y, self.x + self.w - 14.0, y, 1.0, self.pal.separator);
        }
        self.group_row += 1;
    }

    /// Allocate a standard row inside a group and paint label + optional subtitle.
    /// Returns the row rect and the right-hand area for controls.
    pub fn row_base(&mut self, label: &str, sub: Option<&str>, height: Option<f32>) -> Rect {
        self.row_sep();
        let h = height.unwrap_or(if sub.is_some() { 50.0 } else { 40.0 });
        let r = Rect::new(self.x, self.y, self.w, h);
        self.y += h;
        let text_max = (self.w * 0.58).max(140.0);
        match sub {
            Some(s) => {
                self.text_elided(label, r.x + 14.0, r.y + 17.0, text_max, 13.0, Weight::Regular, self.pal.text);
                self.text_elided(s, r.x + 14.0, r.y + 34.0, text_max + 60.0, 11.2, Weight::Regular, self.pal.text2);
            }
            None => {
                self.text_elided(label, r.x + 14.0, r.cy(), text_max, 13.0, Weight::Regular, self.pal.text);
            }
        }
        r
    }

    /// Custom-height region inside a group (e.g. theme thumbnails, display preview).
    pub fn custom_row(&mut self, height: f32) -> Rect {
        self.row_sep();
        let r = Rect::new(self.x, self.y, self.w, height);
        self.y += height;
        r
    }

    pub fn row_value(&mut self, label: &str, value: &str) {
        let r = self.row_base(label, None, None);
        let max_w = (self.w * 0.48).max(100.0);
        let s = self.fonts.elide(value, 12.5, Weight::Regular, max_w);
        self.text_right(&s, r.right() - 14.0, r.cy(), 12.5, Weight::Regular, self.pal.text2);
    }

    pub fn row_toggle(&mut self, label: &str, sub: Option<&str>, on: &mut bool) -> bool {
        let r = self.row_base(label, sub, None);
        let tr = Rect::new(r.right() - 14.0 - 34.0, r.cy() - 9.5, 34.0, 19.0);
        self.toggle_at(tr, on)
    }

    pub fn row_slider(
        &mut self,
        id_key: &str,
        label: &str,
        sub: Option<&str>,
        value: &mut f64,
        min: f64,
        max: f64,
        left: &str,
        right: &str,
    ) -> bool {
        let has_labels = !left.is_empty() || !right.is_empty();
        let r = self.row_base(label, sub, Some(if has_labels { 48.0 } else { 40.0 }));
        let sw = 200.0f32.min(self.w * 0.42);
        let sr = Rect::new(
            r.right() - 14.0 - sw,
            if has_labels { r.y + 8.0 } else { r.cy() - 11.0 },
            sw,
            if has_labels { 34.0 } else { 22.0 },
        );
        self.slider_at(hash_id(id_key), sr, value, min, max, left, right)
    }

    pub fn row_popup(
        &mut self,
        id_key: &str,
        label: &str,
        sub: Option<&str>,
        current: &str,
        options: &[(String, String)],
    ) -> Option<String> {
        let r = self.row_base(label, sub, None);
        let id = hash_id(id_key);
        let display = options
            .iter()
            .find(|(k, _)| k == current)
            .map(|(_, l)| l.as_str())
            .unwrap_or("—");
        let text_w = self.fonts.measure(display, 13.0, Weight::Regular).min(260.0);
        let pw = text_w + 30.0;
        let pr = Rect::new(r.right() - 6.0 - pw, r.cy() - 12.0, pw, 24.0);
        self.popup_at(id, pr, current, display, options)
    }

    pub fn row_button(&mut self, label: &str, sub: Option<&str>, btn: &str) -> bool {
        let r = self.row_base(label, sub, None);
        let bw = (self.fonts.measure(btn, 12.5, Weight::Regular) + 24.0).max(64.0);
        let br = Rect::new(r.right() - 14.0 - bw, r.cy() - 12.0, bw, 24.0);
        self.button_at(br, btn, false, true)
    }

    pub fn row_clickable(&mut self, label: &str, sub: Option<&str>, checked: bool) -> bool {
        self.row_sep();
        let h = if sub.is_some() { 50.0 } else { 40.0 };
        let r = Rect::new(self.x, self.y, self.w, h);
        self.y += h;
        if self.hovered(r) {
            self.fill_rect(r.inset(2.0), 6.0, self.pal.hover);
        }
        let text_max = self.w - 56.0;
        match sub {
            Some(s) => {
                self.text_elided(label, r.x + 14.0, r.y + 17.0, text_max, 13.0, Weight::Regular, self.pal.text);
                self.text_elided(s, r.x + 14.0, r.y + 34.0, text_max, 11.2, Weight::Regular, self.pal.text2);
            }
            None => {
                self.text_elided(label, r.x + 14.0, r.cy(), text_max, 13.0, Weight::Regular, self.pal.text);
            }
        }
        if checked {
            self.checkmark(r.right() - 22.0, r.cy(), self.pal.accent);
        }
        self.clicked(r)
    }

    // ---------------------------------------------------------------- Primitives

    pub fn toggle_at(&mut self, r: Rect, on: &mut bool) -> bool {
        let clicked = self.clicked(r);
        if clicked {
            *on = !*on;
        }
        let bg = if *on { self.pal.accent } else { self.pal.toggle_off };
        self.fill_rect(r, r.h * 0.5, bg);
        let kr = r.h * 0.5 - 2.0;
        let cx = if *on { r.right() - kr - 2.0 } else { r.x + kr + 2.0 };
        self.circle_filled(cx, r.cy() + 0.5, kr + 0.3, [0, 0, 0, 40]);
        self.circle_filled(cx, r.cy(), kr, self.pal.knob);
        clicked
    }

    pub fn slider_at(
        &mut self,
        id: u64,
        r: Rect,
        value: &mut f64,
        min: f64,
        max: f64,
        left: &str,
        right: &str,
    ) -> bool {
        let knob_r = 8.0f32;
        let cy = r.y + 10.0;
        let x0 = r.x + knob_r;
        let x1 = (r.right() - knob_r).max(x0 + 1.0);
        let mut changed = false;

        if !self.dry_run && !self.block_input {
            if self.hovered(r) {
                self.st.hovered_pointer = true;
                if self.st.just_pressed {
                    self.st.active_slider = Some(id);
                }
            }
            if self.st.active_slider == Some(id) && self.st.pointer_down {
                let t = ((self.st.pointer.0 - x0) / (x1 - x0)).clamp(0.0, 1.0) as f64;
                let nv = min + (max - min) * t;
                if (nv - *value).abs() > 1e-6 {
                    *value = nv;
                    changed = true;
                }
            }
        }

        let span = (max - min).max(1e-9);
        let t = ((*value - min) / span).clamp(0.0, 1.0) as f32;
        let kx = x0 + (x1 - x0) * t;

        self.fill_rect(Rect::new(x0, cy - 2.0, x1 - x0, 4.0), 2.0, self.pal.toggle_off);
        self.fill_rect(Rect::new(x0, cy - 2.0, (kx - x0).max(0.0), 4.0), 2.0, self.pal.accent);
        self.circle_filled(kx, cy + 0.8, knob_r + 0.5, [0, 0, 0, 40]);
        self.circle_filled(kx, cy, knob_r, self.pal.knob);
        self.circle_stroke(kx, cy, knob_r, 0.8, self.pal.control_border);

        if !left.is_empty() || !right.is_empty() {
            let ly = r.bottom() - 5.0;
            self.text(left, r.x, ly, 10.5, Weight::Regular, self.pal.text2);
            self.text_right(right, r.right(), ly, 10.5, Weight::Regular, self.pal.text2);
        }
        changed
    }

    pub fn button_at(&mut self, r: Rect, label: &str, primary: bool, enabled: bool) -> bool {
        let hov = enabled && self.hovered(r);
        let down = hov && self.st.pointer_down;
        let mut fill = if primary { self.pal.accent } else { self.pal.control };
        if down {
            fill = mix(fill, [0, 0, 0, 255], 0.14);
        } else if hov && !primary {
            fill = mix(fill, self.pal.text, 0.05);
        }
        if !enabled {
            fill = with_alpha(fill, 120);
        }
        self.fill_rect(r, 6.0, fill);
        if !primary {
            self.stroke_rect(r, 6.0, 1.0, self.pal.control_border);
        }
        let fg = if primary {
            [255, 255, 255, if enabled { 255 } else { 150 }]
        } else if enabled {
            self.pal.text
        } else {
            self.pal.text3
        };
        self.text_center(label, r.cx(), r.cy(), 12.5, Weight::Regular, fg);
        enabled && self.clicked(r)
    }

    pub fn popup_at(
        &mut self,
        id: u64,
        r: Rect,
        current: &str,
        display: &str,
        options: &[(String, String)],
    ) -> Option<String> {
        let hov = self.hovered(r) || self.st.open_popup.as_ref().map(|p| p.id) == Some(id);
        if hov {
            self.fill_rect(r, 5.0, self.pal.hover);
        }
        self.text_elided(display, r.x + 8.0, r.cy(), r.w - 26.0, 13.0, Weight::Regular, self.pal.text);
        self.updown_chevrons(r.right() - 10.0, r.cy(), self.pal.text2);

        if self.clicked(r) && !options.is_empty() {
            self.st.open_popup = Some(PopupMenu {
                id,
                anchor: r,
                options: options.to_vec(),
                current: current.to_string(),
                scroll: 0.0,
            });
        }
        self.st.take_popup_result(id)
    }

    /// Focus + keyboard handling shared by text fields. Returns (focused, submitted).
    fn edit_text(&mut self, id: u64, r: Rect, value: &mut String) -> (bool, bool) {
        if (self.st.just_pressed && self.hovered(r)) || self.clicked(r) {
            self.st.focused_input = Some(id);
        }
        let focused = self.st.focused_input == Some(id);
        let mut submitted = false;
        if focused && !self.dry_run {
            let events = std::mem::take(&mut self.st.text_events);
            for ev in events {
                match ev {
                    TextEvent::Char(c) => {
                        if !c.is_control() {
                            value.push(c);
                        }
                    }
                    TextEvent::Backspace => {
                        value.pop();
                    }
                    TextEvent::Enter => submitted = true,
                    TextEvent::Escape => self.st.focused_input = None,
                }
            }
        }
        (self.st.focused_input == Some(id), submitted)
    }

    fn field_text(&mut self, r: Rect, x: f32, value: &str, hint: &str, password: bool, focused: bool) {
        let shown = if value.is_empty() {
            hint.to_string()
        } else if password {
            "•".repeat(value.chars().count())
        } else {
            value.to_string()
        };
        let color = if value.is_empty() { self.pal.text3 } else { self.pal.text };
        let max_w = (r.right() - x - 8.0).max(10.0);
        let disp = self.fonts.elide(&shown, 13.0, Weight::Regular, max_w);
        self.text(&disp, x, r.cy(), 13.0, Weight::Regular, color);
        if focused {
            let tw = if value.is_empty() { 0.0 } else { self.fonts.measure(&disp, 13.0, Weight::Regular) };
            let cx = (x + tw + 1.0).min(r.right() - 6.0);
            self.line(cx, r.y + 5.0, cx, r.bottom() - 5.0, 1.2, self.pal.text);
        }
    }

    /// Single-line text input. Returns true when Enter was pressed.
    pub fn text_input_at(&mut self, id: u64, r: Rect, value: &mut String, hint: &str, password: bool) -> bool {
        let (focused, submitted) = self.edit_text(id, r, value);
        self.fill_rect(r, 6.0, self.pal.field);
        if focused {
            self.stroke_rect(r.inset(-2.5), 8.0, 3.0, with_alpha(self.pal.accent, 110));
        }
        self.stroke_rect(r, 6.0, 1.0, self.pal.control_border);
        self.field_text(r, r.x + 8.0, value, hint, password, focused);
        submitted
    }

    /// Sidebar search field with a magnifying glass, macOS style.
    pub fn search_field(&mut self, id: u64, r: Rect, value: &mut String) -> bool {
        let (focused, submitted) = self.edit_text(id, r, value);
        self.fill_rect(r, 7.0, self.pal.field);
        if focused {
            self.stroke_rect(r.inset(-2.5), 9.0, 3.0, with_alpha(self.pal.accent, 110));
        }
        // magnifying glass
        let (gx, gy) = (r.x + 13.0, r.cy() - 0.8);
        self.circle_stroke(gx, gy, 4.2, 1.3, self.pal.text2);
        self.line(gx + 3.0, gy + 3.0, gx + 6.0, gy + 6.0, 1.5, self.pal.text2);
        self.field_text(r, r.x + 25.0, value, "Search", false, focused);
        if !value.is_empty() {
            let cr = Rect::new(r.right() - 22.0, r.cy() - 8.0, 16.0, 16.0);
            self.circle_filled(cr.cx(), cr.cy(), 6.5, self.pal.text3);
            let c = cr.cx();
            let m = cr.cy();
            self.line(c - 2.3, m - 2.3, c + 2.3, m + 2.3, 1.3, self.pal.sidebar);
            self.line(c + 2.3, m - 2.3, c - 2.3, m + 2.3, 1.3, self.pal.sidebar);
            if self.clicked(cr) {
                value.clear();
            }
        }
        submitted
    }

    pub fn checkmark(&mut self, cx: f32, cy: f32, color: Rgba) {
        self.line(cx - 4.0, cy, cx - 1.0, cy + 3.5, 1.8, color);
        self.line(cx - 1.0, cy + 3.5, cx + 4.5, cy - 4.0, 1.8, color);
    }

    pub fn updown_chevrons(&mut self, cx: f32, cy: f32, color: Rgba) {
        self.line(cx - 2.8, cy - 2.0, cx, cy - 4.8, 1.3, color);
        self.line(cx, cy - 4.8, cx + 2.8, cy - 2.0, 1.3, color);
        self.line(cx - 2.8, cy + 2.0, cx, cy + 4.8, 1.3, color);
        self.line(cx, cy + 4.8, cx + 2.8, cy + 2.0, 1.3, color);
    }

    pub fn signal_bars(&mut self, x: f32, cy: f32, strength: u8) {
        let lit = match strength {
            0..=25 => 1,
            26..=50 => 2,
            51..=75 => 3,
            _ => 4,
        };
        let bottom = cy + 6.0;
        for i in 0..4 {
            let h = 3.0 + i as f32 * 3.0;
            let r = Rect::new(x + i as f32 * 4.0, bottom - h, 2.6, h);
            let col = if i < lit { self.pal.text } else { self.pal.text3 };
            self.fill_rect(r, 1.0, col);
        }
    }

    /// Render and handle interaction for the currently open popup menu (overlay pass).
    pub fn render_popup_overlay(&mut self) {
        if self.dry_run {
            return;
        }
        let Some(popup) = self.st.open_popup.clone() else {
            return;
        };
        self.clip = None;
        self.block_input = false;

        let item_h = 26.0f32;
        let pad = 5.0f32;
        let max_label = popup
            .options
            .iter()
            .map(|(_, l)| self.fonts.measure(l, 12.5, Weight::Regular))
            .fold(0.0f32, f32::max);
        let menu_w = (max_label + 40.0).max(popup.anchor.w).clamp(130.0, 320.0);
        let total_h = popup.options.len() as f32 * item_h + pad * 2.0;
        let max_h = (self.win_h - 32.0).min(340.0);
        let menu_h = total_h.min(max_h);

        let mut mx = popup.anchor.right() - menu_w;
        if mx < 12.0 {
            mx = 12.0;
        }
        if mx + menu_w > self.win_w - 12.0 {
            mx = self.win_w - 12.0 - menu_w;
        }
        let mut my = popup.anchor.bottom() + 4.0;
        if my + menu_h > self.win_h - 12.0 {
            my = (popup.anchor.y - menu_h - 4.0).max(12.0);
        }
        let menu_r = Rect::new(mx, my, menu_w, menu_h);

        // Soft drop shadow
        self.fill_rect(
            Rect::new(menu_r.x - 2.0, menu_r.y + 4.0, menu_r.w + 4.0, menu_r.h + 4.0),
            10.0,
            [0, 0, 0, if self.pal.dark { 110 } else { 42 }],
        );
        self.fill_rect(menu_r, 8.0, self.pal.menu);
        self.stroke_rect(menu_r, 8.0, 1.0, self.pal.control_border);

        let inner = Rect::new(menu_r.x + pad, menu_r.y + pad, menu_r.w - pad * 2.0, menu_r.h - pad * 2.0);
        self.clip = Some(inner);

        let max_scroll = (total_h - menu_h).max(0.0);
        let scroll = popup.scroll.clamp(0.0, max_scroll);
        if let Some(p) = self.st.open_popup.as_mut() {
            p.scroll = scroll;
        }

        let mut chosen: Option<String> = None;
        for (i, (key, label)) in popup.options.iter().enumerate() {
            let iy = inner.y + i as f32 * item_h - scroll;
            let ir = Rect::new(inner.x, iy, inner.w, item_h);
            if !ir.intersects_y(inner.y, inner.bottom()) {
                continue;
            }
            let hov = self.hovered(ir);
            let fg = if hov {
                self.fill_rect(ir, 5.0, self.pal.accent);
                [255, 255, 255, 255]
            } else {
                self.pal.text
            };
            if key == &popup.current {
                self.checkmark(ir.x + 11.0, ir.cy(), fg);
            }
            self.text_elided(label, ir.x + 24.0, ir.cy(), ir.w - 30.0, 12.5, Weight::Regular, fg);
            if self.clicked(ir) {
                chosen = Some(key.clone());
            }
        }
        self.clip = None;

        if let Some(val) = chosen {
            self.st.popup_result = Some((popup.id, val));
            self.st.open_popup = None;
        } else if self.st.clicked && !menu_r.contains(self.st.pointer.0, self.st.pointer.1) {
            self.st.open_popup = None;
        }
    }
}
