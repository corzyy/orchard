static POSSIBLE_BACKENDS: &[&str] = &[
    #[cfg(feature = "winit")]
    "--nested (--winit) : Run orchard as a X11 or Wayland client using winit.",
    #[cfg(feature = "headless")]
    "--headless : Render offscreen without a window (testing; see src/headless.rs).",
    #[cfg(feature = "udev")]
    "--tty (--tty-udev) : Run orchard as a tty udev client (requires root if without logind).",
    #[cfg(feature = "x11")]
    "--x11 : Run orchard as an X11 client.",
];

#[cfg(feature = "profile-with-tracy-mem")]
#[global_allocator]
static GLOBAL: profiling::tracy_client::ProfiledAllocator<std::alloc::System> =
    profiling::tracy_client::ProfiledAllocator::new(std::alloc::System, 10);

fn main() {
    use std::io::IsTerminal;
    // the session wrapper redirects stderr into orchard.log: no colour codes there
    let ansi = std::io::stderr().is_terminal();
    if let Ok(env_filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        tracing_subscriber::fmt()
            .compact()
            .with_ansi(ansi)
            .with_env_filter(env_filter)
            .init();
    } else {
        tracing_subscriber::fmt().compact().with_ansi(ansi).init();
    }
    orchard::crash::install_panic_hook();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "orchard starting");

    #[cfg(feature = "profile-with-tracy")]
    profiling::tracy_client::Client::start();

    profiling::register_thread!("Main Thread");

    #[cfg(feature = "profile-with-puffin")]
    let _server = puffin_http::Server::new(&format!("0.0.0.0:{}", puffin_http::DEFAULT_PORT)).unwrap();
    #[cfg(feature = "profile-with-puffin")]
    profiling::puffin::set_scopes_on(true);

    let arg = ::std::env::args().nth(1);
    match arg.as_ref().map(|s| &s[..]) {
        #[cfg(feature = "winit")]
        Some("--winit") | Some("--nested") => {
            tracing::info!("Starting orchard with winit backend");
            orchard::winit::run_winit();
        }
        #[cfg(feature = "headless")]
        Some("--headless") => {
            tracing::info!("Starting orchard headless");
            orchard::headless::run_headless();
        }
        #[cfg(feature = "udev")]
        Some("--tty-udev") | Some("--tty") => {
            tracing::info!("Starting orchard on a tty using udev");
            orchard::udev::run_udev();
        }
        #[cfg(feature = "x11")]
        Some("--x11") => {
            tracing::info!("Starting orchard with x11 backend");
            orchard::x11::run_x11();
        }
        Some(other) => {
            tracing::error!("Unknown backend: {}", other);
        }
        None => {
            #[allow(clippy::disallowed_macros)]
            {
                println!("USAGE: orchard --backend");
                println!();
                println!("Possible backends are:");
                for b in POSSIBLE_BACKENDS {
                    println!("\t{}", b);
                }
            }
        }
    }
}
