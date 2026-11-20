//! Global app menus in the menu bar (like macOS).
//!
//! * We own `com.canonical.AppMenu.Registrar`; its presence is what makes Qt
//!   (and GTK with appmenu-gtk-module) export their menu bars over D-Bus.
//!   X11 apps register their menu by window id here.
//! * Wayland Qt apps announce their menu to the compositor instead
//!   (`org_kde_kwin_appmenu`), which passes it on in the window list.
//! * Menus are read with `com.canonical.dbusmenu` (GetLayout / AboutToShow) and
//!   clicks are sent back as `Event(id, "clicked")`.
//! * Wayland Electron apps register with the Registrar too, but with an id that
//!   is no X11 window: those menus are matched to windows by process (the D-Bus
//!   sender's pid, or the Flatpak sandbox it runs in).
//! * GTK apps export `GMenuModel` menus instead (see `gtkmenu`).

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use zbus::{
    blocking::Connection,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};

use crate::menu::{Action, MenuItem};

const DBUSMENU: &str = "com.canonical.dbusmenu";
pub const REGISTRAR: &str = "com.canonical.AppMenu.Registrar";

/// One entry of an app's menu.
#[derive(Debug, Clone, Default)]
pub struct Node {
    pub id: i32,
    pub label: String,
    pub enabled: bool,
    pub visible: bool,
    pub separator: bool,
    /// Some(checked) for checkbox / radio items
    pub toggle: Option<bool>,
    pub shortcut: String,
    pub children: Vec<Node>,
    /// GTK items: the action to run (dbusmenu items are clicked by id)
    pub gtk: Option<crate::gtkmenu::Activate>,
}

/// Where the focused window's menu comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuSource {
    DbusMenu { service: String, path: String },
    Gtk(crate::gtkmenu::GtkMenuAddr),
}

impl MenuSource {
    /// The menu bar's top-level menus `(id, title)`.
    pub fn tops(&self, conn: &Connection) -> Vec<(i32, String)> {
        let root = match self {
            MenuSource::DbusMenu { service, path } => fetch(conn, service, path, 0),
            MenuSource::Gtk(addr) => crate::gtkmenu::fetch(conn, addr),
        };
        root.map(|r| {
            r.children
                .into_iter()
                .filter(|c| c.visible && !c.separator && !c.label.is_empty())
                .map(|c| (c.id, c.label))
                .collect()
        })
        .unwrap_or_default()
    }

    /// Items of the top-level menu `id`, freshly read (check marks, recent files …).
    pub fn items(&self, conn: &Connection, id: i32) -> Vec<MenuItem> {
        match self {
            MenuSource::DbusMenu { service, path } => {
                fetch(conn, service, path, id).map(|n| to_items(&n, service, path)).unwrap_or_default()
            }
            MenuSource::Gtk(addr) => crate::gtkmenu::fetch(conn, addr)
                .and_then(|root| root.children.into_iter().find(|c| c.id == id))
                .map(|n| to_items(&n, &addr.bus, ""))
                .unwrap_or_default(),
        }
    }
}

/// One `RegisterWindow` call.
#[derive(Debug, Clone)]
struct Registration {
    window_id: u32,
    sender: String,
    path: String,
    /// sender's process (for Wayland Electron apps, matched by process)
    pid: Option<u32>,
    seq: u64,
}

type Windows = Arc<Mutex<Vec<Registration>>>;

struct Registrar {
    windows: Windows,
    seq: std::sync::atomic::AtomicU64,
}

#[zbus::interface(name = "com.canonical.AppMenu.Registrar")]
impl Registrar {
    async fn register_window(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        window_id: u32,
        menu_object_path: OwnedObjectPath,
    ) {
        let sender = hdr.sender().map(|s| s.to_string()).unwrap_or_default();
        let pid = match (zbus::fdo::DBusProxy::new(conn).await, hdr.sender()) {
            (Ok(proxy), Some(s)) => proxy.get_connection_unix_process_id(s.clone().into()).await.ok(),
            _ => None,
        };
        tracing::debug!(window_id, sender, ?pid, path = %menu_object_path.as_str(), "menu registered");
        let seq = self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut w) = self.windows.lock() {
            w.retain(|r| !(r.window_id == window_id && r.sender == sender));
            w.push(Registration { window_id, sender, path: menu_object_path.as_str().to_string(), pid, seq });
        }
    }

    fn unregister_window(&self, #[zbus(header)] hdr: zbus::message::Header<'_>, window_id: u32) {
        let sender = hdr.sender().map(|s| s.to_string()).unwrap_or_default();
        if let Ok(mut w) = self.windows.lock() {
            w.retain(|r| !(r.window_id == window_id && r.sender == sender));
        }
    }

    fn get_menu_for_window(&self, window_id: u32) -> zbus::fdo::Result<(String, OwnedObjectPath)> {
        let w = self.windows.lock().map_err(|_| zbus::fdo::Error::Failed("lock".into()))?;
        match w.iter().rev().find(|r| r.window_id == window_id) {
            Some(r) => Ok((
                r.sender.clone(),
                OwnedObjectPath::try_from(r.path.as_str()).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?,
            )),
            None => Err(zbus::fdo::Error::Failed("no menu for this window".into())),
        }
    }
}

/// The `xdg-dbus-proxy` that carries a Flatpak app's session bus.
///
/// A sandboxed app talks to D-Bus through its own proxy, so the Registrar
/// sees the proxy as the sender. Flatpak daemonizes the proxy (it shares no
/// parent with the app), but the app's sandbox bind-mounts the proxy's socket
/// (`…/.dbus-proxy/session-bus-proxy-XXXX` at `/run/flatpak/bus`), and the
/// proxy is the process holding that listening socket.
pub fn flatpak_session_proxy(app_pid: u32) -> Option<u32> {
    let mounts = std::fs::read_to_string(format!("/proc/{app_pid}/mountinfo")).ok()?;
    let name = mounts.lines().find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        let (root, at) = (f.get(3)?, f.get(4)?);
        (*at == "/run/flatpak/bus").then(|| root.rsplit('/').next()).flatten()
    })?;
    if !name.starts_with("session-bus-proxy-") {
        return None;
    }
    let unix = std::fs::read_to_string("/proc/net/unix").ok()?;
    let inode = unix.lines().find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        let path = f.get(7)?;
        (path.rsplit('/').next() == Some(name)).then(|| f.get(6).map(|s| s.to_string())).flatten()
    })?;
    let needle = format!("socket:[{inode}]");
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else { continue };
        if std::fs::read_to_string(format!("/proc/{pid}/comm")).map(|c| c.trim() != "xdg-dbus-proxy").unwrap_or(true) {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else { continue };
        if fds.flatten().any(|fd| std::fs::read_link(fd.path()).map(|t| t.to_string_lossy() == needle).unwrap_or(false)) {
            return Some(pid);
        }
    }
    None
}

pub struct GlobalMenu {
    pub conn: Connection,
    windows: Windows,
    /// window process -> its Flatpak D-Bus proxy (None: not sandboxed)
    proxies: Mutex<HashMap<u32, Option<u32>>>,
}

impl GlobalMenu {
    /// Connect and take the Registrar name (unless someone else has it).
    pub fn start() -> Option<GlobalMenu> {
        if std::env::var_os("ORCHARD_NO_GLOBAL_MENU").is_some() {
            return None;
        }
        let windows: Windows = Default::default();
        let registrar = Registrar { windows: windows.clone(), seq: Default::default() };
        let conn = zbus::blocking::connection::Builder::session()
            .map(|b| b.method_timeout(Duration::from_millis(800)))
            .and_then(|b| b.serve_at("/com/canonical/AppMenu/Registrar", registrar))
            .and_then(|b| b.build())
            .map_err(|e| tracing::warn!("global menu: no session bus: {e}"))
            .ok()?;
        use zbus::fdo::{RequestNameFlags, RequestNameReply};
        match conn.request_name_with_flags(REGISTRAR, RequestNameFlags::DoNotQueue.into()) {
            Ok(RequestNameReply::PrimaryOwner) | Ok(RequestNameReply::AlreadyOwner) => {
                tracing::info!("global menu: serving {REGISTRAR}");
            }
            other => {
                // still useful: Wayland apps announce their menus through the compositor
                tracing::info!("global menu: {REGISTRAR} not acquired ({other:?})");
            }
        }
        Some(GlobalMenu { conn, windows, proxies: Default::default() })
    }

    /// Menu address registered for an X11 window.
    pub fn x11_menu(&self, window_id: u32) -> Option<(String, String)> {
        let w = self.windows.lock().ok()?;
        w.iter().rev().find(|r| r.window_id == window_id).map(|r| (r.sender.clone(), r.path.clone()))
    }

    /// Menu registered by the process of a Wayland window that has no menu
    /// address of its own (Electron). `nth` is the window's position among the
    /// app's windows (oldest first); registrations are matched in the same order.
    pub fn process_menu(&self, pid: u32, nth: usize) -> Option<(String, String)> {
        let mut w = self.windows.lock().ok()?;
        // senders that exited (their windows closed without unregistering)
        w.retain(|r| r.pid.map(|p| std::path::Path::new(&format!("/proc/{p}")).exists()).unwrap_or(true));
        let proxy = self.proxy_of(pid);
        let mut mine: Vec<&Registration> =
            w.iter().filter(|r| r.pid.is_some_and(|p| p == pid || Some(p) == proxy)).collect();
        mine.sort_by_key(|r| r.seq);
        let r = mine.get(nth).or(mine.last())?;
        Some((r.sender.clone(), r.path.clone()))
    }

    /// The Flatpak D-Bus proxy of a window's process (cached; looked up once).
    fn proxy_of(&self, pid: u32) -> Option<u32> {
        let mut cache = self.proxies.lock().ok()?;
        if let Some(p) = cache.get(&pid) {
            // a proxy that went away (app restarted under a reused pid)
            if p.is_none_or(|p| std::path::Path::new(&format!("/proc/{p}")).exists()) {
                return *p;
            }
        }
        let p = flatpak_session_proxy(pid);
        cache.insert(pid, p);
        p
    }

    /// Children of `parent` (0 = the menu bar itself).
    pub fn fetch(&self, service: &str, path: &str, parent: i32) -> Option<Node> {
        fetch(&self.conn, service, path, parent)
    }
}

pub fn fetch(conn: &Connection, service: &str, path: &str, parent: i32) -> Option<Node> {
    if parent != 0 {
        // Qt fills some menus only when they are about to show (recent files …)
        let _ = conn.call_method(Some(service), path, Some(DBUSMENU), "AboutToShow", &(parent));
    }
    let reply = conn
        .call_method(Some(service), path, Some(DBUSMENU), "GetLayout", &(parent, -1i32, Vec::<&str>::new()))
        .map_err(|e| tracing::debug!("GetLayout {service} {path}: {e}"))
        .ok()?;
    let body = reply.body();
    let (_rev, (id, props, children)): (u32, (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>)) =
        body.deserialize().map_err(|e| tracing::debug!("GetLayout body: {e}")).ok()?;
    let mut node = Node {
        id,
        enabled: true,
        visible: true,
        ..Default::default()
    };
    for (k, v) in &props {
        apply_prop(&mut node, k, v);
    }
    node.children = children.iter().filter_map(|c| node_from_value(c)).collect();
    Some(node)
}

/// Send a click on menu item `id`.
pub fn click(conn: &Connection, service: &str, path: &str, id: i32) {
    let ts = 0u32;
    if let Err(e) = conn.call_method(Some(service), path, Some(DBUSMENU), "Event", &(id, "clicked", Value::from(0i32), ts)) {
        tracing::debug!("menu Event: {e}");
    }
}

fn unwrap_variant<'a>(v: &'a Value<'a>) -> &'a Value<'a> {
    match v {
        Value::Value(inner) => unwrap_variant(inner),
        other => other,
    }
}

fn node_from_value(v: &Value<'_>) -> Option<Node> {
    let Value::Structure(st) = unwrap_variant(v) else { return None };
    let f = st.fields();
    if f.len() != 3 {
        return None;
    }
    let Value::I32(id) = unwrap_variant(&f[0]) else { return None };
    let mut node = Node {
        id: *id,
        enabled: true,
        visible: true,
        ..Default::default()
    };
    if let Value::Dict(d) = unwrap_variant(&f[1]) {
        for (k, v) in d.iter() {
            if let Value::Str(k) = k {
                apply_prop(&mut node, k.as_str(), v);
            }
        }
    }
    if let Value::Array(a) = unwrap_variant(&f[2]) {
        node.children = a.iter().filter_map(node_from_value).collect();
    }
    Some(node)
}

fn apply_prop(node: &mut Node, key: &str, v: &Value<'_>) {
    let v = unwrap_variant(v);
    match (key, v) {
        ("label", Value::Str(s)) => node.label = strip_mnemonic(s.as_str()),
        ("enabled", Value::Bool(b)) => node.enabled = *b,
        ("visible", Value::Bool(b)) => node.visible = *b,
        ("type", Value::Str(s)) => node.separator = s.as_str() == "separator",
        ("toggle-state", Value::I32(i)) => node.toggle = Some(*i == 1),
        ("toggle-type", Value::Str(s)) if !s.as_str().is_empty() && node.toggle.is_none() => node.toggle = Some(false),
        ("shortcut", Value::Array(outer)) => {
            if let Some(Value::Array(keys)) = outer.iter().next().map(unwrap_variant) {
                let parts: Vec<String> = keys
                    .iter()
                    .filter_map(|k| match unwrap_variant(k) {
                        Value::Str(s) => Some(s.as_str().to_string()),
                        _ => None,
                    })
                    .collect();
                node.shortcut = format_shortcut(&parts);
            }
        }
        _ => {}
    }
}

/// "_File" -> "File", "Save __As" -> "Save _As"
pub fn strip_mnemonic(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '_' || c == '&' {
            if chars.peek() == Some(&c) {
                out.push(c);
                chars.next();
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// ["Control", "Shift", "S"] -> "⇧⌃S" (macOS order: ⌃⌥⇧⌘)
pub fn format_shortcut(parts: &[String]) -> String {
    let mut mods = String::new();
    let mut key = String::new();
    let has = |m: &str| parts.iter().any(|p| p == m);
    if has("Control") {
        mods.push('⌃');
    }
    if has("Alt") {
        mods.push('⌥');
    }
    if has("Shift") {
        mods.push('⇧');
    }
    if has("Super") {
        mods.push('⌘');
    }
    for p in parts {
        if !matches!(p.as_str(), "Control" | "Alt" | "Shift" | "Super") {
            key = match p.as_str() {
                "Return" => "↩".into(),
                "Delete" => "⌦".into(),
                "BackSpace" => "⌫".into(),
                "Escape" => "⎋".into(),
                "Tab" => "⇥".into(),
                "Up" => "↑".into(),
                "Down" => "↓".into(),
                "Left" => "←".into(),
                "Right" => "→".into(),
                "plus" => "+".into(),
                "minus" => "-".into(),
                "comma" => ",".into(),
                "period" => ".".into(),
                k if k.chars().count() == 1 => k.to_uppercase(),
                k => k.to_string(),
            };
        }
    }
    format!("{mods}{key}")
}

/// Menu items for the drop-down menu of a node's children.
pub fn to_items(node: &Node, service: &str, path: &str) -> Vec<MenuItem> {
    let mut items: Vec<MenuItem> = Vec::new();
    for c in node.children.iter().filter(|c| c.visible) {
        if c.separator {
            // no leading / doubled separators
            if items.last().map(|i| !i.separator).unwrap_or(false) {
                items.push(MenuItem::separator());
            }
            continue;
        }
        let mut item = if !c.children.is_empty() {
            let mut i = MenuItem::disabled(c.label.clone());
            i.submenu = to_items(c, service, path);
            i
        } else if c.enabled {
            MenuItem::new(
                c.label.clone(),
                Action::DbusMenu {
                    service: service.to_string(),
                    path: path.to_string(),
                    id: c.id,
                },
            )
        } else {
            MenuItem::disabled(c.label.clone())
        };
        if let (Some(act), true) = (&c.gtk, c.enabled && c.children.is_empty()) {
            item.action = Some(Action::GtkMenu { bus: service.to_string(), act: act.clone() });
        }
        item.shortcut = c.shortcut.clone();
        item.checked = c.toggle;
        items.push(item);
    }
    while items.last().map(|i| i.separator).unwrap_or(false) {
        items.pop();
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsandboxed_processes_have_no_proxy() {
        assert_eq!(flatpak_session_proxy(std::process::id()), None);
        assert_eq!(flatpak_session_proxy(u32::MAX), None);
    }

    /// Start a Flatpak (no window) and check its proxy is found:
    /// `ORCHARD_FLATPAK_TEST=<app pid inside the sandbox> cargo test -p orchard-shell flatpak_proxy -- --ignored`
    #[test]
    #[ignore]
    fn flatpak_proxy_is_found() {
        let Some(pid) = std::env::var("ORCHARD_FLATPAK_TEST").ok().and_then(|p| p.parse().ok()) else { return };
        let proxy = flatpak_session_proxy(pid).expect("proxy found");
        let comm = std::fs::read_to_string(format!("/proc/{proxy}/comm")).unwrap();
        assert_eq!(comm.trim(), "xdg-dbus-proxy");
    }

    /// A Wayland Electron app registers a non-X11 id; the registrar must find
    /// the sender's process and hand the menu to that process's window.
    /// Needs a session bus: `cargo test -p orchard-shell electron -- --ignored`
    #[test]
    #[ignore]
    fn electron_registration_is_found_by_process() {
        let Some(g) = GlobalMenu::start() else { return };
        let me = g.conn.unique_name().unwrap().to_string();
        let client = Connection::session().unwrap();
        let path = OwnedObjectPath::try_from("/MenuBar/7").unwrap();
        client
            .call_method(Some(me.as_str()), "/com/canonical/AppMenu/Registrar", Some(REGISTRAR), "RegisterWindow", &(7u32, path))
            .unwrap();
        let sender = client.unique_name().unwrap().to_string();
        let found = g.process_menu(std::process::id(), 0);
        assert_eq!(found, Some((sender.clone(), "/MenuBar/7".to_string())));
        // not an X11 id of ours, but still looked up by id for X11 apps
        assert_eq!(g.x11_menu(7), Some((sender, "/MenuBar/7".to_string())));
        // some other process gets nothing
        assert_eq!(g.process_menu(1, 0), None);
    }

    #[test]
    fn mnemonics() {
        assert_eq!(strip_mnemonic("_File"), "File");
        assert_eq!(strip_mnemonic("Save __As"), "Save _As");
        assert_eq!(strip_mnemonic("&Edit"), "Edit");
    }

    #[test]
    fn shortcuts() {
        let p = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(format_shortcut(&p(&["Control", "S"])), "⌃S");
        assert_eq!(format_shortcut(&p(&["Control", "Shift", "s"])), "⌃⇧S");
        assert_eq!(format_shortcut(&p(&["Alt", "Return"])), "⌥↩");
    }
}
