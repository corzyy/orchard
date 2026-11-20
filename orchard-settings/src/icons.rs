//! Cached symbolic SVG icons from Adwaita, rasterized as alpha masks via resvg.

use std::{collections::HashMap, path::PathBuf};

use tiny_skia::Pixmap;

use crate::theme::{blit_mask, Rgba};

struct Mask {
    w: u32,
    h: u32,
    alpha: Vec<u8>,
}

#[derive(Default)]
pub struct Icons {
    cache: HashMap<(String, u32), Option<Mask>>,
}

fn find_symbolic(name: &str) -> Option<PathBuf> {
    let file = format!("{name}-symbolic.svg");
    for base in [
        "/usr/share/icons/Adwaita/symbolic",
        "/usr/share/icons/AdwaitaLegacy/symbolic",
    ] {
        for sub in [
            "actions",
            "devices",
            "status",
            "legacy",
            "categories",
            "places",
            "apps",
            "mimetypes",
            "emblems",
            "ui",
        ] {
            let p = PathBuf::from(base).join(sub).join(&file);
            if p.exists() {
                return Some(p);
            }
        }
    }
    for sub in ["actions", "devices", "status", "categories", "places", "apps"] {
        let p = PathBuf::from("/usr/share/icons/Adwaita/scalable")
            .join(sub)
            .join(&file);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

fn rasterize(path: &std::path::Path, px: u32) -> Option<Mask> {
    if px == 0 {
        return None;
    }
    let data = std::fs::read(path).ok()?;
    let tree = resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default()).ok()?;
    let size = tree.size();
    let mut pm = resvg::tiny_skia::Pixmap::new(px, px)?;
    let s = px as f32 / size.width().max(size.height()).max(1.0);
    let tx = (px as f32 - size.width() * s) * 0.5;
    let ty = (px as f32 - size.height() * s) * 0.5;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(s, s).post_translate(tx, ty),
        &mut pm.as_mut(),
    );
    let alpha = pm.pixels().iter().map(|p| p.alpha()).collect();
    Some(Mask { w: px, h: px, alpha })
}

impl Icons {
    /// Draw a symbolic icon centered inside `(x, y, size, size)` in physical pixels.
    pub fn draw(
        &mut self,
        pm: &mut Pixmap,
        name: &str,
        x: i32,
        y: i32,
        px: u32,
        tint: Rgba,
        clip: Option<[i32; 4]>,
    ) -> bool {
        let entry = self
            .cache
            .entry((name.to_string(), px))
            .or_insert_with(|| rasterize(&find_symbolic(name)?, px));
        let Some(mask) = entry.as_ref() else {
            return false;
        };
        blit_mask(pm, &mask.alpha, mask.w, mask.h, x, y, tint, clip);
        true
    }
}
