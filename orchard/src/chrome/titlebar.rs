//! CPU painter for the macOS-style title bar (traffic lights + centered title).

use tiny_skia::{Color, FillRule, LineCap, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

use super::{
    font::title_font,
    theme::{
        palette, Appearance, Rgba, BUTTON_DIAMETER, BUTTON_FIRST_CENTER_X, BUTTON_SPACING,
        TITLEBAR_HEIGHT,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrafficLight {
    Close,
    Minimize,
    Zoom,
}

impl TrafficLight {
    pub const ALL: [TrafficLight; 3] = [TrafficLight::Close, TrafficLight::Minimize, TrafficLight::Zoom];

    fn index(self) -> usize {
        match self {
            TrafficLight::Close => 0,
            TrafficLight::Minimize => 1,
            TrafficLight::Zoom => 2,
        }
    }

    /// Center in logical coordinates relative to the title bar origin.
    pub fn center(self) -> (f32, f32) {
        (
            BUTTON_FIRST_CENTER_X + BUTTON_SPACING * self.index() as f32,
            TITLEBAR_HEIGHT as f32 / 2.0,
        )
    }

    /// Which button (if any) is under a logical point in title bar coordinates.
    pub fn hit(x: f64, y: f64) -> Option<TrafficLight> {
        TrafficLight::ALL.into_iter().find(|b| {
            let (cx, cy) = b.center();
            let (dx, dy) = (x as f32 - cx, y as f32 - cy);
            // a little more generous than the visible circle, like macOS
            dx * dx + dy * dy <= (BUTTON_DIAMETER / 2.0 + 2.0).powi(2)
        })
    }

    /// Whether a logical point lies in the traffic-light group (hovering it reveals glyphs).
    pub fn in_group(x: f64, y: f64) -> bool {
        let left = BUTTON_FIRST_CENTER_X - BUTTON_DIAMETER / 2.0 - 2.0;
        let right = TrafficLight::Zoom.center().0 + BUTTON_DIAMETER / 2.0 + 2.0;
        let top = TITLEBAR_HEIGHT as f32 / 2.0 - BUTTON_DIAMETER / 2.0 - 2.0;
        let bottom = TITLEBAR_HEIGHT as f32 / 2.0 + BUTTON_DIAMETER / 2.0 + 2.0;
        (x as f32) >= left && (x as f32) <= right && (y as f32) >= top && (y as f32) <= bottom
    }
}

/// Everything that influences the title bar pixels. Used as a cache key.
#[derive(Debug, Clone, PartialEq)]
pub struct TitlebarParams {
    pub width: i32,
    pub scale: f64,
    pub appearance: Appearance,
    pub radius: f32,
    pub active: bool,
    pub group_hovered: bool,
    pub pressed: Option<TrafficLight>,
    pub title: String,
    /// green leaves full screen (arrows) instead of maximizing (+)
    pub fullscreen: bool,
}

fn color(c: Rgba) -> Color {
    Color::from_rgba8(c[0], c[1], c[2], c[3])
}

fn paint(c: Rgba) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(color(c));
    p.anti_alias = true;
    p
}

fn darken(c: Rgba, f: f32) -> Rgba {
    [
        (c[0] as f32 * f) as u8,
        (c[1] as f32 * f) as u8,
        (c[2] as f32 * f) as u8,
        c[3],
    ]
}

/// Paint the title bar. Returns the pixmap (premultiplied RGBA) in physical pixels.
pub fn paint_titlebar(p: &TitlebarParams) -> Option<Pixmap> {
    let s = p.scale as f32;
    let w = ((p.width as f32) * s).ceil().max(1.0) as u32;
    let h = ((TITLEBAR_HEIGHT as f32) * s).ceil() as u32;
    let mut pm = Pixmap::new(w, h)?;
    let pal = palette(p.appearance);
    let ts = Transform::from_scale(s, s);
    let lw = p.width as f32;
    let lh = TITLEBAR_HEIGHT as f32;

    // background with rounded top corners
    let r = p.radius;
    let mut pb = PathBuilder::new();
    pb.move_to(0.0, lh);
    pb.line_to(0.0, r);
    pb.quad_to(0.0, 0.0, r, 0.0);
    pb.line_to(lw - r, 0.0);
    pb.quad_to(lw, 0.0, lw, r);
    pb.line_to(lw, lh);
    pb.close();
    let bg_path = pb.finish()?;
    let bg = if p.active {
        pal.titlebar_active
    } else {
        pal.titlebar_inactive
    };
    pm.fill_path(&bg_path, &paint(bg), FillRule::Winding, ts, None);

    // subtle top highlight / hairline outline along the rounded top edge
    let mut hl = Stroke::default();
    hl.width = 1.0 / s;
    pm.stroke_path(&bg_path, &paint(pal.hairline), &hl, ts, None);

    // bottom separator
    if let Some(rect) = Rect::from_xywh(0.0, lh - 1.0 / s, lw, 1.0 / s) {
        pm.fill_rect(rect, &paint(pal.separator), ts, None);
    }

    // traffic lights
    for b in TrafficLight::ALL {
        let (cx, cy) = b.center();
        let rad = BUTTON_DIAMETER / 2.0;
        let (mut fill, mut stroke) = if p.active || p.group_hovered {
            match b {
                TrafficLight::Close => pal.close,
                TrafficLight::Minimize => pal.minimize,
                TrafficLight::Zoom => pal.zoom,
            }
        } else {
            (pal.button_inactive_fill, pal.button_inactive_stroke)
        };
        if p.pressed == Some(b) {
            fill = darken(fill, 0.8);
            stroke = darken(stroke, 0.8);
        }
        if let Some(circle) = PathBuilder::from_circle(cx, cy, rad - 0.25) {
            pm.fill_path(&circle, &paint(fill), FillRule::Winding, ts, None);
            let mut st = Stroke::default();
            st.width = 0.5;
            pm.stroke_path(&circle, &paint(stroke), &st, ts, None);
        }

        if p.group_hovered {
            let mut st = Stroke::default();
            st.width = 1.1;
            st.line_cap = LineCap::Round;
            match b {
                TrafficLight::Close => {
                    let d = 2.6;
                    let mut g = PathBuilder::new();
                    g.move_to(cx - d, cy - d);
                    g.line_to(cx + d, cy + d);
                    g.move_to(cx + d, cy - d);
                    g.line_to(cx - d, cy + d);
                    if let Some(g) = g.finish() {
                        pm.stroke_path(&g, &paint(pal.close_glyph), &st, ts, None);
                    }
                }
                TrafficLight::Minimize => {
                    let mut g = PathBuilder::new();
                    g.move_to(cx - 3.2, cy);
                    g.line_to(cx + 3.2, cy);
                    if let Some(g) = g.finish() {
                        st.width = 1.3;
                        pm.stroke_path(&g, &paint(pal.minimize_glyph), &st, ts, None);
                    }
                }
                TrafficLight::Zoom if !p.fullscreen => {
                    // + (maximize)
                    let mut g = PathBuilder::new();
                    g.move_to(cx - 3.4, cy);
                    g.line_to(cx + 3.4, cy);
                    g.move_to(cx, cy - 3.4);
                    g.line_to(cx, cy + 3.4);
                    if let Some(g) = g.finish() {
                        st.width = 1.3;
                        pm.stroke_path(&g, &paint(pal.zoom_glyph), &st, ts, None);
                    }
                }
                TrafficLight::Zoom => {
                    // two opposing triangles (full screen)
                    let mut g = PathBuilder::new();
                    g.move_to(cx - 3.0, cy - 3.0);
                    g.line_to(cx + 1.4, cy - 3.0);
                    g.line_to(cx - 3.0, cy + 1.4);
                    g.close();
                    g.move_to(cx + 3.0, cy + 3.0);
                    g.line_to(cx - 1.4, cy + 3.0);
                    g.line_to(cx + 3.0, cy - 1.4);
                    g.close();
                    if let Some(g) = g.finish() {
                        pm.fill_path(&g, &paint(pal.zoom_glyph), FillRule::Winding, ts, None);
                    }
                }
            }
        }
    }

    // centered title
    if let Some(font) = title_font() {
        let px = 13.0 * s;
        let left_reserved = (TrafficLight::Zoom.center().0 + BUTTON_DIAMETER) * s;
        let max_w = (w as f32 - 2.0 * left_reserved).max(0.0);
        let title = font.ellipsize(&p.title, px, max_w);
        let tw = font.measure(&title, px);
        let x = ((w as f32 - tw) / 2.0).max(left_reserved);
        let asc = font.ascent(px);
        let desc = font.descent(px);
        let baseline = (h as f32 + asc + desc) / 2.0;
        let c = if p.active {
            pal.title_active
        } else {
            pal.title_inactive
        };
        font.draw(&mut pm, &title, x, baseline.round(), px, c);
    }

    Some(pm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_testing() {
        let (cx, cy) = TrafficLight::Close.center();
        assert_eq!(TrafficLight::hit(cx as f64, cy as f64), Some(TrafficLight::Close));
        let (cx, cy) = TrafficLight::Zoom.center();
        assert_eq!(TrafficLight::hit(cx as f64 + 3.0, cy as f64), Some(TrafficLight::Zoom));
        assert_eq!(TrafficLight::hit(200.0, 14.0), None);
        assert!(TrafficLight::in_group(30.0, 14.0));
        assert!(!TrafficLight::in_group(30.0, 27.0));
    }

    #[test]
    fn paints_at_fractional_scale() {
        let pm = paint_titlebar(&TitlebarParams {
            width: 640,
            scale: 1.25,
            appearance: Appearance::Light,
            radius: 10.0,
            active: true,
            group_hovered: true,
            pressed: None,
            title: "Konsole".into(),
            fullscreen: false,
        })
        .unwrap();
        assert_eq!(pm.width(), 800);
        assert_eq!(pm.height(), 35);
        // top-left corner pixel must be transparent (rounded corner)
        assert_eq!(pm.pixel(0, 0).unwrap().alpha(), 0);
        // middle of the bar is opaque
        assert_eq!(pm.pixel(400, 20).unwrap().alpha(), 255);
    }
}
