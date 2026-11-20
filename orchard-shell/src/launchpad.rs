//! Launchpad as a Spotlight "Apps" panel (macOS Tahoe style): a centered,
//! frosted, rounded window with a search field on top and a vertically
//! scrolling grid of every app below. Typing filters the grid, the arrow keys
//! move the selection, Enter opens it, a click outside or Esc closes it.
//! The compositor blurs behind the panel (namespace `orchard-launchpad`);
//! this paints the tint, the field, the icons and labels.

use tiny_skia::{FillRule, FilterQuality, Mask, Pixmap, PixmapPaint, Stroke, Transform};

use crate::{
    apps::Apps,
    draw::{composite, fill_rounded, paint, rounded_rect, stroke_rounded},
    launcher::score,
    text::{Fonts, Weight},
    theme::Theme,
};

/// panel corner radius
const RADIUS: f32 = 26.0;
/// search field row at the top of the panel
const HEADER_H: f32 = 62.0;
/// gap above / below the grid
const GRID_PAD: f32 = 12.0;
/// left / right margin of the grid
const SIDE: f32 = 18.0;
const CELL_W: f32 = 116.0;
const CELL_H: f32 = 110.0;
const ICON: f32 = 64.0;
const LABEL_PX: f32 = 12.5;
/// one mouse wheel notch
const NOTCH: f32 = 48.0;

/// What a click / hover hits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Hit {
    /// index into `results`
    App(usize),
    Field,
    /// inside the panel, but on nothing
    Panel,
    /// outside the panel: closes it
    Outside,
}

/// Panel geometry for a surface of `w` × `h` logical px.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geometry {
    /// panel rect (logical, settled / fully open)
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub cols: usize,
    /// visible height of the grid viewport
    pub view_h: f32,
}

impl Geometry {
    pub fn new(sw: f32, sh: f32) -> Geometry {
        let avail_w = (sw - 120.0).min(980.0);
        let cols = (((avail_w - 2.0 * SIDE) / CELL_W).floor() as usize).clamp(3, 7);
        let avail_h = (sh - 190.0).min(740.0);
        let rows = (((avail_h - HEADER_H - 2.0 * GRID_PAD) / CELL_H).floor() as usize).clamp(2, 6);
        let w = cols as f32 * CELL_W + 2.0 * SIDE;
        let view_h = rows as f32 * CELL_H;
        let h = HEADER_H + 2.0 * GRID_PAD + view_h;
        // a touch above the centre, like Spotlight, but clear of the menu bar
        let y = ((sh - h) / 2.0 - sh * 0.03).max(40.0).round();
        Geometry {
            x: ((sw - w) / 2.0).round(),
            y,
            w,
            h,
            cols,
            view_h,
        }
    }

    fn grid_top(&self) -> f32 {
        self.y + HEADER_H + GRID_PAD
    }
}

pub struct Launchpad {
    /// indices into `Apps::entries`, alphabetical
    all: Vec<usize>,
    pub query: String,
    /// what is shown: `all`, or the search results
    pub results: Vec<usize>,
    /// grid scroll offset in logical px (animated) and where it is heading
    pub scroll: f32,
    pub scroll_target: f32,
    /// chosen with the keyboard (index into `results`)
    pub selected: Option<usize>,
    pub hovered: Option<usize>,
    pub pressed: Option<Hit>,
    /// 0 = closed, 1 = open (animated)
    pub open: f32,
    pub closing: bool,
    /// last layout, for keyboard navigation and scrolling
    geo: Geometry,
    cache: Cache,
}

/// room around the panel for its shadow (logical px)
const SHADOW_PAD: f32 = 48.0;

struct Cached<K, P> {
    key: K,
    pm: P,
}

/// What the static panel chrome depends on.
#[derive(Clone, PartialEq)]
struct BaseKey {
    size: (u32, u32),
    query: String,
    colors: [[u8; 4]; 4],
    german: bool,
}

/// What the rendered panel depends on.
#[derive(PartialEq)]
struct PanelKey {
    base: BaseKey,
    scroll: u32,
    selected: Option<usize>,
    hovered: Option<usize>,
    pressed: Option<isize>,
    icon_gen: Option<usize>,
    results: usize,
}

/// Pre-rendered layers, so animation frames only composite.
#[derive(Default)]
struct Cache {
    shadow: Option<Cached<(u32, u32), Pixmap>>,
    base: Option<Cached<BaseKey, Pixmap>>,
    clip: Option<Cached<(u32, u32), Mask>>,
    panel: Option<Cached<PanelKey, Pixmap>>,
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
        Launchpad::with(all)
    }

    fn with(all: Vec<usize>) -> Launchpad {
        Launchpad {
            results: all.clone(),
            all,
            query: String::new(),
            scroll: 0.0,
            scroll_target: 0.0,
            selected: None,
            hovered: None,
            pressed: None,
            open: 0.0,
            closing: false,
            geo: Geometry::new(1920.0, 1080.0),
            cache: Cache::default(),
        }
    }

    pub fn geometry(&mut self, w: f32, h: f32) -> Geometry {
        let g = Geometry::new(w, h);
        if g != self.geo {
            self.geo = g;
            self.clamp_scroll();
        }
        g
    }

    fn content_h(&self) -> f32 {
        self.results.len().div_ceil(self.geo.cols) as f32 * CELL_H
    }

    fn max_scroll(&self) -> f32 {
        (self.content_h() - self.geo.view_h).max(0.0)
    }

    fn clamp_scroll(&mut self) {
        let max = self.max_scroll();
        self.scroll_target = self.scroll_target.clamp(0.0, max);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    /// Scale of the panel during the open / close animation.
    fn zoom(&self) -> f32 {
        let a = self.open.clamp(0.0, 1.0);
        0.94 + 0.06 * a
    }

    /// The panel as it is drawn right now (scaled while animating), logical:
    /// (x, y, w, h, radius). The compositor blurs behind this.
    pub fn backdrop(&mut self, sw: f32, sh: f32) -> (f32, f32, f32, f32, f32) {
        let g = self.geometry(sw, sh);
        let k = self.zoom();
        let (cx, cy) = (g.x + g.w / 2.0, g.y + g.h / 2.0);
        (cx - g.w * k / 2.0, cy - g.h * k / 2.0, g.w * k, g.h * k, RADIUS * k)
    }

    /// Icon rect of result `i` (logical, settled panel, current scroll).
    fn icon_rect(&self, g: &Geometry, i: usize) -> (f32, f32, f32) {
        let (col, row) = ((i % g.cols) as f32, (i / g.cols) as f32);
        let x = g.x + SIDE + col * CELL_W + (CELL_W - ICON) / 2.0;
        let y = g.grid_top() + row * CELL_H + 10.0 - self.scroll;
        (x, y, ICON)
    }

    pub fn hit(&mut self, w: f32, h: f32, x: f32, y: f32) -> Hit {
        let g = self.geometry(w, h);
        if x < g.x || x > g.x + g.w || y < g.y || y > g.y + g.h {
            return Hit::Outside;
        }
        if y < g.y + HEADER_H {
            return Hit::Field;
        }
        let gx = x - g.x - SIDE;
        let gy = y - g.grid_top() + self.scroll;
        if gx < 0.0 || gx >= g.cols as f32 * CELL_W || gy < 0.0 || y > g.y + g.h - 2.0 {
            return Hit::Panel;
        }
        let (col, row) = ((gx / CELL_W) as usize, (gy / CELL_H) as usize);
        let i = row * g.cols + col;
        if i >= self.results.len() {
            return Hit::Panel;
        }
        // the icon plus its label, not the gaps between cells
        let (ix, iy, s) = self.icon_rect(&g, i);
        if x >= ix - 18.0 && x <= ix + s + 18.0 && y >= iy - 8.0 && y <= iy + s + LABEL_PX + 16.0 {
            Hit::App(i)
        } else {
            Hit::Panel
        }
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
        self.scroll = 0.0;
        self.scroll_target = 0.0;
        self.hovered = None;
    }

    /// Arrow keys: move the selection through the grid, scrolling along.
    pub fn move_selection(&mut self, dx: i32, dy: i32) {
        let n = self.results.len() as i32;
        if n == 0 {
            return;
        }
        let cols = self.geo.cols as i32;
        let Some(cur) = self.selected.or(self.hovered) else {
            // start at the first app in view
            let first = ((self.scroll_target / CELL_H).ceil() as i32 * cols).min(n - 1);
            return self.select(first as usize);
        };
        let cur = cur as i32;
        let next = if dx != 0 {
            cur + dx
        } else {
            let r = cur + dy * cols;
            // moving down onto a short last row lands on its last app
            if dy > 0 && r >= n && cur / cols < (n - 1) / cols {
                n - 1
            } else {
                r
            }
        };
        if (0..n).contains(&next) {
            self.select(next as usize);
        }
    }

    /// Page Up / Down: a screenful of rows.
    pub fn page(&mut self, dir: i32) {
        let rows = (self.geo.view_h / CELL_H).floor().max(1.0) as i32;
        let n = self.results.len() as i32;
        if n == 0 {
            return;
        }
        let cur = self.selected.or(self.hovered).unwrap_or(0) as i32;
        self.select((cur + dir * rows * self.geo.cols as i32).clamp(0, n - 1) as usize);
    }

    pub fn select_edge(&mut self, end: bool) {
        if !self.results.is_empty() {
            self.select(if end { self.results.len() - 1 } else { 0 });
        }
    }

    fn select(&mut self, i: usize) {
        self.selected = Some(i);
        self.hovered = None;
        // keep its row in view
        let top = (i / self.geo.cols) as f32 * CELL_H;
        if top < self.scroll_target {
            self.scroll_target = top;
        } else if top + CELL_H > self.scroll_target + self.geo.view_h {
            self.scroll_target = top + CELL_H - self.geo.view_h;
        }
        self.clamp_scroll();
    }

    pub fn press(&mut self, hit: Hit) {
        self.pressed = Some(hit);
    }

    /// Button released. Returns what was clicked, if it was a click.
    pub fn release(&mut self, hit: Hit) -> Option<Hit> {
        let pressed = self.pressed.take();
        (pressed == Some(hit)).then_some(hit)
    }

    /// Mouse wheel / touchpad: `delta` in logical px, positive scrolls down.
    pub fn scroll_by(&mut self, delta: f32) {
        self.scroll_target += delta;
        self.clamp_scroll();
        // touchpads move 1:1 with the fingers
        if delta.abs() < NOTCH {
            self.scroll = self.scroll_target;
        }
    }

    pub fn is_animating(&self) -> bool {
        let to = if self.closing { 0.0 } else { 1.0 };
        (self.open - to).abs() > 0.002 || (self.scroll - self.scroll_target).abs() > 0.3
    }

    /// Fully closed after the closing animation.
    pub fn is_closed(&self) -> bool {
        self.closing && self.open <= 0.0
    }

    pub fn step(&mut self, dt: f32) {
        let to = if self.closing { 0.0 } else { 1.0 };
        let k = 1.0 - (-dt * if self.closing { 16.0 } else { 13.0 }).exp();
        self.open += (to - self.open) * k;
        if (self.open - to).abs() < 0.004 {
            self.open = to;
        }
        let ks = 1.0 - (-dt * 18.0).exp();
        self.scroll += (self.scroll_target - self.scroll) * ks;
        if (self.scroll - self.scroll_target).abs() < 0.3 {
            self.scroll = self.scroll_target;
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn paint(&mut self, pm: &mut Pixmap, scale: f32, apps: &Apps, fonts: &Fonts, theme: &Theme, german: bool) {
        let s = scale;
        let sw = pm.width() as f32 / s;
        let sh = pm.height() as f32 / s;
        let g = self.geometry(sw, sh);
        let a = self.open.clamp(0.0, 1.0);
        if a <= 0.0 {
            return;
        }
        // Everything expensive (the soft shadow, the frosted tint, the clip
        // mask, the icon grid) is rendered once into cached pixmaps; a frame
        // of the open / close animation only composites them, and the panel
        // contents are re-rendered only when what they show changes.
        let (pw, ph) = (((g.w * s).round() as u32).max(1), ((g.h * s).round() as u32).max(1));
        self.ensure_shadow(pw, ph, s, &g);
        self.ensure_panel(pw, ph, s, &g, apps, fonts, theme, german);

        let k = self.zoom();
        let (bx, by, _, _, _) = self.backdrop(sw, sh);
        let settled = a >= 1.0;
        let (k, ox, oy, px, py) = if settled {
            (1.0, ((g.x - SHADOW_PAD) * s).round(), ((g.y - SHADOW_PAD) * s).round(), (g.x * s).round(), (g.y * s).round())
        } else {
            (k, (bx - SHADOW_PAD * k) * s, (by - SHADOW_PAD * k) * s, bx * s, by * s)
        };
        if let Some(c) = self.cache.shadow.as_ref() {
            composite(pm, &c.pm, ox, oy, k, a);
        }
        if let Some(c) = self.cache.panel.as_ref() {
            composite(pm, &c.pm, px, py, k, a);
        }
    }

    /// The soft shadow around the settled panel, with the panel itself cut
    /// out (it must not darken the blurred glass). Offset by `SHADOW_PAD`.
    fn ensure_shadow(&mut self, pw: u32, ph: u32, s: f32, g: &Geometry) {
        let key = (pw, ph);
        if self.cache.shadow.as_ref().is_some_and(|c| c.key == key) {
            return;
        }
        let pad = SHADOW_PAD * s;
        let Some(mut sp) = Pixmap::new(pw + 2 * pad.ceil() as u32, ph + 2 * pad.ceil() as u32) else { return };
        for i in 1..=10 {
            let t = i as f32 / 10.0;
            let (grow, al, dy) = (30.0 * t, 5.0 * (1.0 - t) + 1.0, 4.0 + 10.0 * t);
            fill_rounded(
                &mut sp,
                pad - grow * s,
                pad + (dy - grow) * s,
                (g.w + 2.0 * grow) * s,
                (g.h + 2.0 * grow) * s,
                (RADIUS + grow) * s,
                [0, 0, 0, al as u8],
            );
        }
        if let Some(p) = rounded_rect(pad, pad, g.w * s, g.h * s, RADIUS * s) {
            let mut clear = paint([0, 0, 0, 0]);
            clear.blend_mode = tiny_skia::BlendMode::Clear;
            sp.fill_path(&p, &clear, FillRule::Winding, Transform::identity(), None);
        }
        self.cache.shadow = Some(Cached { key, pm: sp });
    }

    /// The panel at full size: tint, search field, grid, glass edge.
    #[allow(clippy::too_many_arguments)]
    fn ensure_panel(&mut self, pw: u32, ph: u32, s: f32, g: &Geometry, apps: &Apps, fonts: &Fonts, theme: &Theme, german: bool) {
        let base_key = BaseKey {
            size: (pw, ph),
            query: self.query.clone(),
            colors: [theme.menu_text, theme.menu_bg, theme.menu_selection, theme.menu_separator],
            german,
        };
        let key = PanelKey {
            base: base_key.clone(),
            scroll: self.scroll.to_bits(),
            selected: self.selected,
            hovered: self.hovered,
            pressed: self.pressed.map(|h| match h {
                Hit::App(i) => i as isize,
                _ => -1,
            }),
            // a new icon theme clears the icon cache: the fallback icon is a
            // fresh pixmap then
            icon_gen: apps
                .icon("application-x-executable", (ICON * s).round() as u32)
                .map(|p| std::rc::Rc::as_ptr(&p) as usize),
            results: self.results.len(),
        };
        if self.cache.panel.as_ref().is_some_and(|c| c.key == key) {
            return;
        }

        // static chrome: tint, search field, hairline, glass edge
        if !self.cache.base.as_ref().is_some_and(|c| c.key == base_key) {
            let Some(mut base) = Pixmap::new(pw, ph) else { return };
            let dark = theme.menu_text[0] > 128;
            let tint = match (dark, theme.menu_bg[3] == 255) {
                (_, true) => theme.menu_bg,
                (true, false) => [32, 32, 34, 168],
                (false, false) => [248, 248, 250, 160],
            };
            fill_rounded(&mut base, 0.0, 0.0, g.w * s, g.h * s, RADIUS * s, tint);
            self.paint_header(&mut base, s, g, fonts, theme, german);
            let edge = if dark { [255, 255, 255, 46] } else { [255, 255, 255, 150] };
            stroke_rounded(&mut base, 0.5, 0.5, g.w * s - 1.0, g.h * s - 1.0, RADIUS * s, 1.0, edge);
            self.cache.base = Some(Cached { key: base_key, pm: base });
        }
        if !self.cache.clip.as_ref().is_some_and(|c| c.key == (pw, ph)) {
            let (Some(mut mask), Some(path)) = (Mask::new(pw, ph), rounded_rect(0.0, 0.0, g.w * s, g.h * s, RADIUS * s)) else { return };
            mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
            self.cache.clip = Some(Cached { key: (pw, ph), pm: mask });
        }

        // reuse the previous panel's allocation
        let mut panel = match self.cache.panel.take() {
            Some(c) if c.pm.width() == pw && c.pm.height() == ph => c.pm,
            _ => match Pixmap::new(pw, ph) {
                Some(p) => p,
                None => return,
            },
        };
        let (Some(base), Some(clip)) = (self.cache.base.as_ref(), self.cache.clip.as_ref()) else { return };
        panel.data_mut().copy_from_slice(base.pm.data());
        self.paint_grid(&mut panel, &clip.pm, s, g, apps, fonts, theme, german);
        self.cache.panel = Some(Cached { key, pm: panel });
    }

    /// Search field: magnifier, query or placeholder, and the result count.
    fn paint_header(&self, pm: &mut Pixmap, s: f32, g: &Geometry, fonts: &Fonts, theme: &Theme, german: bool) {
        let text = theme.menu_text;
        let with_alpha = |a: u8| [text[0], text[1], text[2], a];

        // magnifier
        let gx = 28.0 * s;
        let gy = (HEADER_H / 2.0 - 2.0) * s;
        if let Some(circle) = tiny_skia::PathBuilder::from_circle(gx, gy, 8.0 * s) {
            let st = Stroke {
                width: 2.2 * s,
                line_cap: tiny_skia::LineCap::Round,
                ..Default::default()
            };
            let c = with_alpha(150);
            pm.stroke_path(&circle, &paint(c), &st, Transform::identity(), None);
            let mut pb = tiny_skia::PathBuilder::new();
            pb.move_to(gx + 6.0 * s, gy + 6.0 * s);
            pb.line_to(gx + 11.5 * s, gy + 11.5 * s);
            if let Some(p) = pb.finish() {
                pm.stroke_path(&p, &paint(c), &st, Transform::identity(), None);
            }
        }

        let px = 21.0 * s;
        let baseline = fonts.baseline_centered(0.0, HEADER_H * s, px, Weight::Regular);
        let tx = 52.0 * s;

        // "Apps" scope chip on the right, like Spotlight's category filter
        let chip = if german { "Programme" } else { "Applications" };
        let cpx = 12.0 * s;
        let cw = fonts.measure(chip, cpx, Weight::Regular) + 22.0 * s;
        let ch = 24.0 * s;
        let cx = g.w * s - 18.0 * s - cw;
        let cy = (HEADER_H * s - ch) / 2.0;
        fill_rounded(pm, cx, cy, cw, ch, ch / 2.0, with_alpha(22));
        let cb = fonts.baseline_centered(cy, ch, cpx, Weight::Regular);
        fonts.draw(pm, chip, cx + 11.0 * s, cb, cpx, Weight::Regular, with_alpha(190));

        let max_w = cx - tx - 16.0 * s;
        if self.query.is_empty() {
            let ph = if german { "Apps durchsuchen" } else { "Search Apps" };
            fonts.draw(pm, ph, tx, baseline, px, Weight::Regular, with_alpha(105));
        } else {
            let shown = fonts.elide(&self.query, px, Weight::Regular, max_w);
            let w = fonts.draw(pm, &shown, tx, baseline, px, Weight::Regular, text);
            fill_rounded(pm, tx + w + 2.0 * s, baseline - 19.0 * s, 2.0 * s, 23.0 * s, 1.0, theme.menu_selection);
        }

        // hairline under the field
        fill_rounded(pm, 0.0, (HEADER_H * s).round() - s, g.w * s, s.max(1.0), 0.0, theme.menu_separator);
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_grid(&self, pm: &mut Pixmap, clip: &Mask, s: f32, g: &Geometry, apps: &Apps, fonts: &Fonts, theme: &Theme, german: bool) {
        let text = theme.menu_text;
        // grid area: below the hairline down to the panel's bottom edge
        let area_top = HEADER_H;
        let area_h = g.h - HEADER_H;
        let (aw, ah) = ((g.w * s).round() as u32, (area_h * s).round() as u32);
        let Some(mut area) = Pixmap::new(aw.max(1), ah.max(1)) else { return };

        if self.results.is_empty() {
            let msg = if german { "Keine Ergebnisse" } else { "No Results" };
            let px = 17.0 * s;
            let tw = fonts.measure(msg, px, Weight::Regular);
            let bl = fonts.baseline_centered(0.0, ah as f32, px, Weight::Regular);
            fonts.draw(&mut area, msg, (aw as f32 - tw) / 2.0, bl, px, Weight::Regular, [text[0], text[1], text[2], 120]);
        } else {
            let first_row = (self.scroll / CELL_H).floor().max(0.0) as usize;
            let last_row = ((self.scroll + g.view_h + GRID_PAD) / CELL_H).ceil() as usize;
            let lo = first_row * g.cols;
            let hi = ((last_row + 1) * g.cols).min(self.results.len());
            let img_px = (ICON * s).round() as u32;
            // local coordinates inside `area`
            let ox = -g.x;
            let oy = -(g.y + area_top);
            for i in lo..hi {
                let (x, y, size) = self.icon_rect(g, i);
                let (x, y) = (x + ox, y + oy);
                if y + size + LABEL_PX + 20.0 < 0.0 || y - 12.0 > area_h {
                    continue;
                }
                let e = &apps.entries[self.results[i]];
                let sel = self.selected == Some(i);
                let hov = self.hovered == Some(i) && self.selected.is_none();
                if sel || hov {
                    let pad = 12.0;
                    let c = if sel { [text[0], text[1], text[2], 34] } else { [text[0], text[1], text[2], 20] };
                    fill_rounded(
                        &mut area,
                        (x - pad - 6.0) * s,
                        (y - pad + 2.0) * s,
                        (size + 2.0 * pad + 12.0) * s,
                        (size + 2.0 * pad + LABEL_PX + 4.0) * s,
                        14.0 * s,
                        c,
                    );
                }
                let icon = e
                    .icon
                    .as_deref()
                    .and_then(|n| apps.icon(n, img_px))
                    .or_else(|| apps.icon("application-x-executable", img_px));
                if let Some(icon) = icon {
                    let k = img_px as f32 / icon.width() as f32;
                    let p = PixmapPaint {
                        quality: if icon.width() == img_px { FilterQuality::Nearest } else { FilterQuality::Bilinear },
                        ..Default::default()
                    };
                    let (ix, iy) = ((x * s).round(), (y * s).round());
                    area.draw_pixmap(0, 0, icon.as_ref().as_ref(), &p, Transform::from_row(k, 0.0, 0.0, k, ix, iy), None);
                    if self.pressed == Some(Hit::App(i)) {
                        fill_rounded(&mut area, ix, iy, size * s, size * s, size * 0.22 * s, [0, 0, 0, 70]);
                    }
                }
                let px = LABEL_PX * s;
                let label = fonts.elide(&e.name, px, Weight::Regular, (CELL_W - 12.0) * s);
                let tw = fonts.measure(&label, px, Weight::Regular);
                let lx = ((x + size / 2.0) * s - tw / 2.0).round();
                let ly = (y + size + 8.0) * s + px * 0.8;
                fonts.draw(&mut area, &label, lx, ly, px, Weight::Regular, text);
            }

            // thin scroll indicator while there is more than one screenful
            let max = self.max_scroll();
            if max > 0.0 {
                let track_top = GRID_PAD;
                let track_h = area_h - 2.0 * GRID_PAD - RADIUS * 0.4;
                let thumb_h = (track_h * g.view_h / self.content_h()).max(28.0);
                let ty = track_top + (track_h - thumb_h) * (self.scroll / max).clamp(0.0, 1.0);
                fill_rounded(&mut area, (g.w - 9.0) * s, ty * s, 4.0 * s, thumb_h * s, 2.0 * s, [text[0], text[1], text[2], 60]);
            }
        }

        pm.draw_pixmap(
            0,
            (area_top * s).round() as i32,
            area.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            // rounded bottom corners
            Some(clip),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pad(n: usize) -> Launchpad {
        let mut l = Launchpad::with((0..n).collect());
        l.open = 1.0;
        l.geometry(1920.0, 1080.0);
        l
    }

    #[test]
    fn panel_is_centered_and_fits() {
        for (w, h) in [(1280.0, 720.0), (1920.0, 1080.0), (3840.0, 2160.0), (1024.0, 600.0)] {
            let g = Geometry::new(w, h);
            assert!((g.x + g.w / 2.0 - w / 2.0).abs() <= 1.0, "centered on {w}x{h}");
            assert!(g.x >= 0.0 && g.x + g.w <= w);
            assert!(g.y >= 40.0 && g.y + g.h <= h, "fits on {w}x{h}: {g:?}");
        }
        assert_eq!(Geometry::new(1920.0, 1080.0).cols, 7);
    }

    #[test]
    fn selection_moves_through_grid_and_scrolls() {
        let mut l = pad(100);
        let cols = l.geo.cols;
        l.move_selection(1, 0);
        assert_eq!(l.selected, Some(0));
        l.move_selection(1, 0);
        assert_eq!(l.selected, Some(1));
        l.move_selection(-1, 0);
        l.move_selection(-1, 0);
        assert_eq!(l.selected, Some(0), "stops at the first app");
        l.move_selection(0, 1);
        assert_eq!(l.selected, Some(cols));
        l.select_edge(true);
        assert_eq!(l.selected, Some(99));
        assert!(l.scroll_target > 0.0, "scrolled to the last row");
        assert!((l.scroll_target - l.max_scroll()).abs() < 0.01);
        l.select_edge(false);
        assert_eq!(l.scroll_target, 0.0);
    }

    #[test]
    fn down_onto_short_last_row_picks_last_app() {
        let mut l = pad(9);
        let cols = l.geo.cols; // 7: rows of 7 + 2
        l.select(cols - 1);
        l.move_selection(0, 1);
        assert_eq!(l.selected, Some(8));
    }

    #[test]
    fn scrolling_is_clamped() {
        let mut l = pad(10);
        l.scroll_by(500.0);
        assert_eq!(l.scroll_target, 0.0, "everything fits, nothing to scroll");
        let mut l = pad(200);
        l.scroll_by(-100.0);
        assert_eq!(l.scroll_target, 0.0);
        l.scroll_by(1e6);
        assert!((l.scroll_target - l.max_scroll()).abs() < 0.01);
    }

    #[test]
    fn hit_finds_icons_field_and_outside() {
        let mut l = pad(10);
        let g = l.geometry(1920.0, 1080.0);
        let (x, y, s) = l.icon_rect(&g, 3);
        assert_eq!(l.hit(1920.0, 1080.0, x + s / 2.0, y + s / 2.0), Hit::App(3));
        assert_eq!(l.hit(1920.0, 1080.0, 960.0, g.y + 20.0), Hit::Field);
        assert_eq!(l.hit(1920.0, 1080.0, 10.0, 1000.0), Hit::Outside);
        // the empty part of the last row is still the panel
        let (x, y, s) = l.icon_rect(&g, 4 * g.cols + 2);
        assert_eq!(l.hit(1920.0, 1080.0, x + s / 2.0, y + s / 2.0), Hit::Panel);
    }
}

