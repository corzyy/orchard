//! Shell glue for the volume/brightness HUD and notification banners.

use std::time::Instant;

use serde_json::json;
use smithay_client_toolkit::{
    compositor::Region,
    reexports::calloop::timer::{TimeoutAction, Timer},
    seat::pointer::PointerEventKind,
    shell::{
        wlr_layer::{Anchor, KeyboardInteractivity, Layer},
        WaylandSurface,
    },
};

use crate::{notify, osd, App, Surf, Which, BTN_LEFT};

impl App {
    /// Start a program in the session. Goes through the compositor so it gets the
    /// complete session environment (DISPLAY appears only once Xwayland is up,
    /// after the shell started) and is reaped when it exits.
    pub(crate) fn spawn(&mut self, cmd: &str) {
        if let Some(ipc) = self.ipc.as_mut() {
            ipc.request("spawn", json!({ "command": cmd }));
            return;
        }
        match std::process::Command::new("sh").arg("-c").arg(cmd).spawn() {
            Ok(mut child) => {
                let _ = std::thread::Builder::new().name("reaper".into()).spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(e) => tracing::warn!("spawn {cmd:?}: {e}"),
        }
    }

    // ---- HUD -----------------------------------------------------------------

    pub(crate) fn show_osd(&mut self, level: osd::Level) {
        if level.kind == osd::Kind::Volume {
            let vol = Some((level.value, level.muted));
            if self.menubar.volume != vol {
                self.menubar.volume = vol;
                self.redraw(Which::Bar);
            }
        }
        let until = Instant::now() + osd::SHOW_FOR;
        self.osd = Some(osd::Osd { level, until });
        match self.osd_surf.as_ref() {
            Some(s) if s.configured => self.draw(Which::Osd),
            Some(_) => {}
            None => {
                let surface = self.compositor.create_surface(&self.qh);
                let layer = self.layer_shell.create_layer_surface(
                    &self.qh,
                    surface,
                    Layer::Overlay,
                    Some("orchard-osd"),
                    self.output.as_ref(),
                );
                layer.set_size(osd::SIZE, osd::SIZE);
                layer.set_anchor(Anchor::BOTTOM);
                layer.set_margin(0, 0, osd::BOTTOM_MARGIN, 0);
                layer.set_exclusive_zone(-1);
                layer.set_keyboard_interactivity(KeyboardInteractivity::None);
                // clicks go through
                if let Ok(region) = Region::new(&self.compositor) {
                    layer.wl_surface().set_input_region(Some(region.wl_region()));
                }
                layer.commit();
                self.osd_surf = Some(Surf::new(layer));
                let sz = osd::SIZE as f32;
                self.set_backdrop("orchard-osd", Some((0.0, 0.0, sz, sz, 22.0)));
            }
        }
        let _ = self.loop_handle.insert_source(Timer::from_duration(osd::SHOW_FOR), |_, _, app: &mut App| {
            if app.osd.as_ref().map(|o| o.until <= Instant::now()).unwrap_or(false) {
                app.osd = None;
                app.osd_surf = None;
                app.set_backdrop("orchard-osd", None);
            }
            TimeoutAction::Drop
        });
    }

    // ---- notifications ---------------------------------------------------------

    pub(crate) fn on_notification(&mut self, msg: notify::Msg) {
        match msg {
            notify::Msg::Notify(n) => {
                tracing::info!(id = n.id, app = %n.app_name, "notification: {}", n.summary);
                let id = n.id;
                self.notes.add(*n);
                // with the Notification Center open, new ones go straight into its list
                if self.panel.as_ref().map(|p| p.kind == crate::panel::Kind::Notifications).unwrap_or(false) {
                    self.notes.banners.retain(|b| b.n.id != id);
                }
                self.schedule_expiry();
            }
            notify::Msg::Close(id) => {
                if self.notes.remove(id) {
                    if let Some(c) = &self.dbus {
                        notify::emit_closed(c, id, notify::Reason::Closed);
                    }
                }
            }
        }
        self.update_notes_surface();
        self.redraw(Which::Panel);
    }

    fn schedule_expiry(&mut self) {
        let Some(deadline) = self.notes.next_deadline() else { return };
        let wait = deadline.saturating_duration_since(Instant::now()) + std::time::Duration::from_millis(20);
        let _ = self.loop_handle.insert_source(Timer::from_duration(wait), |_, _, app: &mut App| {
            app.expire_notes();
            TimeoutAction::Drop
        });
    }

    fn expire_notes(&mut self) {
        let gone = self.notes.expire(Instant::now());
        if let Some(c) = &self.dbus {
            for id in &gone {
                notify::emit_closed(c, *id, notify::Reason::Expired);
            }
        }
        if !gone.is_empty() {
            self.update_notes_surface();
        }
        // a hovered banner outlives its deadline: look again shortly
        if self.notes.next_deadline().is_some() {
            let next = self.notes.next_deadline().unwrap();
            if next <= Instant::now() {
                let _ = self
                    .loop_handle
                    .insert_source(Timer::from_duration(std::time::Duration::from_millis(500)), |_, _, app: &mut App| {
                        app.expire_notes();
                        TimeoutAction::Drop
                    });
            } else {
                self.schedule_expiry();
            }
        }
    }

    /// Create, resize or destroy the banner surface to match the model.
    pub(crate) fn update_notes_surface(&mut self) {
        if self.notes.banners.is_empty() {
            if self.notes_surf.take().is_some() {
                self.set_backdrop("orchard-notifications", None);
            }
            self.notes.hover = None;
            return;
        }
        if self.output.is_none() {
            return;
        }
        let (w, h) = self.notes.size(&self.fonts);
        match self.notes_surf.as_mut() {
            Some(s) => {
                if s.width != w || s.height != h {
                    s.layer.set_size(w, h);
                    s.layer.commit();
                } else if s.configured {
                    self.redraw(Which::Notify);
                }
            }
            None => {
                let surface = self.compositor.create_surface(&self.qh);
                let layer = self.layer_shell.create_layer_surface(
                    &self.qh,
                    surface,
                    Layer::Overlay,
                    Some("orchard-notifications"),
                    self.output.as_ref(),
                );
                layer.set_size(w, h);
                layer.set_anchor(Anchor::TOP | Anchor::RIGHT);
                // below the menu bar (its exclusive zone already offsets us)
                layer.set_margin(0, 0, 0, 0);
                layer.set_keyboard_interactivity(KeyboardInteractivity::None);
                layer.commit();
                self.notes_surf = Some(Surf::new(layer));
            }
        }
        let qh = self.qh.clone();
        self.flush_frames(&qh);
    }

    /// Blur behind each banner, and accept input only over them.
    pub(crate) fn send_notes_backdrop(&mut self) {
        let rects = self.notes.rects(&self.fonts);
        if self.last_notes_backdrop.as_ref() == Some(&rects) {
            return;
        }
        self.last_notes_backdrop = Some(rects.clone());
        if let Some(ipc) = self.ipc.as_mut() {
            let v: Vec<serde_json::Value> = rects
                .iter()
                .map(|(x, y, w, h)| json!({"x": x, "y": y, "w": w, "h": h, "radius": 16.0}))
                .collect();
            ipc.request("set_backdrop", json!({"namespace": "orchard-notifications", "rects": v}));
        }
        if let Some(s) = self.notes_surf.as_ref() {
            if let Ok(region) = Region::new(&self.compositor) {
                for (x, y, w, h) in &rects {
                    // the close button pokes out of the top-left corner
                    region.add(*x as i32 - 8, *y as i32 - 8, *w as i32 + 8, *h as i32 + 8);
                }
                s.layer.wl_surface().set_input_region(Some(region.wl_region()));
            }
        }
    }

    pub(crate) fn notes_pointer(&mut self, kind: &PointerEventKind, x: f64, y: f64) {
        let (x, y) = (x as f32, y as f32);
        match kind {
            PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                let prev_hit = self.notes.hover.and_then(|(hx, hy)| self.notes.hit(hx, hy));
                let was_none = self.notes.hover.is_none();
                self.notes.hover = Some((x, y));
                let new_hit = self.notes.hit(x, y);
                if was_none || prev_hit != new_hit {
                    self.redraw(Which::Notify);
                }
            }
            PointerEventKind::Leave { .. } => {
                self.notes.hover = None;
                self.redraw(Which::Notify);
                // deadlines that passed while hovering apply now
                self.expire_notes();
            }
            PointerEventKind::Press { button: BTN_LEFT, .. } => {
                if let Some(hit) = self.notes.hit(x, y) {
                    self.click_notification(hit);
                }
            }
            _ => {}
        }
    }

    /// A running window's app_id that belongs to desktop entry `desktop`.
    pub(crate) fn app_id_for_desktop(&self, desktop: &str) -> Option<String> {
        let d = desktop.trim_end_matches(".desktop");
        let lower = d.to_ascii_lowercase();
        self.windows
            .iter()
            .map(|w| &w.app_id)
            .find(|a| {
                let a = a.to_ascii_lowercase();
                a == lower || lower.ends_with(&format!(".{a}")) || a.ends_with(&format!(".{lower}"))
            })
            .cloned()
            .or_else(|| Some(d.to_string()))
    }
}
