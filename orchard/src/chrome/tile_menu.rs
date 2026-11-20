//! macOS Sequoia-style "Move & Resize" popover when hovering the green traffic light.

use std::time::{Duration, Instant};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::element::{
            memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
            Id, Kind,
        },
        renderer::gles::GlesRenderer,
    },
    desktop::space::SpaceElement,
    reexports::calloop::timer::{TimeoutAction, Timer},
    utils::{Logical, Physical, Point, Rectangle},
    utils::Transform,
};
use tiny_skia::{Color, FillRule, Paint, Pixmap, Stroke, Transform as SkTransform};

use crate::{
    chrome::{
        font::title_font,
        theme::{self, Appearance, Rgba},
        titlebar::TrafficLight,
    },
    shell::WindowElement,
    state::{Backend, OrchardState},
    tiling::Tile,
};

const WIDTH: f32 = 200.0;
const PAD_X: f32 = 8.0;
const PAD_Y: f32 = 8.0;
const HEADER_H: f32 = 20.0;
const TILE_ROW_H: f32 = 34.0;
const SEP_H: f32 = 9.0;
const ROW_H: f32 = 26.0;
const RADIUS: f32 = 10.0;
const HOVER_DELAY: Duration = Duration::from_millis(350);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TileMenuItem {
    Tile(Tile),
    Center,
    Fullscreen,
    MoveToOutput(String),
}

#[derive(Debug, Clone, PartialEq)]
struct RenderKey {
    appearance: Appearance,
    scale_milli: u32,
    hovered: Option<TileMenuItem>,
    outputs: Vec<String>,
    accent: [u8; 3],
    german: bool,
}

pub struct TileMenu {
    pub window: WindowElement,
    /// Top-left of the popover in global logical coordinates.
    pub origin: Point<f64, Logical>,
    /// Global logical rect of the green button (kept as a hover bridge).
    pub green_rect: Rectangle<f64, Logical>,
    pub hovered: Option<TileMenuItem>,
    pub outputs: Vec<String>,
    pub backdrop_id: Id,
    cache: Option<(RenderKey, MemoryRenderBuffer)>,
}

/// Pending hover arm before the popover opens.
pub struct TileMenuArm {
    pub window: WindowElement,
    pub started: Instant,
}

fn is_german() -> bool {
    std::env::var("LANG").map(|l| l.starts_with("de")).unwrap_or(false)
}

impl TileMenu {
    pub fn size(&self) -> (f64, f64) {
        let mut h = PAD_Y + HEADER_H + 4.0 + TILE_ROW_H + SEP_H + ROW_H * 3.0 + PAD_Y;
        if !self.outputs.is_empty() {
            h += SEP_H + ROW_H * self.outputs.len() as f32;
        }
        (WIDTH as f64, h as f64)
    }

    pub fn rect(&self) -> Rectangle<f64, Logical> {
        let (w, h) = self.size();
        Rectangle::new(self.origin, (w, h).into())
    }

    /// True if `pos` (global logical) is inside the popover, the green button,
    /// or the narrow vertical bridge between them.
    pub fn contains_or_bridge(&self, pos: Point<f64, Logical>) -> bool {
        let r = self.rect();
        if pos.x >= r.loc.x - 4.0
            && pos.x <= r.loc.x + r.size.w + 4.0
            && pos.y >= r.loc.y - 4.0
            && pos.y <= r.loc.y + r.size.h + 4.0
        {
            return true;
        }
        let g = self.green_rect;
        // Bridge corridor between green button and popover top
        let bx0 = g.loc.x - 8.0;
        let bx1 = g.loc.x + g.size.w + 24.0;
        let by0 = g.loc.y - 4.0;
        let by1 = r.loc.y + 4.0;
        pos.x >= bx0 && pos.x <= bx1 && pos.y >= by0 && pos.y <= by1
    }

    /// Hit-test an item at global logical `pos`.
    pub fn hit(&self, pos: Point<f64, Logical>) -> Option<TileMenuItem> {
        let lx = (pos.x - self.origin.x) as f32;
        let ly = (pos.y - self.origin.y) as f32;
        let (w, h) = self.size();
        if lx < 0.0 || lx > w as f32 || ly < 0.0 || ly > h as f32 {
            return None;
        }
        let tile_y = PAD_Y + HEADER_H + 4.0;
        if ly >= tile_y && ly <= tile_y + TILE_ROW_H && lx >= PAD_X && lx <= WIDTH - PAD_X {
            let cell_w = (WIDTH - 2.0 * PAD_X) / 4.0;
            let idx = ((lx - PAD_X) / cell_w).floor().clamp(0.0, 3.0) as usize;
            return Some(match idx {
                0 => TileMenuItem::Tile(Tile::Left),
                1 => TileMenuItem::Tile(Tile::Right),
                2 => TileMenuItem::Tile(Tile::Top),
                _ => TileMenuItem::Tile(Tile::Bottom),
            });
        }
        let list_y = tile_y + TILE_ROW_H + SEP_H;
        let fixed = [
            TileMenuItem::Tile(Tile::Fill),
            TileMenuItem::Center,
            TileMenuItem::Fullscreen,
        ];
        if ly >= list_y && ly < list_y + ROW_H * fixed.len() as f32 {
            let idx = ((ly - list_y) / ROW_H).floor() as usize;
            return fixed.get(idx).cloned();
        }
        if !self.outputs.is_empty() {
            let out_y = list_y + ROW_H * fixed.len() as f32 + SEP_H;
            if ly >= out_y && ly < out_y + ROW_H * self.outputs.len() as f32 {
                let idx = ((ly - out_y) / ROW_H).floor() as usize;
                return self.outputs.get(idx).cloned().map(TileMenuItem::MoveToOutput);
            }
        }
        None
    }

    pub fn render_element(
        &mut self,
        renderer: &mut GlesRenderer,
        output_geo: Rectangle<i32, Logical>,
        scale: f64,
    ) -> Option<(MemoryRenderBufferRenderElement<GlesRenderer>, Rectangle<i32, Physical>, f32)> {
        let r = self.rect();
        let local = Rectangle::<f64, Logical>::new(
            (r.loc.x - output_geo.loc.x as f64, r.loc.y - output_geo.loc.y as f64).into(),
            r.size,
        );
        if local.loc.x + local.size.w <= 0.0
            || local.loc.y + local.size.h <= 0.0
            || local.loc.x >= output_geo.size.w as f64
            || local.loc.y >= output_geo.size.h as f64
        {
            return None;
        }
        let key = RenderKey {
            appearance: theme::appearance(),
            scale_milli: (scale * 1000.0).round() as u32,
            hovered: self.hovered.clone(),
            outputs: self.outputs.clone(),
            accent: crate::settings::get().appearance.accent_rgb(),
            german: is_german(),
        };
        let fresh = !matches!(&self.cache, Some((k, _)) if *k == key);
        if fresh {
            let pm = paint_popover(&key, self.size())?;
            let buf = MemoryRenderBuffer::from_slice(
                pm.data(),
                Fourcc::Abgr8888,
                (pm.width() as i32, pm.height() as i32),
                1,
                Transform::Normal,
                None,
            );
            self.cache = Some((key, buf));
        }
        let buf = &self.cache.as_ref()?.1;
        let phys = local.to_physical_precise_round(scale);
        let el = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            phys.loc.to_f64(),
            buf,
            None,
            None,
            Some((r.size.w.round() as i32, r.size.h.round() as i32).into()),
            Kind::Unspecified,
        )
        .ok()?;
        Some((el, phys, (RADIUS as f64 * scale) as f32))
    }
}

fn paint_rgba(c: Rgba) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(Color::from_rgba8(c[0], c[1], c[2], c[3]));
    p.anti_alias = true;
    p
}

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    super::squircle_rect_path(x, y, w, h, r)
}

fn paint_popover(key: &RenderKey, size: (f64, f64)) -> Option<Pixmap> {
    let s = key.scale_milli as f32 / 1000.0;
    let lw = size.0 as f32;
    let lh = size.1 as f32;
    let pw = (lw * s).ceil().max(1.0) as u32;
    let ph = (lh * s).ceil().max(1.0) as u32;
    let mut pm = Pixmap::new(pw, ph)?;
    let ts = SkTransform::from_scale(s, s);
    let dark = matches!(key.appearance, Appearance::Dark);
    let bg: Rgba = if dark { [38, 38, 42, 205] } else { [248, 248, 252, 215] };
    let border: Rgba = if dark { [255, 255, 255, 42] } else { [0, 0, 0, 32] };
    let text: Rgba = if dark { [242, 242, 247, 255] } else { [28, 28, 30, 255] };
    let muted: Rgba = if dark { [168, 168, 176, 255] } else { [110, 110, 118, 255] };
    let sep: Rgba = if dark { [255, 255, 255, 26] } else { [0, 0, 0, 20] };
    let accent: Rgba = [key.accent[0], key.accent[1], key.accent[2], 235];
    let tile_bg: Rgba = if dark { [255, 255, 255, 20] } else { [0, 0, 0, 14] };
    let tile_fg: Rgba = if dark { [230, 230, 236, 215] } else { [60, 60, 66, 205] };

    if let Some(p) = rounded_rect(0.5, 0.5, lw - 1.0, lh - 1.0, RADIUS) {
        pm.fill_path(&p, &paint_rgba(bg), FillRule::Winding, ts, None);
        let mut st = Stroke::default();
        st.width = 1.0;
        pm.stroke_path(&p, &paint_rgba(border), &st, ts, None);
    }

    let font = title_font();
    let header = if key.german { "Bewegen & Größe" } else { "Move & Resize" };
    if let Some(f) = font {
        let px = 11.0 * s;
        f.draw(&mut pm, header, (PAD_X + 4.0) * s, (PAD_Y + 13.0) * s, px, muted);
    }

    // 4 quick-tile icons: Left, Right, Top, Bottom
    let tile_y = PAD_Y + HEADER_H + 4.0;
    let cell_w = (WIDTH - 2.0 * PAD_X) / 4.0;
    let tiles = [Tile::Left, Tile::Right, Tile::Top, Tile::Bottom];
    for (i, tile) in tiles.into_iter().enumerate() {
        let cx = PAD_X + i as f32 * cell_w;
        let is_hov = key.hovered == Some(TileMenuItem::Tile(tile));
        if is_hov {
            if let Some(p) = rounded_rect(cx + 2.0, tile_y + 1.0, cell_w - 4.0, TILE_ROW_H - 2.0, 6.0) {
                pm.fill_path(&p, &paint_rgba(accent), FillRule::Winding, ts, None);
            }
        }
        // Mini screen outline
        let mw = 30.0;
        let mh = 20.0;
        let mx = cx + (cell_w - mw) * 0.5;
        let my = tile_y + (TILE_ROW_H - mh) * 0.5;
        if let Some(p) = rounded_rect(mx, my, mw, mh, 3.5) {
            pm.fill_path(&p, &paint_rgba(if is_hov { [255, 255, 255, 50] } else { tile_bg }), FillRule::Winding, ts, None);
            let mut st = Stroke::default();
            st.width = 1.1;
            pm.stroke_path(&p, &paint_rgba(if is_hov { [255, 255, 255, 230] } else { tile_fg }), &st, ts, None);
        }
        // Active half fill inside mini screen
        let pad = 2.5;
        let (fx, fy, fw, fh) = match tile {
            Tile::Left => (mx + pad, my + pad, mw * 0.5 - pad * 1.3, mh - pad * 2.0),
            Tile::Right => (mx + mw * 0.5 + pad * 0.3, my + pad, mw * 0.5 - pad * 1.3, mh - pad * 2.0),
            Tile::Top => (mx + pad, my + pad, mw - pad * 2.0, mh * 0.5 - pad * 1.3),
            _ => (mx + pad, my + mh * 0.5 + pad * 0.3, mw - pad * 2.0, mh * 0.5 - pad * 1.3),
        };
        if let Some(p) = rounded_rect(fx, fy, fw, fh, 1.8) {
            pm.fill_path(&p, &paint_rgba(if is_hov { [255, 255, 255, 240] } else { tile_fg }), FillRule::Winding, ts, None);
        }
    }

    // Separator
    let sep_y = tile_y + TILE_ROW_H + SEP_H * 0.5;
    if let Some(rect) = tiny_skia::Rect::from_xywh(PAD_X + 4.0, sep_y, lw - 2.0 * (PAD_X + 4.0), 1.0 / s) {
        pm.fill_rect(rect, &paint_rgba(sep), ts, None);
    }

    let mut y = tile_y + TILE_ROW_H + SEP_H;
    let rows: [(&str, TileMenuItem); 3] = [
        (if key.german { "Ausfüllen" } else { "Fill" }, TileMenuItem::Tile(Tile::Fill)),
        (if key.german { "Zentrieren" } else { "Center" }, TileMenuItem::Center),
        (if key.german { "Vollbild" } else { "Enter Full Screen" }, TileMenuItem::Fullscreen),
    ];
    for (label, item) in rows {
        draw_row(&mut pm, font, s, ts, y, label, key.hovered.as_ref() == Some(&item), accent, text);
        y += ROW_H;
    }

    if !key.outputs.is_empty() {
        let sep2_y = y + SEP_H * 0.5;
        if let Some(rect) = tiny_skia::Rect::from_xywh(PAD_X + 4.0, sep2_y, lw - 2.0 * (PAD_X + 4.0), 1.0 / s) {
            pm.fill_rect(rect, &paint_rgba(sep), ts, None);
        }
        y += SEP_H;
        for out in &key.outputs {
            let item = TileMenuItem::MoveToOutput(out.clone());
            let label = if key.german {
                format!("Auf {out} bewegen")
            } else {
                format!("Move to {out}")
            };
            draw_row(&mut pm, font, s, ts, y, &label, key.hovered.as_ref() == Some(&item), accent, text);
            y += ROW_H;
        }
    }

    Some(pm)
}

#[allow(clippy::too_many_arguments)]
fn draw_row(
    pm: &mut Pixmap,
    font: Option<&crate::chrome::font::TextRenderer>,
    s: f32,
    ts: SkTransform,
    y: f32,
    label: &str,
    hovered: bool,
    accent: Rgba,
    text: Rgba,
) {
    if hovered {
        if let Some(p) = rounded_rect(PAD_X, y + 1.0, WIDTH - 2.0 * PAD_X, ROW_H - 2.0, 5.0) {
            pm.fill_path(&p, &paint_rgba(accent), FillRule::Winding, ts, None);
        }
    }
    if let Some(f) = font {
        let px = 12.5 * s;
        let max_w = (WIDTH - 2.0 * PAD_X - 16.0) * s;
        let txt = f.ellipsize(label, px, max_w);
        let c = if hovered { [255, 255, 255, 255] } else { text };
        f.draw(pm, &txt, (PAD_X + 8.0) * s, (y + 17.5) * s, px, c);
    }
}

impl<B: Backend + 'static> OrchardState<B> {
    /// Arm (or disarm) the 350ms hover timer on the green traffic light.
    pub fn tile_menu_hover(&mut self, window: &WindowElement, on_zoom: bool) {
        if !on_zoom {
            if self.fx.tile_menu_arm.as_ref().map(|a| a.window == *window).unwrap_or(false) {
                self.fx.tile_menu_arm = None;
            }
            return;
        }
        if self.fx.tile_menu.as_ref().map(|m| m.window == *window).unwrap_or(false) {
            return;
        }
        if self.fx.tile_menu_arm.as_ref().map(|a| a.window == *window).unwrap_or(false) {
            return;
        }
        let started = Instant::now();
        self.fx.tile_menu_arm = Some(TileMenuArm {
            window: window.clone(),
            started,
        });
        let win = window.clone();
        let _ = self.handle.insert_source(Timer::from_duration(HOVER_DELAY), move |_, _, state| {
            let armed = state
                .fx
                .tile_menu_arm
                .as_ref()
                .map(|a| a.window == win && a.started == started)
                .unwrap_or(false);
            if armed {
                state.fx.tile_menu_arm = None;
                state.open_tile_menu(&win);
            }
            TimeoutAction::Drop
        });
    }

    pub fn open_tile_menu(&mut self, window: &WindowElement) {
        let Some(win_loc) = self.space.element_location(window) else { return };
        if !window.is_ssd() {
            return;
        }
        // Only open if pointer is still over the green button
        let pos = self.pointer.current_location();
        let rel = pos - win_loc.to_f64();
        if TrafficLight::hit(rel.x, rel.y) != Some(TrafficLight::Zoom) {
            return;
        }
        let (gx, gy) = TrafficLight::Zoom.center();
        let green_rect = Rectangle::new(
            (win_loc.x as f64 + gx as f64 - 8.0, win_loc.y as f64 + gy as f64 - 8.0).into(),
            (16.0, 16.0).into(),
        );
        // Other outputs for "Move to <Display>"
        let cur_out = self
            .space
            .output_under(pos)
            .next()
            .map(|o| o.name())
            .unwrap_or_default();
        let outputs: Vec<String> = self
            .space
            .outputs()
            .map(|o| o.name())
            .filter(|n| *n != cur_out)
            .collect();
        let mut origin = Point::from((win_loc.x as f64 + gx as f64 - 24.0, win_loc.y as f64 + 32.0));
        // Keep inside the current output bounds
        if let Some(out) = self.space.output_under(pos).next().cloned() {
            if let Some(og) = self.space.output_geometry(&out) {
                let menu_tmp = TileMenu {
                    window: window.clone(),
                    origin,
                    green_rect,
                    hovered: None,
                    outputs: outputs.clone(),
                    backdrop_id: Id::new(),
                    cache: None,
                };
                let (mw, mh) = menu_tmp.size();
                origin.x = origin.x.clamp(og.loc.x as f64 + 4.0, (og.loc.x + og.size.w) as f64 - mw - 4.0);
                origin.y = origin.y.clamp(og.loc.y as f64 + 4.0, (og.loc.y + og.size.h) as f64 - mh - 4.0);
            }
        }
        self.fx.tile_menu = Some(TileMenu {
            window: window.clone(),
            origin,
            green_rect,
            hovered: None,
            outputs,
            backdrop_id: Id::new(),
            cache: None,
        });
    }

    pub fn close_tile_menu(&mut self) {
        self.fx.tile_menu = None;
        self.fx.tile_menu_arm = None;
    }

    /// Update hover state when the pointer moves; returns true if the pointer is
    /// over the open popover (caller should not forward motion to windows below).
    pub fn tile_menu_motion(&mut self, pos: Point<f64, Logical>) -> bool {
        let Some(menu) = self.fx.tile_menu.as_mut() else {
            return false;
        };
        if !menu.contains_or_bridge(pos) {
            self.fx.tile_menu = None;
            return false;
        }
        menu.hovered = menu.hit(pos);
        let r = menu.rect();
        pos.x >= r.loc.x && pos.x <= r.loc.x + r.size.w && pos.y >= r.loc.y && pos.y <= r.loc.y + r.size.h
    }

    /// Handle a pointer button while the popover is open; returns true if consumed.
    pub fn tile_menu_button(&mut self, button: u32, pressed: bool) -> bool {
        let Some(menu) = self.fx.tile_menu.as_ref() else {
            return false;
        };
        let pos = self.pointer.current_location();
        let hit = menu.hit(pos);
        let in_menu = {
            let r = menu.rect();
            pos.x >= r.loc.x && pos.x <= r.loc.x + r.size.w && pos.y >= r.loc.y && pos.y <= r.loc.y + r.size.h
        };
        if !in_menu {
            if pressed {
                self.close_tile_menu();
            }
            return false;
        }
        if button == 0x110 && !pressed {
            let window = menu.window.clone();
            self.close_tile_menu();
            if let Some(item) = hit {
                match item {
                    TileMenuItem::Tile(t) => self.tile_window(&window, t),
                    TileMenuItem::Center => self.center_window(&window),
                    TileMenuItem::Fullscreen => self.toggle_fullscreen(&window),
                    TileMenuItem::MoveToOutput(out_name) => {
                        let og = self
                            .space
                            .outputs()
                            .find(|o| o.name() == out_name)
                            .and_then(|o| self.space.output_geometry(o));
                        if let Some(og) = og {
                            let geo = window.geometry();
                            let x = og.loc.x + (og.size.w - geo.size.w).max(0) / 2;
                            let y = og.loc.y + (og.size.h - geo.size.h).max(0) / 2;
                            self.space.map_element(window.clone(), (x, y), true);
                            self.focus_topmost();
                        }
                    }
                }
            }
        }
        true
    }
}
