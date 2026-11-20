//! Main-loop watchdog for the real (udev) session.
//!
//! A compositor that stops dispatching (deadlock, endless loop, a driver call
//! that never returns) also stops handling Ctrl+Alt+F-keys, so the only way
//! out is the power button. The watchdog thread notices when the event loop
//! has not ticked for [`STALL_LIMIT`], writes the stacks of all threads
//! (`eu-stack`) to `$XDG_STATE_HOME/orchard/hang-<unix time>.txt` and the log,
//! and then aborts, which ends the session and returns to the login screen.
//!
//! `ORCHARD_NO_WATCHDOG=1` turns it off (e.g. while a debugger is attached).

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

/// How long the loop may go without a tick. The loop dispatches with a
/// 500 ms timeout, so a healthy compositor ticks at least twice a second.
const STALL_LIMIT: Duration = Duration::from_secs(10);

static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
static LAST_TICK_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Call once per event-loop iteration.
pub fn tick() {
    LAST_TICK_MS.store(now_ms(), Ordering::Relaxed);
}

/// Start the watchdog thread (no-op when disabled).
pub fn start() {
    if std::env::var_os("ORCHARD_NO_WATCHDOG").is_some_and(|v| v != "0") {
        tracing::info!("watchdog disabled by ORCHARD_NO_WATCHDOG");
        return;
    }
    tick();
    let spawned = std::thread::Builder::new()
        .name("orchard-watchdog".into())
        .spawn(|| loop {
            std::thread::sleep(Duration::from_secs(1));
            let stalled = now_ms().saturating_sub(LAST_TICK_MS.load(Ordering::Relaxed));
            if stalled >= STALL_LIMIT.as_millis() as u64 {
                report_and_abort(stalled);
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("watchdog thread could not start: {e}");
    }
}

fn report_and_abort(stalled_ms: u64) -> ! {
    let pid = std::process::id();
    let stacks = std::process::Command::new("eu-stack")
        .args(["-p", &pid.to_string(), "-s", "-m"])
        .output()
        .map(|o| {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            s
        })
        .unwrap_or_else(|e| format!("eu-stack failed: {e}"));
    let dir = crate::crash::state_dir().unwrap_or_else(|| std::path::PathBuf::from("/tmp/orchard"));
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("hang-{stamp}.txt"));
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(&path, format!("main loop stalled for {stalled_ms} ms\n\n{stacks}"));
    tracing::error!(
        stalled_ms,
        report = %path.display(),
        "watchdog: main loop is stuck, aborting so the session can end\n{stacks}"
    );
    std::process::abort();
}
