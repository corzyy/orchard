//! Polkit authentication agent (`org.freedesktop.PolicyKit1.AuthenticationAgent`)
//! with a macOS-style authentication sheet.
//!
//! Registers on the system D-Bus for the current logind session and spawns
//! `polkit-agent-helper-1` to verify credentials via PAM.

use std::{
    collections::HashMap,
    future::Future,
    io::{BufRead, BufReader, Write},
    path::Path,
    pin::Pin,
    process::{Child, Command, Stdio},
    sync::{
        mpsc,
        Arc, Mutex,
    },
    task::{Context, Poll, Waker},
    time::{Duration, Instant},
};

use smithay_client_toolkit::reexports::calloop::channel::Sender;
use tiny_skia::{
    Color, FillRule, GradientStop, LineCap, LineJoin, LinearGradient, Paint, PathBuilder, Pixmap,
    PixmapPaint, Point, SpreadMode, Stroke, Transform,
};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::{
    apps::Apps,
    draw::{fill_circle, fill_rounded, paint, rounded_rect, stroke_rounded},
    text::{Fonts, Rgba, Weight},
    theme::Theme,
};

pub const PATH: &str = "/org/orchard/PolicyKit1/AuthenticationAgent";
const AUTHORITY_BUS: &str = "org.freedesktop.PolicyKit1";
const AUTHORITY_PATH: &str = "/org/freedesktop/PolicyKit1/Authority";
const AUTHORITY_IFACE: &str = "org.freedesktop.PolicyKit1.Authority";

const HELPER_PATHS: &[&str] = &[
    "/usr/lib/polkit-1/polkit-agent-helper-1",
    "/usr/libexec/polkit-1/polkit-agent-helper-1",
    "/usr/lib/policykit-1/polkit-agent-helper-1",
];

const WIDTH: f32 = 360.0;
const RADIUS: f32 = 16.0;
const SHAKE_SECS: f32 = 0.42;

// ---- Async oneshot (zero extra dependencies, safe on zbus' executor) ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Authenticated,
    Cancelled,
}

struct OneshotState {
    result: Option<Outcome>,
    waker: Option<Waker>,
}

pub struct Responder {
    state: Arc<Mutex<OneshotState>>,
}

impl Responder {
    pub fn finish(self, outcome: Outcome) {
        complete_state(&self.state, outcome);
    }
}

impl Drop for Responder {
    fn drop(&mut self) {
        complete_state(&self.state, Outcome::Cancelled);
    }
}

fn complete_state(state: &Mutex<OneshotState>, outcome: Outcome) {
    let mut g = state.lock().unwrap();
    if g.result.is_none() {
        g.result = Some(outcome);
        if let Some(w) = g.waker.take() {
            w.wake();
        }
    }
}

struct OneshotFuture {
    state: Arc<Mutex<OneshotState>>,
}

impl Future for OneshotFuture {
    type Output = Outcome;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut g = self.state.lock().unwrap();
        if let Some(res) = g.result {
            Poll::Ready(res)
        } else {
            g.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

fn oneshot() -> (Responder, Arc<Mutex<OneshotState>>, OneshotFuture) {
    let state = Arc::new(Mutex::new(OneshotState {
        result: None,
        waker: None,
    }));
    (
        Responder { state: Arc::clone(&state) },
        Arc::clone(&state),
        OneshotFuture { state },
    )
}

// ---- Identities & Requests ----------------------------------------------

#[derive(Debug, Clone)]
pub struct Identity {
    pub login: String,
    pub display: String,
}

pub struct Request {
    pub action_id: String,
    pub message: String,
    pub icon_name: String,
    pub details: Vec<(String, String)>,
    pub cookie: String,
    pub identities: Vec<Identity>,
    pub responder: Option<Responder>,
}

/// Messages sent from the D-Bus agent thread and `polkit-agent-helper-1` worker
/// threads into the shell's calloop event loop.
pub enum Msg {
    Begin(Box<Request>),
    Cancel(String),
    HelperPrompt {
        cookie: String,
        attempt: u64,
        echo: bool,
        prompt: String,
    },
    HelperInfo {
        cookie: String,
        attempt: u64,
        text: String,
    },
    HelperError {
        cookie: String,
        attempt: u64,
        text: String,
    },
    HelperFinished {
        cookie: String,
        attempt: u64,
        success: bool,
    },
}

// ---- D-Bus Interface ----------------------------------------------------

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.PolicyKit1.Error")]
enum PolkitError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Cancelled(String),
}

type IdentityTuple = (String, HashMap<String, OwnedValue>);

struct Agent {
    tx: Sender<Msg>,
    pending: Arc<Mutex<HashMap<String, Arc<Mutex<OneshotState>>>>>,
}

#[zbus::interface(name = "org.freedesktop.PolicyKit1.AuthenticationAgent")]
impl Agent {
    async fn begin_authentication(
        &self,
        action_id: String,
        message: String,
        icon_name: String,
        details: HashMap<String, String>,
        cookie: String,
        identities: Vec<IdentityTuple>,
    ) -> Result<(), PolkitError> {
        let parsed = parse_identities(&identities);
        let mut filtered_details: Vec<(String, String)> = details
            .into_iter()
            .filter(|(k, v)| !v.is_empty() && !k.starts_with("polkit.gettext_domain") && k != "polkit.message")
            .collect();
        filtered_details.sort_by(|a, b| a.0.cmp(&b.0));

        let (responder, cancel_handle, fut) = oneshot();
        self.pending.lock().unwrap().insert(cookie.clone(), cancel_handle);

        let req = Request {
            action_id,
            message,
            icon_name,
            details: filtered_details,
            cookie: cookie.clone(),
            identities: parsed,
            responder: Some(responder),
        };
        if self.tx.send(Msg::Begin(Box::new(req))).is_err() {
            self.pending.lock().unwrap().remove(&cookie);
            return Err(PolkitError::Cancelled("Shell unavailable".into()));
        }

        let outcome = fut.await;
        self.pending.lock().unwrap().remove(&cookie);
        match outcome {
            Outcome::Authenticated => Ok(()),
            Outcome::Cancelled => Err(PolkitError::Cancelled("Authentication cancelled".into())),
        }
    }

    async fn cancel_authentication(&self, cookie: String) {
        if let Some(handle) = self.pending.lock().unwrap().remove(&cookie) {
            complete_state(&handle, Outcome::Cancelled);
        }
        let _ = self.tx.send(Msg::Cancel(cookie));
    }
}

/// Keeps the system D-Bus connection alive and unregisters the agent on drop.
pub struct AgentHandle {
    conn: zbus::blocking::Connection,
    session_id: Option<String>,
}

impl Drop for AgentHandle {
    fn drop(&mut self) {
        let mut map: HashMap<&str, Value<'_>> = HashMap::new();
        let subject_kind = if let Some(ref sid) = self.session_id {
            map.insert("session-id", Value::from(sid.as_str()));
            "unix-session"
        } else {
            map.insert("pid", Value::from(std::process::id()));
            map.insert("start-time", Value::from(0u64));
            "unix-process"
        };
        let _ = self.conn.call_method(
            Some(AUTHORITY_BUS),
            AUTHORITY_PATH,
            Some(AUTHORITY_IFACE),
            "UnregisterAuthenticationAgent",
            &((subject_kind, map), PATH),
        );
    }
}

/// Register Orchard's Polkit authentication agent on the system bus.
pub fn start(tx: Sender<Msg>) -> Option<AgentHandle> {
    if std::env::var_os("ORCHARD_NO_POLKIT").is_some() {
        return None;
    }
    let agent = Agent {
        tx,
        pending: Arc::new(Mutex::new(HashMap::new())),
    };
    let conn = match zbus::blocking::connection::Builder::system()
        .and_then(|b| b.serve_at(PATH, agent))
        .and_then(|b| b.build())
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("polkit: no system bus: {e}");
            return None;
        }
    };

    let session_id = resolve_session_id(&conn);
    let mut map: HashMap<&str, Value<'_>> = HashMap::new();
    let subject_kind = if let Some(ref sid) = session_id {
        map.insert("session-id", Value::from(sid.as_str()));
        "unix-session"
    } else {
        map.insert("pid", Value::from(std::process::id()));
        map.insert("start-time", Value::from(0u64));
        "unix-process"
    };
    let locale = std::env::var("LANG")
        .or_else(|_| std::env::var("LC_MESSAGES"))
        .unwrap_or_else(|_| "en_US.UTF-8".into());

    match conn.call_method(
        Some(AUTHORITY_BUS),
        AUTHORITY_PATH,
        Some(AUTHORITY_IFACE),
        "RegisterAuthenticationAgent",
        &((subject_kind, map), locale.as_str(), PATH),
    ) {
        Ok(_) => {
            tracing::info!("polkit: registered authentication agent ({subject_kind})");
            Some(AgentHandle { conn, session_id })
        }
        Err(e) => {
            tracing::warn!("polkit: RegisterAuthenticationAgent failed: {e}");
            None
        }
    }
}

fn resolve_session_id(conn: &zbus::blocking::Connection) -> Option<String> {
    if let Ok(sid) = std::env::var("XDG_SESSION_ID") {
        let sid = sid.trim();
        if !sid.is_empty() {
            return Some(sid.to_string());
        }
    }
    // Ask systemd-logind for the session owning our PID, then fall back to "auto".
    let session_path = conn
        .call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1",
            Some("org.freedesktop.login1.Manager"),
            "GetSessionByPID",
            &(std::process::id(),),
        )
        .ok()
        .and_then(|m| m.body().deserialize::<OwnedObjectPath>().ok())
        .or_else(|| {
            conn.call_method(
                Some("org.freedesktop.login1"),
                "/org/freedesktop/login1",
                Some("org.freedesktop.login1.Manager"),
                "GetSession",
                &("auto",),
            )
            .ok()
            .and_then(|m| m.body().deserialize::<OwnedObjectPath>().ok())
        })?;

    let val = conn
        .call_method(
            Some("org.freedesktop.login1"),
            session_path.as_str(),
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.freedesktop.login1.Session", "Id"),
        )
        .ok()?
        .body()
        .deserialize::<OwnedValue>()
        .ok()?;
    String::try_from(val).ok().filter(|s| !s.is_empty())
}

fn parse_identities(raw: &[IdentityTuple]) -> Vec<Identity> {
    let mut out = Vec::new();
    for (kind, details) in raw {
        if kind != "unix-user" {
            continue;
        }
        if let Some(val) = details.get("uid") {
            if let Ok(uid) = u32::try_from(val) {
                if let Some(id) = lookup_uid(uid) {
                    out.push(id);
                }
            }
        }
    }
    if out.is_empty() {
        let login = std::env::var("USER").unwrap_or_else(|_| "root".into());
        let display = capitalize(&login);
        out.push(Identity { login, display });
    }
    // Prefer the current logged-in user first (macOS defaults to the current user).
    if let Ok(cur) = std::env::var("USER") {
        if let Some(pos) = out.iter().position(|id| id.login == cur) {
            if pos > 0 {
                let id = out.remove(pos);
                out.insert(0, id);
            }
        }
    }
    out
}

fn lookup_uid(uid: u32) -> Option<Identity> {
    if let Ok(contents) = std::fs::read_to_string("/etc/passwd") {
        for line in contents.lines() {
            if let Some(id) = parse_passwd_line(line, uid) {
                return Some(id);
            }
        }
    }
    // Fallback for NSS/SSSD/LDAP users not listed in /etc/passwd.
    if let Ok(out) = Command::new("getent").args(["passwd", &uid.to_string()]).output() {
        if out.status.success() {
            let line = String::from_utf8_lossy(&out.stdout);
            if let Some(id) = parse_passwd_line(line.trim(), uid) {
                return Some(id);
            }
        }
    }
    None
}

fn parse_passwd_line(line: &str, want_uid: u32) -> Option<Identity> {
    let mut parts = line.split(':');
    let login = parts.next()?.trim();
    let _pw = parts.next()?;
    let uid: u32 = parts.next()?.trim().parse().ok()?;
    if uid != want_uid || login.is_empty() {
        return None;
    }
    let _gid = parts.next()?;
    let gecos = parts.next().unwrap_or("");
    let full = gecos.split(',').next().unwrap_or("").trim();
    let display = if full.is_empty() || full == login {
        capitalize(login)
    } else {
        full.to_string()
    };
    Some(Identity {
        login: login.to_string(),
        display,
    })
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

// ---- polkit-agent-helper-1 worker ---------------------------------------

struct HelperSession {
    stdin_tx: Option<mpsc::Sender<String>>,
    child: Arc<Mutex<Option<Child>>>,
}

impl HelperSession {
    fn spawn(username: String, cookie: String, attempt: u64, tx: Sender<Msg>) -> Option<Self> {
        let helper = HELPER_PATHS.iter().copied().find(|p| Path::new(p).is_file())?;
        let mut child = match Command::new(helper)
            .arg(&username)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("polkit: failed to spawn {helper}: {e}");
                return None;
            }
        };

        let mut stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        let child_arc = Arc::new(Mutex::new(Some(child)));
        let child_ref = Arc::clone(&child_arc);
        let (stdin_tx, stdin_rx) = mpsc::channel::<String>();

        let _ = std::thread::Builder::new()
            .name("polkit-helper".into())
            .spawn(move || {
                if writeln!(stdin, "{cookie}").is_err() || stdin.flush().is_err() {
                    let _ = tx.send(Msg::HelperFinished {
                        cookie,
                        attempt,
                        success: false,
                    });
                    return;
                }

                let reader = BufReader::new(stdout);
                let mut finished = false;
                for line in reader.lines().map_while(Result::ok) {
                    if let Some(rest) = line.strip_prefix("PAM_PROMPT_ECHO_OFF") {
                        let _ = tx.send(Msg::HelperPrompt {
                            cookie: cookie.clone(),
                            attempt,
                            echo: false,
                            prompt: rest.trim().to_string(),
                        });
                        match stdin_rx.recv() {
                            Ok(response) => {
                                if writeln!(stdin, "{response}").is_err() || stdin.flush().is_err() {
                                    break;
                                }
                            }
                            Err(_) => {
                                // Cancelled by UI.
                                kill_child(&child_ref);
                                return;
                            }
                        }
                    } else if let Some(rest) = line.strip_prefix("PAM_PROMPT_ECHO_ON") {
                        let _ = tx.send(Msg::HelperPrompt {
                            cookie: cookie.clone(),
                            attempt,
                            echo: true,
                            prompt: rest.trim().to_string(),
                        });
                        match stdin_rx.recv() {
                            Ok(response) => {
                                if writeln!(stdin, "{response}").is_err() || stdin.flush().is_err() {
                                    break;
                                }
                            }
                            Err(_) => {
                                kill_child(&child_ref);
                                return;
                            }
                        }
                    } else if let Some(rest) = line.strip_prefix("PAM_ERROR_MSG") {
                        let msg = rest.trim();
                        if !msg.is_empty() {
                            let _ = tx.send(Msg::HelperError {
                                cookie: cookie.clone(),
                                attempt,
                                text: msg.to_string(),
                            });
                        }
                    } else if let Some(rest) = line.strip_prefix("PAM_TEXT_INFO") {
                        let msg = rest.trim();
                        if !msg.is_empty() {
                            let _ = tx.send(Msg::HelperInfo {
                                cookie: cookie.clone(),
                                attempt,
                                text: msg.to_string(),
                            });
                        }
                    } else if line.trim() == "SUCCESS" {
                        finished = true;
                        let _ = tx.send(Msg::HelperFinished {
                            cookie: cookie.clone(),
                            attempt,
                            success: true,
                        });
                        break;
                    } else if line.trim() == "FAILURE" {
                        finished = true;
                        let _ = tx.send(Msg::HelperFinished {
                            cookie: cookie.clone(),
                            attempt,
                            success: false,
                        });
                        break;
                    }
                }

                let ok = wait_child(&child_ref);
                if !finished {
                    let _ = tx.send(Msg::HelperFinished {
                        cookie,
                        attempt,
                        success: ok,
                    });
                }
            });

        Some(Self {
            stdin_tx: Some(stdin_tx),
            child: child_arc,
        })
    }

    fn send_response(&self, response: String) -> bool {
        self.stdin_tx
            .as_ref()
            .map(|tx| tx.send(response).is_ok())
            .unwrap_or(false)
    }

    fn cancel(&mut self) {
        self.stdin_tx = None;
        kill_child(&self.child);
    }
}

impl Drop for HelperSession {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn kill_child(cell: &Mutex<Option<Child>>) {
    if let Ok(mut g) = cell.lock() {
        if let Some(mut c) = g.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn wait_child(cell: &Mutex<Option<Child>>) -> bool {
    if let Ok(mut g) = cell.lock() {
        if let Some(mut c) = g.take() {
            return c.wait().map(|s| s.success()).unwrap_or(false);
        }
    }
    false
}

// ---- macOS-Style Authentication Sheet UI --------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Cancel,
    Confirm,
    UserField,
    PasswordField,
    DetailsToggle,
}

pub struct PolkitDialog {
    pub req: Request,
    pub selected: usize,
    pub input: String,
    pub echo: bool,
    pub prompt_label: Option<String>,
    pub info: Option<String>,
    pub error: Option<String>,
    pub verifying: bool,
    pub verify_started: Instant,
    pub shake_started: Option<Instant>,
    pub show_details: bool,
    pub hovered: Option<Hit>,
    pub rect: (f32, f32, f32, f32),
    attempt: u64,
    helper: Option<HelperSession>,
    tx: Sender<Msg>,
    cancel_rect: (f32, f32, f32, f32),
    confirm_rect: (f32, f32, f32, f32),
    user_rect: (f32, f32, f32, f32),
    pass_rect: (f32, f32, f32, f32),
    details_rect: (f32, f32, f32, f32),
}

impl PolkitDialog {
    pub fn new(req: Request, tx: Sender<Msg>) -> Self {
        let mut d = Self {
            req,
            selected: 0,
            input: String::new(),
            echo: false,
            prompt_label: None,
            info: None,
            error: None,
            verifying: false,
            verify_started: Instant::now(),
            shake_started: None,
            show_details: false,
            hovered: None,
            rect: (0.0, 0.0, WIDTH, 280.0),
            attempt: 0,
            helper: None,
            tx,
            cancel_rect: (0.0, 0.0, 0.0, 0.0),
            confirm_rect: (0.0, 0.0, 0.0, 0.0),
            user_rect: (0.0, 0.0, 0.0, 0.0),
            pass_rect: (0.0, 0.0, 0.0, 0.0),
            details_rect: (0.0, 0.0, 0.0, 0.0),
        };
        d.start_helper();
        d
    }

    pub fn cookie(&self) -> &str {
        &self.req.cookie
    }

    pub fn current_identity(&self) -> &Identity {
        &self.req.identities[self.selected.min(self.req.identities.len().saturating_sub(1))]
    }

    fn start_helper(&mut self) {
        self.helper = None;
        self.attempt += 1;
        self.echo = false;
        self.prompt_label = None;
        let user = self.current_identity().login.clone();
        let cookie = self.req.cookie.clone();
        self.helper = HelperSession::spawn(user, cookie, self.attempt, self.tx.clone());
        if self.helper.is_none() {
            self.error = Some("polkit-agent-helper-1 not found".into());
        }
    }

    pub fn cycle_identity(&mut self) {
        if self.req.identities.len() <= 1 || self.verifying {
            return;
        }
        self.selected = (self.selected + 1) % self.req.identities.len();
        self.input.clear();
        self.error = None;
        self.info = None;
        self.start_helper();
    }

    pub fn push_str(&mut self, s: &str) {
        if self.verifying {
            return;
        }
        self.error = None;
        self.input.push_str(s);
    }

    pub fn backspace(&mut self, clear_all: bool) {
        if self.verifying {
            return;
        }
        self.error = None;
        if clear_all {
            self.input.clear();
        } else {
            self.input.pop();
        }
    }

    pub fn submit(&mut self) {
        if self.verifying {
            return;
        }
        if self.helper.is_none() {
            self.start_helper();
        }
        let response = std::mem::take(&mut self.input);
        self.error = None;
        if let Some(ref helper) = self.helper {
            if helper.send_response(response) {
                self.verifying = true;
                self.verify_started = Instant::now();
                return;
            }
        }
        self.trigger_failure(None, false);
    }

    pub fn on_prompt(&mut self, attempt: u64, echo: bool, prompt: String) {
        if attempt != self.attempt {
            return;
        }
        self.echo = echo;
        let clean = prompt.trim().trim_end_matches(':').trim();
        self.prompt_label = if clean.is_empty() || clean.eq_ignore_ascii_case("password") {
            None
        } else {
            Some(clean.to_string())
        };
    }

    pub fn on_info(&mut self, attempt: u64, text: String) {
        if attempt != self.attempt {
            return;
        }
        self.info = Some(text);
    }

    pub fn on_error(&mut self, attempt: u64, text: String) {
        if attempt != self.attempt {
            return;
        }
        self.error = Some(text);
    }

    /// Returns `true` if authentication succeeded and the dialog should close.
    pub fn on_finished(&mut self, attempt: u64, success: bool, german: bool) -> bool {
        if attempt != self.attempt {
            return false;
        }
        self.verifying = false;
        self.helper = None;
        if success {
            if let Some(r) = self.req.responder.take() {
                r.finish(Outcome::Authenticated);
            }
            return true;
        }
        self.trigger_failure(None, german);
        false
    }

    fn trigger_failure(&mut self, custom: Option<String>, german: bool) {
        self.verifying = false;
        self.input.clear();
        self.shake_started = Some(Instant::now());
        if let Some(m) = custom {
            self.error = Some(m);
        } else if self.error.is_none() {
            self.error = Some(if german {
                "Falsches Passwort. Erneut versuchen.".into()
            } else {
                "Incorrect password. Try again.".into()
            });
        }
        self.start_helper();
    }

    pub fn cancel(&mut self) {
        self.helper = None;
        if let Some(r) = self.req.responder.take() {
            r.finish(Outcome::Cancelled);
        }
    }

    pub fn is_animating(&self) -> bool {
        if self.verifying {
            return true;
        }
        if let Some(t0) = self.shake_started {
            if t0.elapsed() < Duration::from_secs_f32(SHAKE_SECS) {
                return true;
            }
        }
        false
    }

    fn shake_offset(&self) -> f32 {
        let Some(t0) = self.shake_started else {
            return 0.0;
        };
        let elapsed = t0.elapsed().as_secs_f32();
        if elapsed >= SHAKE_SECS {
            return 0.0;
        }
        let t = elapsed / SHAKE_SECS;
        (1.0 - t).powi(2) * (t * std::f32::consts::TAU * 3.5).sin() * 13.0
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        let in_r = |r: (f32, f32, f32, f32)| r.2 > 0.0 && x >= r.0 && x <= r.0 + r.2 && y >= r.1 && y <= r.1 + r.3;
        if in_r(self.confirm_rect) {
            Some(Hit::Confirm)
        } else if in_r(self.cancel_rect) {
            Some(Hit::Cancel)
        } else if in_r(self.details_rect) {
            Some(Hit::DetailsToggle)
        } else if self.req.identities.len() > 1 && in_r(self.user_rect) {
            Some(Hit::UserField)
        } else if in_r(self.pass_rect) {
            Some(Hit::PasswordField)
        } else {
            None
        }
    }

    pub fn paint(
        &mut self,
        pm: &mut Pixmap,
        scale: f32,
        fonts: &Fonts,
        apps: &Apps,
        theme: &Theme,
        caps_lock: bool,
        german: bool,
    ) {
        let s = scale;
        let sw = pm.width() as f32 / s;
        let sh = pm.height() as f32 / s;

        // Subtle screen dim behind the authentication sheet
        fill_rounded(pm, 0.0, 0.0, pm.width() as f32, pm.height() as f32, 0.0, [0, 0, 0, 56]);

        let dark = theme.menu_text[0] > 128;
        let opaque = theme.menu_bg[3] == 255;
        let bg: Rgba = match (dark, opaque) {
            (true, true) => [40, 40, 44, 255],
            (true, false) => [40, 40, 44, 226],
            (false, true) => [248, 248, 252, 255],
            (false, false) => [248, 248, 252, 234],
        };
        let border: Rgba = if dark { [255, 255, 255, 44] } else { [0, 0, 0, 32] };
        let text = theme.menu_text;
        let sub: Rgba = if dark { [220, 220, 226, 175] } else { [40, 40, 46, 165] };
        let accent = theme.menu_selection;
        let err_col: Rgba = if dark { [255, 82, 72, 255] } else { [212, 36, 28, 255] };
        let field_bg: Rgba = if dark { [255, 255, 255, 18] } else { [255, 255, 255, 232] };
        let field_ro_bg: Rgba = if dark { [255, 255, 255, 10] } else { [0, 0, 0, 10] };
        let field_border: Rgba = if dark { [255, 255, 255, 42] } else { [0, 0, 0, 36] };
        let cancel_bg: Rgba = if dark { [255, 255, 255, 28] } else { [0, 0, 0, 18] };
        let cancel_hov: Rgba = if dark { [255, 255, 255, 44] } else { [0, 0, 0, 30] };

        let title_str = if self.req.message.trim().is_empty() {
            if german {
                "Authentifizierung erforderlich"
            } else {
                "Authentication Required"
            }
        } else {
            self.req.message.trim()
        };
        let subtitle_str: &str = if let Some(ref info) = self.info {
            info.as_str()
        } else if german {
            "Gib dein Passwort ein, um dies zu erlauben."
        } else {
            "Enter your password to allow this."
        };

        let max_w = (WIDTH - 44.0) * s;
        let title_px = 13.5 * s;
        let sub_px = 11.5 * s;
        let title_lines = fonts.wrap(title_str, title_px, Weight::Bold, max_w, 3);
        let sub_lines = fonts.wrap(subtitle_str, sub_px, Weight::Regular, max_w, 2);

        // Compute dynamic sheet height
        let mut h = 76.0; // top padding + icon
        h += title_lines.len() as f32 * 18.0;
        h += 4.0 + sub_lines.len() as f32 * 15.5;
        h += 14.0; // gap before form
        h += 26.0 + 10.0 + 26.0; // User Name row + gap + Password row
        if self.error.is_some() {
            h += 22.0;
        } else {
            h += 8.0;
        }
        // Details disclosure row
        let has_details = !self.req.action_id.is_empty() || !self.req.details.is_empty();
        let detail_rows: Vec<(String, String)> = if has_details && self.show_details {
            let mut rows = Vec::new();
            if !self.req.action_id.is_empty() {
                rows.push((
                    if german { "Aktion".into() } else { "Action".into() },
                    self.req.action_id.clone(),
                ));
            }
            for (k, v) in self.req.details.iter().take(3) {
                rows.push((k.clone(), v.clone()));
            }
            rows
        } else {
            Vec::new()
        };
        if has_details {
            h += 20.0;
            if self.show_details {
                h += 12.0 + detail_rows.len() as f32 * 15.0 + 8.0;
            }
        }
        h += 14.0 + 30.0 + 18.0; // gap + buttons + bottom pad

        let shake = self.shake_offset();
        let x0 = ((sw - WIDTH) * 0.5 + shake).round();
        let y0 = ((sh - h) * 0.42).round();
        self.rect = (x0, y0, WIDTH, h);

        // Soft drop shadow
        fill_rounded(
            pm,
            (x0 - 6.0) * s,
            (y0 + 10.0) * s,
            (WIDTH + 12.0) * s,
            (h + 12.0) * s,
            (RADIUS + 6.0) * s,
            [0, 0, 0, 52],
        );
        // Frosted sheet background + hairline border
        fill_rounded(pm, x0 * s, y0 * s, WIDTH * s, h * s, RADIUS * s, bg);
        stroke_rounded(pm, x0 * s, y0 * s, WIDTH * s, h * s, RADIUS * s, 1.0 * s, border);

        // Top emblem: app icon with padlock badge, or classic macOS padlock
        let cx = x0 + WIDTH * 0.5;
        let icon_cy = y0 + 38.0;
        let app_icon = if !self.req.icon_name.is_empty() {
            apps.icon_any(&self.req.icon_name, (44.0 * s).round() as u32)
        } else {
            None
        };
        if let Some(icon) = app_icon {
            let iw = icon.width() as f32;
            let ih = icon.height() as f32;
            let pp = PixmapPaint::default();
            pm.draw_pixmap(
                (cx * s - iw * 0.5).round() as i32,
                (icon_cy * s - ih * 0.5).round() as i32,
                (*icon).as_ref(),
                &pp,
                Transform::identity(),
                None,
            );
            // Small lock badge at bottom-right of app icon
            let bx = (cx + 14.0) * s;
            let by = (icon_cy + 13.0) * s;
            fill_circle(pm, bx, by, 10.5 * s, if dark { [56, 56, 62, 245] } else { [245, 245, 248, 245] });
            draw_padlock(pm, bx, by + 0.5 * s, 0.52 * s, dark);
        } else {
            draw_lock_emblem(pm, cx * s, icon_cy * s, s, dark);
        }

        // Title
        let mut y = y0 + 78.0;
        for line in &title_lines {
            let lw = fonts.measure(line, title_px, Weight::Bold);
            fonts.draw(pm, line, cx * s - lw * 0.5, y * s, title_px, Weight::Bold, text);
            y += 18.0;
        }
        // Subtitle
        y += 4.0;
        for line in &sub_lines {
            let lw = fonts.measure(line, sub_px, Weight::Regular);
            fonts.draw(pm, line, cx * s - lw * 0.5, y * s, sub_px, Weight::Regular, sub);
            y += 15.5;
        }

        // Form rows: User Name + Password
        y += 8.0;
        let label_px = 12.0 * s;
        let input_px = 12.5 * s;
        let label_col_w = 92.0;
        let field_x = x0 + 20.0 + label_col_w + 10.0;
        let field_w = WIDTH - 40.0 - label_col_w - 10.0;
        let field_h = 26.0;

        // Row 1: User Name
        let user_label = if german { "Benutzername:" } else { "User Name:" };
        let ulw = fonts.measure(user_label, label_px, Weight::Regular);
        let u_base = fonts.baseline_centered(y * s, field_h * s, label_px, Weight::Regular);
        fonts.draw(
            pm,
            user_label,
            (x0 + 20.0 + label_col_w) * s - ulw,
            u_base,
            label_px,
            Weight::Regular,
            sub,
        );
        self.user_rect = (field_x, y, field_w, field_h);
        let multi = self.req.identities.len() > 1;
        let u_bg = if multi && self.hovered == Some(Hit::UserField) {
            if dark { [255, 255, 255, 26] } else { [0, 0, 0, 18] }
        } else if multi {
            field_bg
        } else {
            field_ro_bg
        };
        fill_rounded(pm, field_x * s, y * s, field_w * s, field_h * s, 6.0 * s, u_bg);
        stroke_rounded(pm, field_x * s, y * s, field_w * s, field_h * s, 6.0 * s, 1.0 * s, field_border);
        let uname = &self.current_identity().display;
        let uname_elided = fonts.elide(uname, input_px, Weight::Regular, (field_w - if multi { 28.0 } else { 16.0 }) * s);
        let u_text_base = fonts.baseline_centered(y * s, field_h * s, input_px, Weight::Regular);
        fonts.draw(pm, &uname_elided, (field_x + 8.0) * s, u_text_base, input_px, Weight::Regular, text);
        if multi {
            draw_chevrons(pm, (field_x + field_w - 12.0) * s, (y + field_h * 0.5) * s, s, sub);
        }

        // Row 2: Password (or PAM prompt)
        y += field_h + 10.0;
        let pass_label_owned;
        let pass_label = if let Some(ref custom) = self.prompt_label {
            pass_label_owned = format!("{custom}:");
            pass_label_owned.as_str()
        } else if german {
            "Passwort:"
        } else {
            "Password:"
        };
        let pass_label = fonts.elide(pass_label, label_px, Weight::Regular, label_col_w * s);
        let plw = fonts.measure(&pass_label, label_px, Weight::Regular);
        let p_base = fonts.baseline_centered(y * s, field_h * s, label_px, Weight::Regular);
        fonts.draw(
            pm,
            &pass_label,
            (x0 + 20.0 + label_col_w) * s - plw,
            p_base,
            label_px,
            Weight::Regular,
            sub,
        );
        self.pass_rect = (field_x, y, field_w, field_h);

        // macOS accent focus ring around password field
        if !self.verifying {
            let glow: Rgba = [accent[0], accent[1], accent[2], if dark { 95 } else { 78 }];
            stroke_rounded(
                pm,
                (field_x - 1.5) * s,
                (y - 1.5) * s,
                (field_w + 3.0) * s,
                (field_h + 3.0) * s,
                7.5 * s,
                2.5 * s,
                glow,
            );
        }
        fill_rounded(pm, field_x * s, y * s, field_w * s, field_h * s, 6.0 * s, field_bg);
        stroke_rounded(
            pm,
            field_x * s,
            y * s,
            field_w * s,
            field_h * s,
            6.0 * s,
            1.1 * s,
            if self.verifying { field_border } else { accent },
        );

        let right_reserve = if self.verifying || caps_lock { 24.0 } else { 10.0 };
        let p_cy = (y + field_h * 0.5) * s;
        let inner_x = (field_x + 9.0) * s;
        let inner_w = (field_w - 9.0 - right_reserve).max(16.0) * s;

        if self.echo {
            // Plaintext PAM prompt
            let shown = tail_fit(fonts, &self.input, input_px, Weight::Regular, inner_w);
            let t_base = fonts.baseline_centered(y * s, field_h * s, input_px, Weight::Regular);
            let tw = fonts.draw(pm, &shown, inner_x, t_base, input_px, Weight::Regular, text);
            if !self.verifying {
                fill_rounded(pm, inner_x + tw + 1.0 * s, (y + 5.5) * s, 1.5 * s, (field_h - 11.0) * s, 0.7 * s, accent);
            }
        } else {
            // Password dots
            let count = self.input.chars().count();
            let gap = 9.0 * s;
            let max_dots = ((inner_w / gap).floor() as usize).max(1);
            let visible = count.min(max_dots);
            for i in 0..visible {
                fill_circle(pm, inner_x + 3.0 * s + i as f32 * gap, p_cy, 3.0 * s, text);
            }
            if !self.verifying {
                let caret_x = if visible == 0 {
                    inner_x
                } else {
                    inner_x + visible as f32 * gap + 1.0 * s
                };
                fill_rounded(pm, caret_x, (y + 5.5) * s, 1.5 * s, (field_h - 11.0) * s, 0.7 * s, accent);
            }
        }

        if self.verifying {
            let spin = self.verify_started.elapsed().as_secs_f32();
            draw_spinner(pm, (field_x + field_w - 13.0) * s, p_cy, s, spin, sub);
        } else if caps_lock {
            draw_caps_lock(pm, (field_x + field_w - 13.0) * s, p_cy, s, sub);
        }

        // Error message
        y += field_h;
        if let Some(ref err) = self.error {
            y += 16.0;
            let err_px = 11.5 * s;
            let elided = fonts.elide(err, err_px, Weight::Regular, max_w);
            let ew = fonts.measure(&elided, err_px, Weight::Regular);
            fonts.draw(pm, &elided, cx * s - ew * 0.5, y * s, err_px, Weight::Regular, err_col);
            y += 6.0;
        } else {
            y += 8.0;
        }

        // Details disclosure
        if has_details {
            let det_label = if german {
                if self.show_details { "▾ Details" } else { "▸ Details" }
            } else if self.show_details {
                "▾ Details"
            } else {
                "▸ Details"
            };
            let det_px = 11.5 * s;
            let dw = fonts.measure(det_label, det_px, Weight::Regular) / s;
            self.details_rect = (x0 + 20.0, y, dw + 10.0, 20.0);
            let det_col = if self.hovered == Some(Hit::DetailsToggle) { text } else { sub };
            let det_base = fonts.baseline_centered(y * s, 20.0 * s, det_px, Weight::Regular);
            fonts.draw(pm, det_label, (x0 + 20.0) * s, det_base, det_px, Weight::Regular, det_col);
            y += 20.0;

            if self.show_details {
                let box_h = 12.0 + detail_rows.len() as f32 * 15.0;
                let box_x = x0 + 20.0;
                let box_w = WIDTH - 40.0;
                fill_rounded(pm, box_x * s, (y + 4.0) * s, box_w * s, box_h * s, 8.0 * s, field_ro_bg);
                stroke_rounded(pm, box_x * s, (y + 4.0) * s, box_w * s, box_h * s, 8.0 * s, 1.0 * s, border);
                let mut dy = y + 18.0;
                let kpx = 10.5 * s;
                for (k, v) in &detail_rows {
                    let line = format!("{k}: {v}");
                    let elided = fonts.elide(&line, kpx, Weight::Regular, (box_w - 16.0) * s);
                    fonts.draw(pm, &elided, (box_x + 8.0) * s, dy * s, kpx, Weight::Regular, sub);
                    dy += 15.0;
                }
            }
        } else {
            self.details_rect = (0.0, 0.0, 0.0, 0.0);
        }

        // Buttons at bottom
        let btn_h = 30.0;
        let btn_gap = 10.0;
        let btn_w = (WIDTH - 40.0 - btn_gap) * 0.5;
        let btn_y = y0 + h - 18.0 - btn_h;
        let cancel_x = x0 + 20.0;
        let confirm_x = cancel_x + btn_w + btn_gap;
        self.cancel_rect = (cancel_x, btn_y, btn_w, btn_h);
        self.confirm_rect = (confirm_x, btn_y, btn_w, btn_h);

        let cancel_label = if german { "Abbrechen" } else { "Cancel" };
        let ok_label = "OK";
        let btn_px = 13.0 * s;
        let base = fonts.baseline_centered(btn_y * s, btn_h * s, btn_px, Weight::Regular);

        // Cancel button
        let c_fill = if self.hovered == Some(Hit::Cancel) { cancel_hov } else { cancel_bg };
        fill_rounded(pm, cancel_x * s, btn_y * s, btn_w * s, btn_h * s, 8.0 * s, c_fill);
        let cw = fonts.measure(cancel_label, btn_px, Weight::Regular);
        fonts.draw(pm, cancel_label, (cancel_x + btn_w * 0.5) * s - cw * 0.5, base, btn_px, Weight::Regular, text);

        // OK button
        let ok_fill = if self.verifying {
            [accent[0], accent[1], accent[2], 145]
        } else if self.hovered == Some(Hit::Confirm) {
            [
                accent[0].saturating_add(16),
                accent[1].saturating_add(16),
                accent[2].saturating_add(16),
                255,
            ]
        } else {
            accent
        };
        fill_rounded(pm, confirm_x * s, btn_y * s, btn_w * s, btn_h * s, 8.0 * s, ok_fill);
        let ok_w = fonts.measure(ok_label, btn_px, Weight::Bold);
        fonts.draw(
            pm,
            ok_label,
            (confirm_x + btn_w * 0.5) * s - ok_w * 0.5,
            base,
            btn_px,
            Weight::Bold,
            [255, 255, 255, if self.verifying { 200 } else { 255 }],
        );
    }
}

fn tail_fit(fonts: &Fonts, text: &str, px: f32, w: Weight, max_w: f32) -> String {
    if fonts.measure(text, px, w) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut start = chars.len();
    while start > 0 {
        let cand: String = chars[start - 1..].iter().collect();
        if fonts.measure(&cand, px, w) > max_w {
            break;
        }
        start -= 1;
    }
    chars[start..].iter().collect()
}

// ---- Vector Icons -------------------------------------------------------

/// macOS-style metallic/frosted padlock emblem at top of sheet.
fn draw_lock_emblem(pm: &mut Pixmap, cx: f32, cy: f32, s: f32, dark: bool) {
    let r = 22.0 * s;
    let grad = LinearGradient::new(
        Point::from_xy(cx, cy - r),
        Point::from_xy(cx, cy + r),
        if dark {
            vec![
                GradientStop::new(0.0, Color::from_rgba8(96, 98, 108, 240)),
                GradientStop::new(1.0, Color::from_rgba8(60, 62, 70, 240)),
            ]
        } else {
            vec![
                GradientStop::new(0.0, Color::from_rgba8(240, 242, 246, 255)),
                GradientStop::new(1.0, Color::from_rgba8(204, 208, 216, 255)),
            ]
        },
        SpreadMode::Pad,
        Transform::identity(),
    );
    if let (Some(circle), Some(shader)) = (PathBuilder::from_circle(cx, cy, r), grad) {
        let p = Paint { shader, anti_alias: true, ..Default::default() };
        pm.fill_path(&circle, &p, FillRule::Winding, Transform::identity(), None);
        let ring = if dark { [255, 255, 255, 44] } else { [0, 0, 0, 28] };
        let st = Stroke { width: 1.0 * s, ..Default::default() };
        pm.stroke_path(&circle, &paint(ring), &st, Transform::identity(), None);
    }
    draw_padlock(pm, cx, cy + 1.0 * s, s, dark);
}

fn draw_padlock(pm: &mut Pixmap, cx: f32, cy: f32, s: f32, dark: bool) {
    // Shackle (arch)
    let shackle_col: Rgba = if dark { [228, 230, 236, 245] } else { [92, 96, 106, 255] };
    let mut pb = PathBuilder::new();
    let rw = 5.6 * s;
    let top_y = cy - 11.2 * s;
    let join_y = cy - 2.0 * s;
    pb.move_to(cx - rw, join_y);
    pb.line_to(cx - rw, top_y + rw);
    for i in 0..=16 {
        let a = std::f32::consts::PI + (i as f32 / 16.0) * std::f32::consts::PI;
        pb.line_to(cx + rw * a.cos(), top_y + rw + rw * a.sin());
    }
    pb.line_to(cx + rw, join_y);
    if let Some(path) = pb.finish() {
        let st = Stroke {
            width: 2.5 * s,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Default::default()
        };
        pm.stroke_path(&path, &paint(shackle_col), &st, Transform::identity(), None);
    }

    // Padlock body (subtle gold/amber-tinted metallic gradient like macOS)
    let bw = 18.0 * s;
    let bh = 13.5 * s;
    let bx = cx - bw * 0.5;
    let by = cy - 2.5 * s;
    if let Some(body) = rounded_rect(bx, by, bw, bh, 3.4 * s) {
        let shader = LinearGradient::new(
            Point::from_xy(cx, by),
            Point::from_xy(cx, by + bh),
            vec![
                GradientStop::new(0.0, Color::from_rgba8(255, 206, 72, 255)),
                GradientStop::new(1.0, Color::from_rgba8(228, 158, 22, 255)),
            ],
            SpreadMode::Pad,
            Transform::identity(),
        );
        if let Some(shader) = shader {
            let p = Paint { shader, anti_alias: true, ..Default::default() };
            pm.fill_path(&body, &p, FillRule::Winding, Transform::identity(), None);
        }
        let st = Stroke { width: 0.8 * s, ..Default::default() };
        pm.stroke_path(&body, &paint([160, 102, 10, 110]), &st, Transform::identity(), None);
    }

    // Keyhole
    let kh_col: Rgba = [92, 58, 8, 220];
    fill_circle(pm, cx, by + bh * 0.42, 1.8 * s, kh_col);
    fill_rounded(pm, cx - 1.0 * s, by + bh * 0.45, 2.0 * s, 4.2 * s, 0.8 * s, kh_col);
}

fn draw_chevrons(pm: &mut Pixmap, cx: f32, cy: f32, s: f32, col: Rgba) {
    let mut pb = PathBuilder::new();
    pb.move_to(cx - 3.2 * s, cy - 2.0 * s);
    pb.line_to(cx, cy - 5.0 * s);
    pb.line_to(cx + 3.2 * s, cy - 2.0 * s);
    pb.move_to(cx - 3.2 * s, cy + 2.0 * s);
    pb.line_to(cx, cy + 5.0 * s);
    pb.line_to(cx + 3.2 * s, cy + 2.0 * s);
    if let Some(path) = pb.finish() {
        let st = Stroke {
            width: 1.4 * s,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Default::default()
        };
        pm.stroke_path(&path, &paint(col), &st, Transform::identity(), None);
    }
}

fn draw_caps_lock(pm: &mut Pixmap, cx: f32, cy: f32, s: f32, col: Rgba) {
    let mut pb = PathBuilder::new();
    pb.move_to(cx, cy - 6.0 * s);
    pb.line_to(cx + 5.0 * s, cy - 0.8 * s);
    pb.line_to(cx + 2.4 * s, cy - 0.8 * s);
    pb.line_to(cx + 2.4 * s, cy + 2.2 * s);
    pb.line_to(cx - 2.4 * s, cy + 2.2 * s);
    pb.line_to(cx - 2.4 * s, cy - 0.8 * s);
    pb.line_to(cx - 5.0 * s, cy - 0.8 * s);
    pb.close();
    if let Some(path) = pb.finish() {
        pm.fill_path(&path, &paint(col), FillRule::Winding, Transform::identity(), None);
    }
    fill_rounded(pm, cx - 2.4 * s, cy + 3.6 * s, 4.8 * s, 1.8 * s, 0.5 * s, col);
}

fn draw_spinner(pm: &mut Pixmap, cx: f32, cy: f32, s: f32, spin: f32, base: Rgba) {
    let step = (spin * 12.0) as i32;
    let st = Stroke {
        width: 1.6 * s,
        line_cap: LineCap::Round,
        ..Default::default()
    };
    for k in 0..8 {
        let ang = (k as f32 * 45.0 - 90.0).to_radians();
        let fade = ((k - step).rem_euclid(8)) as f32 / 8.0;
        let a = (60.0 + 185.0 * (1.0 - fade)).round() as u8;
        let (c, sn) = (ang.cos(), ang.sin());
        let mut pb = PathBuilder::new();
        pb.move_to(cx + c * 3.0 * s, cy + sn * 3.0 * s);
        pb.line_to(cx + c * 6.5 * s, cy + sn * 6.5 * s);
        if let Some(path) = pb.finish() {
            pm.stroke_path(&path, &paint([base[0], base[1], base[2], a]), &st, Transform::identity(), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_passwd_line() {
        let id = parse_passwd_line("jakob:x:1000:1000:Jakob Doe,Room 1,,:/home/jakob:/bin/bash", 1000).unwrap();
        assert_eq!(id.login, "jakob");
        assert_eq!(id.display, "Jakob Doe");

        let id2 = parse_passwd_line("alice:x:1001:1001::/home/alice:/bin/bash", 1001).unwrap();
        assert_eq!(id2.login, "alice");
        assert_eq!(id2.display, "Alice");

        assert!(parse_passwd_line("root:x:0:0:root:/root:/bin/bash", 1000).is_none());
    }

    #[test]
    fn oneshot_completes_and_cancels_on_drop() {
        let (resp, _cancel, fut) = oneshot();
        resp.finish(Outcome::Authenticated);
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut fut = Box::pin(fut);
        assert_eq!(fut.as_mut().poll(&mut cx), Poll::Ready(Outcome::Authenticated));

        let (resp2, _cancel2, fut2) = oneshot();
        drop(resp2);
        let mut fut2 = Box::pin(fut2);
        assert_eq!(fut2.as_mut().poll(&mut cx), Poll::Ready(Outcome::Cancelled));
    }
}
