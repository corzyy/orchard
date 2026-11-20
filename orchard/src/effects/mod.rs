//! Visual effects: wallpaper, rounded corners, springs, window animations.

pub mod blur;
pub mod genie;
pub mod label;
pub mod rounded;
pub mod snapshot;
pub mod spring;
pub mod wallpaper;

use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        RwLock,
    },
    time::Duration,
};

static SHADER_GENERATION: AtomicU64 = AtomicU64::new(0);
static SHADER_DIR: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Monotonic counter bumped on every `reload_shaders` IPC request.
pub fn shader_generation() -> u64 {
    SHADER_GENERATION.load(Ordering::Relaxed)
}

/// Trigger live GLSL shader reload on the next frame.
/// If `dir` is provided, shaders are loaded from that directory (falling back
/// to the compiled-in sources for any missing file).
pub fn reload_shaders(dir: Option<PathBuf>) {
    if let Some(d) = dir {
        *SHADER_DIR.write().unwrap() = Some(d);
    }
    SHADER_GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Read a shader source from the override directory (or `ORCHARD_SHADER_DIR`),
/// falling back to the compiled-in `fallback`.
pub fn load_shader(filename: &str, fallback: &'static str) -> String {
    let dir = SHADER_DIR
        .read()
        .unwrap()
        .clone()
        .or_else(|| std::env::var_os("ORCHARD_SHADER_DIR").map(PathBuf::from));
    if let Some(dir) = dir {
        let path = dir.join(filename);
        match std::fs::read_to_string(&path) {
            Ok(src) => {
                tracing::info!("loaded shader from {}", path.display());
                return src;
            }
            Err(e) => {
                tracing::warn!("failed to read shader {}: {e}; using built-in", path.display());
            }
        }
    }
    fallback.to_string()
}

use smithay::{
    backend::renderer::{element::Id, utils::CommitCounter},
    utils::{IsAlive, Logical, Point, Rectangle, Size},
};

use crate::shell::WindowElement;

use self::{
    genie::{MeshElement, RectF},
    snapshot::Snapshot,
    spring::{Spring, SpringParams},
};

#[derive(Debug)]
pub enum TransientKind {
    /// window closed: fade + shrink
    Close,
    /// window minimized: genie into the Dock
    Minimize,
    /// window restored from the Dock: reverse genie, then map it here
    Restore {
        window: WindowElement,
        location: Point<i32, Logical>,
    },
}

/// A window that is only drawn from a snapshot while it animates.
#[derive(Debug)]
pub struct Transient {
    pub id: Id,
    pub snapshot: Snapshot,
    /// global logical rect the snapshot covers when the window is in place
    pub rect: Rectangle<f64, Logical>,
    /// global logical rect of the animation target (Dock icon)
    pub target: Rectangle<f64, Logical>,
    pub spring: Spring,
    pub kind: TransientKind,
    pub commit: std::cell::Cell<CommitCounter>,
}

impl Transient {
    pub fn progress(&self, now: Duration) -> f32 {
        self.spring.value_at(now) as f32
    }

    pub fn is_done(&self, now: Duration) -> bool {
        self.spring.is_done_at(now)
    }

    /// Render element for an output whose global logical origin is `origin`.
    pub fn element(&self, origin: Point<i32, Logical>, scale: f64, now: Duration) -> MeshElement {
        let to_phys = |r: Rectangle<f64, Logical>| RectF {
            x: ((r.loc.x - origin.x as f64) * scale) as f32,
            y: ((r.loc.y - origin.y as f64) * scale) as f32,
            w: (r.size.w * scale) as f32,
            h: (r.size.h * scale) as f32,
        };
        let src = to_phys(self.rect);
        let p = self.progress(now);
        let (dst, alpha, genie) = match self.kind {
            TransientKind::Close => {
                let s = 0.86;
                let r = Rectangle::<f64, Logical>::new(
                    (
                        self.rect.loc.x + self.rect.size.w * (1.0 - s) / 2.0,
                        self.rect.loc.y + self.rect.size.h * (1.0 - s) / 2.0,
                    )
                        .into(),
                    (self.rect.size.w * s, self.rect.size.h * s).into(),
                );
                (to_phys(r), (1.0 - p).clamp(0.0, 1.0), false)
            }
            TransientKind::Minimize | TransientKind::Restore { .. } => {
                let s = crate::settings::get();
                let genie = s.dock.minimize_effect == orchard_config::MinimizeEffect::Genie && !s.appearance.reduce_motion;
                (to_phys(self.target), 1.0, genie)
            }
        };
        // a new commit every frame: the mesh changes continuously
        let mut commit = self.commit.get();
        commit.increment();
        self.commit.set(commit);
        let mut mesh = MeshElement::new(
            self.id.clone(),
            commit,
            self.snapshot.texture.clone(),
            (self.snapshot.size.w, self.snapshot.size.h),
            src,
            dst,
            p,
            alpha,
            genie,
        );
        // a Dock on the left or right: the window pours in sideways
        mesh.horizontal = crate::settings::get().dock.position != orchard_config::DockPosition::Bottom;
        mesh
    }
}

/// Compositor-global effect state, owned by `OrchardState`.
#[derive(Default)]
pub struct Effects {
    pub wallpaper: wallpaper::Wallpaper,
    pub transients: Vec<Transient>,
    pub space_transition: Option<crate::spaces::SpaceTransition>,
    pub labels: label::Labels,
    /// stable ids / buffers for Mission Control decorations
    pub mc_ids: Vec<Id>,
    pub mc_solids: Vec<smithay::backend::renderer::element::solid::SolidColorBuffer>,
    /// 4×4 single-colour textures for rounded fills (rings, buttons)
    pub fill_textures: std::collections::HashMap<[u8; 3], smithay::backend::renderer::gles::GlesTexture>,
    /// drop target while dragging a window to a screen edge (global logical)
    pub tile_preview: Option<smithay::utils::Rectangle<i32, smithay::utils::Logical>>,
    /// where the preview animates from (the dragged window, or the previous target)
    /// (from, spring, fades in)
    pub tile_preview_anim: Option<(Rectangle<f64, Logical>, Spring, bool)>,
    /// fill + 4 border strips
    pub tile_preview_bufs: [smithay::backend::renderer::element::solid::SolidColorBuffer; 5],
    /// ⌘⇧4 selection (drawn above everything but the cursor)
    pub shot_overlay: Option<crate::screenshot::Overlay>,
    pub shot_bufs: [smithay::backend::renderer::element::solid::SolidColorBuffer; 5],
    /// hide the `orchard-screenshot` layer surface while capturing a screenshot
    pub hide_screenshot_ui: bool,
    /// live-blur textures of the frosted-glass backdrops
    pub blur: blur::LiveBlur,
    /// lock screen backdrop: blurred wallpaper + a light dim (stable ids)
    pub lock_bg_id: StableId,
    pub lock_dim: smithay::backend::renderer::element::solid::SolidColorBuffer,
    pub lock_fallback: smithay::backend::renderer::element::solid::SolidColorBuffer,
    /// when the session was unlocked: the lock backdrop fades out over the desktop
    pub unlock_fade: Option<Duration>,
    /// green traffic-light Move & Resize popover
    pub tile_menu: Option<crate::chrome::tile_menu::TileMenu>,
    pub tile_menu_arm: Option<crate::chrome::tile_menu::TileMenuArm>,
}

/// An element id created on first use (`Id` has no `Default`).
pub struct StableId(pub Id);

impl Default for StableId {
    fn default() -> Self {
        StableId(Id::new())
    }
}

/// Length of the unlock cross-fade.
pub const UNLOCK_FADE: Duration = Duration::from_millis(320);

impl Effects {
    /// Opacity of the lock backdrop drawn over the desktop right after unlocking
    /// (1 → 0, ease-out), or None once the fade is over.
    pub fn unlock_fade_alpha(&mut self, now: Duration) -> Option<f32> {
        let start = self.unlock_fade?;
        let t = now.saturating_sub(start).as_secs_f32() / UNLOCK_FADE.as_secs_f32();
        if t >= 1.0 {
            self.unlock_fade = None;
            return None;
        }
        let e = 1.0 - (1.0 - t).powi(3);
        Some(1.0 - e)
    }
}

impl Effects {
    pub fn is_animating(&self, now: Duration) -> bool {
        self.transients.iter().any(|t| !t.is_done(now))
            || self.unlock_fade.map(|s| now.saturating_sub(s) < UNLOCK_FADE).unwrap_or(false)
            || self.tile_preview_anim.as_ref().map(|(_, s, _)| !s.is_done_at(now)).unwrap_or(false)
            || self
                .space_transition
                .as_ref()
                .map(|t| !t.spring.is_done_at(now))
                .unwrap_or(false)
    }
}

impl Effects {
    /// Drop preview rect at `now` (global logical), and its opacity 0..1.
    pub fn tile_preview_at(&self, now: Duration) -> Option<(Rectangle<f64, Logical>, f32)> {
        let to = self.tile_preview?.to_f64();
        match self.tile_preview_anim {
            Some((from, spring, fade_in)) if !spring.is_done_at(now) => {
                let p = spring.value_at(now);
                let a = if fade_in { (p * 2.0).clamp(0.0, 1.0) as f32 } else { 1.0 };
                Some((lerp_rect(from, to, p), a))
            }
            _ => Some((to, 1.0)),
        }
    }
}

pub fn lerp_rect(a: Rectangle<f64, Logical>, b: Rectangle<f64, Logical>, t: f64) -> Rectangle<f64, Logical> {
    let l = |x: f64, y: f64| x + (y - x) * t;
    Rectangle::new(
        (l(a.loc.x, b.loc.x), l(a.loc.y, b.loc.y)).into(),
        (l(a.size.w, b.size.w).max(1.0), l(a.size.h, b.size.h).max(1.0)).into(),
    )
}

impl std::fmt::Debug for Effects {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Effects").finish_non_exhaustive()
    }
}

/// A window changing its frame (tile, zoom, restore): the frame springs from
/// the old rect to the new one, the live surface is scaled to fit it, and a
/// snapshot of the old contents fades out once the client has resized.
#[derive(Debug)]
pub struct FrameAnim {
    /// geometry rect (global logical) the animation starts from
    pub from: Rectangle<f64, Logical>,
    /// geometry size the window is heading for (its location is read live,
    /// so a drag during the animation is followed)
    pub to_size: Size<i32, Logical>,
    pub spring: Spring,
    pub snapshot: Option<Snapshot>,
    /// window.geometry() when the snapshot was taken
    pub snap_geo: Rectangle<i32, Logical>,
    pub id: Id,
    pub commit: Cell<CommitCounter>,
    /// when the client's buffer first matched the target size
    pub resized_at: Cell<Option<Duration>>,
}

impl FrameAnim {
    /// The frame at `now`, heading for `to_loc`.
    pub fn current(&self, to_loc: Point<i32, Logical>, now: Duration) -> Rectangle<f64, Logical> {
        let to = Rectangle::new(to_loc, self.to_size).to_f64();
        lerp_rect(self.from, to, self.spring.value_at(now))
    }

    /// Opacity of the old-content snapshot.
    pub fn snapshot_alpha(&self, now: Duration) -> f32 {
        // clients that never hit the exact size (terminals snap to cells,
        // min/max sizes) fade anyway after a moment
        let fade_start = self.resized_at.get().unwrap_or(self.spring.start + Duration::from_millis(180));
        let t = now.saturating_sub(fade_start).as_secs_f32() / 0.12;
        (1.0 - t).clamp(0.0, 1.0)
    }

    pub fn next_commit(&self) -> CommitCounter {
        let mut c = self.commit.get();
        c.increment();
        self.commit.set(c);
        c
    }
}

/// Per-window animation state, stored in the window's user data.
#[derive(Debug, Default)]
pub struct WindowAnim {
    /// 0 → 1 when the window appears
    pub open: Option<Spring>,
    pub frame: Option<FrameAnim>,
}

impl WindowAnim {
    pub fn is_animating(&self, now: Duration) -> bool {
        self.open.map(|s| !s.is_done_at(now)).unwrap_or(false)
            || self.frame.as_ref().map(|f| !f.spring.is_done_at(now)).unwrap_or(false)
    }

    /// The running frame animation, dropping a finished one.
    pub fn frame_at(&mut self, now: Duration) -> Option<&FrameAnim> {
        if self.frame.as_ref().map(|f| f.spring.is_done_at(now)).unwrap_or(false) {
            self.frame = None;
        }
        self.frame.as_ref()
    }

    /// (scale, alpha) of the window at `now`.
    pub fn transform(&self, now: Duration) -> (f64, f32) {
        match self.open {
            Some(s) if !s.is_done_at(now) => {
                let p = s.value_at(now).clamp(0.0, 1.2);
                (0.94 + 0.06 * p, p.clamp(0.0, 1.0) as f32)
            }
            _ => (1.0, 1.0),
        }
    }
}

impl WindowElement {
    pub fn anim(&self) -> std::cell::RefMut<'_, WindowAnim> {
        self.user_data()
            .insert_if_missing(|| RefCell::new(WindowAnim::default()));
        self.user_data()
            .get::<RefCell<WindowAnim>>()
            .unwrap()
            .borrow_mut()
    }

    pub fn start_open_animation(&self, now: Duration) {
        self.anim().open = Some(Spring::new(0.0, 1.0, SpringParams::SMOOTH, now));
    }
}

/// Whether any window is mid-animation (the backend then keeps redrawing).
pub fn any_animating<'a>(windows: impl Iterator<Item = &'a WindowElement>, now: Duration) -> bool {
    windows.filter(|w| w.alive()).any(|w| w.anim().is_animating(now))
}
