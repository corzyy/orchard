//! The user's Orchard settings (`~/.config/orchard/settings.json`, written by
//! `orchard-settings`).
//!
//! Kept in a process-wide store because many readers are free functions
//! (chrome painting, shortcuts). `reload_settings` over IPC re-reads the file;
//! `OrchardState::apply_settings` then pushes the parts that need it into
//! live objects (keyboard, input devices, outputs, wallpaper).

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, RwLock,
};

pub use orchard_config::Settings;

static CURRENT: RwLock<Option<Arc<Settings>>> = RwLock::new(None);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static FOCUS_FOLLOWS_MOUSE: AtomicBool = AtomicBool::new(false);
static ON_BATTERY: AtomicBool = AtomicBool::new(false);

/// Query `/sys/class/power_supply` for whether the machine is running on battery.
fn probe_on_battery() -> bool {
    let Ok(rd) = std::fs::read_dir("/sys/class/power_supply") else {
        return false;
    };
    let mut has_battery = false;
    let mut mains_online = None;
    let mut discharging = false;
    for entry in rd.flatten() {
        let p = entry.path();
        let kind = std::fs::read_to_string(p.join("type")).unwrap_or_default();
        match kind.trim() {
            "Mains" => {
                if let Ok(on) = std::fs::read_to_string(p.join("online")) {
                    mains_online = Some(on.trim() == "1");
                }
            }
            "Battery" => {
                // ignore peripheral (mouse/keyboard) batteries
                if std::fs::read_to_string(p.join("scope")).map(|s| s.trim() == "Device").unwrap_or(false) {
                    continue;
                }
                has_battery = true;
                if std::fs::read_to_string(p.join("status")).map(|s| s.trim() == "Discharging").unwrap_or(false) {
                    discharging = true;
                }
            }
            _ => {}
        }
    }
    if !has_battery {
        return false;
    }
    match mains_online {
        Some(online) => !online,
        None => discharging,
    }
}

/// Refresh cached AC / battery status. Returns true if it changed.
pub fn refresh_power_source() -> bool {
    let now = probe_on_battery();
    let prev = ON_BATTERY.swap(now, Ordering::Relaxed);
    prev != now
}

/// Whether the machine is currently running on battery power (cached).
pub fn on_battery() -> bool {
    ON_BATTERY.load(Ordering::Relaxed)
}

/// Whether live frosted-glass blur is active right now, combining env override,
/// settings, preset, and automatic battery power-saving.
pub fn effective_live_blur() -> bool {
    if !crate::effects::blur::enabled() {
        return false;
    }
    let s = get();
    if !s.performance.live_blur || s.performance.preset == orchard_config::PerformancePreset::PowerSaver {
        return false;
    }
    // a full-screen game covers the panels anyway: no blur passes behind them
    if crate::game_mode::active() {
        return false;
    }
    if s.performance.battery_saver && on_battery() {
        return false;
    }
    true
}

/// Whether low-latency frame pacing is active right now.
pub fn effective_low_latency() -> bool {
    let s = get();
    s.performance.low_latency || s.performance.preset == orchard_config::PerformancePreset::Performance
}

/// The current settings (loaded from disk on first use).
pub fn get() -> Arc<Settings> {
    if let Some(s) = CURRENT.read().unwrap().as_ref() {
        return s.clone();
    }
    reload()
}

/// Re-read the settings file. Returns the new settings.
pub fn reload() -> Arc<Settings> {
    let s = Arc::new(Settings::load());
    FOCUS_FOLLOWS_MOUSE.store(s.windows.focus_follows_mouse, Ordering::Relaxed);
    *CURRENT.write().unwrap() = Some(s.clone());
    GENERATION.fetch_add(1, Ordering::Relaxed);
    crate::chrome::theme::refresh_appearance();
    s
}

/// Bumped on every reload; caches keyed on it are rebuilt after a change.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// Command for ⌘Return (env var wins, then settings, then auto-detected terminal).
pub fn terminal() -> String {
    orchard_config::resolve_terminal(&get().apps.terminal)
}

/// Command used to lock the screen: env var, then settings, then Orchard's own
/// lock screen (`orchard-lock`, next to the compositor binary or on PATH).
pub fn locker() -> String {
    std::env::var("ORCHARD_LOCKER")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let l = get().lock.locker.trim().to_string();
            // empty, or the old swaylock default: use Orchard's lock screen
            if l.is_empty() || l == orchard_config::LEGACY_LOCKER || l == "orchard-lock" {
                default_locker()
            } else {
                l
            }
        })
}

fn default_locker() -> String {
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.join("orchard-lock")))
        .filter(|p| p.is_file());
    match sibling {
        Some(p) => format!("'{}'", p.display().to_string().replace('\'', "'\\''")),
        None => "orchard-lock".into(),
    }
}

use orchard_config::CornerAction;
use smithay::{
    input::keyboard::XkbConfig,
    utils::{Logical, Point},
};

use crate::state::{Backend, OrchardState};

/// File metadata fingerprint `(mtime, size)` used to detect external edits.
type FileStamp = (std::time::SystemTime, u64);

fn settings_file_stamp() -> Option<FileStamp> {
    let meta = std::fs::metadata(orchard_config::settings_path()).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// Runtime state for settings-driven behaviour.
#[derive(Debug)]
pub struct Runtime {
    /// Corner the pointer is currently in (fires once per visit).
    pub corner: Option<Corner>,
    pub last_activity: std::time::Instant,
    /// The idle locker already ran for this idle period.
    pub idle_locked: bool,
    /// Last observed `(mtime, len)` of `settings.json` for live auto-reload.
    pub settings_stamp: Option<FileStamp>,
    /// Snapping gaps last applied, so a change refits tiled/zoomed windows.
    pub tiling_cfg: orchard_config::TilingSettings,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            corner: None,
            last_activity: std::time::Instant::now(),
            idle_locked: false,
            settings_stamp: settings_file_stamp(),
            tiling_cfg: orchard_config::TilingSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// How close (logical px) the pointer must be to count as "in the corner".
const CORNER_SIZE: f64 = 2.0;

impl<BackendData: Backend + 'static> OrchardState<BackendData> {
    /// `reload_settings` IPC request: re-read the file, apply it, tell clients.
    pub fn reload_settings(&mut self) {
        self.prefs_rt.settings_stamp = settings_file_stamp();
        let s = reload();
        tracing::info!("settings reloaded");
        self.apply_settings(&s);
        self.ipc_broadcast(&crate::ipc::Event::SettingsChanged);
    }

    /// Auto-reload `settings.json` when it was edited outside `orchard-settings`.
    pub fn check_settings_file(&mut self) {
        let stamp = settings_file_stamp();
        if stamp.is_some() && stamp != self.prefs_rt.settings_stamp {
            self.reload_settings();
        }
    }

    /// Push settings into live objects. Things read on demand (corner radius,
    /// appearance, shortcuts, double-click, minimize effect, …) need nothing.
    pub fn apply_settings(&mut self, s: &Settings) {
        // keyboard layout + repeat
        if let Some(keyboard) = self.seat.get_keyboard() {
            let kb = crate::wm::keyboard_settings();
            let xkb = XkbConfig {
                rules: "",
                model: &kb.model,
                layout: &kb.layout,
                variant: &kb.variant,
                options: kb.options.clone(),
            };
            if let Err(e) = keyboard.set_xkb_config(self, xkb) {
                tracing::warn!(layout = kb.layout, "keyboard layout from settings rejected: {e:?}");
            }
            keyboard.change_repeat_info(kb.repeat_rate, kb.repeat_delay);
        }

        // mouse / trackpad
        self.backend_data.apply_input_settings(&s.pointer);

        // display scale
        let outputs: Vec<_> = self.space.outputs().cloned().collect();
        let mut scaled = false;
        for output in outputs {
            let Some(&want) = s.displays.scale.get(&output.name()) else { continue };
            if !(0.5..=4.0).contains(&want) {
                continue;
            }
            if (output.current_scale().fractional_scale() - want).abs() > 1e-3 {
                output.change_current_state(None, None, Some(smithay::output::Scale::Fractional(want)), None);
                self.backend_data.reset_buffers(&output);
                scaled = true;
            }
        }
        // refresh rate (at the output's current resolution)
        for output in self.space.outputs().cloned().collect::<Vec<_>>() {
            let Some(&want) = s.displays.refresh.get(&output.name()) else { continue };
            let Some(cur) = output.current_mode() else { continue };
            if (cur.refresh - want).abs() < 500 {
                continue;
            }
            let best = self
                .backend_data
                .output_modes(&output)
                .into_iter()
                .filter(|m| m.size == cur.size)
                .min_by_key(|m| (m.refresh - want).abs());
            if let Some(mode) = best.filter(|m| m.refresh != cur.refresh) {
                if self.backend_data.set_output_mode(&output, mode) {
                    output.change_current_state(Some(mode), None, None, None);
                    self.backend_data.reset_buffers(&output);
                }
            }
        }
        if scaled {
            crate::shell::fixup_positions(&mut self.space, self.pointer.current_location());
        }

        // snapping gaps or display scale changed: refit tiled/zoomed windows
        let tiling = s.tiling.clamped();
        if scaled || tiling != self.prefs_rt.tiling_cfg {
            self.prefs_rt.tiling_cfg = tiling;
            self.refit_all_windows();
        }

        // pointer theme, and the appearance settings apps read
        self.backend_data.reload_cursor();
        crate::session::sync_app_appearance(s);

        // wallpaper & blur cache (live_blur / VRR / direct_scanout may have changed)
        self.fx.wallpaper.reset();
        self.fx.blur.invalidate();
        for output in self.space.outputs().cloned().collect::<Vec<_>>() {
            self.backend_data.reset_buffers(&output);
        }

        self.prefs_rt.idle_locked = false;

        // Night Shift schedule / manual switch / warmth
        let now: std::time::Duration = self.clock.now().into();
        self.fx.night.refresh(now);
    }

    /// Run the action bound to a hot corner, if the pointer just entered one.
    pub fn check_hot_corner(&mut self, pos: Point<f64, Logical>) {
        let Some(output) = self.space.output_under(pos).next().cloned() else {
            self.prefs_rt.corner = None;
            return;
        };
        let Some(g) = self.space.output_geometry(&output) else { return };
        let (l, t) = (g.loc.x as f64, g.loc.y as f64);
        let (r, b) = (l + g.size.w as f64 - 1.0, t + g.size.h as f64 - 1.0);
        let near = |a: f64, b: f64| (a - b).abs() <= CORNER_SIZE;
        let corner = match (near(pos.x, l), near(pos.x, r), near(pos.y, t), near(pos.y, b)) {
            (true, _, true, _) => Some(Corner::TopLeft),
            (_, true, true, _) => Some(Corner::TopRight),
            (true, _, _, true) => Some(Corner::BottomLeft),
            (_, true, _, true) => Some(Corner::BottomRight),
            _ => None,
        };
        if corner == self.prefs_rt.corner {
            return;
        }
        self.prefs_rt.corner = corner;
        let Some(corner) = corner else { return };
        // don't fire while dragging a window or holding a button, nor in a
        // game (flicking the mouse to a corner is part of playing)
        if self.pointer.is_grabbed() || crate::game_mode::active() {
            return;
        }
        let hc = &get().desktop.hot_corners;
        let action = match corner {
            Corner::TopLeft => hc.top_left,
            Corner::TopRight => hc.top_right,
            Corner::BottomLeft => hc.bottom_left,
            Corner::BottomRight => hc.bottom_right,
        };
        let name = match action {
            CornerAction::None => return,
            CornerAction::MissionControl => "mission-control",
            CornerAction::AppExpose => "app-expose",
            CornerAction::Launcher => "launcher",
            CornerAction::LockScreen => "lock",
            CornerAction::Launchpad => "launchpad",
        };
        let name = name.to_string();
        self.handle.insert_idle(move |state| state.run_named_action(&name));
    }

    /// After every pointer motion: hot corners, focus follows mouse.
    pub fn after_pointer_motion(&mut self, pos: Point<f64, Logical>) {
        if get().pointer.shake_to_locate
            && !self.pointer.is_grabbed()
            && !matches!(self.cursor_status, smithay::input::pointer::CursorImageStatus::Hidden)
        {
            let now: std::time::Duration = self.clock.now().into();
            self.fx.shake.motion(pos, now);
        }
        self.update_fs_reveal(pos);
        self.fs_reveal_cursor(pos);
        self.check_hot_corner(pos);
        if FOCUS_FOLLOWS_MOUSE.load(Ordering::Relaxed) {
            self.focus_under_pointer(pos);
        }
    }

    /// Called on every input event.
    pub fn note_activity(&mut self) {
        self.prefs_rt.last_activity = std::time::Instant::now();
        self.prefs_rt.idle_locked = false;
    }

    /// Periodic timer: start the locker after the configured idle time.
    pub fn check_idle_lock(&mut self) {
        let minutes = get().lock.lock_after_minutes;
        // a game played with a controller sends no keyboard / mouse input
        if minutes == 0
            || self.prefs_rt.idle_locked
            || self.is_locked()
            || !self.protocols.idle_inhibitors.is_empty()
            || crate::game_mode::active()
        {
            return;
        }
        if self.prefs_rt.last_activity.elapsed().as_secs() >= minutes as u64 * 60 {
            self.prefs_rt.idle_locked = true;
            tracing::info!("idle for {minutes} min, locking");
            self.spawn(&locker());
        }
    }

    /// Start a 15 s timer that drives the idle lock and AC/battery detection.
    pub fn init_settings_timers(&mut self) {
        use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
        refresh_power_source();
        let now: std::time::Duration = self.clock.now().into();
        self.fx.night.refresh(now);
        let res = self.handle.insert_source(Timer::from_duration(std::time::Duration::from_secs(15)), |_, _, state| {
            state.check_idle_lock();
            let now: std::time::Duration = state.clock.now().into();
            state.fx.night.refresh(now);
            if refresh_power_source() && get().performance.battery_saver {
                state.fx.blur.invalidate();
                for output in state.space.outputs().cloned().collect::<Vec<_>>() {
                    state.backend_data.reset_buffers(&output);
                }
            }
            TimeoutAction::ToDuration(std::time::Duration::from_secs(15))
        });
        if let Err(e) = res {
            tracing::warn!("idle timer: {e}");
        }
        let res = self.handle.insert_source(Timer::from_duration(std::time::Duration::from_secs(1)), |_, _, state| {
            state.supervise_shell();
            state.supervise_locker();
            state.check_settings_file();
            // nothing repaints while idle: catch a game that went full screen meanwhile
            state.update_game_mode();
            TimeoutAction::ToDuration(std::time::Duration::from_secs(1))
        });
        if let Err(e) = res {
            tracing::warn!("shell supervisor timer: {e}");
        }
        // Re-apply pointer settings after startup: wireless receivers (Logitech
        // Lightspeed etc.) can re-create their input devices once the HID++
        // driver binds, and anything that slipped past DeviceAdded gets fixed here.
        for secs in [3u64, 10] {
            let _ = self.handle.insert_source(Timer::from_duration(std::time::Duration::from_secs(secs)), |_, _, state| {
                let prefs = get().pointer.clone();
                state.backend_data.apply_input_settings(&prefs);
                TimeoutAction::Drop
            });
        }
    }

    /// Xwayland is up (or failed / timed out): publish the session environment
    /// and run the autostart entries. Runs once.
    pub fn on_session_ready(&mut self) {
        if self.session_ready {
            return;
        }
        self.session_ready = true;
        if BackendData::IS_SESSION {
            crate::session::sync_app_appearance(&get());
            let mut extra: Vec<(&str, String)> = Vec::new();
            if let Some(s) = &self.socket_name {
                extra.push(("WAYLAND_DISPLAY", s.clone()));
            }
            #[cfg(feature = "xwayland")]
            if let Some(d) = self.xdisplay {
                extra.push(("DISPLAY", format!(":{d}")));
            }
            if let Some(p) = &self.ipc.path {
                extra.push(("ORCHARD_SOCKET", p.display().to_string()));
            }
            crate::session::start(crate::session::exported_env(&extra), || {});
        }
        self.run_autostart();
    }

    /// Launch `~/.config/autostart` + `/etc/xdg/autostart` entries (once, at startup).
    pub fn run_autostart(&self) {
        if !get().apps.run_autostart || std::env::var_os("ORCHARD_NO_AUTOSTART").is_some() {
            return;
        }
        // programs systemd starts itself as part of graphical-session.target
        // (e.g. kunifiedpush-distributor): launching them again makes the
        // second copy fail on its bus name and leaves a failed unit behind
        let mut systemd_programs = None;
        for item in orchard_config::desktop::autostart_items() {
            if !item.entry.autostarts_in_orchard() {
                continue;
            }
            let Some(cmd) = item.entry.command() else { continue };
            if item.entry.systemd_skip() {
                let programs = systemd_programs.get_or_insert_with(crate::session::session_unit_programs);
                if programs.contains(crate::session::program_name(&cmd)) {
                    tracing::info!("autostart: {cmd} skipped (started by a systemd unit)");
                    continue;
                }
            }
            tracing::info!("autostart: {cmd}");
            self.spawn_autostart(&cmd);
        }
    }
}
