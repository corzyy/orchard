//! GTK global menus: `GMenuModel` menu bars exported over D-Bus.
//!
//! GTK 3/4 apps announce where their menu bar lives through the compositor
//! (`gtk_shell1.set_dbus_properties`). The menu itself is read with
//! `org.gtk.Menus.Start` (menus come in numbered groups; sections and
//! submenus point at other groups), and items run `org.gtk.Actions.Activate`
//! on the app (`app.*`) or window (`win.*`) action group.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use zbus::{
    blocking::Connection,
    zvariant::{OwnedValue, Value},
};

use crate::globalmenu::{format_shortcut, strip_mnemonic, Node};

const MENUS: &str = "org.gtk.Menus";
const ACTIONS: &str = "org.gtk.Actions";
/// A malformed export must not make us loop forever.
const MAX_GROUPS: usize = 64;

/// Where a GTK window's menus live (sent by the compositor in the window list).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GtkMenuAddr {
    pub bus: String,
    pub menubar: String,
    #[serde(default)]
    pub app: Option<String>,
    #[serde(default)]
    pub win: Option<String>,
}

/// A GVariant action parameter (the menu item's `target`).
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Str(String),
    Bool(bool),
    I32(i32),
    U32(u32),
    I64(i64),
    U64(u64),
    F64(f64),
}

impl Target {
    fn from_value(v: &Value<'_>) -> Option<Target> {
        Some(match unwrap(v) {
            Value::Str(s) => Target::Str(s.as_str().to_string()),
            Value::Bool(b) => Target::Bool(*b),
            Value::I32(i) => Target::I32(*i),
            Value::U32(u) => Target::U32(*u),
            Value::I64(i) => Target::I64(*i),
            Value::U64(u) => Target::U64(*u),
            Value::F64(f) => Target::F64(*f),
            _ => return None,
        })
    }

    fn to_value(&self) -> Value<'static> {
        match self {
            Target::Str(s) => Value::from(s.clone()),
            Target::Bool(b) => Value::from(*b),
            Target::I32(i) => Value::from(*i),
            Target::U32(u) => Value::from(*u),
            Target::I64(i) => Value::from(*i),
            Target::U64(u) => Value::from(*u),
            Target::F64(f) => Value::from(*f),
        }
    }
}

/// What clicking a GTK menu item runs.
#[derive(Debug, Clone, PartialEq)]
pub struct Activate {
    /// object path of the action group (`app` or `win`)
    pub path: String,
    /// action name without its `app.` / `win.` prefix
    pub name: String,
    pub target: Option<Target>,
}

fn unwrap<'a>(v: &'a Value<'a>) -> &'a Value<'a> {
    match v {
        Value::Value(inner) => unwrap(inner),
        other => other,
    }
}

/// One `org.gtk.Menus` item, decoded.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Item {
    pub label: Option<String>,
    pub action: Option<String>,
    pub target: Option<Target>,
    pub accel: Option<String>,
    pub hidden_when: Option<String>,
    pub section: Option<(u32, u32)>,
    pub submenu: Option<(u32, u32)>,
}

fn pair(v: &Value<'_>) -> Option<(u32, u32)> {
    let Value::Structure(st) = unwrap(v) else { return None };
    match st.fields() {
        [a, b] => match (unwrap(a), unwrap(b)) {
            (Value::U32(a), Value::U32(b)) => Some((*a, *b)),
            _ => None,
        },
        _ => None,
    }
}

fn decode_item(props: &HashMap<String, OwnedValue>) -> Item {
    let s = |k: &str| match props.get(k).map(|v| unwrap(v)) {
        Some(Value::Str(s)) => Some(s.as_str().to_string()),
        _ => None,
    };
    Item {
        label: s("label"),
        action: s("action"),
        target: props.get("target").and_then(|v| Target::from_value(v)),
        accel: s("accel"),
        hidden_when: s("hidden-when"),
        section: props.get(":section").and_then(|v| pair(v)),
        submenu: props.get(":submenu").and_then(|v| pair(v)),
    }
}

/// State of one action from `DescribeAll`.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionState {
    pub enabled: bool,
    pub state: Option<Target>,
}

/// `<Primary><Shift>s` → `⌃⇧S` (macOS modifier order)
pub fn accel_to_shortcut(accel: &str) -> String {
    let mut parts = Vec::new();
    let mut rest = accel;
    while let Some(start) = rest.find('<') {
        let Some(end) = rest[start..].find('>') else { break };
        let m = &rest[start + 1..start + end];
        parts.push(
            match m {
                "Primary" | "Control" | "Ctrl" | "ctrl" | "control" => "Control",
                "Shift" | "shift" => "Shift",
                "Alt" | "Mod1" | "alt" => "Alt",
                "Super" | "Meta" | "super" => "Super",
                other => other,
            }
            .to_string(),
        );
        rest = &rest[start + end + 1..];
    }
    let key = rest.trim();
    if key.is_empty() {
        return String::new();
    }
    parts.push(key.to_string());
    format_shortcut(&parts)
}

/// Build the menu bar from the loaded menus. `actions` maps `app.x` / `win.x`
/// to their state; ids are handed out in order so a re-read gives the same ids.
pub fn build(
    menus: &HashMap<(u32, u32), Vec<Item>>,
    actions: &HashMap<String, ActionState>,
    paths: (&Option<String>, &Option<String>),
) -> Node {
    let mut next = 1;
    let mut root = Node { id: 0, enabled: true, visible: true, ..Default::default() };
    let mut seen = HashSet::new();
    root.children = build_menu(menus, actions, paths, (0, 0), &mut next, &mut seen);
    root
}

fn build_menu(
    menus: &HashMap<(u32, u32), Vec<Item>>,
    actions: &HashMap<String, ActionState>,
    paths: (&Option<String>, &Option<String>),
    key: (u32, u32),
    next: &mut i32,
    seen: &mut HashSet<(u32, u32)>,
) -> Vec<Node> {
    // a menu that (indirectly) contains itself
    if !seen.insert(key) {
        return Vec::new();
    }
    let mut out: Vec<Node> = Vec::new();
    for item in menus.get(&key).map(Vec::as_slice).unwrap_or(&[]) {
        if let Some(sec) = item.section {
            let inner = build_menu(menus, actions, paths, sec, next, seen);
            if inner.iter().any(|n| n.visible) {
                if !out.is_empty() {
                    out.push(Node { separator: true, visible: true, ..Default::default() });
                }
                out.extend(inner);
            }
            continue;
        }
        let label = strip_mnemonic(item.label.as_deref().unwrap_or(""));
        let id = *next;
        *next += 1;
        if let Some(sub) = item.submenu {
            let children = build_menu(menus, actions, paths, sub, next, seen);
            out.push(Node { id, label, enabled: true, visible: true, children, ..Default::default() });
            continue;
        }
        let state = item.action.as_deref().and_then(|a| actions.get(a));
        let missing = item.action.is_some() && state.is_none();
        let enabled = state.map(|s| s.enabled).unwrap_or(false);
        let visible = match item.hidden_when.as_deref() {
            Some("action-missing") => !missing,
            Some("action-disabled") => enabled,
            _ => true,
        };
        let toggle = state.and_then(|s| match (&s.state, &item.target) {
            (Some(Target::Bool(b)), None) => Some(*b),
            (Some(cur), Some(t)) => Some(cur == t),
            _ => None,
        });
        let activate = item.action.as_deref().and_then(|a| {
            let (group, name) = a.split_once('.')?;
            let path = match group {
                "app" => paths.0.clone()?,
                "win" => paths.1.clone()?,
                _ => return None,
            };
            Some(Activate { path, name: name.to_string(), target: item.target.clone() })
        });
        out.push(Node {
            id,
            label,
            enabled: enabled && activate.is_some(),
            visible,
            toggle,
            shortcut: item.accel.as_deref().map(accel_to_shortcut).unwrap_or_default(),
            gtk: activate,
            ..Default::default()
        });
    }
    seen.remove(&key);
    out
}

/// Read the whole menu bar of a GTK window.
pub fn fetch(conn: &Connection, addr: &GtkMenuAddr) -> Option<Node> {
    let mut menus: HashMap<(u32, u32), Vec<Item>> = HashMap::new();
    let mut loaded: Vec<u32> = Vec::new();
    let mut pending = vec![0u32];
    while !pending.is_empty() && loaded.len() < MAX_GROUPS {
        let reply = conn
            .call_method(Some(addr.bus.as_str()), addr.menubar.as_str(), Some(MENUS), "Start", &(pending.clone(),))
            .map_err(|e| tracing::debug!("gtk menus Start {}: {e}", addr.bus))
            .ok()?;
        let body = reply.body();
        let (groups,): (Vec<(u32, u32, Vec<HashMap<String, OwnedValue>>)>,) = body
            .deserialize()
            .map_err(|e| tracing::debug!("gtk menus Start body: {e}"))
            .ok()?;
        loaded.append(&mut pending);
        for (g, m, items) in groups {
            menus.insert((g, m), items.iter().map(decode_item).collect());
        }
        let mut want: Vec<u32> = menus
            .values()
            .flatten()
            .filter_map(|i| i.section.or(i.submenu).map(|(g, _)| g))
            .filter(|g| !loaded.contains(g))
            .collect();
        want.sort_unstable();
        want.dedup();
        pending = want;
    }
    // we read once and re-read when a menu opens: no change signals needed
    let _ = conn.call_method(Some(addr.bus.as_str()), addr.menubar.as_str(), Some(MENUS), "End", &(loaded,));

    let mut actions = HashMap::new();
    for (prefix, path) in [("app", &addr.app), ("win", &addr.win)] {
        let Some(path) = path else { continue };
        for (name, st) in describe_all(conn, &addr.bus, path) {
            actions.insert(format!("{prefix}.{name}"), st);
        }
    }
    Some(build(&menus, &actions, (&addr.app, &addr.win)))
}

fn describe_all(conn: &Connection, bus: &str, path: &str) -> HashMap<String, ActionState> {
    let Ok(reply) = conn.call_method(Some(bus), path, Some(ACTIONS), "DescribeAll", &()) else {
        return HashMap::new();
    };
    let body = reply.body();
    let parsed: Result<(HashMap<String, (bool, zbus::zvariant::Signature, Vec<OwnedValue>)>,), _> = body.deserialize();
    match parsed {
        Ok((map,)) => map
            .into_iter()
            .map(|(name, (enabled, _sig, state))| {
                let state = state.first().and_then(|v| Target::from_value(v));
                (name, ActionState { enabled, state })
            })
            .collect(),
        Err(e) => {
            tracing::debug!("gtk actions DescribeAll {bus}{path}: {e}");
            HashMap::new()
        }
    }
}

/// Run a GTK menu item.
pub fn activate(conn: &Connection, bus: &str, act: &Activate) {
    let params: Vec<Value<'_>> = act.target.iter().map(Target::to_value).collect();
    let platform: HashMap<&str, Value<'_>> = HashMap::new();
    if let Err(e) = conn.call_method(Some(bus), act.path.as_str(), Some(ACTIONS), "Activate", &(act.name.as_str(), params, platform)) {
        tracing::debug!("gtk Activate {}: {e}", act.name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str, action: &str) -> Item {
        Item { label: Some(label.into()), action: Some(action.into()), ..Default::default() }
    }

    #[test]
    fn builds_sections_submenus_and_states() {
        let mut menus = HashMap::new();
        // menu bar: File, View
        menus.insert(
            (0, 0),
            vec![
                Item { label: Some("_File".into()), submenu: Some((1, 0)), ..Default::default() },
                Item { label: Some("_View".into()), submenu: Some((1, 1)), ..Default::default() },
            ],
        );
        // File: two sections
        menus.insert((1, 0), vec![Item { section: Some((2, 0)), ..Default::default() }, Item { section: Some((2, 1)), ..Default::default() }]);
        menus.insert(
            (2, 0),
            vec![
                Item { accel: Some("<Primary>o".into()), ..item("_Open…", "win.open") },
                Item { hidden_when: Some("action-missing".into()), ..item("Gone", "app.nothing") },
            ],
        );
        menus.insert((2, 1), vec![item("_Quit", "app.quit")]);
        // View: a check item and radio items
        menus.insert(
            (1, 1),
            vec![
                item("Statusbar", "win.statusbar"),
                Item { target: Some(Target::Str("light".into())), ..item("Light", "app.theme") },
                Item { target: Some(Target::Str("dark".into())), ..item("Dark", "app.theme") },
            ],
        );
        let mut actions = HashMap::new();
        actions.insert("win.open".to_string(), ActionState { enabled: true, state: None });
        actions.insert("app.quit".to_string(), ActionState { enabled: true, state: None });
        actions.insert("win.statusbar".to_string(), ActionState { enabled: true, state: Some(Target::Bool(true)) });
        actions.insert("app.theme".to_string(), ActionState { enabled: true, state: Some(Target::Str("dark".into())) });
        let app = Some("/org/example/App".to_string());
        let win = Some("/org/example/App/window/1".to_string());
        let root = build(&menus, &actions, (&app, &win));

        let titles: Vec<&str> = root.children.iter().map(|n| n.label.as_str()).collect();
        assert_eq!(titles, ["File", "View"]);

        let file = &root.children[0];
        let labels: Vec<(&str, bool, bool)> =
            file.children.iter().map(|n| (n.label.as_str(), n.separator, n.visible)).collect();
        assert_eq!(labels, [("Open…", false, true), ("Gone", false, false), ("", true, true), ("Quit", false, true)]);
        let open = &file.children[0];
        assert_eq!(open.shortcut, "⌃O");
        assert!(open.enabled);
        assert_eq!(open.gtk.as_ref().unwrap().path, "/org/example/App/window/1");
        assert_eq!(open.gtk.as_ref().unwrap().name, "open");
        assert_eq!(file.children[3].gtk.as_ref().unwrap().path, "/org/example/App");

        let view = &root.children[1];
        assert_eq!(view.children[0].toggle, Some(true));
        assert_eq!(view.children[1].toggle, Some(false));
        assert_eq!(view.children[2].toggle, Some(true));
        assert_eq!(view.children[2].gtk.as_ref().unwrap().target, Some(Target::Str("dark".into())));

        // ids are stable across rebuilds (menus are re-read when opened)
        let again = build(&menus, &actions, (&app, &win));
        assert_eq!(again.children[1].id, view.id);
        assert_eq!(again.children[1].children[2].id, view.children[2].id);
    }

    #[test]
    fn missing_or_unknown_actions_are_disabled_and_cycles_end() {
        let mut menus = HashMap::new();
        menus.insert((0, 0), vec![Item { label: Some("Loop".into()), submenu: Some((0, 0)), ..Default::default() }]);
        let root = build(&menus, &HashMap::new(), (&None, &None));
        assert_eq!(root.children.len(), 1);
        assert!(root.children[0].children.is_empty(), "self-reference is cut");

        let mut menus = HashMap::new();
        menus.insert((0, 0), vec![item("Do", "app.do"), item("Other", "foo.bar")]);
        let mut actions = HashMap::new();
        actions.insert("app.do".to_string(), ActionState { enabled: true, state: None });
        actions.insert("foo.bar".to_string(), ActionState { enabled: true, state: None });
        // no app path known: cannot activate
        let root = build(&menus, &actions, (&None, &None));
        assert!(!root.children[0].enabled);
        assert!(!root.children[1].enabled, "unknown action group");
    }

    /// Against a real GLib export on the session bus:
    /// `ORCHARD_GTKMENU_TEST=<unique bus name> cargo test -p orchard-shell real_glib -- --ignored`
    /// (the exporting script mirrors what GtkApplication publishes).
    #[test]
    #[ignore]
    fn real_glib_export() {
        let Ok(bus) = std::env::var("ORCHARD_GTKMENU_TEST") else { return };
        let conn = Connection::session().unwrap();
        let addr = GtkMenuAddr {
            bus: bus.clone(),
            menubar: "/org/orchard/MenuTest/menus/menubar".into(),
            app: Some("/org/orchard/MenuTest".into()),
            win: Some("/org/orchard/MenuTest/window/1".into()),
        };
        let root = fetch(&conn, &addr).expect("menu read");
        let titles: Vec<&str> = root.children.iter().map(|n| n.label.as_str()).collect();
        assert_eq!(titles, ["File", "View"]);
        let file = &root.children[0];
        let rows: Vec<(&str, bool, bool, &str)> =
            file.children.iter().map(|n| (n.label.as_str(), n.separator, n.enabled, n.shortcut.as_str())).collect();
        assert_eq!(rows, [("Open…", false, true, "⌃O"), ("Disabled", false, false, ""), ("", true, false, ""), ("Quit", false, true, "")]);
        let view = &root.children[1];
        let toggles: Vec<Option<bool>> = view.children.iter().map(|n| n.toggle).collect();
        assert_eq!(toggles, [Some(true), Some(false), Some(true)]);
        // run "Open…", "Quit" and the "Light" radio item
        activate(&conn, &bus, file.children[0].gtk.as_ref().unwrap());
        activate(&conn, &bus, file.children[3].gtk.as_ref().unwrap());
        activate(&conn, &bus, view.children[1].gtk.as_ref().unwrap());
        // MenuSource path the shell uses when a menu opens
        let src = crate::globalmenu::MenuSource::Gtk(addr);
        let tops = src.tops(&conn);
        assert_eq!(tops.iter().map(|t| t.1.as_str()).collect::<Vec<_>>(), ["File", "View"]);
        let items = src.items(&conn, tops[1].0);
        assert_eq!(items.len(), 3);
        assert!(matches!(items[0].action, Some(crate::menu::Action::GtkMenu { .. })));
    }

    #[test]
    fn accelerators() {
        assert_eq!(accel_to_shortcut("<Primary><Shift>z"), "⌃⇧Z");
        assert_eq!(accel_to_shortcut("<Control>q"), "⌃Q");
        assert_eq!(accel_to_shortcut("<Alt>Return"), "⌥↩");
        assert_eq!(accel_to_shortcut("F11"), "F11");
        assert_eq!(accel_to_shortcut(""), "");
    }
}
