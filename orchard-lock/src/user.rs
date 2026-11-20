//! Who is logged in: login name, display name and account picture.

use std::path::PathBuf;

use tiny_skia::{FillRule, GradientStop, LinearGradient, Paint, PathBuilder, Pixmap, Point, SpreadMode, Transform};

pub struct User {
    pub login: String,
    pub display: String,
    pub home: PathBuf,
}

impl User {
    pub fn current() -> User {
        let mut login = std::env::var("USER").unwrap_or_default();
        let mut display = String::new();
        let mut home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        unsafe {
            let mut pw: libc::passwd = std::mem::zeroed();
            let mut out: *mut libc::passwd = std::ptr::null_mut();
            let mut buf = vec![0 as libc::c_char; 16 * 1024];
            if libc::getpwuid_r(libc::getuid(), &mut pw, buf.as_mut_ptr(), buf.len(), &mut out) == 0 && !out.is_null() {
                let s = |p: *const libc::c_char| {
                    if p.is_null() {
                        String::new()
                    } else {
                        std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
                    }
                };
                login = s(pw.pw_name);
                display = s(pw.pw_gecos).split(',').next().unwrap_or_default().trim().to_string();
                home = PathBuf::from(s(pw.pw_dir));
            }
        }
        if display.is_empty() || display == login {
            // "jakob" → "Jakob": macOS shows the full name, never the short one
            let mut c = login.chars();
            display = c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default();
        }
        User { login, display, home }
    }

    /// The account picture (KDE/GNOME conventions), decoded.
    pub fn picture(&self) -> Option<image::RgbaImage> {
        let candidates = [
            self.home.join(".face"),
            self.home.join(".face.icon"),
            PathBuf::from("/var/lib/AccountsService/icons").join(&self.login),
        ];
        candidates.iter().filter(|p| p.is_file()).find_map(|p| image::open(p).ok()).map(|i| i.to_rgba8())
    }
}

/// A round avatar of `d` px: the picture (centre-cropped), or macOS's grey
/// silhouette.
pub fn avatar(picture: Option<&image::RgbaImage>, d: u32) -> Option<Pixmap> {
    let mut pm = Pixmap::new(d, d)?;
    let df = d as f32;
    let r = df / 2.0;
    let circle = PathBuilder::from_circle(r, r, r - 0.5)?;
    match picture {
        Some(img) => {
            let side = img.width().min(img.height());
            let crop = image::imageops::crop_imm(img, (img.width() - side) / 2, (img.height() - side) / 2, side, side).to_image();
            let scaled = image::imageops::resize(&crop, d, d, image::imageops::FilterType::CatmullRom);
            let mut src = Pixmap::new(d, d)?;
            for (dst, p) in src.data_mut().chunks_exact_mut(4).zip(scaled.pixels()) {
                let a = p[3] as u32;
                dst[0] = (p[0] as u32 * a / 255) as u8;
                dst[1] = (p[1] as u32 * a / 255) as u8;
                dst[2] = (p[2] as u32 * a / 255) as u8;
                dst[3] = p[3];
            }
            let paint = Paint {
                shader: tiny_skia::Pattern::new(
                    src.as_ref(),
                    SpreadMode::Pad,
                    tiny_skia::FilterQuality::Bilinear,
                    1.0,
                    Transform::identity(),
                ),
                anti_alias: true,
                ..Default::default()
            };
            pm.fill_path(&circle, &paint, FillRule::Winding, Transform::identity(), None);
        }
        None => {
            let bg = Paint {
                shader: LinearGradient::new(
                    Point::from_xy(0.0, 0.0),
                    Point::from_xy(0.0, df),
                    vec![
                        GradientStop::new(0.0, tiny_skia::Color::from_rgba8(0xB4, 0xB8, 0xC0, 0xFF)),
                        GradientStop::new(1.0, tiny_skia::Color::from_rgba8(0x80, 0x85, 0x8E, 0xFF)),
                    ],
                    SpreadMode::Pad,
                    Transform::identity(),
                )?,
                anti_alias: true,
                ..Default::default()
            };
            pm.fill_path(&circle, &bg, FillRule::Winding, Transform::identity(), None);
            // head and shoulders, clipped to the circle
            let mut mask = tiny_skia::Mask::new(d, d)?;
            mask.fill_path(&circle, FillRule::Winding, true, Transform::identity());
            let mut fg = Paint::default();
            fg.set_color_rgba8(0xFF, 0xFF, 0xFF, 0xF0);
            fg.anti_alias = true;
            if let Some(head) = PathBuilder::from_circle(r, df * 0.385, df * 0.19) {
                pm.fill_path(&head, &fg, FillRule::Winding, Transform::identity(), Some(&mask));
            }
            if let Some(body) = tiny_skia::Rect::from_xywh(r - df * 0.34, df * 0.63, df * 0.68, df * 0.62)
                .and_then(PathBuilder::from_oval)
            {
                pm.fill_path(&body, &fg, FillRule::Winding, Transform::identity(), Some(&mask));
            }
        }
    }
    Some(pm)
}
