//! Desktop-session plumbing for the real (`--tty`) session.
//!
//! Without this, nothing started through D-Bus or systemd knows about Orchard:
//! `xdg-desktop-portal` is `Requisite=graphical-session.target` and refuses to
//! start ("Dependency failed"), so file dialogs, Flatpak integration and the
//! appearance (dark mode) portal are all missing. On startup we
//!
//! 1. export the session environment (WAYLAND_DISPLAY, DISPLAY, …) to the
//!    systemd user manager and the D-Bus activation environment,
//! 2. start `orchard-session.target`, which binds `graphical-session.target`,
//! 3. make sure a portal configuration for `XDG_CURRENT_DESKTOP=Orchard` exists,
//!
//! and undo 1 + 2 on a clean exit. Headless / nested runs never touch the user's
//! session (they could be running inside Plasma).
//!
//! Apps are themed from Orchard's settings: the session gets its own dconf
//! database (`DCONF_PROFILE`, seeded once from the user database), so the icon /
//! pointer theme and dark mode Orchard writes there reach GTK apps and the
//! settings portal without changing other sessions; Qt apps use the GTK platform
//! theme (which reads the same settings).

use std::{
    collections::HashSet,
    path::PathBuf,
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

/// set once we exported the environment and started the target, so `stop`
/// only undoes what we did
static ACTIVE: AtomicBool = AtomicBool::new(false);

const TARGET: &str = "orchard-session.target";

const TARGET_UNIT: &str = "\
[Unit]
Description=Orchard session
BindsTo=graphical-session.target
Wants=graphical-session-pre.target
After=graphical-session-pre.target
PropagatesStopTo=graphical-session.target
";

/// Portal backends for XDG_CURRENT_DESKTOP=Orchard. GTK handles file dialogs
/// and `org.gnome.desktop.interface color-scheme` (which Orchard keeps in sync);
/// Hyprland's portal handles ScreenCast/Screenshot via wlr-screencopy.
/// First line of a portal config Orchard may replace with a newer version.
const MANAGED: &str = "# Written by Orchard (managed: delete this line to keep your edits).";

const PORTALS_CONF: &str = "\
# Written by Orchard (managed: delete this line to keep your edits).
[preferred]
default=gtk
org.freedesktop.impl.portal.Settings=gtk
org.freedesktop.impl.portal.Secret=gnome-keyring
org.freedesktop.impl.portal.ScreenCast=hyprland
org.freedesktop.impl.portal.Screenshot=hyprland
";

/// Variables exported to systemd / D-Bus activated services.
const EXPORTED: &[&str] = &[
    "WAYLAND_DISPLAY",
    "DISPLAY",
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_TYPE",
    "XDG_SESSION_DESKTOP",
    "ORCHARD_SOCKET",
    "QT_QPA_PLATFORMTHEME",
    "QT_STYLE_OVERRIDE",
    "DCONF_PROFILE",
    "XCURSOR_THEME",
    "XCURSOR_SIZE",
];

/// dconf database name (~/.config/dconf/orchard) used inside the session.
const DCONF_DB: &str = "orchard";

fn dconf_profile_path() -> PathBuf {
    config_home().join("orchard/dconf-profile")
}

/// True when this process uses Orchard's own dconf database, i.e. writing
/// `org.gnome.desktop.interface` keys can't change another desktop's settings.
fn owns_dconf() -> bool {
    std::env::var_os("DCONF_PROFILE").map(PathBuf::from) == Some(dconf_profile_path())
}

/// Point dconf at Orchard's database. The first time, it starts as a copy of
/// the user database so fonts, GTK theme etc. carry over.
fn setup_dconf() -> bool {
    let profile = dconf_profile_path();
    let contents = format!("user-db:{DCONF_DB}\nsystem-db:local\nsystem-db:site\nsystem-db:distro\n");
    ensure_file(&profile, &contents, true);
    if !profile.is_file() {
        return false;
    }
    let db = config_home().join("dconf").join(DCONF_DB);
    if !db.exists() {
        let user = config_home().join("dconf/user");
        if user.is_file() {
            match std::fs::copy(&user, &db) {
                Ok(_) => tracing::info!("session: seeded {} from {}", db.display(), user.display()),
                Err(e) => tracing::warn!("session: seeding {}: {e}", db.display()),
            }
        }
    }
    std::env::set_var("DCONF_PROFILE", &profile);
    true
}

fn set_cursor_env(s: &orchard_config::Settings) -> bool {
    let theme = s.appearance.cursor_theme().to_string();
    let size = s.appearance.cursor_size().to_string();
    let changed = std::env::var("XCURSOR_THEME").ok().as_deref() != Some(theme.as_str())
        || std::env::var("XCURSOR_SIZE").ok().as_deref() != Some(size.as_str());
    std::env::set_var("XCURSOR_THEME", theme);
    std::env::set_var("XCURSOR_SIZE", size);
    changed
}

/// Push Orchard's appearance settings to apps: dconf (GTK, GTK-themed Qt, the
/// settings portal) and the XCURSOR_* variables. Only in the real session, where
/// the dconf database is Orchard's own.
pub fn sync_app_appearance(s: &orchard_config::Settings) {
    if !owns_dconf() {
        return;
    }
    let cursor_changed = set_cursor_env(s);
    let a = &s.appearance;
    let mut keys: Vec<(&str, String)> = vec![
        ("icon-theme", a.icon_theme().to_string()),
        ("cursor-theme", a.cursor_theme().to_string()),
        ("cursor-size", a.cursor_size().to_string()),
    ];
    match a.mode {
        orchard_config::ThemeMode::Dark => keys.push(("color-scheme", "prefer-dark".into())),
        orchard_config::ThemeMode::Light => keys.push(("color-scheme", "default".into())),
        orchard_config::ThemeMode::Auto => {}
    }
    let export: Vec<String> = if cursor_changed && ACTIVE.load(Ordering::SeqCst) {
        ["XCURSOR_THEME", "XCURSOR_SIZE"]
            .iter()
            .filter_map(|k| std::env::var(k).ok().map(|v| format!("{k}={v}")))
            .collect()
    } else {
        Vec::new()
    };
    let _ = std::thread::Builder::new().name("app-appearance".into()).spawn(move || {
        for (k, v) in keys {
            let cur = Command::new("gsettings")
                .args(["get", "org.gnome.desktop.interface", k])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().trim_matches('\'').to_string());
            if cur.as_deref() != Some(v.as_str()) {
                run(Command::new("gsettings").args(["set", "org.gnome.desktop.interface", k, &v]));
            }
        }
        if !export.is_empty() {
            run(Command::new("dbus-update-activation-environment").arg("--systemd").args(&export));
        }
    });
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

fn data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".local/share"))
}

fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".config"))
}

/// Defaults that make apps fit in, set before anything is spawned. Called once
/// from the udev backend; never overrides a value the user set.
pub fn set_env_defaults() {
    let defaults: &[(&str, &str)] = &[
        ("XDG_CURRENT_DESKTOP", "Orchard"),
        ("XDG_SESSION_DESKTOP", "orchard"),
        ("XDG_SESSION_TYPE", "wayland"),
    ];
    for (k, v) in defaults {
        if std::env::var_os(k).is_none() {
            std::env::set_var(k, v);
        }
    }
    // Apps follow Orchard's settings, kept in Orchard's own dconf database.
    let own_dconf = std::env::var_os("DCONF_PROFILE").is_none() && setup_dconf();
    set_cursor_env(&crate::settings::get());
    // Qt: the GTK platform theme reads icon theme, dark mode and fonts from
    // Orchard's dconf database without needing any KDE theme packages.
    let plugin = |dir: &str, file: &str| {
        ["/usr/lib64/qt6/plugins", "/usr/lib/qt6/plugins"]
            .iter()
            .any(|base| std::path::Path::new(base).join(dir).join(file).exists())
    };
    if own_dconf && std::env::var_os("QT_QPA_PLATFORMTHEME").is_none() && plugin("platformthemes", "libqgtk3.so") {
        std::env::set_var("QT_QPA_PLATFORMTHEME", "gtk3");
    }
}

/// Write `path` if it doesn't exist yet (or differs, when `overwrite`).
/// Returns true when the file was (re)written.
fn ensure_file(path: &std::path::Path, contents: &str, overwrite: bool) -> bool {
    match std::fs::read_to_string(path) {
        Ok(cur) if cur == contents || !overwrite => return false,
        _ => {}
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match std::fs::write(path, contents) {
        Ok(()) => {
            tracing::info!("session: wrote {}", path.display());
            true
        }
        Err(e) => {
            tracing::warn!("session: writing {} failed: {e}", path.display());
            false
        }
    }
}

fn run(cmd: &mut Command) {
    let desc = format!("{cmd:?}");
    match cmd.output() {
        Ok(out) if out.status.success() => tracing::debug!("session: {desc} ok"),
        Ok(out) => tracing::warn!(
            "session: {desc} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => tracing::warn!("session: {desc}: {e}"),
    }
}

/// Export `env` and start the session target. Runs on a helper thread: systemctl
/// can take a moment and the compositor must keep drawing meanwhile.
/// `then` runs afterwards on that thread (used to signal the main loop).
pub fn start(env: Vec<(String, String)>, then: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .name("session-start".into())
        .spawn(move || {
            // Another graphical session of this user (e.g. Plasma on another VT)
            // shares the systemd user manager: don't take over its environment,
            // and never stop its graphical-session.target on our way out.
            let is_active = |unit: &str| {
                Command::new("systemctl")
                    .args(["--user", "is-active", "--quiet", unit])
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false)
            };
            // still active from an earlier Orchard that crashed (lingering user manager): ours, take it over
            let other = is_active("graphical-session.target") && !is_active(TARGET);
            if other {
                tracing::warn!("session: another graphical session is active; not exporting the environment");
                then();
                return;
            }
            ACTIVE.store(true, Ordering::SeqCst);
            let unit_path = data_home().join("systemd/user").join(TARGET);
            let unit_changed = ensure_file(&unit_path, TARGET_UNIT, true);
            // screen sharing goes through xdg-desktop-portal-hyprland, which talks
            // wlr-screencopy (screencopy.rs); replace older Orchard-written versions
            let portals = config_home().join("xdg-desktop-portal/orchard-portals.conf");
            let ours = std::fs::read_to_string(&portals).map(|c| c.starts_with(MANAGED)).unwrap_or(true);
            let portals_changed = ensure_file(&portals, PORTALS_CONF, ours);

            let assignments: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();
            // --systemd updates both the D-Bus activation env and the systemd user manager
            run(Command::new("dbus-update-activation-environment").arg("--systemd").args(&assignments));
            if unit_changed {
                run(Command::new("systemctl").args(["--user", "daemon-reload"]));
            }
            // a portal that failed earlier in this user manager's life stays failed;
            // a running one has to re-read a changed portal config
            run(Command::new("systemctl").args(["--user", "reset-failed", "xdg-desktop-portal.service"]));
            if portals_changed {
                run(Command::new("systemctl").args(["--user", "try-restart", "xdg-desktop-portal.service"]));
            }
            run(Command::new("systemctl").args(["--user", "start", TARGET]));
            tracing::info!("session: {TARGET} started");
            then();
        })
        .map(|_| ())
        .unwrap_or_else(|e| tracing::warn!("session: thread: {e}"));
}

/// Undo `start`: stop the target (and with it the portals) and remove our
/// variables from the user manager, so a later Plasma login starts clean.
pub fn stop() {
    if !ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    run(Command::new("systemctl").args(["--user", "--no-block", "stop", TARGET]));
    run(Command::new("systemctl").args(["--user", "unset-environment"]).args(EXPORTED));
}

/// The variables to export, with their current values.
pub fn exported_env(extra: &[(&str, String)]) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = EXPORTED
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
        .collect();
    for (k, v) in extra {
        env.retain(|(ek, _)| ek != k);
        env.push((k.to_string(), v.clone()));
    }
    env
}

/// Executable name of a shell command line (`/usr/bin/foo --bar` -> `foo`).
pub fn program_name(command: &str) -> &str {
    let first = command.split_whitespace().next().unwrap_or("");
    first.rsplit('/').next().unwrap_or(first)
}

/// Executable names of the units systemd starts with the graphical session
/// (`Wants=` of graphical-session.target and our target), so autostart entries
/// marked `X-systemd-skip=true` that one of them covers are not run twice.
pub fn session_unit_programs() -> HashSet<String> {
    let show = |args: &[&str]| -> String {
        Command::new("systemctl")
            .args(["--user", "show"])
            .args(args)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    let wants = show(&["-p", "Wants", "--value", "graphical-session.target", TARGET]);
    let units: Vec<&str> = wants.split_whitespace().filter(|u| u.ends_with(".service")).collect();
    if units.is_empty() {
        return HashSet::new();
    }
    let mut args = vec!["-p", "ExecStart", "--value"];
    args.extend(&units);
    // ExecStart prints as `{ path=/usr/bin/foo ; argv[]=… }`
    show(&args)
        .split_whitespace()
        .filter_map(|w| w.strip_prefix("path="))
        .map(|p| program_name(p).to_string())
        .collect()
}

/// process groups of autostarted programs (their pgid = the `sh -c` pid)
static AUTOSTARTED: Mutex<Vec<i32>> = Mutex::new(Vec::new());

pub fn track_autostarted(pid: u32) {
    AUTOSTARTED.lock().unwrap().push(pid as i32);
}

/// Terminate everything `spawn_autostart` started (whole process groups, so
/// daemons that forked are included): SIGTERM, then SIGKILL after 1 s.
pub fn stop_autostarted() {
    let groups: Vec<i32> = std::mem::take(&mut *AUTOSTARTED.lock().unwrap());
    let alive = |g: i32| unsafe { libc::kill(-g, 0) } == 0;
    let groups: Vec<i32> = groups.into_iter().filter(|&g| g > 1 && alive(g)).collect();
    if groups.is_empty() {
        return;
    }
    for &g in &groups {
        unsafe { libc::kill(-g, libc::SIGTERM) };
    }
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline && groups.iter().any(|&g| alive(g)) {
        std::thread::sleep(Duration::from_millis(20));
    }
    for &g in groups.iter().filter(|&&g| alive(g)) {
        unsafe { libc::kill(-g, libc::SIGKILL) };
    }
    tracing::info!("session: stopped {} autostarted process group(s)", groups.len());
}

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn on_shutdown_signal(_: libc::c_int) {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

/// SIGTERM / SIGHUP / SIGINT (logout through the display manager, shutdown)
/// end the main loop normally, so the session cleanup runs instead of the
/// default action killing the compositor on the spot.
pub fn install_signal_handlers() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_shutdown_signal as *const () as usize;
        sa.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut sa.sa_mask);
        for sig in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
            libc::sigaction(sig, &sa, std::ptr::null_mut());
        }
    }
}

pub fn shutdown_requested() -> bool {
    SHUTDOWN.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_names() {
        assert_eq!(program_name("kunifiedpush-distributor"), "kunifiedpush-distributor");
        assert_eq!(program_name("/usr/libexec/kf6/polkit-kde-authentication-agent-1 --x"), "polkit-kde-authentication-agent-1");
        assert_eq!(program_name(""), "");
    }

    #[test]
    fn autostarted_groups_are_stopped() {
        use std::os::unix::process::CommandExt;
        // a daemon-like child that forks a grandchild and outlives its parent shell
        let mut child = Command::new("sh")
            .args(["-c", "sleep 300 & sleep 300"])
            .process_group(0)
            .spawn()
            .unwrap();
        let pgid = child.id() as i32;
        track_autostarted(child.id());
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(unsafe { libc::kill(-pgid, 0) }, 0);
        stop_autostarted();
        let _ = child.wait();
        std::thread::sleep(Duration::from_millis(100));
        assert_ne!(unsafe { libc::kill(-pgid, 0) }, 0, "group still alive");
    }
}
