//! Crash reporting.
//!
//! A panic in the compositor ends the whole graphical session, so the one thing we
//! can still do is leave a precise record behind: the panic message, its location and a
//! full backtrace go to the log (`~/.local/state/orchard/orchard.log` in a real session)
//! and to a separate `crash-<unix time>.txt` next to it, which survives the log rotation
//! done by `dist/orchard-session` at the next login.

use std::{
    backtrace::Backtrace,
    io::Write,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

/// Directory for logs and crash reports (`$XDG_STATE_HOME/orchard`).
pub fn state_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .map(|p| p.join("orchard"))
}

pub fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".into());
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".into());
        let thread = std::thread::current();
        let thread = thread.name().unwrap_or("<unnamed>");
        let backtrace = Backtrace::force_capture();

        tracing::error!(%location, thread, "PANIC: {payload}");
        let report = format!(
            "orchard {} panicked\nthread: {thread}\nlocation: {location}\nmessage: {payload}\n\nbacktrace:\n{backtrace}\n",
            env!("CARGO_PKG_VERSION"),
        );
        // stderr is the log file in a real session
        let _ = writeln!(std::io::stderr(), "{report}");

        if let Some(dir) = state_dir() {
            let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let path = dir.join(format!("crash-{secs}.txt"));
            if std::fs::create_dir_all(&dir).is_ok() && std::fs::write(&path, &report).is_ok() {
                let _ = writeln!(std::io::stderr(), "crash report written to {}", path.display());
            }
        }

        // keep the default behaviour (e.g. RUST_BACKTRACE handling for debuggers)
        if std::env::var_os("ORCHARD_DEFAULT_PANIC_HOOK").is_some() {
            default_hook(info);
        }
    }));
}
