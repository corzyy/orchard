//! Spotlight inside the shell: its layer surface, animations, keyboard and
//! pointer input, opening results, and the background work (file index,
//! previews, clipboard).

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use serde_json::json;
use smithay_client_toolkit::{
    compositor::Region,
    reexports::calloop::{
        channel::Sender,
        timer::{TimeoutAction, Timer},
    },
    seat::{keyboard::KeyEvent, pointer::PointerEventKind},
    shell::{
        wlr_layer::{Anchor, KeyboardInteractivity, Layer},
        WaylandSurface,
    },
};
use tiny_skia::Pixmap;

use super::{files, Act, Launcher, Outcome, Preview, Usage, BODY_H, FIELD_H, RADIUS, WIDTH};
use crate::{apps::ExtraIcons, menu::Action, menubar, App, Surf, Which, BTN_LEFT};

/// Rebuild the file index when Spotlight opens and it is older than this.
const REINDEX_AFTER: Duration = Duration::from_secs(10 * 60);
/// Reopening within this time shows the previous search again (selected).
const KEEP_QUERY: Duration = Duration::from_secs(10 * 60);

/// Results of background work, delivered on the shell's event loop.
pub enum Msg {
    Index(Arc<files::FileIndex>, Option<(Option<String>, ExtraIcons)>),
    Preview(String, files::PreviewData),
    Paste(String),
}

/// Spotlight state that lives as long as the shell.
pub struct Spot {
    pub index: Option<Arc<files::FileIndex>>,
    indexing: bool,
    pub usage: Usage,
    last_query: String,
    last_close: Option<Instant>,
    gen: u64,
    tx: Sender<Msg>,
    /// last backdrop sent (x, y, w, h, radius, alpha)
    backdrop: Option<[f32; 6]>,
}

impl Spot {
    pub fn new(tx: Sender<Msg>) -> Spot {
        Spot {
            index: None,
            indexing: false,
            usage: Usage::load(),
            last_query: String::new(),
            last_close: None,
            gen: 0,
            tx,
            backdrop: None,
        }
    }
}

impl App {
    /// Build the file index (and the extra icon index) on a helper thread.
    pub(crate) fn spot_index(&mut self, delay: Duration) {
        if self.spot.indexing {
            return;
        }
        self.spot.indexing = true;
        let tx = self.spot.tx.clone();
        let theme = self.apps.icon_theme_name();
        let want_icons = !self.apps.has_extra_icons();
        let _ = std::thread::Builder::new().name("spotlight-index".into()).spawn(move || {
            std::thread::sleep(delay);
            let icons = want_icons.then(|| (theme.clone(), ExtraIcons::build(theme.as_deref())));
            let index = Arc::new(files::FileIndex::build());
            let _ = tx.send(Msg::Index(index, icons));
        });
    }

    pub(crate) fn on_spot_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Index(ix, icons) => {
                self.spot.indexing = false;
                if let Some((theme, icons)) = icons {
                    self.apps.set_extra_icons(theme, icons);
                }
                // Drop old index references before installing the new one
                super::FILE_ICON_DB.with(|db| *db.borrow_mut() = None);
                self.spot.index = None;
                super::FILE_ICON_DB.with(|db| *db.borrow_mut() = Some(ix.clone()));
                self.spot.index = Some(ix);
                // show file results that were missing while the index was built
                let refresh = self.launcher.as_ref().is_some_and(|l| !l.closing && !l.query.is_empty() && l.selected == 0);
                if refresh {
                    self.launcher_changed();
                }
            }
            Msg::Preview(path, data) => {
                let Some(l) = self.launcher.as_mut() else { return };
                let p = match data {
                    files::PreviewData::Image { w, h, rgba } => tiny_skia::IntSize::from_wh(w, h)
                        .and_then(|sz| Pixmap::from_vec(rgba, sz))
                        .map(|p| Preview::Image(std::rc::Rc::new(p)))
                        .unwrap_or(Preview::Nothing),
                    files::PreviewData::Text(t) => Preview::Text(t),
                    files::PreviewData::Folder { items } => Preview::Folder(items),
                    files::PreviewData::Nothing => Preview::Nothing,
                };
                l.previews.insert(path, p);
                self.redraw(Which::Launcher);
            }
            Msg::Paste(text) => {
                let Some(l) = self.launcher.as_mut() else { return };
                let line = text.lines().next().unwrap_or("").trim().to_string();
                if !line.is_empty() {
                    l.insert(&line);
                    self.launcher_changed();
                }
            }
        }
    }

    /// Top-left of the panel on the launcher surface (logical px).
    pub(crate) fn launcher_origin(&self) -> (f32, f32) {
        let (w, h) = self
            .launcher_surf
            .as_ref()
            .filter(|s| s.configured)
            .map(|s| (s.width as f32, s.height as f32))
            .unwrap_or((self.output_size.0 as f32, self.output_size.1 as f32));
        let full = FIELD_H + BODY_H;
        let y = (h * 0.2).max(menubar::HEIGHT + 24.0).min((h - full - 40.0).max(menubar::HEIGHT + 8.0));
        (((w - WIDTH) / 2.0).round(), y.round())
    }

    pub(crate) fn toggle_launcher(&mut self) {
        if let Some(l) = self.launcher.as_mut() {
            if l.closing {
                // ⌘Space again while it fades out: bring it back
                l.closing = false;
                l.last_step_reset();
                if let Some(s) = self.launcher_surf.as_ref() {
                    s.layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
                    s.layer.wl_surface().set_input_region(None);
                    s.layer.commit();
                }
                self.refresh_apps();
                self.redraw(Which::Launcher);
                return;
            }
            self.close_launcher();
            return;
        }
        if self.launchpad.is_some() {
            self.close_launchpad();
        }
        self.close_menu();
        self.refresh_apps();
        let keep = self.spot.last_close.map(|t| t.elapsed() < KEEP_QUERY).unwrap_or(false);
        let last = if keep { self.spot.last_query.clone() } else { String::new() };
        self.spot.gen += 1;
        let gen = self.spot.gen;
        self.launcher = Some(Launcher::new(&last, gen, self.german));
        self.spot.backdrop = None;
        if !last.is_empty() {
            self.launcher_search();
        }
        let stale = self.spot.index.as_ref().map(|ix| ix.built.elapsed() > REINDEX_AFTER).unwrap_or(true);
        if stale {
            self.spot_index(Duration::ZERO);
        }

        let surface = self.compositor.create_surface(&self.qh);
        let layer = self.layer_shell.create_layer_surface(&self.qh, surface, Layer::Overlay, Some("orchard-launcher"), self.output.as_ref());
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        layer.commit();
        self.launcher_surf = Some(Surf::new(layer));

        // blinking caret
        let _ = self.loop_handle.insert_source(Timer::from_duration(Duration::from_millis(530)), move |_, _, app: &mut App| {
            match app.launcher.as_mut() {
                Some(l) if l.gen == gen && !l.closing => {
                    l.caret_on = !l.caret_on;
                    app.redraw(Which::Launcher);
                    TimeoutAction::ToDuration(Duration::from_millis(530))
                }
                _ => TimeoutAction::Drop,
            }
        });
    }

    /// Fade out; the surface goes away when the animation ends. Keyboard and
    /// pointer are released right away.
    pub(crate) fn close_launcher(&mut self) {
        let Some(l) = self.launcher.as_mut() else { return };
        if l.closing {
            return;
        }
        l.closing = true;
        l.last_step_reset();
        self.spot.last_query = l.query.clone();
        self.spot.last_close = Some(Instant::now());
        if let Some(s) = self.launcher_surf.as_ref() {
            s.layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            if let Ok(r) = Region::new(&self.compositor) {
                s.layer.wl_surface().set_input_region(Some(r.wl_region()));
            }
            s.layer.commit();
        }
        if self.launcher_surf.as_ref().map(|s| s.configured).unwrap_or(false) {
            self.redraw(Which::Launcher);
        } else {
            self.close_launcher_now();
        }
    }

    /// Remove Spotlight without animation.
    pub(crate) fn close_launcher_now(&mut self) {
        if let Some(l) = self.launcher.take() {
            if !l.closing {
                self.spot.last_query = l.query.clone();
                self.spot.last_close = Some(Instant::now());
            }
        }
        self.launcher_surf = None;
        self.spot.backdrop = None;
        self.set_backdrop("orchard-launcher", None);
    }

    fn launcher_search(&mut self) {
        if let Some(l) = self.launcher.as_mut() {
            l.update(&self.apps, self.spot.index.as_ref(), &self.spot.usage);
        }
    }

    /// The query changed: search again and repaint.
    pub(crate) fn launcher_changed(&mut self) {
        self.launcher_search();
        self.redraw(Which::Launcher);
    }

    /// Paint Spotlight onto its surface. `None` when it finished closing.
    pub(crate) fn paint_launcher(&mut self, pm: &mut Pixmap, scale: f32) -> Option<bool> {
        let animating = self.launcher.as_mut()?.step();
        if self.launcher.as_ref().is_some_and(|l| l.is_closed()) {
            self.close_launcher_now();
            return None;
        }
        self.spot_request_preview(scale);
        let (x, y) = self.launcher_origin();
        let l = self.launcher.as_ref()?;
        l.paint(pm, x, y, scale, &self.apps, &self.fonts, &self.theme, self.spot.index.as_deref(), &self.spot.usage);

        // frosted glass behind it, following the fade and zoom
        let (alpha, k) = l.anim();
        let h = l.shown_height();
        let cx = x + WIDTH / 2.0;
        let cy = y + FIELD_H / 2.0;
        let rect = [cx + (x - cx) * k, cy + (y - cy) * k, WIDTH * k, h * k, RADIUS * k, alpha];
        let changed = self.spot.backdrop.map(|b| b.iter().zip(rect.iter()).any(|(a, b)| (a - b).abs() > 0.01)).unwrap_or(true);
        if changed {
            self.spot.backdrop = Some(rect);
            if let Some(ipc) = self.ipc.as_mut() {
                ipc.request(
                    "set_backdrop",
                    json!({"namespace": "orchard-launcher", "rects": [{"x": rect[0], "y": rect[1], "w": rect[2], "h": rect[3], "radius": rect[4], "alpha": rect[5]}]}),
                );
            }
        }
        Some(animating)
    }

    /// Load the selected file's preview on a helper thread.
    fn spot_request_preview(&mut self, scale: f32) {
        let Some(l) = self.launcher.as_mut() else { return };
        let Some((path, is_dir, class)) = l.wanted_preview() else { return };
        l.previews.insert(path.clone(), Preview::Loading);
        let tx = self.spot.tx.clone();
        let max = (440.0 * scale) as u32;
        let _ = std::thread::Builder::new().name("spotlight-preview".into()).spawn(move || {
            let data = files::load_preview(&path, is_dir, class, max);
            let _ = tx.send(Msg::Preview(path, data));
        });
    }

    // ---- input ------------------------------------------------------------------

    pub(crate) fn launcher_active(&self) -> bool {
        self.launcher.as_ref().is_some_and(|l| !l.closing)
    }

    pub(crate) fn launcher_key(&mut self, event: &KeyEvent) {
        let mods = self.mods;
        let Some(l) = self.launcher.as_mut() else { return };
        let outcome = l.key(event.keysym, event.utf8.as_deref(), &mods);
        match outcome {
            Outcome::Nothing => {}
            Outcome::Redraw => self.redraw(Which::Launcher),
            Outcome::Changed => self.launcher_changed(),
            Outcome::Close => self.close_launcher(),
            Outcome::Open { reveal } => self.launcher_open(reveal),
            Outcome::Copy => {
                if let Some(t) = self.launcher.as_ref().and_then(|l| l.copy_text(&self.apps)) {
                    self.run_spot_act(Act::Copy(t));
                }
            }
            Outcome::Paste => {
                let tx = self.spot.tx.clone();
                let _ = std::thread::Builder::new().name("spotlight-paste".into()).spawn(move || {
                    let out = std::process::Command::new("wl-paste").args(["--no-newline", "--type", "text"]).output();
                    if let Ok(o) = out {
                        if o.status.success() {
                            let _ = tx.send(Msg::Paste(String::from_utf8_lossy(&o.stdout).into_owned()));
                        }
                    }
                });
            }
            Outcome::WebSearch => {
                let q = self.launcher.as_ref().map(|l| l.query.clone()).unwrap_or_default();
                if !q.trim().is_empty() {
                    self.spot.usage.record(&q, "web");
                    self.close_launcher();
                    self.run_spot_act(super::web_action(&q));
                }
            }
        }
    }

    /// Open the selected result (`reveal`: show a file in its folder instead).
    fn launcher_open(&mut self, reveal: bool) {
        let Some(l) = self.launcher.as_ref() else { return };
        let Some((act, key)) = l.action(&self.apps, reveal) else { return };
        let query = l.query.clone();
        self.spot.usage.record(&query, &key);
        self.close_launcher();
        self.run_spot_act(act);
    }

    fn run_spot_act(&mut self, act: Act) {
        match act {
            Act::App(i) => {
                let Some(entry) = self.apps.entries.get(i).cloned() else { return };
                // a running app comes forward instead of starting again
                let running = self
                    .windows
                    .iter()
                    .find(|w| self.apps.for_app_id(&w.app_id).map(|e| e.id == entry.id).unwrap_or(false))
                    .map(|w| w.app_id.clone());
                if let Some(app_id) = running {
                    self.perform(Action::ActivateApp(app_id));
                } else if let Some(cmd) = entry.command() {
                    for it in self.dock.items.iter_mut() {
                        if matches!(&it.kind, crate::dock::ItemKind::App { desktop_id: Some(d), .. } if *d == entry.id) {
                            it.bounce_start = Some(Instant::now());
                        }
                    }
                    self.redraw(Which::Dock);
                    self.perform(Action::Launch(cmd));
                }
            }
            Act::Spawn(cmd) => match cmd.as_str() {
                "systemctl poweroff" => self.open_dialog(crate::dialog::DialogKind::ShutDown),
                "systemctl reboot" => self.open_dialog(crate::dialog::DialogKind::Restart),
                "systemctl suspend" => self.open_dialog(crate::dialog::DialogKind::Sleep),
                _ => self.spawn(&cmd),
            },
            Act::Wm(name) => {
                // after Spotlight has faded out (a screenshot must not show it)
                let _ = self.loop_handle.insert_source(Timer::from_duration(Duration::from_millis(180)), move |_, _, app: &mut App| {
                    app.perform(Action::Wm(name));
                    TimeoutAction::Drop
                });
            }
            Act::LogOut => self.open_dialog(crate::dialog::DialogKind::LogOut),
            Act::Copy(text) => self.spawn(&format!("wl-copy -- {}", super::sh_quote(&text))),
            Act::Reveal(path) => {
                let _ = std::thread::Builder::new().name("spotlight-reveal".into()).spawn(move || reveal_in_folder(&path));
            }
        }
    }

    pub(crate) fn launcher_pointer(&mut self, kind: &PointerEventKind, x: f64, y: f64) {
        if !self.launcher_active() {
            return;
        }
        let (ox, oy) = self.launcher_origin();
        let (lx, ly) = (x as f32 - ox, y as f32 - oy);
        let Some(l) = self.launcher.as_mut() else { return };
        let inside = lx >= 0.0 && lx <= WIDTH && ly >= 0.0 && ly <= l.shown_height();
        match kind {
            PointerEventKind::Motion { .. } | PointerEventKind::Enter { .. } => {
                let h = if inside { l.item_at(lx, ly) } else { None };
                if l.set_hover(h) {
                    self.redraw(Which::Launcher);
                }
            }
            PointerEventKind::Leave { .. } => {
                if l.set_hover(None) {
                    self.redraw(Which::Launcher);
                }
            }
            PointerEventKind::Press { button, .. } => {
                if !inside {
                    self.close_launcher();
                    return;
                }
                if *button != BTN_LEFT {
                    return;
                }
                if ly < FIELD_H {
                    l.click_field();
                    self.redraw(Which::Launcher);
                } else if let Some(i) = l.item_at(lx, ly) {
                    let double = l.click(i);
                    self.redraw(Which::Launcher);
                    if double {
                        self.launcher_open(false);
                    }
                }
            }
            PointerEventKind::Axis { vertical, .. } => {
                if inside && ly > FIELD_H {
                    let dy = if vertical.absolute != 0.0 { vertical.absolute as f32 } else { vertical.discrete as f32 * 28.0 };
                    l.scroll_by(dy);
                    self.redraw(Which::Launcher);
                }
            }
            _ => {}
        }
    }
}

impl Launcher {
    /// Restart animation timing (after a pause, `step` must not jump).
    pub(crate) fn last_step_reset(&mut self) {
        self.last_step = Instant::now();
    }
}

/// Show a file selected in the file manager (org.freedesktop.FileManager1),
/// or open its folder.
fn reveal_in_folder(path: &str) {
    let uri = files::file_uri(path);
    let shown = zbus::blocking::Connection::session()
        .and_then(|c| {
            c.call_method(
                Some("org.freedesktop.FileManager1"),
                "/org/freedesktop/FileManager1",
                Some("org.freedesktop.FileManager1"),
                "ShowItems",
                &(vec![uri.as_str()], ""),
            )
        })
        .is_ok();
    if !shown {
        let parent = std::path::Path::new(path).parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let _ = std::process::Command::new("xdg-open").arg(parent).spawn().map(|mut c| c.wait());
    }
}
