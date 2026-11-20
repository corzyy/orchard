//! Game Mode: on while a game is full screen on any display.
//!
//! "Full screen" is the fullscreen state, or a borderless window covering its
//! whole display (how many games run). "A game" is decided once per window
//! from its desktop file (`Categories=Game`), Steam's launch environment,
//! Wine/Proton processes and a few well-known titles, and later by the window
//! locking the pointer (mouse look). A full-screen video is none of these.
//!
//! While active: the shell holds back notification banners, VRR is enabled on
//! the game's display, live blur and hot corners pause, and the idle lock waits.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, RwLock,
    },
};

use smithay::utils::IsAlive;

use crate::{
    shell::{FullscreenSurface, WindowElement},
    state::{Backend, OrchardState},
};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static OUTPUT: RwLock<Option<String>> = RwLock::new(None);

/// Whether Game Mode is on right now.
pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// The display showing the game, while Game Mode is on.
pub fn output() -> Option<String> {
    OUTPUT.read().ok().and_then(|o| o.clone())
}

/// What the compositor last reported (window app id and display).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GameState {
    pub app_id: String,
    pub output: String,
}

/// Facts about a window used to tell games from other full-screen apps.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub app_id: String,
    pub title: String,
    /// its desktop file lists `Game` in Categories
    pub desktop_game: bool,
    /// started by Steam (`SteamAppId` / `SteamGameId` in its environment)
    pub steam: bool,
    /// a Windows program under Wine / Proton
    pub wine: bool,
    /// it locked the pointer (mouse look)
    pub pointer_locked: bool,
}

/// Window classes / titles of games that don't announce themselves otherwise.
const KNOWN: &[&str] = &["steam_app_", "gamescope", "minecraft", "roblox", "org.vinegarhq.sober", "sober"];

pub fn looks_like_game(f: &Facts) -> bool {
    if f.desktop_game || f.steam || f.wine || f.pointer_locked {
        return true;
    }
    let app = f.app_id.to_lowercase();
    let title = f.title.to_lowercase();
    KNOWN.iter().any(|k| app.contains(k)) || title.starts_with("minecraft") || app.ends_with(".exe")
}

/// Per-window cache: facts that don't change while the window lives.
#[derive(Default)]
struct WindowFacts {
    /// (title it was decided for, verdict): re-decided only when the title
    /// changes (Minecraft's appears after loading), never per frame
    stat: RefCell<Option<(String, bool)>>,
    locked: Cell<bool>,
}

/// The window just locked the pointer.
pub fn note_pointer_lock(window: &WindowElement) {
    window.user_data().insert_if_missing(WindowFacts::default);
    if let Some(f) = window.user_data().get::<WindowFacts>() {
        f.locked.set(true);
    }
}

/// app id -> desktop file says Game (desktop files are read once per new app id)
fn desktop_says_game(app_id: &str) -> bool {
    static CACHE: Mutex<Option<HashMap<String, bool>>> = Mutex::new(None);
    let key = app_id.to_lowercase();
    if key.is_empty() {
        return false;
    }
    let Ok(mut cache) = CACHE.lock() else { return false };
    let cache = cache.get_or_insert_with(HashMap::new);
    if let Some(v) = cache.get(&key) {
        return *v;
    }
    let is_game = orchard_config::desktop::applications().iter().any(|e| {
        let id = e.id.trim_end_matches(".desktop").to_lowercase();
        let wm = e.get("StartupWMClass").map(str::to_lowercase);
        (id == key || wm.as_deref() == Some(key.as_str())) && e.categories().iter().any(|c| c == "Game")
    });
    cache.insert(key, is_game);
    is_game
}

fn read_proc(pid: u32, file: &str) -> Option<Vec<u8>> {
    std::fs::read(format!("/proc/{pid}/{file}")).ok()
}

fn parent_of(pid: u32) -> Option<u32> {
    let stat = String::from_utf8(read_proc(pid, "stat")?).ok()?;
    stat[stat.rfind(')')? + 1..].split_whitespace().nth(1)?.parse().ok()
}

/// (started by Steam, Wine/Proton) for the process and its close parents.
fn process_facts(pid: u32) -> (bool, bool) {
    let (mut steam, mut wine) = (false, false);
    let mut cur = Some(pid);
    for _ in 0..3 {
        let Some(p) = cur.filter(|p| *p > 1) else { break };
        if let Some(env) = read_proc(p, "environ") {
            steam |= env
                .split(|b| *b == 0)
                .any(|kv| kv.starts_with(b"SteamAppId=") || kv.starts_with(b"SteamGameId="));
        }
        if let Some(comm) = read_proc(p, "comm") {
            let comm = String::from_utf8_lossy(&comm).trim().to_lowercase();
            wine |= comm.ends_with(".exe") || comm.starts_with("wine") || comm.contains("proton");
        }
        cur = parent_of(p);
    }
    (steam, wine)
}

impl<BackendData: Backend + 'static> OrchardState<BackendData> {
    fn window_pid(&self, w: &WindowElement) -> Option<u32> {
        use smithay::desktop::WindowSurface;
        match w.0.underlying_surface() {
            WindowSurface::Wayland(t) => smithay::reexports::wayland_server::Resource::client(t.wl_surface())
                .and_then(|c| c.get_credentials(&self.display_handle).ok())
                .map(|c| c.pid as u32),
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(x) => x.pid(),
        }
    }

    fn is_game(&self, w: &WindowElement) -> bool {
        w.user_data().insert_if_missing(WindowFacts::default);
        let Some(cache) = w.user_data().get::<WindowFacts>() else { return false };
        if cache.locked.get() {
            return true;
        }
        let title = w.title();
        if let Some((t, v)) = cache.stat.borrow().as_ref() {
            if *v || *t == title {
                return *v;
            }
        }
        let app_id = w.app_id();
        let (steam, wine) = self.window_pid(w).map(process_facts).unwrap_or_default();
        let facts = Facts {
            desktop_game: desktop_says_game(&app_id),
            app_id,
            title: title.clone(),
            steam,
            wine,
            pointer_locked: false,
        };
        let v = looks_like_game(&facts);
        tracing::debug!(?facts, game = v, "game mode: classified window");
        *cache.stat.borrow_mut() = Some((title, v));
        v
    }

    /// The full-screen game on some display, if any.
    fn fullscreen_game(&self) -> Option<GameState> {
        for output in self.space.outputs() {
            let Some(geo) = self.space.output_geometry(output) else { continue };
            let fullscreen = output.user_data().get::<FullscreenSurface>().and_then(|f| f.get());
            // borderless "full screen": the topmost window covers the display
            let covering = || {
                self.space.elements_for_output(output).last().cloned().filter(|w| {
                    self.space
                        .element_geometry(w)
                        .map(|r| r.contains_rect(geo))
                        .unwrap_or(false)
                })
            };
            if let Some(w) = fullscreen.or_else(covering).filter(|w| w.alive()) {
                if self.is_game(&w) {
                    return Some(GameState { app_id: w.app_id(), output: output.name() });
                }
            }
        }
        None
    }

    /// Re-check after window / focus / fullscreen changes (cheap; cached per window).
    pub fn update_game_mode(&mut self) {
        let s = crate::settings::get();
        let found = if s.game_mode.enabled && !self.is_locked() { self.fullscreen_game() } else { None };
        if found == self.game_mode {
            return;
        }
        match &found {
            Some(g) => tracing::info!(app = g.app_id, output = g.output, "game mode on"),
            None => tracing::info!("game mode off"),
        }
        ACTIVE.store(found.is_some(), Ordering::Relaxed);
        if let Ok(mut o) = OUTPUT.write() {
            *o = found.as_ref().map(|g| g.output.clone());
        }
        self.game_mode = found;
        // live blur pauses / resumes
        self.fx.blur.invalidate();
        let ev = crate::ipc::Event::GameMode {
            active: self.game_mode.is_some(),
            app_id: self.game_mode.as_ref().map(|g| g.app_id.clone()),
            silence: s.game_mode.silence_notifications,
        };
        self.ipc_broadcast(&ev);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn games_are_recognised() {
        let f = |app: &str, title: &str| Facts { app_id: app.into(), title: title.into(), ..Default::default() };
        assert!(looks_like_game(&Facts { desktop_game: true, ..f("org.example.Tetris", "") }));
        assert!(looks_like_game(&Facts { steam: true, ..f("", "") }));
        assert!(looks_like_game(&Facts { wine: true, ..f("", "") }));
        assert!(looks_like_game(&Facts { pointer_locked: true, ..f("", "") }));
        assert!(looks_like_game(&f("steam_app_1091500", "Cyberpunk 2077")));
        assert!(looks_like_game(&f("org.vinegarhq.Sober", "Sober")));
        assert!(looks_like_game(&f("", "Minecraft* 1.21.4")));
        assert!(looks_like_game(&f("eldenring.exe", "")));
    }

    #[test]
    fn full_screen_apps_are_not_games() {
        let f = |app: &str, title: &str| Facts { app_id: app.into(), title: title.into(), ..Default::default() };
        assert!(!looks_like_game(&f("firefox", "YouTube — Mozilla Firefox")));
        assert!(!looks_like_game(&f("mpv", "movie.mkv - mpv")));
        assert!(!looks_like_game(&f("org.kde.okular", "slides.pdf")));
        assert!(!looks_like_game(&f("com.vscodium.codium", "main.rs - orchard")));
    }
}
