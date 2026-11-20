//! Cmd+Tab app switcher panel (state comes from the compositor).

use std::time::{Duration, Instant};

use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};

use crate::{
    anim::{self, ease_out_cubic, scale_alpha, SpringFloat, SpringParams},
    apps::Apps,
    draw::fill_rounded,
    text::{Fonts, Weight},
    theme::Theme,
};

pub const ICON: f32 = 96.0;
const GAP: f32 = 16.0;
const PAD: f32 = 20.0;
const LABEL_H: f32 = 28.0;
const OPEN_ANIM: Duration = Duration::from_millis(110);

pub struct Switcher {
    pub apps: Vec<String>,
    pub selected: usize,
    pub opened: Instant,
    sel_x: SpringFloat,
}

impl Switcher {
    pub fn new(apps: Vec<String>, selected: usize) -> Self {
        let x = Self::slot_x(selected);
        Self {
            apps,
            selected,
            opened: Instant::now(),
            sel_x: SpringFloat::new(x, SpringParams::SWITCHER),
        }
    }

    /// Updates the running switcher so the selection highlight glides smoothly
    /// across icons as the user taps Tab.
    pub fn update(&mut self, apps: Vec<String>, selected: usize) {
        let list_changed = self.apps != apps;
        self.apps = apps;
        self.selected = selected;
        let target = Self::slot_x(selected);
        if list_changed {
            self.sel_x.snap(target);
        } else {
            self.sel_x.set_target(target, Instant::now());
        }
    }

    fn slot_x(idx: usize) -> f32 {
        PAD + idx as f32 * (ICON + GAP)
    }

    pub fn open_progress(&self) -> f32 {
        if anim::reduced() {
            return 1.0;
        }
        ease_out_cubic((self.opened.elapsed().as_secs_f32() / OPEN_ANIM.as_secs_f32()).clamp(0.0, 1.0))
    }

    pub fn is_animating(&self) -> bool {
        let now = Instant::now();
        (!anim::reduced() && self.opened.elapsed() < OPEN_ANIM) || self.sel_x.is_animating(now)
    }

    pub fn size(&self) -> (u32, u32) {
        let n = self.apps.len().max(1) as f32;
        let w = 2.0 * PAD + n * ICON + (n - 1.0) * GAP;
        let h = 2.0 * PAD + ICON + LABEL_H;
        (w.ceil() as u32, h.ceil() as u32)
    }

    pub fn paint(&mut self, pm: &mut Pixmap, scale: f32, apps: &Apps, fonts: &Fonts, theme: &Theme) {
        let now = Instant::now();
        let s = scale;
        let (w, h) = self.size();
        let alpha = self.open_progress();
        fill_rounded(pm, 0.0, 0.0, w as f32 * s, h as f32 * s, 22.0 * s, scale_alpha(theme.menu_bg, alpha));

        // Smoothly gliding selection highlight behind the icons
        let sel_x = self.sel_x.sample(now);
        if !self.apps.is_empty() {
            fill_rounded(
                pm,
                (sel_x - 8.0) * s,
                (PAD - 8.0) * s,
                (ICON + 16.0) * s,
                (ICON + 16.0) * s,
                14.0 * s,
                scale_alpha(theme.bar_highlight, alpha),
            );
        }

        for (i, app_id) in self.apps.iter().enumerate() {
            let x = Self::slot_x(i);
            let entry = apps.for_app_id(app_id);
            let icon_name = entry.and_then(|e| e.icon.clone()).unwrap_or_else(|| app_id.clone());
            let px = (ICON * s).round() as u32;
            if let Some(icon) = apps.icon(&icon_name, px).or_else(|| apps.icon("application-x-executable", px)) {
                let k = px as f32 / icon.width() as f32;
                let paint = PixmapPaint {
                    opacity: alpha,
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
                let cx = (sel_x + ICON / 2.0) * s;
                let tx = crate::draw::clamp_span(cx - tw / 2.0, 4.0, w as f32 * s - tw - 4.0);
                let baseline = fonts.baseline_centered((PAD + ICON + 4.0) * s, LABEL_H * s, fpx, Weight::Regular);
                fonts.draw(pm, &name, tx, baseline, fpx, Weight::Regular, scale_alpha(theme.menu_text, alpha));
            }
        }
    }
}
