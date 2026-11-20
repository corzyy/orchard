//! Small vector illustrations drawn with iced's canvas: appearance
//! thumbnails, the display arrangement, the wallpaper-accent palette glyph
//! and the About window's device.

use iced::alignment::Horizontal;
use iced::widget::canvas::{self, Frame, Geometry, Path, Program, Stroke};
use iced::{mouse, Color, Element, Length, Pixels, Point, Rectangle, Renderer, Size, Theme};
use orchard_ui::palette::{from_rgb, mix, palette, with_alpha};

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rectangle {
    Rectangle::new(Point::new(x, y), Size::new(w.max(0.0), h.max(0.0)))
}

fn rrect(f: &mut Frame, r: Rectangle, radius: f32, color: Color) {
    let radius = radius.min(r.width * 0.5).min(r.height * 0.5).max(0.0);
    f.fill(&Path::rounded_rectangle(r.position(), r.size(), radius.into()), color);
}

fn stroke_rrect(f: &mut Frame, r: Rectangle, radius: f32, width: f32, color: Color) {
    let radius = radius.min(r.width * 0.5).min(r.height * 0.5).max(0.0);
    f.stroke(
        &Path::rounded_rectangle(r.position(), r.size(), radius.into()),
        Stroke::default().with_width(width).with_color(color),
    );
}

fn circle(f: &mut Frame, cx: f32, cy: f32, r: f32, color: Color) {
    f.fill(&Path::circle(Point::new(cx, cy), r), color);
}

fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
    Color::from_rgba8(r, g, b, a as f32 / 255.0)
}

/// Wraps a one-shot drawing closure as a canvas program.
struct Art<F>(F);

impl<M, F: Fn(&mut Frame, Rectangle)> Program<M> for Art<F> {
    type State = ();

    fn draw(&self, _: &(), renderer: &Renderer, _: &Theme, bounds: Rectangle, _: mouse::Cursor) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        (self.0)(&mut frame, Rectangle::new(Point::ORIGIN, bounds.size()));
        vec![frame.into_geometry()]
    }
}

fn art<'a, M: 'a>(w: impl Into<Length>, h: impl Into<Length>, f: impl Fn(&mut Frame, Rectangle) + 'a) -> Element<'a, M> {
    canvas::Canvas::new(Art(f)).width(w).height(h).into()
}

// ---------------------------------------------------------------- Mini desktop

/// A tiny desktop with a window: light (`Some(false)`), dark (`Some(true)`)
/// or split light/dark for Auto (`None`).
pub fn draw_mini_desktop(f: &mut Frame, r: Rectangle, dark: Option<bool>) {
    rrect(f, r, 6.0, from_rgb([0x4A, 0x7B, 0xD8]));
    rrect(f, rect(r.x, r.y + r.height * 0.5, r.width, r.height * 0.5), 4.0, from_rgb([0x8A, 0x5C, 0xC8]));
    let w = rect(r.x + r.width * 0.16, r.y + r.height * 0.2, r.width * 0.68, r.height * 0.62);
    match dark {
        Some(is_dark) => {
            let (body, bar) = if is_dark {
                (from_rgb([0x2A, 0x2A, 0x2C]), from_rgb([0x3A, 0x3A, 0x3C]))
            } else {
                (Color::WHITE, from_rgb([0xE8, 0xE6, 0xE8]))
            };
            rrect(f, w, 4.0, body);
            rrect(f, rect(w.x, w.y, w.width, 8.0), 3.0, bar);
            let line_c = if is_dark { rgba(255, 255, 255, 42) } else { rgba(0, 0, 0, 30) };
            for i in 0..3 {
                let y = w.y + 13.0 + i as f32 * 5.5;
                rrect(f, rect(w.x + 6.0, y, w.width * (0.7 - i as f32 * 0.15), 2.2), 1.0, line_c);
            }
        }
        None => {
            let half = w.width * 0.5;
            rrect(f, rect(w.x, w.y, half, w.height), 3.0, Color::WHITE);
            rrect(f, rect(w.x + half, w.y, half, w.height), 3.0, from_rgb([0x2A, 0x2A, 0x2C]));
            rrect(f, rect(w.x, w.y, half, 8.0), 2.0, from_rgb([0xE8, 0xE6, 0xE8]));
            rrect(f, rect(w.x + half, w.y, half, 8.0), 2.0, from_rgb([0x3A, 0x3A, 0x3C]));
            for i in 0..3 {
                let y = w.y + 13.0 + i as f32 * 5.5;
                rrect(f, rect(w.x + 6.0, y, (half - 8.0).max(4.0), 2.2), 1.0, rgba(0, 0, 0, 30));
                rrect(f, rect(w.x + half + 3.0, y, (half - 9.0).max(4.0), 2.2), 1.0, rgba(255, 255, 255, 42));
            }
        }
    }
    for (i, c) in [[0xFF, 0x5F, 0x57], [0xFE, 0xBC, 0x2E], [0x28, 0xC8, 0x40]].iter().enumerate() {
        circle(f, w.x + 5.0 + i as f32 * 5.0, w.y + 4.0, 1.5, from_rgb(*c));
    }
}

pub fn mini_desktop<'a, M: 'a>(dark: Option<bool>, w: f32, h: f32) -> Element<'a, M> {
    art(w, h, move |f, r| draw_mini_desktop(f, r, dark))
}

// ---------------------------------------------------------------- Wallpaper palette glyph

/// Artist's palette in a circle: the "accent from wallpaper" swatch.
pub fn palette_glyph<'a, M: 'a>(selected: bool, accent: Color) -> Element<'a, M> {
    let p = palette();
    art(20.0, 20.0, move |f, b| {
        let (cx, cy) = (b.width * 0.5, b.height * 0.5);
        let bg = if selected { accent } else { p.field };
        let border = if selected { mix(bg, Color::BLACK, 0.22) } else { p.control_border };
        circle(f, cx, cy, 8.5, if selected { bg } else { mix(p.window_bg, p.text, 0.06) });
        f.stroke(&Path::circle(Point::new(cx, cy), 8.5), Stroke::default().with_width(0.8).with_color(border));

        let r = 5.4;
        let pt = |nx: f32, ny: f32| Point::new(cx + nx * r, cy + ny * r);
        let blob = Path::new(|b| {
            b.move_to(pt(0.0, -1.0));
            b.bezier_curve_to(pt(-0.62, -1.0), pt(-1.0, -0.55), pt(-1.0, 0.05));
            b.bezier_curve_to(pt(-1.0, 0.72), pt(-0.46, 1.0), pt(0.10, 1.0));
            b.bezier_curve_to(pt(0.38, 1.0), pt(0.42, 0.72), pt(0.28, 0.52));
            b.bezier_curve_to(pt(0.18, 0.36), pt(0.34, 0.18), pt(0.58, 0.22));
            b.bezier_curve_to(pt(0.88, 0.26), pt(1.0, 0.04), pt(0.98, -0.22));
            b.bezier_curve_to(pt(0.92, -0.76), pt(0.50, -1.0), pt(0.0, -1.0));
            b.close();
        });
        f.fill(&blob, if selected { Color::WHITE } else { p.text_secondary });
        let dots = [
            (-0.50, 0.32, [0x00, 0x7A, 0xFF]),
            (-0.46, -0.28, [0x34, 0xC7, 0x59]),
            (0.04, -0.54, [0xFF, 0xCC, 0x00]),
            (0.48, -0.26, [0xFF, 0x3B, 0x30]),
        ];
        for (dx, dy, c) in dots {
            circle(f, cx + dx * r, cy + dy * r, 1.05, if selected { bg } else { from_rgb(c) });
        }
    })
}

// ---------------------------------------------------------------- Display arrangement

#[derive(Clone)]
pub struct Screen {
    pub name: String,
    pub w: i32,
    pub h: i32,
    pub primary: bool,
}

/// Side-by-side preview of the connected displays.
pub fn arrangement<'a, M: 'a>(screens: Vec<Screen>) -> Element<'a, M> {
    let p = palette();
    art(Length::Fill, 118.0, move |f, r| {
        let n = screens.len();
        let total_w: f32 = screens.iter().map(|o| o.w.max(1) as f32).sum::<f32>() + 20.0 * n.saturating_sub(1) as f32;
        let max_h = screens.iter().map(|o| o.h.max(1)).max().unwrap_or(1) as f32;
        let k = ((r.width - 48.0) / total_w).min((r.height - 36.0) / max_h);
        let mut x = r.width * 0.5 - total_w * k * 0.5;
        for o in &screens {
            let sw = o.w.max(1) as f32 * k;
            let sh = o.h.max(1) as f32 * k;
            let sr = rect(x, r.height * 0.5 - sh * 0.5 - 8.0, sw, sh);
            rrect(f, sr, 6.0, from_rgb([0x2C, 0x2C, 0x2E]));
            let inner = rect(sr.x + 3.5, sr.y + 3.5, sr.width - 7.0, sr.height - 7.0);
            draw_mini_desktop(f, inner, Some(p.dark));
            if o.primary && n > 1 {
                rrect(f, rect(inner.x, inner.y, inner.width, 4.5), 2.0, rgba(255, 255, 255, 210));
            }
            f.fill_text(canvas::Text {
                content: o.name.clone(),
                position: Point::new(sr.x + sr.width * 0.5, sr.y + sr.height + 6.0),
                color: p.text_secondary,
                size: Pixels(11.5),
                font: orchard_ui::fonts::text(),
                horizontal_alignment: Horizontal::Center,
                ..Default::default()
            });
            x += sw + 20.0 * k;
        }
    })
}

// ---------------------------------------------------------------- Device

/// Laptop or desktop monitor illustration tinted with the accent colour.
pub fn device<'a, M: 'a>(is_laptop: bool) -> Element<'a, M> {
    let p = palette();
    art(130.0, 96.0, move |f, b| {
        let (cx, cy) = (b.width * 0.5, b.height * 0.5 - 6.0);
        let body = if p.dark { from_rgb([0x6E, 0x6E, 0x73]) } else { from_rgb([0xC5, 0xC5, 0xCA]) };
        let hi = if p.dark { from_rgb([0x8E, 0x8E, 0x93]) } else { from_rgb([0xE4, 0xE4, 0xE9]) };
        let bezel = from_rgb([0x1C, 0x1C, 0x1E]);
        let screen_top = mix(p.accent, from_rgb([0x1D, 0x26, 0x3B]), 0.35);
        let screen_bot = mix(p.accent, from_rgb([0x0E, 0x11, 0x1A]), 0.68);

        let (dw, dh) = if is_laptop { (96.0, 60.0) } else { (104.0, 64.0) };
        let (x, y) = (cx - dw * 0.5, cy - dh * 0.5);
        rrect(f, rect(x, y + 2.0, dw, dh), 7.0, rgba(0, 0, 0, 35));
        rrect(f, rect(x, y, dw, dh), 7.0, hi);
        stroke_rrect(f, rect(x, y, dw, dh), 7.0, 1.0, body);
        rrect(f, rect(x + 2.0, y + 2.0, dw - 4.0, dh - 4.0), 5.0, bezel);
        rrect(f, rect(x + 5.0, y + 5.0, dw - 10.0, dh - 10.0), 3.0, screen_bot);
        rrect(f, rect(x + 5.0, y + 5.0, dw - 10.0, (dh - 10.0) * 0.55), 3.0, screen_top);
        circle(f, cx + 10.0, cy + 2.0, 18.0, with_alpha(p.accent, 0.35));
        if is_laptop {
            rrect(f, rect(cx - 58.0, y + dh + 1.0, 116.0, 7.0), 3.0, hi);
            stroke_rrect(f, rect(cx - 58.0, y + dh + 1.0, 116.0, 7.0), 3.0, 1.0, body);
        } else {
            rrect(f, rect(cx - 10.0, y + dh, 20.0, 12.0), 1.0, body);
            rrect(f, rect(cx - 22.0, y + dh + 11.0, 44.0, 4.0), 2.0, hi);
        }
    })
}

// ---------------------------------------------------------------- Tiling preview

/// A miniature 16:9 desktop that lays out a few windows the way the compositor
/// will with `t`: snapped halves / quarters in Floating mode, a dwindle spiral
/// in Tiling mode. Gaps and split ratio follow the settings live.
pub fn tiling_preview<'a, M: 'a>(t: &orchard_config::TilingSettings, tiling: bool) -> Element<'a, M> {
    use orchard_config::SplitDirection;
    let p = palette();
    let t = t.clamped();
    art(Length::Fill, 176.0, move |f, r| {
        // the screen, centred, 16:9, with a bezel
        let sh = (r.height - 16.0).min((r.width - 32.0) * 9.0 / 16.0);
        let sw = sh * 16.0 / 9.0;
        let sr = rect(r.x + (r.width - sw) * 0.5, r.y + 8.0, sw, sh);
        rrect(f, sr, 8.0, from_rgb([0x1C, 0x1C, 0x1E]));
        let screen = rect(sr.x + 3.0, sr.y + 3.0, sr.width - 6.0, sr.height - 6.0);
        draw_mini_desktop(f, screen, Some(p.dark));
        // menu bar strip
        let bar = (screen.height * 0.055).max(3.0);
        rrect(f, rect(screen.x, screen.y, screen.width, bar), 1.5, rgba(0, 0, 0, 90));

        // logical 1920 x 1080 desktop -> preview pixels
        let k = screen.width / 1920.0;
        let area = rect(screen.x, screen.y + bar, screen.width, screen.height - bar);
        let (og, ig) = (t.outer_gap as f32 * k, t.inner_gap as f32 * k);
        let inner = rect(area.x + og, area.y + og, area.width - 2.0 * og, area.height - 2.0 * og);

        // split `a` in two along `horizontal` (side by side) at `ratio`
        let split = |a: Rectangle, horizontal: bool, ratio: f32| -> (Rectangle, Rectangle) {
            if horizontal {
                let avail = (a.width - ig).max(2.0);
                let w1 = avail * ratio;
                (rect(a.x, a.y, w1, a.height), rect(a.x + w1 + ig, a.y, avail - w1, a.height))
            } else {
                let avail = (a.height - ig).max(2.0);
                let h1 = avail * ratio;
                (rect(a.x, a.y, a.width, h1), rect(a.x, a.y + h1 + ig, a.width, avail - h1))
            }
        };

        let wins: Vec<Rectangle> = if tiling {
            let dir = |a: Rectangle| match t.split_direction {
                SplitDirection::Auto => a.width >= a.height,
                SplitDirection::Horizontal => true,
                SplitDirection::Vertical => false,
            };
            let (a, rest) = split(inner, dir(inner), t.split_ratio);
            let (b, c) = split(rest, dir(rest), t.split_ratio);
            vec![a, b, c]
        } else {
            // snapped: left half + two right quarters
            let (l, rt) = split(inner, true, 0.5);
            let (tr, br) = split(rt, false, 0.5);
            if t.snap_to_edges {
                if t.snap_corners {
                    vec![l, tr, br]
                } else {
                    vec![l, rt]
                }
            } else {
                // nothing snaps: free-floating windows
                vec![
                    rect(area.x + area.width * 0.08, area.y + area.height * 0.14, area.width * 0.5, area.height * 0.58),
                    rect(area.x + area.width * 0.38, area.y + area.height * 0.32, area.width * 0.52, area.height * 0.58),
                ]
            }
        };

        for (i, w) in wins.iter().enumerate() {
            let radius = (w.height * 0.07).clamp(1.5, 5.0);
            rrect(f, rect(w.x, w.y + 1.0, w.width, w.height), radius, rgba(0, 0, 0, 55));
            rrect(f, *w, radius, p.window_bg);
            let title = (w.height * 0.14).clamp(3.0, 9.0);
            rrect(f, rect(w.x, w.y, w.width, title), radius, mix(p.window_bg, p.text, 0.08));
            if title >= 5.0 {
                for d in 0..3 {
                    let c = [[0xFF, 0x5F, 0x57], [0xFE, 0xBC, 0x2E], [0x28, 0xC8, 0x40]][d];
                    circle(f, w.x + 3.5 + d as f32 * 4.0, w.y + title * 0.5, 1.2, from_rgb(c));
                }
            }
            // the first window is the focused one
            if i == 0 {
                stroke_rrect(f, *w, radius, 1.2, with_alpha(p.accent, 0.9));
            } else {
                stroke_rrect(f, *w, radius, 0.6, p.control_border);
            }
        }
    })
}
