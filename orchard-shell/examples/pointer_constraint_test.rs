//! Test client for pointer locks / confinement (what games do).
//!
//! `pointer_constraint_test lock|confine [HINT_X HINT_Y]`
//!
//! Opens a 400×300 window. When the pointer enters it, locks (or confines)
//! the pointer; once locked, sends a cursor position hint. Prints one line per
//! event (`enter x y`, `motion x y`, `leave`, `locked`, `unlocked`,
//! `relative dx dy`, ...) on stdout. Send it SIGUSR1 to release the
//! constraint (like a game opening its menu).
//!
//! Used with `orchard --headless` and its `rel DX DY` control command.

use std::sync::atomic::{AtomicBool, Ordering};

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_output, delegate_pointer, delegate_pointer_constraints, delegate_registry,
    delegate_relative_pointer, delegate_seat, delegate_shm, delegate_xdg_shell, delegate_xdg_window,
    output::{OutputHandler, OutputState},
    reexports::{
        client::{
            globals::registry_queue_init,
            protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
            Connection, QueueHandle,
        },
        protocols::wp::{
            pointer_constraints::zv1::client::{zwp_confined_pointer_v1, zwp_locked_pointer_v1, zwp_pointer_constraints_v1},
            relative_pointer::zv1::client::zwp_relative_pointer_v1,
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
        pointer_constraints::{PointerConstraintsHandler, PointerConstraintsState},
        relative_pointer::{RelativeMotionEvent, RelativePointerHandler, RelativePointerState},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        xdg::{
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
            XdgShell,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};

const W: u32 = 400;
const H: u32 = 300;

static RELEASE: AtomicBool = AtomicBool::new(false);

extern "C" fn on_usr1(_: i32) {
    RELEASE.store(true, Ordering::SeqCst);
}

enum Constraint {
    Lock(zwp_locked_pointer_v1::ZwpLockedPointerV1),
    Confine(zwp_confined_pointer_v1::ZwpConfinedPointerV1),
}

struct App {
    registry: RegistryState,
    seat: SeatState,
    output: OutputState,
    _compositor: CompositorState,
    shm: Shm,
    relative: RelativePointerState,
    constraints: PointerConstraintsState,
    window: Window,
    pool: SlotPool,
    pointer: Option<wl_pointer::WlPointer>,
    relative_pointer: Option<zwp_relative_pointer_v1::ZwpRelativePointerV1>,
    constraint: Option<Constraint>,
    lock: bool,
    hint: Option<(f64, f64)>,
    configured: bool,
}

fn say(s: String) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{s}");
    let _ = out.flush();
}

fn main() {
    let mut args = std::env::args().skip(1);
    let lock = args.next().as_deref() != Some("confine");
    let hint = match (args.next(), args.next()) {
        (Some(x), Some(y)) => Some((x.parse().unwrap_or(10.0), y.parse().unwrap_or(10.0))),
        _ => None,
    };
    unsafe {
        libc_signal(10, on_usr1);
    }

    let conn = Connection::connect_to_env().expect("no Wayland display");
    let (globals, mut queue) = registry_queue_init(&conn).unwrap();
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).unwrap();
    let xdg = XdgShell::bind(&globals, &qh).unwrap();
    let shm = Shm::bind(&globals, &qh).unwrap();
    let surface = compositor.create_surface(&qh);
    let window = xdg.create_window(surface, WindowDecorations::ServerDefault, &qh);
    window.set_title("pointer constraint test");
    window.set_app_id("org.orchard.pointer-test");
    window.set_min_size(Some((W, H)));
    window.set_max_size(Some((W, H)));
    window.commit();
    let pool = SlotPool::new((W * H * 4) as usize, &shm).unwrap();
    let mut app = App {
        registry: RegistryState::new(&globals),
        seat: SeatState::new(&globals, &qh),
        output: OutputState::new(&globals, &qh),
        _compositor: compositor,
        shm,
        relative: RelativePointerState::bind(&globals, &qh),
        constraints: PointerConstraintsState::bind(&globals, &qh),
        window,
        pool,
        pointer: None,
        relative_pointer: None,
        constraint: None,
        lock,
        hint,
        configured: false,
    };
    loop {
        queue.flush().ok();
        if let Some(guard) = queue.prepare_read() {
            // wake up regularly to notice SIGUSR1
            let fd = guard.connection_fd();
            let mut pfd = [libc_pollfd(std::os::fd::AsRawFd::as_raw_fd(&fd))];
            if unsafe { poll(pfd.as_mut_ptr(), 1, 50) } > 0 {
                let _ = guard.read();
            }
        }
        queue.dispatch_pending(&mut app).unwrap();
        if RELEASE.swap(false, Ordering::SeqCst) {
            match app.constraint.take() {
                Some(Constraint::Lock(l)) => l.destroy(),
                Some(Constraint::Confine(c)) => c.destroy(),
                None => {}
            }
            say("released".into());
            queue.flush().ok();
        }
    }
}

// --- tiny libc shims (no extra dependency) ---
#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}
fn libc_pollfd(fd: i32) -> PollFd {
    PollFd { fd, events: 1, revents: 0 }
}
extern "C" {
    fn poll(fds: *mut PollFd, n: u64, timeout: i32) -> i32;
    fn signal(sig: i32, handler: extern "C" fn(i32)) -> usize;
}
unsafe fn libc_signal(sig: i32, h: extern "C" fn(i32)) {
    signal(sig, h);
}

impl App {
    fn draw(&mut self) {
        let (buffer, canvas) = self
            .pool
            .create_buffer(W as i32, H as i32, (W * 4) as i32, wl_shm::Format::Xrgb8888)
            .unwrap();
        canvas.fill(0x80);
        let s = self.window.wl_surface();
        s.damage_buffer(0, 0, W as i32, H as i32);
        buffer.attach_to(s).unwrap();
        s.commit();
    }

    fn constrain(&mut self, qh: &QueueHandle<Self>) {
        if self.constraint.is_some() {
            return;
        }
        let Some(pointer) = self.pointer.clone() else { return };
        let s = self.window.wl_surface().clone();
        let lifetime = zwp_pointer_constraints_v1::Lifetime::Persistent;
        self.constraint = if self.lock {
            self.constraints.lock_pointer(&s, &pointer, None, lifetime, qh).ok().map(Constraint::Lock)
        } else {
            self.constraints.confine_pointer(&s, &pointer, None, lifetime, qh).ok().map(Constraint::Confine)
        };
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl WindowHandler for App {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        std::process::exit(0);
    }
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window, _: WindowConfigure, _: u32) {
        if !self.configured {
            self.configured = true;
            say("mapped".into());
        }
        self.draw();
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Pointer && self.pointer.is_none() {
            let pointer = self.seat.get_pointer(qh, &seat).unwrap();
            self.relative_pointer = self.relative.get_relative_pointer(&pointer, qh).ok();
            say(format!("relative pointer: {}", self.relative_pointer.is_some()));
            self.pointer = Some(pointer);
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, _: Capability) {}
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for App {
    fn pointer_frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for e in events {
            let (x, y) = e.position;
            match e.kind {
                PointerEventKind::Enter { .. } => {
                    say(format!("enter {x:.1} {y:.1}"));
                    self.constrain(qh);
                }
                PointerEventKind::Leave { .. } => say("leave".into()),
                PointerEventKind::Motion { .. } => say(format!("motion {x:.1} {y:.1}")),
                _ => {}
            }
        }
    }
}

impl PointerConstraintsHandler for App {
    fn confined(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &zwp_confined_pointer_v1::ZwpConfinedPointerV1, _: &wl_surface::WlSurface, _: &wl_pointer::WlPointer) {
        say("confined".into());
    }
    fn unconfined(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &zwp_confined_pointer_v1::ZwpConfinedPointerV1, _: &wl_surface::WlSurface, _: &wl_pointer::WlPointer) {
        say("unconfined".into());
    }
    fn locked(&mut self, _: &Connection, _: &QueueHandle<Self>, l: &zwp_locked_pointer_v1::ZwpLockedPointerV1, _: &wl_surface::WlSurface, _: &wl_pointer::WlPointer) {
        say("locked".into());
        if let Some((x, y)) = self.hint {
            l.set_cursor_position_hint(x, y);
            self.window.wl_surface().commit();
            say(format!("hint {x} {y}"));
        }
    }
    fn unlocked(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &zwp_locked_pointer_v1::ZwpLockedPointerV1, _: &wl_surface::WlSurface, _: &wl_pointer::WlPointer) {
        say("unlocked".into());
    }
}

impl RelativePointerHandler for App {
    fn relative_pointer_motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &zwp_relative_pointer_v1::ZwpRelativePointerV1, _: &wl_pointer::WlPointer, e: RelativeMotionEvent) {
        say(format!("relative {:.0} {:.0}", e.delta.0, e.delta.1));
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(App);
delegate_output!(App);
delegate_shm!(App);
delegate_seat!(App);
delegate_pointer!(App);
delegate_pointer_constraints!(App);
delegate_relative_pointer!(App);
delegate_xdg_shell!(App);
delegate_xdg_window!(App);
delegate_registry!(App);
