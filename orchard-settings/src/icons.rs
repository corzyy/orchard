//! Symbolic icons from the system's Adwaita icon theme, rasterised white so
//! they can be tinted when painted.

use std::{collections::HashMap, path::PathBuf};

use eframe::egui::{self, Color32, ColorImage, Pos2, Rect, TextureHandle, TextureOptions};

#[derive(Default)]
pub struct Icons {
    cache: HashMap<(String, u32), Option<TextureHandle>>,
}

fn find_symbolic(name: &str) -> Option<PathBuf> {
    let file = format!("{name}-symbolic.svg");
    for base in ["/usr/share/icons/Adwaita/symbolic", "/usr/share/icons/AdwaitaLegacy/symbolic"] {
        for sub in ["actions", "devices", "status", "legacy", "categories", "places", "apps", "mimetypes", "emblems", "ui"] {
            let p = PathBuf::from(base).join(sub).join(&file);
            if p.exists() {
                return Some(p);
            }
        }
    }
    // older layout: Adwaita/scalable/<ctx>/
    for sub in ["actions", "devices", "status", "categories", "places", "apps"] {
        let p = PathBuf::from("/usr/share/icons/Adwaita/scalable").join(sub).join(&file);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

fn rasterize(path: &std::path::Path, px: u32) -> Option<ColorImage> {
    let data = std::fs::read(path).ok()?;
    let tree = resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default()).ok()?;
    let size = tree.size();
    let mut pm = resvg::tiny_skia::Pixmap::new(px, px)?;
    let s = px as f32 / size.width().max(size.height());
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(s, s), &mut pm.as_mut());
    // keep coverage only: white with the icon's alpha
    let pixels: Vec<Color32> = pm
        .pixels()
        .iter()
        .map(|p| Color32::from_rgba_premultiplied(p.alpha(), p.alpha(), p.alpha(), p.alpha()))
        .collect();
    Some(ColorImage::new([px as usize, px as usize], pixels))
}

impl Icons {
    pub fn get(&mut self, ctx: &egui::Context, name: &str, logical: f32) -> Option<TextureHandle> {
        let px = (logical * ctx.pixels_per_point()).ceil() as u32;
        self.cache
            .entry((name.to_string(), px))
            .or_insert_with(|| {
                let img = rasterize(&find_symbolic(name)?, px)?;
                Some(ctx.load_texture(format!("icon-{name}-{px}"), img, TextureOptions::LINEAR))
            })
            .clone()
    }

    /// Paint a symbolic icon centred in `rect`, tinted.
    pub fn paint(&mut self, ui: &egui::Ui, name: &str, rect: Rect, tint: Color32) -> bool {
        let Some(tex) = self.get(ui.ctx(), name, rect.width()) else { return false };
        ui.painter().image(
            tex.id(),
            rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            tint,
        );
        true
    }
}
