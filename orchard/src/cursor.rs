use std::{
    io::Read,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use tracing::warn;
use xcursor::{
    parser::{parse_xcursor, Image},
    CursorTheme,
};

static FALLBACK_CURSOR_DATA: &[u8] = include_bytes!("../resources/cursor.rgba");
static NEXT_CURSOR_ID: AtomicU64 = AtomicU64::new(1);

fn tag_images(images: Vec<Image>) -> Vec<(u64, Arc<Image>)> {
    images
        .into_iter()
        .map(|img| (NEXT_CURSOR_ID.fetch_add(1, Ordering::Relaxed), Arc::new(img)))
        .collect()
}

pub struct Cursor {
    icons: Vec<(u64, Arc<Image>)>,
    size: u32,
    theme: CursorTheme,
    named: std::cell::RefCell<std::collections::HashMap<String, Option<Vec<(u64, Arc<Image>)>>>>,
}

impl Cursor {
    /// Pointer theme + size from Orchard's settings.
    pub fn load() -> Cursor {
        let s = crate::settings::get();
        let name = s.appearance.cursor_theme().to_string();
        let size = s.appearance.cursor_size();

        let theme = CursorTheme::load(&name);
        let icons = load_icon(&theme)
            .map_err(|err| warn!("Unable to load xcursor: {}, using fallback cursor", err))
            .unwrap_or_else(|_| {
                vec![Image {
                    size: 32,
                    width: 64,
                    height: 64,
                    xhot: 1,
                    yhot: 1,
                    delay: 1,
                    pixels_rgba: Vec::from(FALLBACK_CURSOR_DATA),
                    pixels_argb: vec![], //unused
                }]
            });

        Cursor {
            icons: tag_images(icons),
            size,
            theme,
            named: Default::default(),
        }
    }

    pub fn get_image(&self, scale: u32, time: Duration) -> Image {
        let (_, img, _) = self.get_image_cached(scale, time);
        (*img).clone()
    }

    /// Cached lookup returning `(unique_id, Arc<Image>, is_animated)` without
    /// cloning `pixels_rgba` on every frame.
    pub fn get_image_cached(&self, scale: u32, time: Duration) -> (u64, Arc<Image>, bool) {
        let size = self.size * scale;
        frame_cached(time.as_millis() as u32, size, &self.icons)
    }

    /// Image for a named cursor (CSS names like "text", "pointer", "ew-resize"),
    /// trying the given alternatives, falling back to the default arrow.
    pub fn get_named_image_cached(&self, names: &[&str], scale: u32, time: Duration) -> (u64, Arc<Image>, bool) {
        for name in names {
            if *name == "default" {
                break;
            }
            let mut cache = self.named.borrow_mut();
            let entry = cache.entry(name.to_string()).or_insert_with(|| {
                let path = self.theme.load_icon(name)?;
                let data = std::fs::read(path).ok()?;
                parse_xcursor(&data).filter(|v| !v.is_empty()).map(tag_images)
            });
            if let Some(icons) = entry {
                return frame_cached(time.as_millis() as u32, self.size * scale, icons);
            }
        }
        self.get_image_cached(scale, time)
    }
}

fn nearest_images(size: u32, images: &[(u64, Arc<Image>)]) -> impl Iterator<Item = &(u64, Arc<Image>)> {
    // Follow the nominal size of the cursor to choose the nearest
    let nearest_image = images
        .iter()
        .min_by_key(|(_, image)| (size as i32 - image.size as i32).abs())
        .unwrap();

    images
        .iter()
        .filter(move |(_, image)| image.width == nearest_image.1.width && image.height == nearest_image.1.height)
}

fn frame_cached(mut millis: u32, size: u32, images: &[(u64, Arc<Image>)]) -> (u64, Arc<Image>, bool) {
    let mut count = 0usize;
    let mut total = 0u32;
    for (_, image) in nearest_images(size, images) {
        count += 1;
        total += image.delay;
    }
    let animated = count > 1 && total > 0;
    if total == 0 {
        let (id, img) = nearest_images(size, images).next().unwrap();
        return (*id, img.clone(), false);
    }
    millis %= total;

    for (id, img) in nearest_images(size, images) {
        if millis < img.delay {
            return (*id, img.clone(), animated);
        }
        millis -= img.delay;
    }

    unreachable!()
}

#[derive(thiserror::Error, Debug)]
enum Error {
    #[error("Theme has no default cursor")]
    NoDefaultCursor,
    #[error("Error opening xcursor file: {0}")]
    File(#[from] std::io::Error),
    #[error("Failed to parse XCursor file")]
    Parse,
}

fn load_icon(theme: &CursorTheme) -> Result<Vec<Image>, Error> {
    let icon_path = theme.load_icon("default").ok_or(Error::NoDefaultCursor)?;
    let mut cursor_file = std::fs::File::open(icon_path)?;
    let mut cursor_data = Vec::new();
    cursor_file.read_to_end(&mut cursor_data)?;
    parse_xcursor(&cursor_data).ok_or(Error::Parse)
}
