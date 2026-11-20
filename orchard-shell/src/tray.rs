//! System tray (`org.kde.StatusNotifierWatcher` + `org.kde.StatusNotifierItem`).
//!
//! Serves `/StatusNotifierWatcher` on the session bus so tray apps (Discord,
//! Steam, Nextcloud, OBS, Telegram, VPNs …) register their icons and menus.
//! Runs on a dedicated background thread and pushes snapshots to the shell
//! over a calloop channel.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use smithay_client_toolkit::reexports::calloop::channel::Sender;
use zbus::{
    blocking::Connection,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};

const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const SNI_IFACE: &str = "org.kde.StatusNotifierItem";

#[derive(Debug, Clone)]
pub struct TrayItem {
    pub service: String,
    pub path: String,
    pub title: String,
    /// `"Active"`, `"NeedsAttention"`, or `"Passive"`
    pub status: String,
    pub icon_name: Option<String>,
    /// Largest ARGB icon converted to premultiplied RGBA8: `(width, height, rgba)`
    pub icon_pixmap: Option<(u32, u32, Vec<u8>)>,
    /// `com.canonical.dbusmenu` object path, if exposed
    pub menu_path: Option<String>,
    /// True if left-click should open the menu directly instead of Activate()
    pub item_is_menu: bool,
}

impl TrayItem {
    pub fn visible(&self) -> bool {
        self.status != "Passive"
    }
}

struct Shared {
    items: Mutex<Vec<TrayItem>>,
    tx: Sender<Vec<TrayItem>>,
    refresh_tx: std::sync::mpsc::Sender<TrayCmd>,
}

enum TrayCmd {
    Register { service: String, path: String },
    RefreshAll,
}

struct Watcher {
    shared: Arc<Shared>,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    fn register_status_notifier_item(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        service: &str,
    ) {
        let (bus, path) = if service.starts_with('/') {
            let sender = hdr.sender().map(|s| s.to_string()).unwrap_or_default();
            (sender, service.to_string())
        } else {
            (service.to_string(), "/StatusNotifierItem".to_string())
        };
        if bus.is_empty() {
            return;
        }
        tracing::debug!(bus, path, "tray: register item");
        let _ = self.shared.refresh_tx.send(TrayCmd::Register { service: bus, path });
    }

    fn register_status_notifier_host(&self, _service: &str) {}

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.shared
            .items
            .lock()
            .map(|items| items.iter().map(|i| format!("{}{}", i.service, i.path)).collect())
            .unwrap_or_default()
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }
}

pub struct Tray {
    conn: Connection,
}

impl Tray {
    pub fn start(tx: Sender<Vec<TrayItem>>) -> Option<Tray> {
        if std::env::var_os("ORCHARD_NO_TRAY").is_some() {
            return None;
        }
        let (refresh_tx, refresh_rx) = std::sync::mpsc::channel::<TrayCmd>();
        let shared = Arc::new(Shared {
            items: Mutex::new(Vec::new()),
            tx,
            refresh_tx: refresh_tx.clone(),
        });

        let conn = zbus::blocking::connection::Builder::session()
            .map(|b| b.method_timeout(Duration::from_millis(800)))
            .and_then(|b| b.serve_at(WATCHER_PATH, Watcher { shared: shared.clone() }))
            .and_then(|b| b.build())
            .map_err(|e| tracing::warn!("tray: no session bus: {e}"))
            .ok()?;

        use zbus::fdo::{RequestNameFlags, RequestNameReply};
        match conn.request_name_with_flags(WATCHER_NAME, RequestNameFlags::DoNotQueue.into()) {
            Ok(RequestNameReply::PrimaryOwner) | Ok(RequestNameReply::AlreadyOwner) => {
                tracing::info!("tray: serving {WATCHER_NAME}");
            }
            other => {
                tracing::info!("tray: {WATCHER_NAME} not acquired ({other:?})");
            }
        }

        // Background worker: fetches SNI properties, removes dead items, pushes snapshots.
        let worker_conn = conn.clone();
        let worker_shared = shared.clone();
        let _ = std::thread::Builder::new().name("orchard-tray".into()).spawn(move || {
            let mut order: Vec<(String, String)> = Vec::new();
            let mut map: HashMap<String, TrayItem> = HashMap::new();

            let publish = |order: &[(String, String)], map: &HashMap<String, TrayItem>| {
                let list: Vec<TrayItem> = order
                    .iter()
                    .filter_map(|(s, p)| map.get(&format!("{s}{p}")).cloned())
                    .collect();
                if let Ok(mut g) = worker_shared.items.lock() {
                    *g = list.clone();
                }
                let _ = worker_shared.tx.send(list);
            };

            loop {
                let cmd = match refresh_rx.recv_timeout(Duration::from_secs(4)) {
                    Ok(c) => c,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => TrayCmd::RefreshAll,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                };
                match cmd {
                    TrayCmd::Register { service, path } => {
                        let key = format!("{service}{path}");
                        if let Some(item) = fetch_item(&worker_conn, &service, &path) {
                            if !map.contains_key(&key) {
                                order.push((service, path));
                            }
                            map.insert(key, item);
                            publish(&order, &map);
                        }
                    }
                    TrayCmd::RefreshAll => {
                        if order.is_empty() {
                            continue;
                        }
                        let mut changed = false;
                        let mut alive = Vec::with_capacity(order.len());
                        for (s, p) in order.drain(..) {
                            let key = format!("{s}{p}");
                            match fetch_item(&worker_conn, &s, &p) {
                                Some(item) => {
                                    let prev = map.get(&key);
                                    let diff = prev.map(|o| {
                                        o.status != item.status
                                            || o.icon_name != item.icon_name
                                            || o.title != item.title
                                            || o.menu_path != item.menu_path
                                            || o.icon_pixmap.as_ref().map(|x| (x.0, x.1, x.2.len()))
                                                != item.icon_pixmap.as_ref().map(|x| (x.0, x.1, x.2.len()))
                                    }).unwrap_or(true);
                                    if diff {
                                        changed = true;
                                    }
                                    map.insert(key, item);
                                    alive.push((s, p));
                                }
                                None => {
                                    map.remove(&key);
                                    changed = true;
                                }
                            }
                        }
                        order = alive;
                        if changed {
                            publish(&order, &map);
                        }
                    }
                }
            }
        });

        Some(Tray { conn })
    }

    pub fn activate(&self, service: String, path: String, x: i32, y: i32) {
        let conn = self.conn.clone();
        let _ = std::thread::Builder::new().name("tray-activate".into()).spawn(move || {
            let _ = conn.call_method(Some(service.as_str()), path.as_str(), Some(SNI_IFACE), "Activate", &(x, y));
        });
    }

    pub fn context_menu(&self, service: String, path: String, x: i32, y: i32) {
        let conn = self.conn.clone();
        let _ = std::thread::Builder::new().name("tray-ctxmenu".into()).spawn(move || {
            let _ = conn.call_method(Some(service.as_str()), path.as_str(), Some(SNI_IFACE), "ContextMenu", &(x, y));
        });
    }
}

fn fetch_item(conn: &Connection, service: &str, path: &str) -> Option<TrayItem> {
    let reply = conn
        .call_method(
            Some(service),
            path,
            Some("org.freedesktop.DBus.Properties"),
            "GetAll",
            &(SNI_IFACE),
        )
        .ok()?;
    let props: HashMap<String, OwnedValue> = reply.body().deserialize().ok()?;

    let str_prop = |k: &str| -> Option<String> {
        props
            .get(k)
            .and_then(|v| <&str>::try_from(v).ok())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
    };
    let id = str_prop("Id").unwrap_or_else(|| service.to_string());
    let title = str_prop("Title").unwrap_or(id);
    let status = str_prop("Status").unwrap_or_else(|| "Active".into());
    let icon_name = str_prop("IconName");
    let item_is_menu = props
        .get("ItemIsMenu")
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(false);
    let menu_path = props.get("Menu").and_then(|v| {
        OwnedObjectPath::try_from(v.clone())
            .ok()
            .map(|p| p.as_str().to_string())
            .or_else(|| <&str>::try_from(v).ok().map(|s| s.to_string()))
    }).filter(|p| !p.is_empty() && p != "/");

    let icon_pixmap = props.get("IconPixmap").and_then(parse_icon_pixmap);

    Some(TrayItem {
        service: service.to_string(),
        path: path.to_string(),
        title,
        status,
        icon_name,
        icon_pixmap,
        menu_path,
        item_is_menu,
    })
}

/// Parse `a(iiay)` (`IconPixmap`) picking the icon closest to 32px and converting
/// big-endian ARGB32 to premultiplied RGBA8 for `tiny-skia`.
fn parse_icon_pixmap(val: &OwnedValue) -> Option<(u32, u32, Vec<u8>)> {
    let Value::Array(arr) = &**val else { return None };
    let mut best: Option<(u32, u32, Vec<u8>)> = None;
    let mut best_dist = i32::MAX;
    for elem in arr.iter() {
        let Value::Structure(st) = elem else { continue };
        let fields = st.fields();
        if fields.len() < 3 {
            continue;
        }
        let w = i32::try_from(&fields[0]).ok().filter(|&v| v > 0 && v <= 512)? as u32;
        let h = i32::try_from(&fields[1]).ok().filter(|&v| v > 0 && v <= 512)? as u32;
        let Value::Array(bytes_arr) = &fields[2] else { continue };
        let mut raw = Vec::with_capacity(bytes_arr.len());
        for b in bytes_arr.iter() {
            if let Ok(byte) = u8::try_from(b) {
                raw.push(byte);
            }
        }
        if raw.len() != (w * h * 4) as usize {
            continue;
        }
        let dist = (w as i32 - 32).abs();
        if dist < best_dist {
            // Convert ARGB32 (network byte order) -> premultiplied RGBA8
            let mut rgba = vec![0u8; raw.len()];
            for i in (0..raw.len()).step_by(4) {
                let a = raw[i] as u32;
                let r = (raw[i + 1] as u32 * a + 127) / 255;
                let g = (raw[i + 2] as u32 * a + 127) / 255;
                let b = (raw[i + 3] as u32 * a + 127) / 255;
                rgba[i] = r as u8;
                rgba[i + 1] = g as u8;
                rgba[i + 2] = b as u8;
                rgba[i + 3] = a as u8;
            }
            best_dist = dist;
            best = Some((w, h, rgba));
        }
    }
    best
}
