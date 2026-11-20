//! CPU painter for the macOS-style title bar (traffic lights + centered title).

use tiny_skia::{
    Color, FillRule, GradientStop, LineCap, LinearGradient, Paint, PathBuilder, Pixmap, Point,
    Rect, SpreadMode, Stroke, Transform,
};

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
    /// Option/Alt is held: live-swap the green + glyph to fullscreen arrows
    pub alt_held: bool,
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
    let w = ((p.width as f32) * s).round().max(1.0) as u32;
    let h = ((TITLEBAR_HEIGHT as f32) * s).round().max(1.0) as u32;
    let mut pm = Pixmap::new(w, h)?;
    let pal = palette(p.appearance);
    let ts = Transform::from_scale(s, s);
    let lw = w as f32 / s;
    let lh = h as f32 / s;
    let px_unit = 1.0 / s;

    // Background with continuous superellipse top corners matching rounded.frag
    let r = p.radius.clamp(0.0, (lw * 0.5).min(lh));
    let bg_path = super::squircle_top_path(0.0, 0.0, lw, lh, r, true)?;
    if p.active {
        let mut bg_paint = Paint::default();
        bg_paint.anti_alias = true;
        if let Some(grad) = LinearGradient::new(
            Point::from_xy(0.0, 0.0),
            Point::from_xy(0.0, lh),
            vec![
                GradientStop::new(0.0, color(pal.titlebar_active_top)),
                GradientStop::new(1.0, color(pal.titlebar_active_bottom)),
            ],
            SpreadMode::Pad,
            Transform::identity(),
        ) {
            bg_paint.shader = grad;
        } else {
            bg_paint.set_color(color(pal.titlebar_active_top));
        }
        pm.fill_path(&bg_path, &bg_paint, FillRule::Winding, ts, None);
    } else {
        pm.fill_path(&bg_path, &paint(pal.titlebar_inactive), FillRule::Winding, ts, None);
    }

    // Subtle 1px bottom separator between titlebar and client area
    let sep = if p.active {
        pal.separator_active
    } else {
        pal.separator_inactive
    };
    if let Some(rect) = Rect::from_xywh(0.0, lh - px_unit, lw, px_unit) {
        pm.fill_rect(rect, &paint(sep), ts, None);
    }

    // Crisp 1px inner hairline rim along left, top-left, top, top-right, right
    // (open at the bottom so it seamlessly meets RoundedElement's rim without
    // drawing a dark bar across the bottom seam).
    if !p.fullscreen {
        let inset = 0.5 * px_unit;
        if let Some(rim_path) =
            super::squircle_top_path(inset, inset, lw - inset, lh, (r - inset).max(0.0), false)
        {
            let rim = if p.active {
                pal.rim_active
            } else {
                pal.rim_inactive
            };
            let mut st = Stroke::default();
            st.width = px_unit;
            pm.stroke_path(&rim_path, &paint(rim), &st, ts, None);
        }

        // Subtle inner top specular highlight on active light titlebars
        if p.active && pal.top_highlight[3] > 0 {
            let hi_inset = 1.5 * px_unit;
            let hi_r = (r - hi_inset).max(0.0);
            if lw > 2.0 * hi_inset + 2.0 * hi_r {
                let mut pb = PathBuilder::new();
                pb.move_to(hi_inset + hi_r, hi_inset);
                pb.line_to(lw - hi_inset - hi_r, hi_inset);
                if let Some(hi_path) = pb.finish() {
                    let mut st = Stroke::default();
                    st.width = px_unit;
                    pm.stroke_path(&hi_path, &paint(pal.top_highlight), &st, ts, None);
                }
            }
        }
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
                TrafficLight::Zoom if !(p.fullscreen || p.alt_held) => {
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
            alt_held: false,
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
