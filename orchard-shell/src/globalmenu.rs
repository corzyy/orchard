//! Global app menus in the menu bar (like macOS).
//!
//! * We own `com.canonical.AppMenu.Registrar`; its presence is what makes Qt
//!   (and GTK with appmenu-gtk-module) export their menu bars over D-Bus.
//!   X11 apps register their menu by window id here.
//! * Wayland Qt apps announce their menu to the compositor instead
//!   (`org_kde_kwin_appmenu`), which passes it on in the window list.
//! * Menus are read with `com.canonical.dbusmenu` (GetLayout / AboutToShow) and
//!   clicks are sent back as `Event(id, "clicked")`.

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
}

/// window id -> (sender, menu object path), for X11 apps
type Windows = Arc<Mutex<HashMap<u32, (String, String)>>>;

struct Registrar {
    windows: Windows,
}

#[zbus::interface(name = "com.canonical.AppMenu.Registrar")]
impl Registrar {
    fn register_window(&self, #[zbus(header)] hdr: zbus::message::Header<'_>, window_id: u32, menu_object_path: OwnedObjectPath) {
        let sender = hdr.sender().map(|s| s.to_string()).unwrap_or_default();
        tracing::debug!(window_id, sender, path = %menu_object_path.as_str(), "menu registered");
        if let Ok(mut w) = self.windows.lock() {
            w.insert(window_id, (sender, menu_object_path.as_str().to_string()));
        }
    }

    fn unregister_window(&self, window_id: u32) {
        if let Ok(mut w) = self.windows.lock() {
            w.remove(&window_id);
        }
    }

    fn get_menu_for_window(&self, window_id: u32) -> zbus::fdo::Result<(String, OwnedObjectPath)> {
        let w = self.windows.lock().map_err(|_| zbus::fdo::Error::Failed("lock".into()))?;
        match w.get(&window_id) {
            Some((s, p)) => Ok((s.clone(), OwnedObjectPath::try_from(p.as_str()).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?)),
            None => Err(zbus::fdo::Error::Failed("no menu for this window".into())),
        }
    }
}

pub struct GlobalMenu {
    pub conn: Connection,
    windows: Windows,
}

impl GlobalMenu {
    /// Connect and take the Registrar name (unless someone else has it).
    pub fn start() -> Option<GlobalMenu> {
        if std::env::var_os("ORCHARD_NO_GLOBAL_MENU").is_some() {
            return None;
        }
        let windows: Windows = Default::default();
        let conn = zbus::blocking::connection::Builder::session()
            .map(|b| b.method_timeout(Duration::from_millis(800)))
            .and_then(|b| b.serve_at("/com/canonical/AppMenu/Registrar", Registrar { windows: windows.clone() }))
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
        Some(GlobalMenu { conn, windows })
    }

    /// Menu address registered for an X11 window.
    pub fn x11_menu(&self, window_id: u32) -> Option<(String, String)> {
        self.windows.lock().ok()?.get(&window_id).cloned()
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
