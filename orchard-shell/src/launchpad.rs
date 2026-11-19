//! Launchpad: every app as a page grid over the blurred desktop, with a
//! search field, page dots, scroll / drag / arrow-key paging. The compositor
//! draws the frosted backdrop (namespace `orchard-launchpad`); this paints the
//! dimming, the icons and labels.

use std::time::Instant;

use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Rect, Stroke, Transform};

use crate::{
    apps::Apps,
    draw::{fill_circle, fill_rounded, paint, stroke_rounded},
    launcher::score,
    text::{Fonts, Weight},
};

const FIELD_W: f32 = 240.0;
const FIELD_H: f32 = 30.0;
const FIELD_TOP: f32 = 48.0;
const GRID_TOP: f32 = 128.0;
/// room kept free at the bottom for the page dots and the Dock
const BOTTOM_RESERVE: f32 = 150.0;
const LABEL_PX: f32 = 13.0;
/// page flip by dragging further than this share of the width
const FLIP_SHARE: f32 = 0.12;

/// What a click / hover hits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Hit {
    /// index into `results`
    App(usize),
    Dot(usize),
    Field,
    Empty,
}

pub struct Launchpad {
    /// indices into `Apps::entries`, alphabetical
    all: Vec<usize>,
    pub query: String,
    /// what is shown: `all`, or the search results
    pub results: Vec<usize>,
    /// current page position (animated, fractional while sliding)
    pub page: f32,
    pub page_target: usize,
    /// chosen with the keyboard (index into `results`)
    pub selected: Option<usize>,
    pub hovered: Option<usize>,
    pub pressed: Option<Hit>,
    /// 0 = closed, 1 = open (animated)
    pub open: f32,
    pub closing: bool,
    /// pointer drag that pages: (start x, page at start, became a drag)
    drag: Option<(f32, f32, bool)>,
    last_scroll: Option<Instant>,
    pub cols: usize,
    pub rows: usize,
}

/// Grid geometry for a surface of `w` × `h` logical px.
#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    pub w: f32,
    pub left: f32,
    pub top: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    pub icon: f32,
    pub dots_y: f32,
}

impl Launchpad {
    pub fn new(apps: &Apps) -> Launchpad {
        let mut all: Vec<usize> = apps
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.no_display && e.exec.is_some())
            .map(|(i, _)| i)
            .collect();
        all.sort_by_key(|i| apps.entries[*i].name.to_lowercase());
        all.dedup_by(|a, b| apps.entries[*a].name == apps.entries[*b].name);
        Launchpad {
            results: all.clone(),
            all,
            query: String::new(),
            page: 0.0,
            page_target: 0,
            selected: None,
            hovered: None,
            pressed: None,
            open: 0.0,
            closing: false,
            drag: None,
            last_scroll: None,
            cols: 7,
            rows: 5,
        }
    }

    pub fn per_page(&self) -> usize {
        self.cols * self.rows
    }

    pub fn pages(&self) -> usize {
        self.results.len().div_ceil(self.per_page()).max(1)
    }

    pub fn geometry(&mut self, w: f32, h: f32) -> Geometry {
        self.rows = if h < 820.0 { 4 } else { 5 };
        self.cols = if w < 1100.0 { 5 } else if w < 1500.0 { 6 } else { 7 };
        let grid_w = (w * 0.74).min(self.cols as f32 * 210.0);
        let grid_h = (h - GRID_TOP - BOTTOM_RESERVE).max(100.0);
        let cell_w = grid_w / self.cols as f32;
        let cell_h = grid_h / self.rows as f32;
        let icon = (cell_h * 0.56).min(cell_w * 0.5).min(96.0).floor();
        Geometry {
            w,
            left: (w - grid_w) / 2.0,
            top: GRID_TOP,
            cell_w,
            cell_h,
            icon,
            dots_y: GRID_TOP + grid_h + 22.0,
        }
    }

    /// Icon rect of result `i` at the settled page position (logical).
    fn icon_rect(&self, g: &Geometry, i: usize, page_pos: f32) -> (f32, f32, f32) {
        let pp = self.per_page();
        let (page, k) = (i / pp, i % pp);
        let (col, row) = ((k % self.cols) as f32, (k / self.cols) as f32);
        let x = (page as f32 - page_pos) * g.w + g.left + col * g.cell_w + (g.cell_w - g.icon) / 2.0;
        let y = g.top + row * g.cell_h + (g.cell_h - g.icon - LABEL_PX - 10.0) / 2.0;
        (x, y, g.icon)
    }

    fn field_rect(g: &Geometry) -> (f32, f32, f32, f32) {
        ((g.w - FIELD_W) / 2.0, FIELD_TOP, FIELD_W, FIELD_H)
    }

    fn dot_x(&self, g: &Geometry, i: usize) -> f32 {
        let n = self.pages() as f32;
        g.w / 2.0 + (i as f32 - (n - 1.0) / 2.0) * 18.0
    }

    pub fn hit(&mut self, w: f32, h: f32, x: f32, y: f32) -> Hit {
        let g = self.geometry(w, h);
        let (fx, fy, fw, fh) = Self::field_rect(&g);
        if x >= fx && x <= fx + fw && y >= fy && y <= fy + fh {
            return Hit::Field;
        }
        if self.pages() > 1 && (y - g.dots_y).abs() < 10.0 {
            for i in 0..self.pages() {
                if (x - self.dot_x(&g, i)).abs() < 9.0 {
                    return Hit::Dot(i);
                }
            }
        }
        let page = self.page.round().max(0.0) as usize;
        let pp = self.per_page();
        for i in page * pp..((page + 1) * pp).min(self.results.len()) {
            let (ix, iy, s) = self.icon_rect(&g, i, self.page);
            // icon plus its label
            if x >= ix - 10.0 && x <= ix + s + 10.0 && y >= iy - 6.0 && y <= iy + s + LABEL_PX + 14.0 {
                return Hit::App(i);
            }
        }
        Hit::Empty
    }

    /// New search text: show matches, first one selected.
    pub fn set_query(&mut self, apps: &Apps, q: String) {
        self.query = q;
        if self.query.is_empty() {
            self.results = self.all.clone();
            self.selected = None;
        } else {
            let mut scored: Vec<(u32, usize)> = self
                .all
                .iter()
                .filter_map(|i| {
                    let e = &apps.entries[*i];
                    let s = score(&e.name, &self.query).or_else(|| score(&e.id, &self.query).map(|s| s / 2))?;
                    Some((s, *i))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(apps.entries[a.1].name.cmp(&apps.entries[b.1].name)));
            self.results = scored.into_iter().map(|(_, i)| i).collect();
            self.selected = (!self.results.is_empty()).then_some(0);
        }
        self.page_target = 0;
        self.hovered = None;
    }

    pub fn go_to_page(&mut self, p: usize) {
        self.page_target = p.min(self.pages() - 1);
    }

    /// Arrow keys: move the selection, paging at the edges.
    pub fn move_selection(&mut self, dx: i32, dy: i32) {
        let n = self.results.len();
        if n == 0 {
            return;
        }
        let pp = self.per_page();
        let Some(cur) = self.selected else {
            self.selected = Some((self.page_target * pp).min(n - 1));
            return;
        };
        let (page, k) = (cur / pp, cur % pp);
        let (col, row) = ((k % self.cols) as i32, (k / self.cols) as i32);
        let next = if dx != 0 {
            let c = col + dx;
            if c < 0 {
                // previous page, same row, last column
                if page == 0 {
                    return;
                }
                (page - 1) * pp + row as usize * self.cols + self.cols - 1
            } else if c >= self.cols as i32 {
                (page + 1) * pp + row as usize * self.cols
            } else {
                page * pp + row as usize * self.cols + c as usize
            }
        } else {
            let r = row + dy;
            if r < 0 || r >= self.rows as i32 {
                return;
            }
            page * pp + r as usize * self.cols + col as usize
        };
        let next = next.min(n - 1);
        self.selected = Some(next);
        self.page_target = next / pp;
    }

    pub fn press(&mut self, hit: Hit, x: f32) {
        self.pressed = Some(hit);
        self.drag = Some((x, self.page, false));
    }

    /// Pointer motion with the button down: drag the pages along.
    pub fn drag_to(&mut self, x: f32, w: f32) {
        let max = (self.pages() - 1) as f32;
        if let Some((x0, p0, active)) = self.drag.as_mut() {
            if !*active && (x - *x0).abs() > 8.0 {
                *active = true;
            }
            if *active {
                let mut p = *p0 - (x - *x0) / w;
                // rubber band past the first / last page
                if p < 0.0 {
                    p *= 0.3;
                } else if p > max {
                    p = max + (p - max) * 0.3;
                }
                self.page = p;
                self.pressed = None;
            }
        }
    }

    /// Button released. Returns what was clicked, if it was a click.
    pub fn release(&mut self, hit: Hit) -> Option<Hit> {
        let drag = self.drag.take();
        let pressed = self.pressed.take();
        if let Some((_, p0, true)) = drag {
            let d = self.page - p0;
            let base = p0.round();
            let target = if d > FLIP_SHARE {
                base + 1.0
            } else if d < -FLIP_SHARE {
                base - 1.0
            } else {
                base
            };
            self.go_to_page(target.max(0.0) as usize);
            return None;
        }
        (pressed == Some(hit)).then_some(hit)
    }

    /// Mouse wheel / touchpad scroll: one page per gesture.
    pub fn scroll(&mut self, delta: f32) {
        if delta.abs() < 0.5 {
            return;
        }
        if self.last_scroll.map(|t| t.elapsed().as_millis() < 350).unwrap_or(false) {
            self.last_scroll = Some(Instant::now());
            return;
        }
        self.last_scroll = Some(Instant::now());
        let p = self.page_target as i32 + if delta > 0.0 { 1 } else { -1 };
        if p >= 0 {
            self.go_to_page(p as usize);
        }
    }

    pub fn is_animating(&self) -> bool {
        let to = if self.closing { 0.0 } else { 1.0 };
        (self.open - to).abs() > 0.002 || (self.drag.map(|d| !d.2).unwrap_or(true) && (self.page - self.page_target as f32).abs() > 0.002)
    }

    /// Fully closed after the closing animation.
    pub fn is_closed(&self) -> bool {
        self.closing && self.open <= 0.0
    }

    pub fn step(&mut self, dt: f32) {
        let to = if self.closing { 0.0 } else { 1.0 };
        let k = 1.0 - (-dt * if self.closing { 14.0 } else { 11.0 }).exp();
        self.open += (to - self.open) * k;
        if (self.open - to).abs() < 0.004 {
            self.open = to;
        }
        if !self.drag.map(|d| d.2).unwrap_or(false) {
            let t = self.page_target as f32;
            let kp = 1.0 - (-dt * 12.0).exp();
            self.page += (t - self.page) * kp;
            if (self.page - t).abs() < 0.002 {
                self.page = t;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn paint(&mut self, pm: &mut Pixmap, scale: f32, apps: &Apps, fonts: &Fonts, german: bool) {
        let s = scale;
        let w = pm.width() as f32 / s;
        let h = pm.height() as f32 / s;
        let g = self.geometry(w, h);
        let a = self.open.clamp(0.0, 1.0);
        let alpha = |v: f32| (v * a).round().clamp(0.0, 255.0) as u8;

        // darken the frosted desktop a little
        if let Some(r) = Rect::from_xywh(0.0, 0.0, pm.width() as f32, pm.height() as f32) {
            pm.fill_rect(r, &paint([0, 0, 0, alpha(70.0)]), Transform::identity(), None);
        }

        // icons zoom in from slightly larger while opening (and out on close)
        let zoom = 1.0 + 0.1 * (1.0 - a);
        let (cx, cy) = (w / 2.0, h / 2.0);
        let z = |x: f32, y: f32| (cx + (x - cx) * zoom, cy + (y - cy) * zoom);

        // search field
        let (fx, fy, fw, fh) = Self::field_rect(&g);
        fill_rounded(pm, fx * s, fy * s, fw * s, fh * s, 8.0 * s, [255, 255, 255, alpha(46.0)]);
        stroke_rounded(pm, fx * s + 0.5, fy * s + 0.5, fw * s - 1.0, fh * s - 1.0, 8.0 * s, 1.0, [255, 255, 255, alpha(40.0)]);
        let fpx = 14.0 * s;
        let baseline = fonts.baseline_centered(fy * s, fh * s, fpx, Weight::Regular);
        let glyph = (fx + 16.0) * s;
        if let Some(circle) = tiny_skia::PathBuilder::from_circle(glyph, (fy + fh / 2.0 - 1.0) * s, 5.0 * s) {
            let st = Stroke {
                width: 1.6 * s,
                ..Default::default()
            };
            let c = [255, 255, 255, alpha(170.0)];
            pm.stroke_path(&circle, &paint(c), &st, Transform::identity(), None);
            let mut pb = tiny_skia::PathBuilder::new();
            pb.move_to(glyph + 3.8 * s, (fy + fh / 2.0 + 2.8) * s);
            pb.line_to(glyph + 7.5 * s, (fy + fh / 2.0 + 6.5) * s);
            if let Some(p) = pb.finish() {
                pm.stroke_path(&p, &paint(c), &st, Transform::identity(), None);
            }
        }
        if self.query.is_empty() {
            let ph = if german { "Suchen" } else { "Search" };
            let tw = fonts.measure(ph, fpx, Weight::Regular);
            fonts.draw(pm, ph, (g.w / 2.0) * s - tw / 2.0, baseline, fpx, Weight::Regular, [255, 255, 255, alpha(150.0)]);
        } else {
            let tx = (fx + 28.0) * s;
            let shown = fonts.elide(&self.query, fpx, Weight::Regular, (fw - 40.0) * s);
            let tw = fonts.draw(pm, &shown, tx, baseline, fpx, Weight::Regular, [255, 255, 255, alpha(240.0)]);
            fill_rounded(pm, tx + tw + 1.0 * s, (fy + 7.0) * s, 1.5 * s, (fh - 14.0) * s, 0.5, [255, 255, 255, alpha(220.0)]);
        }

        if self.results.is_empty() {
            let msg = if german { "Keine Ergebnisse" } else { "No Results" };
            let px = 22.0 * s;
            let tw = fonts.measure(msg, px, Weight::Regular);
            fonts.draw(pm, msg, (w / 2.0) * s - tw / 2.0, (h * 0.45) * s, px, Weight::Regular, [255, 255, 255, alpha(150.0)]);
            return;
        }

        // the pages next to the current position
        let pp = self.per_page();
        let lo = (self.page.floor().max(0.0) as usize) * pp;
        let hi = ((self.page.ceil() as usize + 1) * pp).min(self.results.len());
        let isz = (g.icon * zoom * s).round().max(1.0) as u32;
        let img_px = (g.icon * s).round() as u32;
        for i in lo..hi {
            let (x, y, size) = self.icon_rect(&g, i, self.page);
            if x + size < -g.cell_w || x > w + g.cell_w {
                continue;
            }
            let (zx, zy) = z(x, y);
            let zs = size * zoom;
            let e = &apps.entries[self.results[i]];
            let highlighted = self.selected == Some(i);
            if highlighted {
                let pad = 12.0;
                fill_rounded(
                    pm,
                    (zx - pad) * s,
                    (zy - pad + 2.0) * s,
                    (zs + 2.0 * pad) * s,
                    (zs + 2.0 * pad + LABEL_PX + 6.0) * s,
                    16.0 * s,
                    [255, 255, 255, alpha(46.0)],
                );
            }
            let icon = e
                .icon
                .as_deref()
                .and_then(|n| apps.icon(n, img_px))
                .or_else(|| apps.icon("application-x-executable", img_px));
            if let Some(icon) = icon {
                let k = isz as f32 / icon.width() as f32;
                let p = PixmapPaint {
                    quality: FilterQuality::Bilinear,
                    opacity: a,
                    ..Default::default()
                };
                pm.draw_pixmap(0, 0, icon.as_ref().as_ref(), &p, Transform::from_row(k, 0.0, 0.0, k, zx * s, zy * s), None);
                if self.pressed == Some(Hit::App(i)) {
                    fill_rounded(pm, zx * s, zy * s, zs * s, zs * s, zs * 0.22 * s, [0, 0, 0, 80]);
                }
            }
            // label, with a soft shadow for legibility on bright wallpapers
            let px = LABEL_PX * s;
            let label = fonts.elide(&e.name, px, Weight::Regular, (g.cell_w - 14.0) * s);
            let tw = fonts.measure(&label, px, Weight::Regular);
            let lx = (zx + zs / 2.0) * s - tw / 2.0;
            let ly = (zy + zs + 8.0) * s + px * 0.8;
            fonts.draw(pm, &label, lx, ly + 1.0 * s, px, Weight::Regular, [0, 0, 0, alpha(110.0)]);
            fonts.draw(pm, &label, lx, ly, px, Weight::Regular, [255, 255, 255, alpha(245.0)]);
        }

        // page dots
        if self.pages() > 1 {
            for i in 0..self.pages() {
                let active = (self.page.round() as usize) == i;
                let c = if active { [255, 255, 255, alpha(235.0)] } else { [255, 255, 255, alpha(90.0)] };
                fill_circle(pm, self.dot_x(&g, i) * s, g.dots_y * s, 3.6 * s, c);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pad(n: usize) -> Launchpad {
        let mut l = Launchpad {
            all: (0..n).collect(),
            query: String::new(),
            results: (0..n).collect(),
            page: 0.0,
            page_target: 0,
            selected: None,
            hovered: None,
            pressed: None,
            open: 1.0,
            closing: false,
            drag: None,
            last_scroll: None,
            cols: 7,
            rows: 5,
        };
        l.geometry(1920.0, 1080.0);
        l
    }

    #[test]
    fn pages_and_selection_wrap_to_next_page() {
        let mut l = pad(40);
        assert_eq!(l.pages(), 2);
        l.move_selection(1, 0);
        assert_eq!(l.selected, Some(0));
        for _ in 0..6 {
            l.move_selection(1, 0);
        }
        assert_eq!(l.selected, Some(6));
        l.move_selection(1, 0);
        assert_eq!(l.selected, Some(35), "row 0 of page 2");
        assert_eq!(l.page_target, 1);
        l.move_selection(-1, 0);
        assert_eq!(l.selected, Some(6));
        assert_eq!(l.page_target, 0);
    }

    #[test]
    fn drag_flips_page() {
        let mut l = pad(80);
        l.press(Hit::Empty, 1000.0);
        l.drag_to(700.0, 1920.0);
        assert!(l.page > 0.1);
        assert_eq!(l.release(Hit::Empty), None);
        assert_eq!(l.page_target, 1);
        // a short drag snaps back
        l.page = 1.0;
        l.press(Hit::Empty, 1000.0);
        l.drag_to(950.0, 1920.0);
        l.release(Hit::Empty);
        assert_eq!(l.page_target, 1);
    }

    #[test]
    fn hit_finds_icons_and_field() {
        let mut l = pad(10);
        let g = l.geometry(1920.0, 1080.0);
        let (x, y, s) = l.icon_rect(&g, 3, 0.0);
        assert_eq!(l.hit(1920.0, 1080.0, x + s / 2.0, y + s / 2.0), Hit::App(3));
        assert_eq!(l.hit(1920.0, 1080.0, 960.0, FIELD_TOP + 10.0), Hit::Field);
        assert_eq!(l.hit(1920.0, 1080.0, 10.0, 1000.0), Hit::Empty);
    }
}
