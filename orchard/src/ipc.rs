//! Orchard IPC: newline-delimited JSON over a Unix socket.
//!
//! Socket: `$XDG_RUNTIME_DIR/orchard-<WAYLAND_DISPLAY>.sock` (exported to
//! children as `ORCHARD_SOCKET`). Used by `orchard-shell` (menu bar, Dock) and
//! for scripting / tests. Each line is one `Request`; replies and events are
//! one JSON object per line.

use std::{
    cell::RefCell,
    collections::HashMap,
    io::{ErrorKind, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};
use smithay::{
    desktop::space::SpaceElement,
    reexports::calloop::{generic::Generic, Interest, LoopHandle, Mode, PostAction},
    utils::{IsAlive, Logical, Rectangle},
};

use crate::{
    shell::WindowElement,
    state::{Backend, OrchardState},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WindowId(pub u64);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    #[serde(default)]
    pub radius: f64,
    /// opacity of the frosted backdrop (fades)
    #[serde(default = "one")]
    pub alpha: f64,
}

fn one() -> f64 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DockTile {
    pub app_id: String,
    pub rect: Rect,
    /// a minimized window's tile
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_id: Option<u64>,
    /// where the next minimized window will go (`app_id` is empty)
    #[serde(default)]
    pub next_minimized: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum Request {
    /// Receive `windows` events whenever the window list changes.
    Subscribe,
    Windows,
    Activate { id: u64 },
    /// Bring all windows of an app to the front (restoring minimized ones).
    ActivateApp { app_id: String },
    Minimize { id: u64 },
    Close { id: u64 },
    QuitApp { app_id: String },
    /// Run a named WM action on the focused window (close, minimize, zoom, …).
    Action { name: String },
    /// Dock tile positions (output-relative logical px) for genie targets,
    /// plus the settled Dock rect for "avoid windows".
    SetDockTiles {
        output: String,
        tiles: Vec<DockTile>,
        #[serde(default)]
        dock_rect: Option<Rect>,
    },
    /// Frosted-glass regions for a layer surface namespace (surface-relative).
    SetBackdrop { namespace: String, rects: Vec<Rect> },
    Spawn { command: String },
    Outputs,
    /// Re-read ~/.config/orchard/settings.json and apply it.
    ReloadSettings,
    /// Recompile GLSL shaders from disk (or built-in) and force a full redraw.
    ReloadShaders {
        #[serde(default)]
        dir: Option<String>,
    },
    /// Restart the menu bar + Dock from the newest `orchard-shell` binary.
    RestartShell,
    /// Capture requested by the macOS-style screenshot toolbar (`Print` / ⌘⇧5).
    ScreenshotCapture {
        /// "screen" | "window" | "area"
        mode: String,
        #[serde(default)]
        output: Option<String>,
        /// output-relative logical rect for "area"
        #[serde(default)]
        rect: Option<Rect>,
        /// IPC window id for "window"
        #[serde(default)]
        window_id: Option<u64>,
        /// "pictures" | "desktop" | "documents" | "clipboard"
        #[serde(default)]
        target: Option<String>,
        #[serde(default)]
        include_cursor: bool,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WindowInfo {
    pub id: u64,
    pub app_id: String,
    pub title: String,
    pub focused: bool,
    pub minimized: bool,
    pub zoomed: bool,
    /// hidden with ⌘H: still running, no Dock thumbnail
    pub hidden: bool,
    /// global menu: D-Bus service + object path (Wayland apps, org_kde_kwin_appmenu)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub menu: Option<MenuAddress>,
    /// X11 window id (X11 apps register their menu by window id)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x11_id: Option<u32>,
    /// output and Space (1-based) the window is on; none while minimized
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space: Option<usize>,
    /// minimized: PNG picture of the window for its Dock tile
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumb: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MenuAddress {
    pub service: String,
    pub path: String,
}

fn menu_of(w: &WindowElement) -> (Option<MenuAddress>, Option<u32>) {
    use smithay::desktop::WindowSurface;
    match w.0.underlying_surface() {
        WindowSurface::Wayland(t) => (
            crate::appmenu::address(t.wl_surface()).map(|(service, path)| MenuAddress { service, path }),
            None,
        ),
        #[cfg(feature = "xwayland")]
        WindowSurface::X11(x) => (None, Some(x.window_id())),
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event<'a> {
    Windows { windows: &'a [WindowInfo] },
    Ok,
    Error { message: String },
    Outputs { outputs: Vec<OutputInfo> },
    /// Cmd+Tab switcher shown / updated
    Switcher { apps: Vec<String>, selected: usize },
    SwitcherHide,
    /// Spotlight-like launcher requested (Cmd+Space)
    LauncherToggle,
    /// Launchpad requested (F4, hot corner)
    LaunchpadToggle,
    /// settings.json changed; clients should reload it
    SettingsChanged,
    /// volume / brightness / media key pressed (orchard-shell runs it and shows the HUD)
    MediaKey { key: &'static str, fine: bool },
    /// whether any visible window currently overlaps the Dock's resting rect
    DockObstructed { obstructed: bool },
    /// macOS-style screenshot toolbar requested (Print / ⌘⇧5)
    ScreenshotTool { output: String, windows: Vec<ShotWindow> },
}

/// A visible window on the active output, for the screenshot tool's window picker.
#[derive(Debug, Clone, Serialize)]
pub struct ShotWindow {
    pub id: u64,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub title: String,
}

/// State of the Cmd+Tab app switcher (drawn by orchard-shell).
#[derive(Debug, Clone)]
pub struct Switcher {
    pub apps: Vec<String>,
    pub selected: usize,
}

#[derive(Debug, Serialize)]
pub struct OutputInfo {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub scale: f64,
    /// current refresh rate, mHz
    pub refresh: i32,
    /// refresh rates (mHz) available at the current resolution, highest first
    pub refresh_rates: Vec<i32>,
    /// current mode in pixels
    pub mode_w: i32,
    pub mode_h: i32,
    /// shows the menu bar and Dock
    pub primary: bool,
    /// output supports seamless Variable Refresh Rate
    pub vrr_supported: bool,
    /// Variable Refresh Rate is currently active on this output
    pub vrr_enabled: bool,
}

#[derive(Default)]
pub struct IpcState {
    pub path: Option<PathBuf>,
    next_client: u64,
    subscribers: HashMap<u64, UnixStream>,
    last_windows: Vec<WindowInfo>,
    /// app_id -> global logical rect of its Dock tile
    pub dock_tiles: HashMap<String, Rectangle<f64, Logical>>,
    /// minimized window id -> global logical rect of its Dock tile
    pub dock_window_tiles: HashMap<u64, Rectangle<f64, Logical>>,
    /// the tile a window minimized now would get
    pub dock_next_minimized: Option<Rectangle<f64, Logical>>,
    /// global logical rect of the Dock pill when visible (for "avoid windows")
    pub dock_rect: Option<Rectangle<f64, Logical>>,
    /// last broadcast `dock_obstructed` state
    last_dock_obstructed: bool,
    /// layer namespace -> surface-relative rects
    pub backdrops: HashMap<String, Vec<Rect>>,
    /// the running orchard-shell, restarted if it exits
    shell: Option<std::process::Child>,
    /// recent shell start times (crash-loop guard)
    shell_starts: Vec<std::time::Instant>,
}

impl IpcState {
    pub fn is_shell_pid(&self, pid: u32) -> bool {
        self.shell.as_ref().map(|c| c.id() == pid).unwrap_or(false)
    }
}

impl std::fmt::Debug for IpcState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IpcState").field("path", &self.path).finish_non_exhaustive()
    }
}

thread_local! {
    static NEXT_WINDOW_ID: RefCell<u64> = const { RefCell::new(1) };
}

impl WindowElement {
    /// Stable id for IPC.
    pub fn ipc_id(&self) -> WindowId {
        *self.user_data().get_or_insert(|| {
            NEXT_WINDOW_ID.with(|n| {
                let mut n = n.borrow_mut();
                let id = *n;
                *n += 1;
                WindowId(id)
            })
        })
    }
}

/// Longest accepted IPC request line.
const MAX_REQUEST_BYTES: usize = 1 << 20;

fn serialize_line(value: &impl Serialize) -> std::io::Result<Vec<u8>> {
    let mut line = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    line.push(b'\n');
    Ok(line)
}

/// Non-blocking write of a pre-serialized IPC line. Returns `true` to keep the
/// subscriber (including on transient `WouldBlock` so a brief burst doesn't
/// disconnect the shell) and `false` if the peer disconnected or errored.
fn write_nonblocking(stream: &mut UnixStream, line: &[u8]) -> bool {
    match stream.write(line) {
        Ok(n) if n == line.len() => true,
        Ok(0) => false,
        Ok(mut written) => {
            // Partial write: try to finish without blocking, else drop to avoid corrupting framing.
            while written < line.len() {
                match stream.write(&line[written..]) {
                    Ok(0) => return false,
                    Ok(n) => written += n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => return false,
                }
            }
            true
        }
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => true,
        Err(_) => false,
    }
}

fn send_line(stream: &mut UnixStream, value: &impl Serialize) -> std::io::Result<()> {
    let line = serialize_line(value)?;
    if write_nonblocking(stream, &line) {
        Ok(())
    } else {
        Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "ipc write failed"))
    }
}

pub fn socket_path() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".into());
    Some(PathBuf::from(dir).join(format!("orchard-{display}.sock")))
}

impl<BackendData: Backend + 'static> OrchardState<BackendData> {
    pub fn init_ipc(&mut self, handle: &LoopHandle<'static, Self>, socket_name: &str) {
        let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") else { return };
        let path = PathBuf::from(dir).join(format!("orchard-{socket_name}.sock"));
        let _ = std::fs::remove_file(&path);
        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("IPC socket {}: {e}", path.display());
                return;
            }
        };
        let _ = listener.set_nonblocking(true);
        tracing::info!("IPC socket: {}", path.display());
        std::env::set_var("ORCHARD_SOCKET", &path);
        self.ipc.path = Some(path);

        let res = handle.insert_source(
            Generic::new(listener, Interest::READ, Mode::Level),
            |_, listener, state| {
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => state.ipc_add_client(stream),
                        Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                        Err(e) => {
                            tracing::warn!("IPC accept: {e}");
                            break;
                        }
                    }
                }
                Ok(PostAction::Continue)
            },
        );
        if let Err(e) = res {
            tracing::warn!("IPC source: {e}");
        }
    }

    fn ipc_add_client(&mut self, stream: UnixStream) {
        let _ = stream.set_nonblocking(true);
        let id = self.ipc.next_client;
        self.ipc.next_client += 1;
        let mut buf: Vec<u8> = Vec::new();
        let res = self.handle.insert_source(
            Generic::new(stream, Interest::READ, Mode::Level),
            move |_, stream, state| {
                // SAFETY: we never drop or replace the stream inside the callback
                let stream = unsafe { stream.get_mut() };
                let mut chunk = [0u8; 4096];
                loop {
                    match stream.read(&mut chunk) {
                        Ok(0) => {
                            state.ipc.subscribers.remove(&id);
                            return Ok(PostAction::Remove);
                        }
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            // requests are tiny; a client that never sends a newline
                            // must not grow our memory without bound
                            if buf.len() > MAX_REQUEST_BYTES && !buf.contains(&b'\n') {
                                tracing::warn!(client = id, "IPC request too long, disconnecting");
                                state.ipc.subscribers.remove(&id);
                                return Ok(PostAction::Remove);
                            }
                        }
                        Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                        Err(_) => {
                            state.ipc.subscribers.remove(&id);
                            return Ok(PostAction::Remove);
                        }
                    }
                }
                while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                    let line: Vec<u8> = buf.drain(..=pos).collect();
                    let line = String::from_utf8_lossy(&line);
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let reply = match serde_json::from_str::<Request>(line) {
                        Ok(req) => state.ipc_handle(id, stream, req),
                        Err(e) => Some(Event::Error {
                            message: e.to_string(),
                        }),
                    };
                    if let Some(reply) = reply {
                        let _ = send_line(stream, &reply);
                    }
                }
                Ok(PostAction::Continue)
            },
        );
        if let Err(e) = res {
            tracing::warn!("IPC client source: {e}");
        }
    }

    fn window_by_ipc_id(&self, id: u64) -> Option<WindowElement> {
        let in_spaces = self
            .spaces
            .outputs
            .values()
            .flat_map(|o| o.slots.iter())
            .flat_map(|s| s.windows.iter().map(|(w, _)| w).chain(s.fullscreen.iter()));
        self.space
            .elements()
            .chain(self.minimized.iter())
            .chain(in_spaces)
            .find(|w| w.ipc_id().0 == id)
            .cloned()
    }

    fn ipc_handle(&mut self, client: u64, stream: &mut UnixStream, req: Request) -> Option<Event<'static>> {
        match req {
            Request::Subscribe => {
                if let Ok(clone) = stream.try_clone() {
                    self.ipc.subscribers.insert(client, clone);
                }
                let windows = self.window_infos();
                let _ = send_line(stream, &Event::Windows { windows: &windows });
                let obstructed = self.is_dock_obstructed();
                self.ipc.last_dock_obstructed = obstructed;
                let _ = send_line(stream, &Event::DockObstructed { obstructed });
                None
            }
            Request::Windows => {
                let windows = self.window_infos();
                let _ = send_line(stream, &Event::Windows { windows: &windows });
                None
            }
            Request::Activate { id } => {
                let w = self.window_by_ipc_id(id)?;
                self.reveal_window(&w);
                if self.minimized.contains(&w) {
                    self.unminimize(Some(&w));
                } else {
                    self.focus_window(&w);
                }
                Some(Event::Ok)
            }
            Request::ActivateApp { app_id } => {
                self.activate_app(&app_id);
                Some(Event::Ok)
            }
            Request::Minimize { id } => {
                let w = self.window_by_ipc_id(id)?;
                self.minimize_window(&w);
                Some(Event::Ok)
            }
            Request::Close { id } => {
                let w = self.window_by_ipc_id(id)?;
                self.close_window(&w);
                Some(Event::Ok)
            }
            Request::QuitApp { app_id } => {
                let windows: Vec<_> = self
                    .space
                    .elements()
                    .chain(self.minimized.iter())
                    .filter(|w| w.app_id() == app_id)
                    .cloned()
                    .collect();
                for w in windows {
                    self.close_window(&w);
                }
                Some(Event::Ok)
            }
            Request::Action { name } => {
                self.run_named_action(&name);
                Some(Event::Ok)
            }
            Request::SetDockTiles { output, tiles, dock_rect } => {
                let origin = self
                    .space
                    .outputs()
                    .find(|o| o.name() == output)
                    .and_then(|o| self.space.output_geometry(o))
                    .map(|g| g.loc)
                    .unwrap_or_default();
                self.ipc.dock_tiles.clear();
                self.ipc.dock_window_tiles.clear();
                self.ipc.dock_next_minimized = None;
                self.ipc.dock_rect = dock_rect.map(|r| {
                    Rectangle::new(
                        (origin.x as f64 + r.x, origin.y as f64 + r.y).into(),
                        (r.w, r.h).into(),
                    )
                });
                for t in tiles {
                    let r = Rectangle::new(
                        (origin.x as f64 + t.rect.x, origin.y as f64 + t.rect.y).into(),
                        (t.rect.w, t.rect.h).into(),
                    );
                    tracing::debug!(app = %t.app_id, window = ?t.window_id, next = t.next_minimized, x = r.loc.x, y = r.loc.y, w = r.size.w, h = r.size.h, "dock tile");
                    if t.next_minimized {
                        self.ipc.dock_next_minimized = Some(r);
                    } else if let Some(id) = t.window_id {
                        self.ipc.dock_window_tiles.insert(id, r);
                    } else {
                        self.ipc.dock_tiles.insert(t.app_id, r);
                    }
                }
                Some(Event::Ok)
            }
            Request::SetBackdrop { namespace, rects } => {
                tracing::debug!(target: "orchard::backdrop", "set_backdrop {namespace}: {rects:?}");
                self.ipc.backdrops.insert(namespace, rects);
                Some(Event::Ok)
            }
            Request::Spawn { command } => {
                self.spawn(&command);
                Some(Event::Ok)
            }
            Request::ReloadSettings => {
                self.reload_settings();
                Some(Event::Ok)
            }
            Request::ReloadShaders { dir } => {
                crate::effects::reload_shaders(dir.map(PathBuf::from));
                self.fx.blur.invalidate();
                for output in self.space.outputs().cloned().collect::<Vec<_>>() {
                    self.backend_data.reset_buffers(&output);
                }
                tracing::info!("shaders reloaded");
                Some(Event::Ok)
            }
            Request::RestartShell => {
                if std::env::var_os("ORCHARD_NO_SHELL").is_some() {
                    Some(Event::Error { message: "shell disabled (ORCHARD_NO_SHELL)".into() })
                } else {
                    self.restart_shell();
                    Some(Event::Ok)
                }
            }
            Request::ScreenshotCapture {
                mode,
                output,
                rect,
                window_id,
                target,
                include_cursor,
            } => {
                self.capture_from_tool(&mode, output.as_deref(), rect, window_id, target.as_deref(), include_cursor);
                Some(Event::Ok)
            }
            Request::Outputs => {
                let primary = self.primary_output();
                let outputs = self
                    .space
                    .outputs()
                    .filter_map(|o| {
                        let g = self.space.output_geometry(o)?;
                        let cur = o.current_mode();
                        let mut refresh_rates: Vec<i32> = self
                            .backend_data
                            .output_modes(o)
                            .into_iter()
                            .filter(|m| Some(m.size) == cur.map(|c| c.size))
                            .map(|m| m.refresh)
                            .collect();
                        refresh_rates.sort_unstable_by(|a, b| b.cmp(a));
                        refresh_rates.dedup_by(|a, b| (*a - *b).abs() < 50);
                        let (vrr_supported, vrr_enabled) = self.backend_data.output_vrr_info(o);
                        Some(OutputInfo {
                            name: o.name(),
                            x: g.loc.x,
                            y: g.loc.y,
                            w: g.size.w,
                            h: g.size.h,
                            scale: o.current_scale().fractional_scale(),
                            refresh: cur.map(|m| m.refresh).unwrap_or(0),
                            refresh_rates,
                            mode_w: cur.map(|m| m.size.w).unwrap_or(0),
                            mode_h: cur.map(|m| m.size.h).unwrap_or(0),
                            primary: primary.as_ref() == Some(o),
                            vrr_supported,
                            vrr_enabled,
                        })
                    })
                    .collect();
                Some(Event::Outputs { outputs })
            }
        }
    }

    pub fn window_infos(&self) -> Vec<WindowInfo> {
        let focused = self.focused_window();
        let mut out: Vec<WindowInfo> = self
            .space
            .elements()
            .rev()
            .filter(|w| w.alive())
            .map(|w| {
                let (menu, x11_id) = menu_of(w);
                let out_obj = self.window_output(w);
                let output = out_obj.as_ref().map(|o| o.name());
                let space = out_obj.as_ref().map(|o| self.spaces.info(&o.name()).0 + 1);
                WindowInfo {
                    id: w.ipc_id().0,
                    app_id: w.app_id(),
                    title: w.title(),
                    focused: Some(w) == focused.as_ref(),
                    minimized: false,
                    zoomed: self.is_zoomed(w),
                    hidden: false,
                    menu,
                    x11_id,
                    output,
                    space,
                    thumb: None,
                }
            })
            .collect();
        for (name, spaces) in &self.spaces.outputs {
            for (i, slot) in spaces.slots.iter().enumerate() {
                for w in slot.windows.iter().map(|(w, _)| w).chain(slot.fullscreen.iter()) {
                    if w.alive() && !out.iter().any(|i| i.id == w.ipc_id().0) {
                        out.push(WindowInfo {
                            id: w.ipc_id().0,
                            app_id: w.app_id(),
                            title: w.title(),
                            focused: false,
                            minimized: false,
                            zoomed: false,
                            hidden: false,
                            menu: None,
                            x11_id: None,
                            output: Some(name.clone()),
                            space: Some(i + 1),
                            thumb: None,
                        });
                    }
                }
            }
        }
        out.extend(self.minimized.iter().filter(|w| w.alive()).map(|w| {
            let hidden = w.decoration_state().hidden;
            WindowInfo {
                id: w.ipc_id().0,
                app_id: w.app_id(),
                title: w.title(),
                focused: false,
                minimized: !hidden,
                zoomed: false,
                hidden,
                menu: None,
                x11_id: None,
                output: None,
                space: None,
                thumb: (!hidden).then(|| crate::wm::thumb_path_of(w)).flatten(),
            }
        }));
        out
    }

    /// Send an event to every subscriber (serialized once, written non-blockingly).
    pub fn ipc_broadcast(&mut self, ev: &Event<'_>) {
        if self.ipc.subscribers.is_empty() {
            return;
        }
        let Ok(line) = serialize_line(ev) else { return };
        self.ipc.subscribers.retain(|_, stream| write_nonblocking(stream, &line));
    }

    /// Apps in most-recently-used order: stacking order of the current Space,
    /// then other Spaces, then minimized windows.
    pub fn apps_mru(&self) -> Vec<String> {
        let mut apps: Vec<String> = Vec::new();
        let mut push = |a: String| {
            if !a.is_empty() && !apps.contains(&a) {
                apps.push(a);
            }
        };
        if let Some(f) = self.focused_window() {
            push(f.app_id());
        }
        for w in self.space.elements().rev() {
            push(w.app_id());
        }
        for o in self.spaces.outputs.values() {
            for s in &o.slots {
                for (w, _) in &s.windows {
                    push(w.app_id());
                }
            }
        }
        for w in self.minimized.iter().rev() {
            push(w.app_id());
        }
        apps
    }

    /// Cmd+Tab pressed (forward) or Cmd+Shift+Tab (backward).
    pub fn switcher_step(&mut self, forward: bool) {
        match self.switcher.as_mut() {
            Some(sw) => {
                let n = sw.apps.len().max(1);
                sw.selected = if forward { (sw.selected + 1) % n } else { (sw.selected + n - 1) % n };
            }
            None => {
                let apps = self.apps_mru();
                if apps.is_empty() {
                    return;
                }
                let selected = if apps.len() > 1 {
                    if forward { 1 } else { apps.len() - 1 }
                } else {
                    0
                };
                self.switcher = Some(Switcher { apps, selected });
            }
        }
        if let Some(sw) = self.switcher.clone() {
            self.ipc_broadcast(&Event::Switcher {
                apps: sw.apps,
                selected: sw.selected,
            });
        }
    }

    /// Cmd released: activate the selected app.
    pub fn switcher_commit(&mut self) {
        if let Some(sw) = self.switcher.take() {
            self.ipc_broadcast(&Event::SwitcherHide);
            if let Some(app) = sw.apps.get(sw.selected) {
                // Settings ▸ Desktop & Dock: "switch to a Space with open windows"
                let switch = crate::settings::get().desktop.switch_space_on_activate;
                self.activate_app_ext(&app.clone(), switch);
            }
        }
    }

    pub fn switcher_cancel(&mut self) {
        if self.switcher.take().is_some() {
            self.ipc_broadcast(&Event::SwitcherHide);
        }
    }

    /// Whether any visible window (or fullscreen surface) overlaps the Dock's resting rect.
    fn is_dock_obstructed(&self) -> bool {
        let Some(dock) = self.ipc.dock_rect else {
            return false;
        };
        if dock.size.w <= 0.0 || dock.size.h <= 0.0 {
            return false;
        }
        let overlaps = |a: Rectangle<f64, Logical>, b: Rectangle<f64, Logical>| {
            a.loc.x < b.loc.x + b.size.w
                && a.loc.x + a.size.w > b.loc.x
                && a.loc.y < b.loc.y + b.size.h
                && a.loc.y + a.size.h > b.loc.y
        };
        for output in self.space.outputs() {
            let has_fs = output
                .user_data()
                .get::<crate::shell::FullscreenSurface>()
                .and_then(|f| f.get())
                .is_some();
            if has_fs {
                if let Some(geo) = self.space.output_geometry(output) {
                    if overlaps(geo.to_f64(), dock) {
                        return true;
                    }
                }
            }
        }
        self.space.elements().any(|w| {
            w.alive()
                && self
                    .window_global_rect(w)
                    .map(|r| overlaps(r.to_f64(), dock))
                    .unwrap_or(false)
        })
    }

    /// Push a `windows` or `dock_obstructed` event to subscribers if anything changed.
    pub fn ipc_broadcast_if_changed(&mut self) {
        if self.ipc.subscribers.is_empty() {
            return;
        }
        let windows = self.window_infos();
        if windows != self.ipc.last_windows {
            let ev = Event::Windows { windows: &windows };
            if let Ok(line) = serialize_line(&ev) {
                self.ipc.subscribers.retain(|_, stream| write_nonblocking(stream, &line));
            }
            self.ipc.last_windows = windows;
        }
        let obstructed = self.is_dock_obstructed();
        if obstructed != self.ipc.last_dock_obstructed {
            let ev = Event::DockObstructed { obstructed };
            if let Ok(line) = serialize_line(&ev) {
                self.ipc.subscribers.retain(|_, stream| write_nonblocking(stream, &line));
            }
            self.ipc.last_dock_obstructed = obstructed;
        }
    }

    /// Frosted-glass rects for a layer surface namespace, if the shell set any.
    pub fn backdrop_rects(&self, namespace: &str) -> Option<&[Rect]> {
        self.ipc.backdrops.get(namespace).map(|v| v.as_slice())
    }

    pub fn spawn(&self, command: &str) {
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg(command);
        self.child_env(&mut cmd);
        match cmd.spawn() {
            // reap it when it exits, or it stays a zombie for the whole session
            Ok(mut child) => {
                let _ = std::thread::Builder::new()
                    .name("reaper".into())
                    .stack_size(64 * 1024)
                    .spawn(move || {
                        let _ = child.wait();
                    });
            }
            Err(e) => tracing::warn!("spawn {command:?} failed: {e}"),
        }
    }

    /// Like `spawn`, but in its own process group that is terminated when
    /// Orchard exits (`session::stop_autostarted`): autostart daemons have no
    /// Wayland connection that would end them, so they used to outlive the
    /// session, pile up across logins and keep their D-Bus names.
    pub fn spawn_autostart(&self, command: &str) {
        use std::os::unix::process::CommandExt;
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg(command).process_group(0);
        self.child_env(&mut cmd);
        match cmd.spawn() {
            Ok(mut child) => {
                crate::session::track_autostarted(child.id());
                let _ = std::thread::Builder::new()
                    .name("reaper".into())
                    .stack_size(64 * 1024)
                    .spawn(move || {
                        let _ = child.wait();
                    });
            }
            Err(e) => tracing::warn!("autostart {command:?} failed: {e}"),
        }
    }

    /// Session environment for processes started by the compositor.
    pub fn child_env(&self, cmd: &mut std::process::Command) {
        // only meant for this compositor (a nested Orchard must not start locked)
        cmd.env_remove("ORCHARD_START_LOCKED");
        if let Some(s) = &self.socket_name {
            cmd.env("WAYLAND_DISPLAY", s);
        }
        #[cfg(feature = "xwayland")]
        if let Some(d) = self.xdisplay {
            cmd.env("DISPLAY", format!(":{d}"));
        }
        if let Some(p) = &self.ipc.path {
            cmd.env("ORCHARD_SOCKET", p);
        }
    }

    /// Start the menu bar + Dock (`orchard-shell`) unless disabled. The path
    /// is resolved on every start, so a rebuilt binary is picked up.
    pub fn start_shell(&mut self) {
        if std::env::var_os("ORCHARD_NO_SHELL").is_some() {
            return;
        }
        let shell = std::env::var_os("ORCHARD_SHELL")
            .map(PathBuf::from)
            .or_else(|| {
                let exe = std::env::current_exe().ok()?;
                let sibling = exe.parent()?.join("orchard-shell");
                sibling.is_file().then_some(sibling)
            })
            .unwrap_or_else(|| PathBuf::from("orchard-shell"));
        tracing::info!("starting shell: {}", shell.display());
        let mut cmd = std::process::Command::new(&shell);
        self.child_env(&mut cmd);
        match cmd.spawn() {
            Ok(child) => self.ipc.shell = Some(child),
            Err(e) => tracing::warn!("starting {} failed: {e}", shell.display()),
        }
        self.ipc.shell_starts.push(std::time::Instant::now());
    }

    /// Stop the running shell and start it again.
    pub fn restart_shell(&mut self) {
        if let Some(mut child) = self.ipc.shell.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.ipc.shell_starts.clear();
        self.start_shell();
    }

    /// Called every second: restart the shell if it exited (crash, or killed
    /// by `dist/dev.sh`), unless it keeps dying right after starting.
    pub fn supervise_shell(&mut self) {
        let Some(child) = self.ipc.shell.as_mut() else { return };
        let status = match child.try_wait() {
            Ok(Some(status)) => status,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!("shell wait: {e}");
                return;
            }
        };
        self.ipc.shell = None;
        let now = std::time::Instant::now();
        self.ipc
            .shell_starts
            .retain(|t| now.duration_since(*t) < std::time::Duration::from_secs(60));
        if self.ipc.shell_starts.len() >= 5 {
            tracing::error!("orchard-shell exited ({status}) 5 times within a minute; not restarting");
            return;
        }
        tracing::warn!("orchard-shell exited ({status}); restarting");
        self.start_shell();
    }

    /// Dock click: bring an app forward (restore minimized windows if it has no visible ones).
    pub fn activate_app(&mut self, app_id: &str) {
        self.activate_app_ext(app_id, true)
    }

    /// `switch_space`: may jump to another Space holding the app's windows.
    pub fn activate_app_ext(&mut self, app_id: &str, switch_space: bool) {
        let visible: Vec<WindowElement> = self
            .space
            .elements()
            .filter(|w| w.app_id() == app_id)
            .cloned()
            .collect();
        if visible.is_empty() {
            // on another Space? go there
            let elsewhere = self
                .spaces
                .outputs
                .values()
                .flat_map(|o| o.slots.iter())
                .flat_map(|s| s.windows.iter().map(|(w, _)| w))
                .find(|w| w.app_id() == app_id)
                .cloned();
            if let Some(w) = elsewhere.filter(|_| switch_space) {
                self.reveal_window(&w);
                self.focus_window(&w);
                return;
            }
            // ⌘H-hidden windows all come back together
            if self.unhide_app(app_id) {
                return;
            }
            let mini = self.minimized.iter().rev().find(|w| w.app_id() == app_id).cloned();
            if let Some(w) = mini {
                self.unminimize(Some(&w));
            }
            return;
        }
        // raise all, focus the most recently used (topmost)
        for w in &visible {
            self.space.raise_element(w, false);
        }
        if let Some(top) = visible.last() {
            self.focus_window(top);
        }
    }

    /// Bounding box of a window in global coordinates (for IPC consumers).
    pub fn window_global_rect(&self, w: &WindowElement) -> Option<Rectangle<i32, Logical>> {
        let loc = self.space.element_location(w)?;
        Some(Rectangle::new(loc, w.geometry().size))
    }
}
