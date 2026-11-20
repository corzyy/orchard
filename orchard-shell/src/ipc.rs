//! Client side of the Orchard JSON IPC.

use std::{
    io::{ErrorKind, Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
};

use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct WindowInfo {
    pub id: u64,
    pub app_id: String,
    pub title: String,
    pub focused: bool,
    pub minimized: bool,
    #[serde(default)]
    pub zoomed: bool,
    /// hidden with ⌘H
    #[serde(default)]
    pub hidden: bool,
    /// global menu address (Wayland apps)
    #[serde(default)]
    pub menu: Option<MenuAddress>,
    /// X11 window id (X11 apps register their menu with us by id)
    #[serde(default)]
    pub x11_id: Option<u32>,
    /// global menu of a GTK app
    #[serde(default)]
    pub gtk_menu: Option<crate::gtkmenu::GtkMenuAddr>,
    /// the window's process (Electron menus are registered per process)
    #[serde(default)]
    pub pid: Option<u32>,
    /// minimized: PNG of the window for its Dock tile
    #[serde(default)]
    pub thumb: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct MenuAddress {
    pub service: String,
    pub path: String,
}

#[derive(Debug)]
pub enum Event {
    Windows(Vec<WindowInfo>),
    Other(Value),
}

pub struct Ipc {
    stream: UnixStream,
    buf: Vec<u8>,
}

fn socket_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ORCHARD_SOCKET") {
        return Some(PathBuf::from(p));
    }
    let dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    let display = std::env::var("WAYLAND_DISPLAY").ok()?;
    Some(PathBuf::from(dir).join(format!("orchard-{display}.sock")))
}

impl Ipc {
    pub fn connect() -> std::io::Result<Ipc> {
        let path = socket_path().ok_or_else(|| std::io::Error::other("no ORCHARD_SOCKET / WAYLAND_DISPLAY"))?;
        let stream = UnixStream::connect(path)?;
        stream.set_nonblocking(true)?;
        Ok(Ipc { stream, buf: Vec::new() })
    }

    pub fn stream(&self) -> &UnixStream {
        &self.stream
    }

    pub fn send(&mut self, value: Value) {
        let mut line = value.to_string().into_bytes();
        line.push(b'\n');
        let _ = self.stream.set_nonblocking(false);
        if let Err(e) = self.stream.write_all(&line) {
            tracing::warn!("IPC send failed: {e}");
        }
        let _ = self.stream.set_nonblocking(true);
    }

    pub fn request(&mut self, name: &str, args: Value) {
        let mut v = json!({ "request": name });
        if let (Some(obj), Value::Object(extra)) = (v.as_object_mut(), args) {
            obj.extend(extra);
        }
        self.send(v);
    }

    /// Read everything available; returns parsed events. `Err` = disconnected.
    pub fn poll(&mut self) -> Result<Vec<Event>, ()> {
        let mut chunk = [0u8; 8192];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err(()),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(_) => return Err(()),
            }
        }
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let Ok(v) = serde_json::from_slice::<Value>(&line) else { continue };
            if v.get("event").and_then(|e| e.as_str()) == Some("windows") {
                if let Ok(ws) = serde_json::from_value::<Vec<WindowInfo>>(v["windows"].clone()) {
                    out.push(Event::Windows(ws));
                    continue;
                }
            }
            out.push(Event::Other(v));
        }
        Ok(out)
    }
}
