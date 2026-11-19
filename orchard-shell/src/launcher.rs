//! Spotlight-like launcher: type to search applications, Enter to open.

use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Stroke, Transform};

use crate::{
    apps::{Apps, DesktopEntry},
    draw::{fill_rounded, paint},
    text::{Fonts, Weight},
    theme::Theme,
};

pub const WIDTH: f32 = 680.0;
const FIELD_H: f32 = 54.0;
const ROW_H: f32 = 40.0;
const MAX_ROWS: usize = 8;

#[derive(Default)]
pub struct Launcher {
    pub query: String,
    pub selected: usize,
    pub results: Vec<usize>,
}

/// Simple ranking: prefix match > word-prefix match > substring > subsequence.
pub fn score(name: &str, query: &str) -> Option<u32> {
    let n = name.to_lowercase();
    let q = query.to_lowercase();
    if q.is_empty() {
        return None;
    }
    if n.starts_with(&q) {
        return Some(400 - n.len().min(100) as u32);
    }
    if n.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(&q)) {
        return Some(300 - n.len().min(100) as u32);
    }
    if n.contains(&q) {
        return Some(200 - n.len().min(100) as u32);
    }
    let mut it = n.chars();
    if q.chars().all(|c| it.any(|x| x == c)) {
        return Some(100 - n.len().min(99) as u32);
    }
    None
}

impl Launcher {
    pub fn update(&mut self, apps: &Apps) {
        let mut scored: Vec<(u32, usize)> = apps
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.no_display && e.exec.is_some())
            .filter_map(|(i, e)| {
                let s = score(&e.name, &self.query).or_else(|| score(&e.id, &self.query).map(|s| s / 2))?;
                Some((s, i))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(apps.entries[a.1].name.cmp(&apps.entries[b.1].name)));
        self.results = scored.into_iter().take(MAX_ROWS).map(|(_, i)| i).collect();
        self.selected = 0;
    }

    pub fn selected_entry<'a>(&self, apps: &'a Apps) -> Option<&'a DesktopEntry> {
        self.results.get(self.selected).map(|i| &apps.entries[*i])
    }

    pub fn height(&self) -> f32 {
        FIELD_H + if self.results.is_empty() { 0.0 } else { 8.0 + self.results.len() as f32 * ROW_H + 8.0 }
    }

    pub fn row_at(&self, y: f32) -> Option<usize> {
        if y < FIELD_H + 8.0 {
            return None;
        }
        let i = ((y - FIELD_H - 8.0) / ROW_H) as usize;
        (i < self.results.len()).then_some(i)
    }

    /// Paint at (x, y) in a surface; sizes in logical px.
    #[allow(clippy::too_many_arguments)]
    pub fn paint(&self, pm: &mut Pixmap, x: f32, y: f32, scale: f32, apps: &Apps, fonts: &Fonts, theme: &Theme, german: bool) {
        let s = scale;
        let h = self.height();
        for (i, a) in [(10.0, 10u8), (4.0, 18), (1.0, 28)] {
            fill_rounded(pm, (x - i) * s, (y - i + 6.0) * s, (WIDTH + 2.0 * i) * s, (h + 2.0 * i) * s, (14.0 + i) * s, [0, 0, 0, a]);
        }
        fill_rounded(pm, x * s, y * s, WIDTH * s, h * s, 14.0 * s, theme.menu_bg);

        // magnifier glyph
        let gx = (x + 26.0) * s;
        let gy = (y + FIELD_H / 2.0 - 2.0) * s;
        if let Some(circle) = tiny_skia::PathBuilder::from_circle(gx, gy, 8.0 * s) {
            let st = Stroke {
                width: 2.2 * s,
                ..Default::default()
            };
            let mut c = theme.menu_text;
            c[3] = 160;
            pm.stroke_path(&circle, &paint(c), &st, Transform::identity(), None);
            let mut pb = tiny_skia::PathBuilder::new();
            pb.move_to(gx + 6.0 * s, gy + 6.0 * s);
            pb.line_to(gx + 12.0 * s, gy + 12.0 * s);
            if let Some(p) = pb.finish() {
                pm.stroke_path(&p, &paint(c), &st, Transform::identity(), None);
            }
        }

        let px = 22.0 * s;
        let baseline = fonts.baseline_centered(y * s, FIELD_H * s, px, Weight::Regular);
        let tx = (x + 50.0) * s;
        if self.query.is_empty() {
            let mut c = theme.menu_text;
            c[3] = 100;
            let ph = if german { "Spotlight-Suche" } else { "Spotlight Search" };
            fonts.draw(pm, ph, tx, baseline, px, Weight::Regular, c);
        } else {
            let w = fonts.draw(pm, &self.query, tx, baseline, px, Weight::Regular, theme.menu_text);
            // caret
            fill_rounded(pm, tx + w + 2.0 * s, baseline - 20.0 * s, 2.0 * s, 24.0 * s, 1.0, theme.menu_selection);
        }

        if self.results.is_empty() {
            return;
        }
        fill_rounded(pm, x * s, (y + FIELD_H) * s, WIDTH * s, 1.0 * s, 0.0, theme.menu_separator);
        for (row, idx) in self.results.iter().enumerate() {
            let e = &apps.entries[*idx];
            let ry = y + FIELD_H + 8.0 + row as f32 * ROW_H;
            let selected = row == self.selected;
            if selected {
                fill_rounded(pm, (x + 8.0) * s, ry * s, (WIDTH - 16.0) * s, ROW_H * s, 8.0 * s, theme.menu_selection);
            }
            let isz = (28.0 * s).round() as u32;
            if let Some(icon) = e.icon.as_deref().and_then(|n| apps.icon(n, isz)) {
                let k = isz as f32 / icon.width() as f32;
                let p = PixmapPaint {
                    quality: FilterQuality::Bilinear,
                    ..Default::default()
                };
                pm.draw_pixmap(
                    0,
                    0,
                    icon.as_ref().as_ref(),
                    &p,
                    Transform::from_row(k, 0.0, 0.0, k, (x + 20.0) * s, (ry + 6.0) * s),
                    None,
                );
            }
            let fpx = 15.0 * s;
            let bl = fonts.baseline_centered(ry * s, ROW_H * s, fpx, Weight::Regular);
            let c = if selected { theme.menu_selection_text } else { theme.menu_text };
            fonts.draw(pm, &e.name, (x + 60.0) * s, bl, fpx, Weight::Regular, c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::score;

    #[test]
    fn ranking() {
        assert!(score("Firefox", "fi") > score("Dateien finden", "fi"));
        assert!(score("Konsole", "kons").is_some());
        assert!(score("System Settings", "set").is_some());
        assert!(score("Konsole", "xyz").is_none());
        assert!(score("Kate", "kt").is_some());
    }
}
