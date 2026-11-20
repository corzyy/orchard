//! Non-blocking background tasks and system command helpers.
//!
//! Workers notify the UI through a [`Wake`] channel; the app turns each
//! notification into a `Message::Wake` (see [`wake_subscription`]), so the
//! UI updates as soon as data arrives without polling.

use std::{
    process::Command,
    sync::{mpsc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use iced::futures::{
    channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender},
    stream::{self, BoxStream, StreamExt},
};

/// Handle to wake the UI from any background thread.
#[derive(Clone)]
pub struct Wake(UnboundedSender<()>);

impl Wake {
    pub fn notify(&self) {
        let _ = self.0.unbounded_send(());
    }
}

fn receiver() -> &'static Mutex<Option<UnboundedReceiver<()>>> {
    static RX: OnceLock<Mutex<Option<UnboundedReceiver<()>>>> = OnceLock::new();
    RX.get_or_init(|| Mutex::new(None))
}

/// Creates the process-wide wake channel. Call once at startup.
pub fn wake_channel() -> Wake {
    let (tx, rx) = unbounded();
    *receiver().lock().unwrap_or_else(|e| e.into_inner()) = Some(rx);
    Wake(tx)
}

/// Emits `msg` whenever a background worker calls [`Wake::notify`].
pub fn wake_subscription<M: Clone + Send + 'static>(msg: M) -> iced::Subscription<M> {
    struct Id;
    // iced keeps the first stream registered under this id alive; later
    // calls build an empty stream that is discarded.
    let rx: BoxStream<'static, ()> = match receiver().lock().unwrap_or_else(|e| e.into_inner()).take() {
        Some(rx) => rx.boxed(),
        None => stream::empty().boxed(),
    };
    iced::Subscription::run_with_id(std::any::TypeId::of::<Id>(), rx.map(move |_| msg.clone()))
}

/// Runs `f` on a new thread and resolves with its result.
pub fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> impl std::future::Future<Output = Option<T>> {
    let (tx, rx) = iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    async move { rx.await.ok() }
}

/// stdout of a successful command (C locale).
pub fn output(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).env("LC_ALL", "C").output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// stdout of a successful command keeping user locale.
pub fn output_localized(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Run a command; Ok(stdout) or Err(first line of stderr/stdout).
pub fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
    match Command::new(cmd).args(args).env("LC_ALL", "C").output() {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            let out = String::from_utf8_lossy(&o.stdout);
            let msg = err
                .lines()
                .chain(out.lines())
                .find(|l| !l.trim().is_empty())
                .unwrap_or("failed");
            Err(msg.trim().to_string())
        }
        Err(e) => Err(format!("{cmd}: {e}")),
    }
}

pub fn spawn_detached(cmd: &str, args: &[&str]) {
    let _ = Command::new(cmd).args(args).spawn();
}

pub fn which(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
        .unwrap_or(false)
}

pub fn read_trim(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string())
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

/// A value computed on a background thread.
pub struct Bg<T> {
    pub value: Option<T>,
    rx: Option<mpsc::Receiver<T>>,
    last: Option<Instant>,
}

impl<T> Default for Bg<T> {
    fn default() -> Self {
        Self {
            value: None,
            rx: None,
            last: None,
        }
    }
}

impl<T: Send + 'static> Bg<T> {
    pub fn poll(&mut self) -> bool {
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok(v) => {
                    self.value = Some(v);
                    self.rx = None;
                    return true;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        false
    }

    pub fn busy(&self) -> bool {
        self.rx.is_some()
    }

    pub fn start(&mut self, wake: &Wake, f: impl FnOnce() -> T + Send + 'static) {
        if self.rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let wake = wake.clone();
        std::thread::spawn(move || {
            let _ = tx.send(f());
            wake.notify();
        });
        self.rx = Some(rx);
        self.last = Some(Instant::now());
    }

    /// Ensure data is loaded, refreshing if older than `ttl`.
    pub fn ensure(&mut self, wake: &Wake, ttl: Duration, f: impl FnOnce() -> T + Send + 'static) {
        self.poll();
        let stale = self.last.map(|l| l.elapsed() > ttl).unwrap_or(true);
        if stale {
            self.start(wake, f);
        }
    }

    pub fn invalidate(&mut self) {
        self.last = None;
    }
}

pub type Job = Bg<Result<String, String>>;

impl Job {
    pub fn run_cmd(&mut self, wake: &Wake, cmd: &str, args: &[&str]) {
        let cmd = cmd.to_string();
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        self.value = None;
        self.start(wake, move || {
            let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            run(&cmd, &a)
        });
    }

    pub fn error(&mut self) -> Option<String> {
        self.poll();
        match &self.value {
            Some(Err(e)) => Some(e.clone()),
            _ => None,
        }
    }

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
