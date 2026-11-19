//! `wlr-screencopy-unstable-v1` (version 3): lets clients capture outputs.
//!
//! Used by `xdg-desktop-portal-hyprland` for screen sharing (browsers, Discord,
//! OBS via PipeWire) and by tools like `grim`. Frames are rendered offscreen
//! from the same scene as the display (optionally with the cursor), into
//! either a shm buffer (read back from the GPU) or a client-allocated dmabuf
//! (rendered into directly). `copy_with_damage` waits until something on that
//! output changed, so a static screen costs nothing.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::Duration,
};

use smithay::{
    backend::{
        allocator::{Buffer as _, Fourcc},
        renderer::{
            damage::OutputDamageTracker,
            element::{memory::MemoryRenderBuffer, AsRenderElements},
            gles::{GlesRenderer, GlesTexture},
            Bind, ExportMem, Offscreen,
        },
    },
    input::pointer::CursorImageStatus,
    output::Output,
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        wayland_protocols_wlr::screencopy::v1::server::{
            zwlr_screencopy_frame_v1::{self, Flags, ZwlrScreencopyFrameV1},
            zwlr_screencopy_manager_v1::{self, ZwlrScreencopyManagerV1},
        },
        wayland_server::{
            backend::{ClientId, GlobalId},
            protocol::{wl_buffer::WlBuffer, wl_shm},
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
        },
    },
    utils::{Logical, Physical, Point, Rectangle, Scale, Size, Transform},
    wayland::{dmabuf::get_dmabuf, shm::with_buffer_contents_mut},
};

use crate::{
    drawing::PointerElement,
    render::{output_elements, CustomRenderElements, OutputRenderElements, RenderCtx},
    state::{Backend, OrchardState},
};

const VERSION: u32 = 3;
/// how often pending `copy_with_damage` requests look for new damage
const POLL: Duration = Duration::from_millis(8);

pub struct ScreencopyState {
    _global: GlobalId,
    pending: Vec<PendingCopy>,
    /// one damage tracker per (output, client): damage since that client's last frame
    trackers: HashMap<(String, ClientId), OutputDamageTracker>,
    polling: bool,
}

impl std::fmt::Debug for ScreencopyState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScreencopyState").field("pending", &self.pending.len()).finish()
    }
}

impl ScreencopyState {
    pub fn new<B: Backend + 'static>(dh: &DisplayHandle) -> Self {
        let global = dh.create_global::<OrchardState<B>, ZwlrScreencopyManagerV1, _>(VERSION, ());
        ScreencopyState {
            _global: global,
            pending: Vec::new(),
            trackers: HashMap::new(),
            polling: false,
        }
    }
}

/// What a frame captures; fixed when the frame is created.
#[derive(Debug, Clone)]
pub struct FrameInfo {
    output: Output,
    /// area of the output in buffer pixels
    region: Rectangle<i32, Physical>,
    /// size of the whole output in pixels
    output_size: Size<i32, Physical>,
    overlay_cursor: bool,
}

#[derive(Debug, Default)]
pub struct FrameData {
    info: Option<FrameInfo>,
    copied: Mutex<bool>,
}

struct PendingCopy {
    frame: ZwlrScreencopyFrameV1,
    buffer: WlBuffer,
    info: FrameInfo,
    with_damage: bool,
}

// ---- protocol ------------------------------------------------------------

impl<B: Backend + 'static> GlobalDispatch<ZwlrScreencopyManagerV1, ()> for OrchardState<B> {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrScreencopyManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl<B: Backend + 'static> Dispatch<ZwlrScreencopyManagerV1, ()> for OrchardState<B> {
    fn request(
        state: &mut Self,
        _client: &Client,
        _manager: &ZwlrScreencopyManagerV1,
        request: zwlr_screencopy_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let (frame, overlay_cursor, wl_output, region) = match request {
            zwlr_screencopy_manager_v1::Request::CaptureOutput {
                frame,
                overlay_cursor,
                output,
            } => (frame, overlay_cursor, output, None),
            zwlr_screencopy_manager_v1::Request::CaptureOutputRegion {
                frame,
                overlay_cursor,
                output,
                x,
                y,
                width,
                height,
            } => (frame, overlay_cursor, output, Some(Rectangle::<i32, Logical>::new((x, y).into(), (width, height).into()))),
            zwlr_screencopy_manager_v1::Request::Destroy => return,
            _ => return,
        };

        let info = Output::from_resource(&wl_output).and_then(|output| {
            let mode = output.current_mode()?;
            let output_size: Size<i32, Physical> = output.current_transform().transform_size(mode.size);
            let full = Rectangle::from_size(output_size);
            let region = match region {
                None => full,
                Some(r) => {
                    let scale = output.current_scale().fractional_scale();
                    r.to_f64().to_physical(scale).to_i32_round().intersection(full)?
                }
            };
            (!region.is_empty()).then_some(FrameInfo {
                output,
                region,
                output_size,
                overlay_cursor: overlay_cursor != 0,
            })
        });

        let frame = data_init.init(
            frame,
            FrameData {
                info: info.clone(),
                copied: Mutex::new(false),
            },
        );
        let Some(info) = info else {
            frame.failed();
            return;
        };
        let (w, h) = (info.region.size.w as u32, info.region.size.h as u32);
        frame.buffer(wl_shm::Format::Xrgb8888, w, h, w * 4);
        if frame.version() >= 3 {
            // GPU buffers only for whole outputs (regions are cut out of a readback)
            if info.region.size == info.output_size {
                frame.linux_dmabuf(Fourcc::Xrgb8888 as u32, w, h);
            }
            frame.buffer_done();
        }
        let _ = state;
    }
}

impl<B: Backend + 'static> Dispatch<ZwlrScreencopyFrameV1, FrameData> for OrchardState<B> {
    fn request(
        state: &mut Self,
        _client: &Client,
        frame: &ZwlrScreencopyFrameV1,
        request: zwlr_screencopy_frame_v1::Request,
        data: &FrameData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let (buffer, with_damage) = match request {
            zwlr_screencopy_frame_v1::Request::Copy { buffer } => (buffer, false),
            zwlr_screencopy_frame_v1::Request::CopyWithDamage { buffer } => (buffer, true),
            zwlr_screencopy_frame_v1::Request::Destroy => return,
            _ => return,
        };
        let Some(info) = data.info.clone() else {
            frame.failed();
            return;
        };
        {
            let mut copied = data.copied.lock().unwrap_or_else(|e| e.into_inner());
            if *copied {
                frame.post_error(zwlr_screencopy_frame_v1::Error::AlreadyUsed, "frame already used");
                return;
            }
            *copied = true;
        }
        if !buffer_fits(&buffer, &info) {
            frame.post_error(zwlr_screencopy_frame_v1::Error::InvalidBuffer, "buffer does not match the frame");
            return;
        }
        state.screencopy_queue(PendingCopy {
            frame: frame.clone(),
            buffer,
            info,
            with_damage,
        });
    }
}

fn buffer_fits(buffer: &WlBuffer, info: &FrameInfo) -> bool {
    let (w, h) = (info.region.size.w, info.region.size.h);
    if let Ok(dmabuf) = get_dmabuf(buffer) {
        let size = dmabuf.size();
        return info.region.size == info.output_size && size.w == w && size.h == h;
    }
    with_buffer_contents_mut(buffer, |_, len, data| {
        data.width == w
            && data.height == h
            && data.stride >= w * 4
            && data.format == wl_shm::Format::Xrgb8888
            && (data.offset as usize + data.stride as usize * h as usize) <= len
    })
    .unwrap_or(false)
}

// ---- rendering --------------------------------------------------------------

/// Cursor elements for a recording of `output` (if the pointer is on it).
fn cursor_elements(
    space_geo: Option<Rectangle<i32, Logical>>,
    pos: Point<f64, Logical>,
    status: &CursorImageStatus,
    scale: f64,
    renderer: &mut GlesRenderer,
    buffer: Option<(MemoryRenderBuffer, Point<i32, Logical>)>,
) -> Vec<CustomRenderElements<GlesRenderer>> {
    let Some(geo) = space_geo else { return Vec::new() };
    if !geo.to_f64().contains(pos) {
        return Vec::new();
    }
    let mut element = PointerElement::default();
    let hotspot = match status {
        CursorImageStatus::Surface(surface) => smithay::wayland::compositor::with_states(surface, |states| {
            states
                .data_map
                .get::<Mutex<smithay::input::pointer::CursorImageAttributes>>()
                .map(|a| a.lock().map(|a| a.hotspot).unwrap_or_default())
                .unwrap_or_default()
        }),
        CursorImageStatus::Named(_) => match buffer {
            Some((buf, hot)) => {
                element.set_buffer(buf);
                hot
            }
            None => return Vec::new(),
        },
        CursorImageStatus::Hidden => return Vec::new(),
    };
    element.set_status(status.clone());
    let loc = (pos - geo.loc.to_f64() - hotspot.to_f64()).to_physical(scale).to_i32_round();
    element.render_elements(renderer, loc, Scale::from(scale), 1.0)
}

impl<B: Backend + 'static> OrchardState<B> {
    fn screencopy_queue(&mut self, copy: PendingCopy) {
        let Some(sc) = self.screencopy.as_mut() else {
            copy.frame.failed();
            return;
        };
        sc.pending.push(copy);
        if !sc.polling {
            sc.polling = true;
            let res = self.handle.insert_source(Timer::immediate(), |_, _, state| {
                state.screencopy_process();
                let more = state.screencopy.as_ref().map(|s| !s.pending.is_empty()).unwrap_or(false);
                if more {
                    TimeoutAction::ToDuration(POLL)
                } else {
                    if let Some(sc) = state.screencopy.as_mut() {
                        sc.polling = false;
                    }
                    TimeoutAction::Drop
                }
            });
            if res.is_err() {
                if let Some(sc) = self.screencopy.as_mut() {
                    sc.polling = false;
                }
            }
        }
    }

    /// Serve every pending copy that can be served now.
    fn screencopy_process(&mut self) {
        let Some(sc) = self.screencopy.as_mut() else { return };
        let pending = std::mem::take(&mut sc.pending);
        let mut keep = Vec::new();
        for copy in pending {
            if !copy.frame.is_alive() || !copy.buffer.is_alive() {
                continue;
            }
            if !self.space.outputs().any(|o| *o == copy.info.output) {
                copy.frame.failed();
                continue;
            }
            match self.screencopy_render(&copy) {
                Ok(Some(damage)) => {
                    let now: Duration = self.clock.now().into();
                    copy.frame.flags(Flags::empty());
                    if copy.with_damage && copy.frame.version() >= 2 {
                        for r in damage {
                            copy.frame.damage(r.loc.x.max(0) as u32, r.loc.y.max(0) as u32, r.size.w as u32, r.size.h as u32);
                        }
                    }
                    let secs = now.as_secs();
                    copy.frame.ready((secs >> 32) as u32, (secs & 0xffff_ffff) as u32, now.subsec_nanos());
                }
                // nothing changed yet: try again on the next poll
                Ok(None) => keep.push(copy),
                Err(e) => {
                    tracing::warn!("screencopy failed: {e}");
                    copy.frame.failed();
                }
            }
        }
        if let Some(sc) = self.screencopy.as_mut() {
            // drop trackers of clients that went away
            let dh = self.display_handle.clone();
            sc.trackers.retain(|(_, client), _| dh.backend_handle().get_client_data(client.clone()).is_ok());
            keep.append(&mut sc.pending);
            sc.pending = keep;
        }
    }

    /// Render one frame. Ok(None): `copy_with_damage` and nothing changed.
    fn screencopy_render(&mut self, copy: &PendingCopy) -> Result<Option<Vec<Rectangle<i32, Physical>>>, String> {
        let output = copy.info.output.clone();
        let scale = output.current_scale().fractional_scale();
        let now = self.clock.now().into();
        let client = copy.frame.client().map(|c| c.id());
        let cursor_buffer = if copy.info.overlay_cursor { self.backend_data.cursor_buffer() } else { None };
        let mut tracker_key = None;
        // the damage tracker lives outside the renderer closure
        let mut tracker = match (&client, copy.with_damage) {
            (Some(c), true) => {
                let key = (output.name(), c.clone());
                let t = self
                    .screencopy
                    .as_mut()
                    .and_then(|sc| sc.trackers.remove(&key))
                    .unwrap_or_else(|| OutputDamageTracker::from_output(&output));
                tracker_key = Some(key);
                Some(t)
            }
            _ => None,
        };

        let cursor_status = self.cursor_status.clone();
        let pointer_pos = self.pointer.current_location();
        let output_geo = self.space.output_geometry(&output);
        let overlay_cursor = copy.info.overlay_cursor;

        let space = &self.space;
        let mut ctx = RenderCtx {
            fx: &mut self.fx,
            backdrops: &self.ipc.backdrops,
            mc: self.mc.as_ref(),
            space_info: self.spaces.info(&output.name()),
            locked: self.protocols.locked,
            lock_surface: crate::protocols::lock_surface_of(&output),
        };
        let info = copy.info.clone();
        let buffer = copy.buffer.clone();
        let result = self.backend_data.with_renderer(|renderer: &mut GlesRenderer| -> Result<Option<Vec<Rectangle<i32, Physical>>>, String> {
            let cursor = if overlay_cursor {
                cursor_elements(output_geo, pointer_pos, &cursor_status, scale, renderer, cursor_buffer)
            } else {
                Vec::new()
            };
            let (elements, clear): (Vec<OutputRenderElements>, _) = output_elements(&output, space, &mut ctx, cursor, renderer, now);

            // damage since this client's last frame
            let damage = match tracker.as_mut() {
                Some(t) => {
                    let (damage, _) = t.damage_output(1, &elements).map_err(|e| format!("{e:?}"))?;
                    match damage {
                        Some(d) if !d.is_empty() => d.clone(),
                        _ => return Ok(None),
                    }
                }
                None => vec![Rectangle::from_size(info.region.size)],
            };
            // damage in buffer coordinates of the captured region
            let damage: Vec<_> = damage
                .into_iter()
                .filter_map(|r| r.intersection(info.region))
                .map(|r| Rectangle::new(r.loc - info.region.loc, r.size))
                .collect();
            let damage = if damage.is_empty() { vec![Rectangle::from_size(info.region.size)] } else { damage };

            if let Ok(dmabuf) = get_dmabuf(&buffer) {
                let mut dmabuf = dmabuf.clone();
                let mut target = renderer.bind(&mut dmabuf).map_err(|e| format!("bind dmabuf: {e:?}"))?;
                let mut t = OutputDamageTracker::new(info.output_size, scale, Transform::Normal);
                let res = t
                    .render_output(renderer, &mut target, 0, &elements, clear)
                    .map_err(|e| format!("render: {e:?}"))?;
                // the client reads the buffer right after `ready`
                let _ = res.sync.wait();
                return Ok(Some(damage));
            }

            // shm: render offscreen, read back, swizzle RGBA -> XRGB (BGRX bytes)
            let size = (info.output_size.w, info.output_size.h).into();
            let mut texture: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, size).map_err(|e| e.to_string())?;
            let mut target = renderer.bind(&mut texture).map_err(|e| e.to_string())?;
            let mut t = OutputDamageTracker::new(info.output_size, scale, Transform::Normal);
            t.render_output(renderer, &mut target, 0, &elements, clear)
                .map_err(|e| format!("render: {e:?}"))?;
            let region = Rectangle::new((info.region.loc.x, info.region.loc.y).into(), (info.region.size.w, info.region.size.h).into());
            let mapping = renderer
                .copy_framebuffer(&target, region, Fourcc::Abgr8888)
                .map_err(|e| e.to_string())?;
            let pixels = renderer.map_texture(&mapping).map_err(|e| e.to_string())?;
            let (w, h) = (info.region.size.w as usize, info.region.size.h as usize);
            with_buffer_contents_mut(&buffer, |ptr, len, data| {
                let stride = data.stride as usize;
                let offset = data.offset as usize;
                if offset + stride * h > len || pixels.len() < w * h * 4 {
                    return Err("buffer too small".to_string());
                }
                // SAFETY: bounds checked above; the pool mapping stays valid for this call
                let dst = unsafe { std::slice::from_raw_parts_mut(ptr.add(offset), stride * h) };
                for y in 0..h {
                    let src = &pixels[y * w * 4..(y + 1) * w * 4];
                    let row = &mut dst[y * stride..y * stride + w * 4];
                    for (d, s) in row.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                        d[0] = s[2];
                        d[1] = s[1];
                        d[2] = s[0];
                        d[3] = 255;
                    }
                }
                Ok(())
            })
            .map_err(|e| format!("shm: {e:?}"))??;
            Ok(Some(damage))
        });

        if let (Some(key), Some(t)) = (tracker_key, tracker) {
            if let Some(sc) = self.screencopy.as_mut() {
                sc.trackers.insert(key, t);
            }
        }
        match result {
            Some(r) => r,
            None => Err("no renderer".into()),
        }
    }
}
