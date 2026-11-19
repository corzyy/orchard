//! Test client for wlr-screencopy `copy_with_damage` (used by screen sharing).
//! Captures the first output N times with damage tracking and prints, per frame,
//! how long it waited and the damage it got.
//!
//!     cargo run --example screencopy_probe -- [frames]

use std::{os::fd::AsFd, time::Instant};

use wayland_client::{
    Proxy,
    protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool},
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols_wlr::screencopy::v1::client::{zwlr_screencopy_frame_v1 as frame, zwlr_screencopy_manager_v1 as mgr};

#[derive(Default)]
struct App {
    output: Option<wl_output::WlOutput>,
    shm: Option<wl_shm::WlShm>,
    manager: Option<mgr::ZwlrScreencopyManagerV1>,
    buffer_info: Option<(u32, u32, u32)>,
    dmabuf_offered: bool,
    buffer_done: bool,
    damage: Vec<(u32, u32, u32, u32)>,
    ready: bool,
    failed: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(app: &mut Self, reg: &wl_registry::WlRegistry, ev: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, version } = ev {
            match interface.as_str() {
                "wl_output" if app.output.is_none() => app.output = Some(reg.bind(name, 1, qh, ())),
                "wl_shm" => app.shm = Some(reg.bind(name, 1, qh, ())),
                "zwlr_screencopy_manager_v1" => app.manager = Some(reg.bind(name, version.min(3), qh, ())),
                _ => {}
            }
        }
    }
}
impl Dispatch<frame::ZwlrScreencopyFrameV1, ()> for App {
    fn event(app: &mut Self, _: &frame::ZwlrScreencopyFrameV1, ev: frame::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            frame::Event::Buffer { width, height, stride, .. } => app.buffer_info = Some((width, height, stride)),
            frame::Event::LinuxDmabuf { .. } => app.dmabuf_offered = true,
            frame::Event::BufferDone => app.buffer_done = true,
            frame::Event::Damage { x, y, width, height } => app.damage.push((x, y, width, height)),
            frame::Event::Ready { .. } => app.ready = true,
            frame::Event::Failed => app.failed = true,
            _ => {}
        }
    }
}
macro_rules! ignore {
    ($($t:ty),*) => {$(impl Dispatch<$t, ()> for App {
        fn event(_: &mut Self, _: &$t, _: <$t as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    })*};
}
ignore!(wl_output::WlOutput, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_buffer::WlBuffer, mgr::ZwlrScreencopyManagerV1);

fn main() {
    let frames: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(3);
    let conn = Connection::connect_to_env().expect("no compositor");
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut app = App::default();
    queue.roundtrip(&mut app).unwrap();
    let (Some(output), Some(shm), Some(manager)) = (app.output.clone(), app.shm.clone(), app.manager.clone()) else {
        eprintln!("missing globals");
        std::process::exit(2);
    };
    for i in 0..frames {
        app.buffer_info = None;
        app.buffer_done = false;
        app.ready = false;
        app.failed = false;
        app.damage.clear();
        let f = manager.capture_output(1, &output, &qh, ());
        while !(app.buffer_done || (manager.version() < 3 && app.buffer_info.is_some())) {
            queue.blocking_dispatch(&mut app).unwrap();
        }
        let (w, h, stride) = app.buffer_info.unwrap();
        let size = (stride * h) as usize;
        let file = tempfile();
        file.set_len(size as u64).unwrap();
        let pool = shm.create_pool(file.as_fd(), size as i32, &qh, ());
        let buf = pool.create_buffer(0, w as i32, h as i32, stride as i32, wl_shm::Format::Xrgb8888, &qh, ());
        let t = Instant::now();
        f.copy_with_damage(&buf);
        while !app.ready && !app.failed {
            queue.blocking_dispatch(&mut app).unwrap();
        }
        println!(
            "frame {i}: {} after {:?}, {w}x{h}, dmabuf offered: {}, damage: {:?}",
            if app.ready { "ready" } else { "FAILED" },
            t.elapsed(),
            app.dmabuf_offered,
            app.damage
        );
        f.destroy();
        buf.destroy();
        pool.destroy();
    }
}

fn tempfile() -> std::fs::File {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let path = format!("{dir}/screencopy-probe-{}", std::process::id());
    let f = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    f
}
