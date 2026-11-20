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
    panel::{self, ControlState, Kind, Panel, Submenu, Target},
    App, Surf, Which, BTN_LEFT,
};

impl App {
    /// Clock / Control Center / Volume / Network icon clicked.
    pub(crate) fn toggle_panel(&mut self, item: BarItem, kind: Kind, submenu: Option<Submenu>) {
        let same = self.panel.is_some() && self.menubar.open == Some(item);
        self.close_panel();
        if same {
            return;
        }
        if self.menu.is_some() {
            self.close_menu();
        }
        let width = self.bar.as_ref().map(|s| s.width).unwrap_or(0) as f32;
        let anchor = match item {
            BarItem::Volume => self
                .menubar
                .volume_range(&self.fonts, width)
                .unwrap_or_else(|| self.menubar.control_range(&self.fonts, width))
                .1
                + 8.0,
            BarItem::Network => self
                .menubar
                .network_range(&self.fonts, width)
                .unwrap_or_else(|| self.menubar.control_range(&self.fonts, width))
                .1
                + 8.0,
            _ => self.menubar.control_range(&self.fonts, width).1 + 8.0,
        };
        let mut p = Panel::new(kind, anchor);
        if kind == Kind::Control {
            p.control = self.control_cache.clone();
            let settings = orchard_config::Settings::load();
            p.night_shift = settings.night_shift.active_now();
            p.stage_manager = settings.stage_manager.enabled;
            p.game_mode_enabled = settings.game_mode.enabled;
            p.game_active = self.game.clone();
            if let Some(sub) = submenu {
                p.standalone = true;
                p.open_submenu(sub);
            }
            self.refresh_control(None, None);
        }
        self.panel = Some(p);
        self.menubar.open = Some(item);
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
        if matches!(
            self.menubar.open,
            Some(BarItem::Clock | BarItem::Control | BarItem::Volume | BarItem::Network)
        ) {
            self.menubar.open = None;
            self.redraw(Which::Bar);
        }
    }

    /// Query (and optionally change) Wi-Fi, Bluetooth, volume … on a helper thread.
    pub(crate) fn refresh_control(&mut self, target: Option<Target>, value: Option<f32>) {
        // Fast path for volume/brightness sliders & mute: apply without spawning the
        // full 15-subprocess `query_control()` on every drag tick.
        if let Some(ref t) = target {
            if matches!(
                t,
                Target::Volume(..) | Target::Brightness(..) | Target::VolumeMute | Target::AppVolume(..) | Target::AppMute(_)
            ) {
                let t = t.clone();
                if let Some(p) = self.panel.as_ref() {
                    self.control_cache.volume = p.control.volume;
                    self.control_cache.displays = p.control.displays.clone();
                    self.control_cache.streams = p.control.streams.clone();
                }
                let displays = self.control_cache.displays.clone();
                let streams = self.control_cache.streams.clone();
                let _ = std::thread::Builder::new().name("control-slider".into()).spawn(move || {
                    panel::run_control_slider(&t, value, &displays, &streams);
                });
                return;
            }
        }
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
        if st.volume.is_some() && self.menubar.volume != st.volume {
            self.menubar.volume = st.volume;
            self.redraw(Which::Bar);
        }
        if let Some(p) = self.panel.as_mut() {
            // keep the slider where the user holds it
            let volume = p.control.volume;
            let displays = p.control.displays.clone();
            let streams = p.control.streams.clone();
            p.control = st;
            p.busy_item = None;
            match p.dragging {
                Some(Target::Volume(..)) => p.control.volume = volume,
                Some(Target::AppVolume(idx, ..)) => {
                    if let (Some(old), Some(cur)) = (streams.get(idx), p.control.streams.get_mut(idx)) {
                        if old.name == cur.name {
                            cur.volume = old.volume;
                            cur.muted = old.muted;
                        }
                    }
                }
                Some(Target::Brightness(idx, ..)) => {
                    if let (Some(old), Some(cur)) = (displays.get(idx), p.control.displays.get_mut(idx)) {
                        cur.value = old.value;
                    }
                }
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

    /// Control Center → Night Shift: the compositor fades the tint in/out.
    pub(crate) fn toggle_night_shift(&mut self) {
        let mut s = orchard_config::Settings::load();
        let on = !s.night_shift.active_now();
        s.night_shift.set_manual_now(on);
        if let Err(e) = s.save() {
            tracing::warn!("saving night shift: {e}");
            return;
        }
        if let Some(p) = self.panel.as_mut() {
            p.night_shift = on;
        }
        if let Some(ipc) = self.ipc.as_mut() {
            ipc.request("reload_settings", serde_json::json!({}));
        }
        self.redraw(Which::Panel);
    }

    /// Control Center → Stage Manager: toggle macOS-style Stage Manager on/off.
    pub(crate) fn toggle_stage_manager(&mut self) {
        let mut s = orchard_config::Settings::load();
        let on = !s.stage_manager.enabled;
        s.stage_manager.enabled = on;
        let tiling_changed = on && s.windows.tiling_mode == orchard_config::TilingMode::Tiling;
        if tiling_changed {
            s.windows.tiling_mode = orchard_config::TilingMode::Floating;
            self.menubar.tiling_mode = orchard_config::TilingMode::Floating;
        }
        if let Err(e) = s.save() {
            tracing::warn!("saving stage manager: {e}");
            return;
        }
        if let Some(p) = self.panel.as_mut() {
            p.stage_manager = on;
        }
        if let Some(ipc) = self.ipc.as_mut() {
            ipc.request("reload_settings", serde_json::json!({}));
        }
        if tiling_changed {
            self.redraw(Which::Bar);
        }
        self.redraw(Which::Panel);
    }

    /// Control Center → Game Mode: switch the automatic Game Mode on or off.
    pub(crate) fn toggle_game_mode(&mut self) {
        let mut s = orchard_config::Settings::load();
        s.game_mode.enabled = !s.game_mode.enabled;
        if let Err(e) = s.save() {
            tracing::warn!("saving game mode: {e}");
            return;
        }
        if let Some(p) = self.panel.as_mut() {
            p.game_mode_enabled = s.game_mode.enabled;
        }
        if let Some(ipc) = self.ipc.as_mut() {
            ipc.request("reload_settings", serde_json::json!({}));
        }
        self.redraw(Which::Panel);
    }

    /// The compositor reports that a game went full screen, or left it.
    pub(crate) fn on_game_mode(&mut self, active: bool, app_id: Option<String>, silence: bool) {
        let name = app_id.map(|id| self.apps.for_app_id(&id).map(|e| e.name.clone()).unwrap_or(id));
        self.game = if active { Some(name.unwrap_or_default()) } else { None };
        self.notes.game_quiet = active && silence;
        if self.notes.game_quiet {
            // banners already up slide away into the Notification Center
            self.notes.banners.retain(|b| b.n.critical);
            self.update_notes_surface();
        }
        self.menubar.game = active;
        if let Some(p) = self.panel.as_mut() {
            p.game_active = self.game.clone();
        }
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
                Target::Volume(..) => {
                    p.control.volume = Some((v, false));
                    if self.menubar.volume != Some((v, false)) {
                        self.menubar.volume = Some((v, false));
                        self.redraw(Which::Bar);
                    }
                }
                Target::Brightness(idx, ..) => {
                    if let Some(d) = p.control.displays.get_mut(*idx) {
                        d.value = v;
                    }
                }
                Target::AppVolume(idx, ..) => {
                    if let Some(a) = p.control.streams.get_mut(*idx) {
                        a.volume = v;
                        a.muted = false;
                    }
                }
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
                let (dragging, prev_hit) = match self.panel.as_ref() {
                    Some(p) => (
                        p.dragging.clone(),
                        p.hover.and_then(|(hx, hy)| p.hit(hx, hy)),
                    ),
                    None => (None, None),
                };
                if let Some(p) = self.panel.as_mut() {
                    p.hover = Some((x, y));
                }
                if let Some(t) = dragging {
                    self.set_slider(&t, x);
                } else {
                    let new_hit = self.panel.as_ref().and_then(|p| p.hit(x, y));
                    if new_hit != prev_hit {
                        self.redraw(Which::Panel);
                    }
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
                    Target::NightShift => self.toggle_night_shift(),
                    Target::StageManager => self.toggle_stage_manager(),
                    Target::GameMode => self.toggle_game_mode(),
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
                    Target::OpenSubmenu(sub) => {
                        if let Some(p) = self.panel.as_mut() {
                            p.open_submenu(sub);
                        }
                        self.redraw(Which::Panel);
                        self.refresh_control(None, None);
                    }
                    Target::SubmenuBack => {
                        if let Some(p) = self.panel.as_mut() {
                            p.close_submenu();
                        }
                        self.redraw(Which::Panel);
                    }
                    Target::WifiToggleOthers => {
                        if let Some(p) = self.panel.as_mut() {
                            p.wifi_others_open = !p.wifi_others_open;
                        }
                        self.redraw(Which::Panel);
                    }
                    Target::OpenSettings(page) => {
                        let _ = std::process::Command::new("orchard-settings").arg(page).spawn();
                        self.close_panel();
                    }
                    Target::WifiConnect { ssid, known, secure } => {
                        if !known && secure {
                            // Secured unknown network needs a password prompt -> open Wi-Fi Settings
                            let _ = std::process::Command::new("orchard-settings").arg("wifi").spawn();
                            self.close_panel();
                        } else {
                            if let Some(p) = self.panel.as_mut() {
                                p.busy_item = Some(ssid.clone());
                            }
                            self.redraw(Which::Panel);
                            self.refresh_control(Some(Target::WifiConnect { ssid, known, secure }), None);
                        }
                    }
                    Target::BtToggleDevice { mac, connected } => {
                        if let Some(p) = self.panel.as_mut() {
                            p.busy_item = Some(mac.clone());
                        }
                        self.redraw(Which::Panel);
                        self.refresh_control(Some(Target::BtToggleDevice { mac, connected }), None);
                    }
                    Target::AudioSelectSink(name) => {
                        if let Some(p) = self.panel.as_mut() {
                            for s in &mut p.control.audio_sinks {
                                s.active = s.name == name;
                            }
                        }
                        self.redraw(Which::Panel);
                        self.refresh_control(Some(Target::AudioSelectSink(name)), None);
                    }
                    Target::VolumeMute => {
                        if let Some(p) = self.panel.as_mut() {
                            if let Some((vol, m)) = p.control.volume.as_mut() {
                                *m = !*m;
                                self.menubar.volume = Some((*vol, *m));
                                self.redraw(Which::Bar);
                            }
                        }
                        self.redraw(Which::Panel);
                        self.refresh_control(Some(Target::VolumeMute), None);
                    }
                    Target::AppMute(idx) => {
                        if let Some(a) = self.panel.as_mut().and_then(|p| p.control.streams.get_mut(idx)) {
                            a.muted = !a.muted;
                        }
                        self.redraw(Which::Panel);
                        self.refresh_control(Some(Target::AppMute(idx)), None);
                    }
                    Target::Volume(..) | Target::Brightness(..) | Target::AppVolume(..) => {
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
                                Target::WifiToggle => {
                                    if let Some(w) = p.control.wifi.as_mut() {
                                        w.0 = !w.0;
                                    }
                                }
                                Target::BluetoothToggle => p.control.bluetooth = p.control.bluetooth.map(|b| !b),
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
