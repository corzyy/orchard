//! macOS-style screenshots.
//!
//! * ⌘⇧3 saves the screen under the pointer, ⌘⇧4 lets you drag out an area
//!   (Space switches to picking a window, Esc cancels). With ⌃ added the image
//!   goes to the clipboard instead of a file.
//! * Files land in the Pictures folder as "Bildschirmfoto … um ….png" /
//!   "Screenshot … at ….png"; a notification with a thumbnail confirms it.

use std::path::PathBuf;

use smithay::{
    backend::renderer::{
        damage::OutputDamageTracker,
        gles::{GlesRenderer, GlesTexture},
        Bind, ExportMem, Offscreen,
    },
    backend::allocator::Fourcc,
    input::pointer::CursorImageStatus,
    output::Output,
    utils::{Logical, Point, Rectangle, Size, Transform},
};

use crate::{
    render::{output_elements, CustomRenderElements, RenderCtx},
    shell::WindowElement,
    state::{Backend, OrchardState},
};

/// Where a screenshot should be saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveTarget {
    Pictures,
    Desktop,
    Documents,
    Clipboard,
}

impl SaveTarget {
    pub fn parse(s: Option<&str>) -> Self {
        match s {
            Some("desktop") => Self::Desktop,
            Some("documents") => Self::Documents,
            Some("clipboard") => Self::Clipboard,
            _ => Self::Pictures,
        }
    }
}

fn xdg_dir(key: &str, fallback: &str) -> PathBuf {
    if let Some(dir) = std::env::var_os(key) {
        let p = PathBuf::from(dir);
        if p.is_absolute() {
            return p;
        }
    }
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    let prefix = format!("{key}=");
    if let Ok(s) = std::fs::read_to_string(config.join("user-dirs.dirs")) {
        for line in s.lines() {
            if let Some(v) = line.strip_prefix(&prefix) {
                let v = v.trim_matches('"').replace("$HOME", &home.to_string_lossy());
                return PathBuf::from(v);
            }
        }
    }
    home.join(fallback)
}

fn screenshots_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("ORCHARD_SCREENSHOT_DIR") {
        return PathBuf::from(dir);
    }
    let pictures = xdg_dir("XDG_PICTURES_DIR", "Pictures");
    let de_sub = pictures.join("Bildschirmfotos");
    let en_sub = pictures.join("Screenshots");
    if de_sub.is_dir() {
        de_sub
    } else if en_sub.is_dir() {
        en_sub
    } else if german() || pictures.file_name().and_then(|n| n.to_str()) == Some("Bilder") {
        de_sub
    } else {
        en_sub
    }
}

fn target_dir(target: SaveTarget) -> PathBuf {
    match target {
        SaveTarget::Pictures => screenshots_dir(),
        SaveTarget::Desktop => xdg_dir("XDG_DESKTOP_DIR", "Desktop"),
        SaveTarget::Documents => xdg_dir("XDG_DOCUMENTS_DIR", "Documents"),
        SaveTarget::Clipboard => {
            std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
        }
    }
}

fn german() -> bool {
    std::env::var("LANG").map(|l| l.starts_with("de")).unwrap_or(false)
}

fn file_name() -> String {
    let now = chrono_like_now();
    if german() {
        format!("Bildschirmfoto {} um {}.png", now.0, now.1)
    } else {
        format!("Screenshot {} at {}.png", now.0, now.1)
    }
}

/// ("YYYY-MM-DD", "HH.MM.SS") in local time, without spawning `date` on the
/// compositor thread at the moment of the capture.
fn chrono_like_now() -> (String, String) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as libc::time_t)
        .unwrap_or(0);
    // SAFETY: localtime_r only writes into the zeroed `tm` we own.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return ("screenshot".into(), "0".into());
        }
        tm
    };
    (
        format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday),
        format!("{:02}.{:02}.{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec),
    )
}

/// ⌘⇧4 in progress.
#[derive(Debug, Clone)]
pub struct Select {
    /// drag start (global logical); None until the button goes down
    pub start: Option<Point<f64, Logical>>,
    /// Space pressed: pick a window instead of an area
    pub window_mode: bool,
    pub clipboard: bool,
}

/// What the render loop draws while selecting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Overlay {
    Area(Rectangle<i32, Logical>),
    Window(Rectangle<i32, Logical>),
}

/// An RGBA image read back from the GPU.
struct Pixels {
    data: Vec<u8>,
    w: u32,
    h: u32,
}

impl Pixels {
    fn crop(self, x: u32, y: u32, w: u32, h: u32) -> Option<Pixels> {
        let x = x.min(self.w);
        let y = y.min(self.h);
        let w = w.min(self.w - x);
        let h = h.min(self.h - y);
        if w == 0 || h == 0 {
            return None;
        }
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for row in y..y + h {
            let start = ((row * self.w + x) * 4) as usize;
            data.extend_from_slice(&self.data[start..start + (w * 4) as usize]);
        }
        Some(Pixels { data, w, h })
    }
}

fn read_back(renderer: &mut GlesRenderer, texture: &mut GlesTexture, size: Size<i32, smithay::utils::Buffer>) -> Result<Vec<u8>, String> {
    let target = renderer.bind(texture).map_err(|e| e.to_string())?;
    let mapping = renderer
        .copy_framebuffer(&target, Rectangle::from_size(size), Fourcc::Abgr8888)
        .map_err(|e| e.to_string())?;
    Ok(renderer.map_texture(&mapping).map_err(|e| e.to_string())?.to_vec())
}

impl<BackendData: Backend> OrchardState<BackendData> {
    /// Render `output` offscreen (no selection overlay, no screenshot toolbar;
    /// optionally includes the mouse pointer).
    fn capture_output(&mut self, output: &Output, include_cursor: bool) -> Option<Pixels> {
        let mode = output.current_mode()?;
        let scale = output.current_scale().fractional_scale();
        let size: Size<i32, smithay::utils::Buffer> = (mode.size.w, mode.size.h).into();
        let now = self.clock.now().into();
        let overlay = self.fx.shot_overlay.take();
        let prev_hide = self.fx.hide_screenshot_ui;
        self.fx.hide_screenshot_ui = true;
        let cursor_buffer = if include_cursor { self.backend_data.cursor_buffer() } else { None };
        let cursor_status = self.cursor_status.clone();
        let pointer_pos = self.pointer.current_location();
        let output_geo = self.space.output_geometry(output);
        let space = &self.space;
        let mut ctx = RenderCtx {
            fx: &mut self.fx,
            backdrops: &self.ipc.backdrops,
            mc: None,
            space_info: self.spaces.info(&output.name()),
            locked: self.protocols.locked,
            lock_surface: crate::protocols::lock_surface_of(output),
        };
        let result = self.backend_data.with_renderer(|renderer: &mut GlesRenderer| {
            let mut texture: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, size).map_err(|e| e.to_string())?;
            {
                let mut target = renderer.bind(&mut texture).map_err(|e| e.to_string())?;
                let cursor = if include_cursor {
                    crate::screencopy::cursor_elements(output_geo, pointer_pos, &cursor_status, scale, renderer, cursor_buffer)
                } else {
                    Vec::<CustomRenderElements<GlesRenderer>>::new()
                };
                let (elements, clear) =
                    output_elements(output, space, &mut ctx, cursor, renderer, now);
                let mut tracker = OutputDamageTracker::new(mode.size, scale, Transform::Normal);
                tracker
                    .render_output(renderer, &mut target, 0, &elements, clear)
                    .map_err(|e| format!("{e:?}"))?;
            }
            read_back(renderer, &mut texture, size)
        });
        self.fx.hide_screenshot_ui = prev_hide;
        self.fx.shot_overlay = overlay;
        match result {
            Some(Ok(data)) => Some(Pixels {
                data,
                w: size.w as u32,
                h: size.h as u32,
            }),
            Some(Err(e)) => {
                tracing::warn!("screenshot failed: {e}");
                None
            }
            None => None,
        }
    }

    /// Render one window on its own, with title bar and shadow (like macOS).
    fn capture_window(&mut self, window: &WindowElement) -> Option<Pixels> {
        let scale = self.window_output(window).as_ref()
            .map(|o| o.current_scale().fractional_scale())
            .unwrap_or(1.0);
        let result = self.backend_data.with_renderer(|renderer: &mut GlesRenderer| {
            let mut snap = crate::effects::snapshot::snapshot_window(renderer, window, scale).ok_or("snapshot failed")?;
            let size = (snap.size.w, snap.size.h).into();
            read_back(renderer, &mut snap.texture, size).map(|d| (d, snap.size))
        });
        match result {
            Some(Ok((data, size))) => Some(Pixels {
                data,
                w: size.w as u32,
                h: size.h as u32,
            }),
            Some(Err(e)) => {
                tracing::warn!("window screenshot failed: {e}");
                None
            }
            None => None,
        }
    }

    /// ⌘⇧3 (⌃⌘⇧3 = to the clipboard): the screen under the pointer.
    pub fn take_screenshot(&mut self) {
        self.screenshot_screen(false);
    }

    pub fn screenshot_screen(&mut self, clipboard: bool) {
        let Some(output) = self.active_output() else { return };
        if let Some(px) = self.capture_output(&output, false) {
            let target = if clipboard { SaveTarget::Clipboard } else { SaveTarget::Pictures };
            self.deliver_screenshot(px, target);
        }
    }

    /// Print / ⌘⇧5: open the macOS-style interactive screenshot toolbar in orchard-shell.
    pub fn open_screenshot_tool(&mut self) {
        if self.protocols.locked {
            return;
        }
        if self.mc.is_some() {
            self.close_mission_control(None);
        }
        self.cancel_area_screenshot();
        let Some(output) = self.active_output() else { return };
        let Some(geo) = self.space.output_geometry(&output) else { return };
        let windows: Vec<crate::ipc::ShotWindow> = self
            .space
            .elements_for_output(&output)
            .rev()
            .filter_map(|w| {
                let loc = self.space.element_location(w)?;
                let sz = smithay::desktop::space::SpaceElement::geometry(w).size;
                Some(crate::ipc::ShotWindow {
                    id: w.ipc_id().0,
                    x: loc.x - geo.loc.x,
                    y: loc.y - geo.loc.y,
                    w: sz.w,
                    h: sz.h,
                    title: w.title(),
                })
            })
            .collect();
        self.ipc_broadcast(&crate::ipc::Event::ScreenshotTool {
            output: output.name(),
            windows,
        });
    }

    /// Perform a capture requested by the screenshot toolbar over IPC.
    pub fn capture_from_tool(
        &mut self,
        mode: &str,
        output_name: Option<&str>,
        rect: Option<crate::ipc::Rect>,
        window_id: Option<u64>,
        target: Option<&str>,
        include_cursor: bool,
    ) {
        let save_target = SaveTarget::parse(target);
        let output = output_name
            .and_then(|name| self.space.outputs().find(|o| o.name() == name).cloned())
            .or_else(|| self.active_output());
        match mode {
            "window" => {
                let window = window_id
                    .and_then(|id| self.space.elements().find(|w| w.ipc_id().0 == id).cloned())
                    .or_else(|| self.focused_window());
                if let Some(px) = window.and_then(|w| self.capture_window(&w)) {
                    self.deliver_screenshot(px, save_target);
                }
            }
            "area" => {
                let (Some(output), Some(r)) = (output, rect) else { return };
                if r.w < 2.0 || r.h < 2.0 {
                    return;
                }
                let scale = output.current_scale().fractional_scale();
                let x = (r.x.max(0.0) * scale).round() as u32;
                let y = (r.y.max(0.0) * scale).round() as u32;
                let w = (r.w.max(1.0) * scale).round() as u32;
                let h = (r.h.max(1.0) * scale).round() as u32;
                if let Some(px) = self.capture_output(&output, include_cursor) {
                    if let Some(cropped) = px.crop(x, y, w, h) {
                        self.deliver_screenshot(cropped, save_target);
                    }
                }
            }
            _ => {
                let Some(output) = output else { return };
                if let Some(px) = self.capture_output(&output, include_cursor) {
                    self.deliver_screenshot(px, save_target);
                }
            }
        }
    }

    /// ⌘⇧4: start picking an area.
    pub fn start_area_screenshot(&mut self, clipboard: bool) {
        if self.protocols.locked {
            return;
        }
        self.shot_select = Some(Select {
            start: None,
            window_mode: false,
            clipboard,
        });
        self.cursor_status = CursorImageStatus::Named(smithay::input::pointer::CursorIcon::Crosshair);
        // take the pointer away from the client underneath
        self.refresh_pointer_focus();
    }

    pub fn cancel_area_screenshot(&mut self) {
        if self.shot_select.take().is_some() {
            self.fx.shot_overlay = None;
            self.cursor_status = CursorImageStatus::default_named();
            self.refresh_pointer_focus();
        }
    }

    /// Space while picking: toggle between area and window.
    pub fn toggle_screenshot_window_mode(&mut self) {
        if let Some(s) = self.shot_select.as_mut() {
            s.window_mode = !s.window_mode;
            s.start = None;
            self.cursor_status = CursorImageStatus::Named(if s.window_mode {
                smithay::input::pointer::CursorIcon::Pointer
            } else {
                smithay::input::pointer::CursorIcon::Crosshair
            });
        }
    }

    fn window_at(&self, pos: Point<f64, Logical>) -> Option<WindowElement> {
        self.space.element_under(pos).map(|(w, _)| w.clone())
    }

    /// What to draw for the current pointer position (called before each frame).
    pub fn update_shot_overlay(&mut self) {
        let Some(sel) = self.shot_select.as_ref() else {
            self.fx.shot_overlay = None;
            return;
        };
        let pos = self.pointer.current_location();
        self.fx.shot_overlay = if sel.window_mode {
            self.window_at(pos).and_then(|w| {
                let loc = self.space.element_location(&w)?;
                Some(Overlay::Window(Rectangle::new(loc, smithay::desktop::space::SpaceElement::geometry(&w).size)))
            })
        } else {
            sel.start.map(|s| Overlay::Area(rect_between(s, pos)))
        };
    }

    /// Pointer button while picking. Returns true if the event was consumed.
    pub fn screenshot_button(&mut self, pressed: bool) -> bool {
        let Some(sel) = self.shot_select.clone() else { return false };
        let pos = self.pointer.current_location();
        if sel.window_mode {
            if pressed {
                let window = self.window_at(pos);
                self.cancel_area_screenshot();
                if let Some(px) = window.and_then(|w| self.capture_window(&w)) {
                    let target = if sel.clipboard { SaveTarget::Clipboard } else { SaveTarget::Pictures };
                    self.deliver_screenshot(px, target);
                }
            }
            return true;
        }
        if pressed {
            if let Some(s) = self.shot_select.as_mut() {
                s.start = Some(pos);
            }
            return true;
        }
        let Some(start) = sel.start else { return true };
        let rect = rect_between(start, pos);
        self.cancel_area_screenshot();
        if rect.size.w < 4 || rect.size.h < 4 {
            // a click without dragging: nothing to capture
            return true;
        }
        // the output holding the area's center; the area is clipped to it
        let center = (rect.loc + Point::from((rect.size.w / 2, rect.size.h / 2))).to_f64();
        let Some(output) = self.space.output_under(center).next().cloned() else { return true };
        let Some(geo) = self.space.output_geometry(&output) else { return true };
        let scale = output.current_scale().fractional_scale();
        let Some(local) = rect.intersection(geo) else { return true };
        let phys = Rectangle::new(local.loc - geo.loc, local.size).to_f64().to_physical(scale).to_i32_round::<i32>();
        if let Some(px) = self.capture_output(&output, false) {
            if let Some(px) = px.crop(phys.loc.x.max(0) as u32, phys.loc.y.max(0) as u32, phys.size.w as u32, phys.size.h as u32) {
                let target = if sel.clipboard { SaveTarget::Clipboard } else { SaveTarget::Pictures };
                self.deliver_screenshot(px, target);
            }
        }
        true
    }

    /// Re-run pointer focus at the current location (e.g. after a mode change).
    fn refresh_pointer_focus(&mut self) {
        let pos = self.pointer.current_location();
        let under = self.surface_under(pos);
        let pointer = self.pointer.clone();
        pointer.motion(
            self,
            under,
            &smithay::input::pointer::MotionEvent {
                location: pos,
                serial: smithay::utils::SERIAL_COUNTER.next_serial(),
                time: self.clock.now().as_millis(),
            },
        );
        pointer.frame(self);
    }

    /// Save to Pictures/Desktop/Documents (or copy to the clipboard), play the shutter sound and
    /// confirm with a notification. The encoding runs on a helper thread.
    fn deliver_screenshot(&self, px: Pixels, target: SaveTarget) {
        let clipboard = target == SaveTarget::Clipboard;
        let mut env: Vec<(String, String)> = Vec::new();
        if let Some(s) = &self.socket_name {
            env.push(("WAYLAND_DISPLAY".into(), s.clone()));
        }
        let path = if clipboard {
            target_dir(SaveTarget::Clipboard).join("orchard-screenshot.png")
        } else {
            let dir = target_dir(target);
            let _ = std::fs::create_dir_all(&dir);
            dir.join(file_name())
        };
        std::thread::spawn(move || {
            let mut data = px.data;
            // GL hands back premultiplied alpha (window shadows); PNG wants straight alpha
            for p in data.chunks_exact_mut(4) {
                let a = p[3] as u32;
                if a > 0 && a < 255 {
                    for c in &mut p[..3] {
                        *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                    }
                }
            }
            let Some(img) = image::RgbaImage::from_raw(px.w, px.h, data) else {
                tracing::warn!("screenshot: bad buffer size");
                return;
            };
            if let Err(e) = img.save(&path) {
                tracing::warn!("screenshot save failed: {e}");
                return;
            }
            tracing::info!("screenshot saved: {}", path.display());
            let sound = "/usr/share/sounds/freedesktop/stereo/screen-capture.oga";
            if std::path::Path::new(sound).exists() {
                let _ = std::process::Command::new("pw-play").arg(sound).envs(env.clone()).spawn().map(|mut c| {
                    std::thread::spawn(move || c.wait());
                });
            }
            let (title, body) = match (clipboard, german()) {
                (true, true) => ("Bildschirmfoto", "In die Zwischenablage kopiert".to_string()),
                (true, false) => ("Screenshot", "Copied to the clipboard".to_string()),
                (false, true) => ("Bildschirmfoto", format!("Gesichert in {}", path.parent().map(|p| p.display().to_string()).unwrap_or_default())),
                (false, false) => ("Screenshot", format!("Saved to {}", path.parent().map(|p| p.display().to_string()).unwrap_or_default())),
            };
            if clipboard {
                match std::fs::File::open(&path) {
                    Ok(f) => {
                        let r = std::process::Command::new("wl-copy")
                            .args(["--type", "image/png"])
                            .stdin(f)
                            .envs(env.clone())
                            .status();
                        if let Err(e) = r {
                            tracing::warn!("wl-copy: {e}");
                        }
                    }
                    Err(e) => tracing::warn!("screenshot: {e}"),
                }
            }
            let _ = std::process::Command::new("notify-send")
                .args(["-a", title, "-t", "4000", "-h"])
                .arg(format!("string:image-path:{}", path.display()))
                .args([title, &body])
                .envs(env)
                .status();
        });
    }
}

fn rect_between(a: Point<f64, Logical>, b: Point<f64, Logical>) -> Rectangle<i32, Logical> {
    let (x0, x1) = (a.x.min(b.x), a.x.max(b.x));
    let (y0, y1) = (a.y.min(b.y), a.y.max(b.y));
    Rectangle::new(
        (x0.round() as i32, y0.round() as i32).into(),
        ((x1 - x0).round() as i32, (y1 - y0).round() as i32).into(),
    )
}
