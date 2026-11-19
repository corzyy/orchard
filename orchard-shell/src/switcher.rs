//! Cmd+Tab app switcher panel (state comes from the compositor).

use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};

use crate::{
    apps::Apps,
    draw::fill_rounded,
    text::{Fonts, Weight},
    theme::Theme,
};

pub const ICON: f32 = 96.0;
const GAP: f32 = 16.0;
const PAD: f32 = 20.0;
const LABEL_H: f32 = 28.0;

pub struct Switcher {
    pub apps: Vec<String>,
    pub selected: usize,
}

impl Switcher {
    pub fn size(&self) -> (u32, u32) {
        let n = self.apps.len().max(1) as f32;
        let w = 2.0 * PAD + n * ICON + (n - 1.0) * GAP;
        let h = 2.0 * PAD + ICON + LABEL_H;
        (w.ceil() as u32, h.ceil() as u32)
    }

    pub fn paint(&self, pm: &mut Pixmap, scale: f32, apps: &Apps, fonts: &Fonts, theme: &Theme) {
        let s = scale;
        let (w, h) = self.size();
        fill_rounded(pm, 0.0, 0.0, w as f32 * s, h as f32 * s, 22.0 * s, theme.menu_bg);
        for (i, app_id) in self.apps.iter().enumerate() {
            let x = PAD + i as f32 * (ICON + GAP);
            if i == self.selected {
                fill_rounded(
                    pm,
                    (x - 8.0) * s,
                    (PAD - 8.0) * s,
                    (ICON + 16.0) * s,
                    (ICON + 16.0) * s,
                    14.0 * s,
                    theme.bar_highlight,
                );
            }
            let entry = apps.for_app_id(app_id);
            let icon_name = entry.and_then(|e| e.icon.clone()).unwrap_or_else(|| app_id.clone());
            let px = (ICON * s).round() as u32;
            if let Some(icon) = apps.icon(&icon_name, px).or_else(|| apps.icon("application-x-executable", px)) {
                let k = px as f32 / icon.width() as f32;
                let paint = PixmapPaint {
                    quality: FilterQuality::Bilinear,
                    ..Default::default()
                };
                pm.draw_pixmap(
                    0,
                    0,
                    icon.as_ref().as_ref(),
                    &paint,
                    Transform::from_row(k, 0.0, 0.0, k, x * s, PAD * s),
                    None,
                );
            }
            if i == self.selected {
                let name = entry.map(|e| e.name.clone()).unwrap_or_else(|| app_id.clone());
                let fpx = 13.0 * s;
                let tw = fonts.measure(&name, fpx, Weight::Regular);
                let cx = (x + ICON / 2.0) * s;
                let tx = crate::draw::clamp_span(cx - tw / 2.0, 4.0, w as f32 * s - tw - 4.0);
                let baseline = fonts.baseline_centered((PAD + ICON + 4.0) * s, LABEL_H * s, fpx, Weight::Regular);
                fonts.draw(pm, &name, tx, baseline, fpx, Weight::Regular, theme.menu_text);
            }
        }
    }
}
