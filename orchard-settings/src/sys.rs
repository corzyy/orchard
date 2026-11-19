//! Running system tools (nmcli, pactl, bluetoothctl, timedatectl, …) without
//! blocking the UI.

use std::{
    process::Command,
    sync::mpsc,
    time::{Duration, Instant},
};

use eframe::egui;

/// stdout of a successful command.
pub fn output(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).env("LC_ALL", "C").output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Like `output`, but keeps the user's locale (for human-readable names).
pub fn output_localized(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Run a command; Ok(stdout) or Err(first line of stderr).
pub fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
    match Command::new(cmd).args(args).env("LC_ALL", "C").output() {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            let out = String::from_utf8_lossy(&o.stdout);
            let msg = err.lines().chain(out.lines()).find(|l| !l.trim().is_empty()).unwrap_or("failed");
            Err(msg.trim().to_string())
        }
        Err(e) => Err(format!("{cmd}: {e}")),
    }
}

/// Start a detached program (e.g. a terminal).
pub fn spawn_detached(cmd: &str, args: &[&str]) {
    let _ = Command::new(cmd).args(args).spawn();
}

/// A value computed on a worker thread.
pub struct Bg<T> {
    pub value: Option<T>,
    rx: Option<mpsc::Receiver<T>>,
    last: Option<Instant>,
}

impl<T> Default for Bg<T> {
    fn default() -> Self {
        Bg {
            value: None,
            rx: None,
            last: None,
        }
    }
}

impl<T: Send + 'static> Bg<T> {
    pub fn poll(&mut self) {
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok(v) => {
                    self.value = Some(v);
                    self.rx = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => self.rx = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
    }

    pub fn busy(&self) -> bool {
        self.rx.is_some()
    }

    pub fn start(&mut self, ctx: &egui::Context, f: impl FnOnce() -> T + Send + 'static) {
        if self.rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(f());
            ctx.request_repaint();
        });
        self.rx = Some(rx);
        self.last = Some(Instant::now());
    }

    /// Refresh when older than `secs` (or never loaded).
    pub fn every(&mut self, ctx: &egui::Context, secs: f32, f: impl FnOnce() -> T + Send + 'static) {
        self.poll();
        let stale = self.last.map(|l| l.elapsed().as_secs_f32() > secs).unwrap_or(true);
        if stale {
            self.start(ctx, f);
        }
        ctx.request_repaint_after(Duration::from_secs_f32(secs.max(0.2)));
    }

    /// Force a reload on the next `every`.
    pub fn invalidate(&mut self) {
        self.last = None;
    }
}

/// A background action with a visible result.
pub type Job = Bg<Result<String, String>>;

impl Job {
    pub fn run_cmd(&mut self, ctx: &egui::Context, cmd: &str, args: &[&str]) {
        let cmd = cmd.to_string();
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        self.value = None;
        self.start(ctx, move || {
            let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            run(&cmd, &a)
        });
    }

    /// Error message to show, if the last action failed.
    pub fn error(&mut self) -> Option<String> {
        self.poll();
        match &self.value {
            Some(Err(e)) => Some(e.clone()),
            _ => None,
        }
    }

    /// True once after a job finished (successfully or not).
    pub fn take_done(&mut self) -> bool {
        self.poll();
        if self.rx.is_none() && self.last.is_some() && self.value.is_some() {
            self.last = None;
            if matches!(self.value, Some(Ok(_))) {
                self.value = None;
            }
            return true;
        }
        false
    }
}

/// Split one `nmcli -t` line, honouring `\:` escapes.
pub fn nm_fields(line: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    out.last_mut().unwrap().push(n);
                }
            }
            ':' => out.push(String::new()),
            c => out.last_mut().unwrap().push(c),
        }
    }
    out
}

pub fn read_trim(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}
