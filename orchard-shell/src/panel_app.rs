//! Shell glue for the Notification Center / Control Center panel.

use std::time::Instant;

use serde_json::json;
use smithay_client_toolkit::{
    seat::pointer::PointerEventKind,
    shell::{
        wlr_layer::{Anchor, KeyboardInteractivity, Layer},
        WaylandSurface,
    },
};

use crate::{
    menubar::BarItem,
    notify::{self, Hit},
    panel::{self, ControlState, Kind, Panel, Target},
    App, Surf, Which, BTN_LEFT,
};

impl App {
    /// Clock / Control Center icon clicked.
    pub(crate) fn toggle_panel(&mut self, kind: Kind) {
        let same = self.panel.as_ref().map(|p| p.kind == kind).unwrap_or(false);
        self.close_panel();
        if same {
            return;
        }
        if self.menu.is_some() {
            self.close_menu();
        }
        let width = self.bar.as_ref().map(|s| s.width).unwrap_or(0) as f32;
        let anchor = self.menubar.control_range(&self.fonts, width).1 + 8.0;
        let mut p = Panel::new(kind, anchor);
        if kind == Kind::Control {
            p.control = self.control_cache.clone();
            self.refresh_control(None, None);
        }
        self.panel = Some(p);
        self.menubar.open = Some(match kind {
            Kind::Notifications => BarItem::Clock,
            Kind::Control => BarItem::Control,
        });
        self.redraw(Which::Bar);

        let surface = self.compositor.create_surface(&self.qh);
        let layer = self.layer_shell.create_layer_surface(
            &self.qh,
            surface,
            Layer::Overlay,
            Some("orchard-panel"),
            self.output.as_ref(),
        );
        // everything below the menu bar: clicks outside the panel close it
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
        layer.commit();
        self.panel_surf = Some(Surf::new(layer));
    }

    pub(crate) fn close_panel(&mut self) {
        if self.panel.take().is_none() && self.panel_surf.is_none() {
            return;
        }
        self.panel_surf = None;
        self.set_backdrop("orchard-panel", None);
        if matches!(self.menubar.open, Some(BarItem::Clock | BarItem::Control)) {
            self.menubar.open = None;
            self.redraw(Which::Bar);
        }
    }

    /// Query (and optionally change) Wi-Fi, Bluetooth, volume … on a helper thread.
    pub(crate) fn refresh_control(&mut self, target: Option<Target>, value: Option<f32>) {
        let tx = self.control_tx.clone();
        let current = self.control_cache.clone();
        let _ = std::thread::Builder::new().name("control-center".into()).spawn(move || {
            let st = match target {
                Some(t) => panel::run_control(&t, value, &current),
                None => panel::query_control(),
            };
            let _ = tx.send(st);
        });
    }

    pub(crate) fn on_control_state(&mut self, st: ControlState) {
        tracing::debug!(?st, "control center state");
        self.control_cache = st.clone();
        if let Some(p) = self.panel.as_mut() {
            // keep the slider where the user holds it
            let volume = p.control.volume;
            let brightness = p.control.brightness;
            p.control = st;
            match p.dragging {
                Some(Target::Volume(..)) => p.control.volume = volume,
                Some(Target::Brightness(..)) => p.control.brightness = brightness,
                _ => {}
            }
            self.redraw(Which::Panel);
        }
    }

    pub(crate) fn toggle_dnd(&mut self) {
        self.notes.do_not_disturb = !self.notes.do_not_disturb;
        save_dnd(self.notes.do_not_disturb);
        self.menubar.dnd = self.notes.do_not_disturb;
        self.redraw(Which::Bar);
        self.redraw(Which::Panel);
    }

    pub(crate) fn send_panel_backdrop(&mut self) {
        let Some(p) = self.panel.as_ref() else { return };
        let (x, y, w, h) = p.rect();
        self.set_backdrop("orchard-panel", Some((x, y, w, h, 18.0)));
    }

    fn set_slider(&mut self, target: &Target, x: f32) {
        let Some(v) = Panel::slider_value(target, x) else { return };
        if let Some(p) = self.panel.as_mut() {
            match target {
                Target::Volume(..) => p.control.volume = Some((v, false)),
                Target::Brightness(..) => p.control.brightness = Some(v),
                _ => {}
            }
        }
        self.redraw(Which::Panel);
        // at most ~25 changes per second while dragging
        let now = Instant::now();
        if now.duration_since(self.last_slider_cmd).as_millis() >= 40 {
            self.last_slider_cmd = now;
            self.refresh_control(Some(target.clone()), Some(v));
        }
    }

    pub(crate) fn panel_pointer(&mut self, kind: &PointerEventKind, x: f64, y: f64) {
        let (x, y) = (x as f32, y as f32);
        match kind {
            PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                let dragging = self.panel.as_ref().and_then(|p| p.dragging.clone());
                if let Some(p) = self.panel.as_mut() {
                    p.hover = Some((x, y));
                }
                if let Some(t) = dragging {
                    self.set_slider(&t, x);
                } else if self.panel.as_ref().map(|p| p.kind == Kind::Notifications).unwrap_or(false) {
                    self.redraw(Which::Panel);
                }
            }
            PointerEventKind::Leave { .. } => {
                if let Some(p) = self.panel.as_mut() {
                    p.hover = None;
                }
                self.redraw(Which::Panel);
            }
            PointerEventKind::Axis { vertical, .. } => {
                let dy = if vertical.discrete != 0 { vertical.discrete as f32 * 40.0 } else { vertical.absolute as f32 };
                if let Some(p) = self.panel.as_mut() {
                    p.scroll_by(dy);
                }
                self.redraw(Which::Panel);
            }
            PointerEventKind::Release { button: BTN_LEFT, .. } => {
                let dragging = self.panel.as_mut().and_then(|p| p.dragging.take());
                if let Some(t) = dragging {
                    // final value, whatever the throttle skipped
                    let v = Panel::slider_value(&t, x);
                    self.refresh_control(Some(t), v);
                }
            }
            PointerEventKind::Press { button: BTN_LEFT, .. } => {
                let Some(p) = self.panel.as_ref() else { return };
                let Some(target) = p.hit(x, y) else {
                    // outside the panel
                    self.close_panel();
                    return;
                };
                match target {
                    Target::Panel => {}
                    Target::Dnd => self.toggle_dnd(),
                    Target::ClearAll => {
                        let ids = self.notes.clear_history();
                        if let Some(c) = &self.dbus {
                            for id in ids {
                                notify::emit_closed(c, id, notify::Reason::Dismissed);
                            }
                        }
                        self.redraw(Which::Panel);
                    }
                    Target::Card(hit) => self.click_notification(hit),
                    Target::Volume(..) | Target::Brightness(..) => {
                        if let Some(p) = self.panel.as_mut() {
                            p.dragging = Some(target.clone());
                        }
                        self.last_slider_cmd = Instant::now() - std::time::Duration::from_secs(1);
                        self.set_slider(&target, x);
                    }
                    other => {
                        // optimistic toggle, then the real state from the system
                        if let Some(p) = self.panel.as_mut() {
                            match other {
                                Target::Wifi => {
                                    if let Some(w) = p.control.wifi.as_mut() {
                                        w.0 = !w.0;
                                    }
                                }
                                Target::Bluetooth => p.control.bluetooth = p.control.bluetooth.map(|b| !b),
                                Target::MediaPlay => {
                                    if let Some(m) = p.control.media.as_mut() {
                                        m.playing = !m.playing;
                                    }
                                }
                                _ => {}
                            }
                        }
                        self.redraw(Which::Panel);
                        self.refresh_control(Some(other), None);
                    }
                }
            }
            _ => {}
        }
    }

    /// A click on a notification (banner or Notification Center card).
    pub(crate) fn click_notification(&mut self, hit: Hit) {
        let (id, action) = match hit {
            Hit::Close(id) => (id, None),
            Hit::Action(id, key) => (id, Some(key)),
            Hit::Body(id) => (id, Some("default".to_string())),
        };
        let n = self
            .notes
            .banners
            .iter()
            .map(|b| &b.n)
            .chain(self.notes.history.iter().map(|p| &p.n))
            .find(|n| n.id == id)
            .cloned();
        let has_default = n.as_ref().map(|n| n.actions.iter().any(|(k, _)| k == "default")).unwrap_or(false);
        let desktop = n.as_ref().and_then(|n| n.desktop_entry.clone());
        let resident = n.as_ref().map(|n| n.resident).unwrap_or(false);
        if let (Some(key), Some(c)) = (action.as_deref(), &self.dbus) {
            if key != "default" || has_default {
                notify::emit_action(c, id, key);
            }
        }
        // clicking the notification brings the app forward, like macOS
        if action.as_deref() == Some("default") {
            if let Some(app_id) = desktop.and_then(|d| self.app_id_for_desktop(&d)) {
                if let Some(ipc) = self.ipc.as_mut() {
                    ipc.request("activate_app", json!({ "app_id": app_id }));
                }
            }
            self.close_panel();
        }
        if !(resident && action.as_deref().is_some_and(|a| a != "default")) {
            self.notes.remove(id);
            if let Some(c) = &self.dbus {
                notify::emit_closed(c, id, notify::Reason::Dismissed);
            }
        }
        self.update_notes_surface();
        self.redraw(Which::Panel);
    }
}

fn dnd_file() -> Option<std::path::PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/state")))?;
    Some(state.join("orchard/do-not-disturb"))
}

/// Do Not Disturb survives a shell restart / new login (a file exists while it is on).
pub(crate) fn load_dnd() -> bool {
    dnd_file().map(|p| p.exists()).unwrap_or(false)
}

fn save_dnd(on: bool) {
    let Some(p) = dnd_file() else { return };
    if on {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(&p, "");
    } else {
        let _ = std::fs::remove_file(&p);
    }
}
