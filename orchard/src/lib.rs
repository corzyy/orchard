#![warn(rust_2018_idioms)]
// If no backend is enabled, a large portion of the codebase is unused.
// So silence this useless warning for the CI.
#![cfg_attr(
    not(any(feature = "winit", feature = "x11", feature = "udev")),
    allow(dead_code, unused_imports)
)]

#[cfg(any(feature = "udev", feature = "xwayland"))]
pub mod appmenu;
pub mod crash;
pub mod cursor;
pub mod chrome;
pub mod drawing;
pub mod effects;
pub mod focus;
pub mod fs_reveal;
pub mod game_mode;
pub mod gtk_shell;
#[cfg(feature = "headless")]
pub mod headless;
pub mod input_handler;
pub mod kde_blur;
pub mod mission_control;
pub mod ipc;
pub mod protocols;
pub mod render;
pub mod screencopy;
pub mod screenshot;
pub mod session;
pub mod settings;
pub mod shell;
pub mod spaces;
pub mod stage_manager;
pub mod watchdog;
pub mod tiling;
pub mod state;
pub mod wm;
#[cfg(feature = "udev")]
pub mod udev;
#[cfg(feature = "winit")]
pub mod winit;

/// app_id of a nested Orchard window (dist/nested.sh, org.orchard.nested.desktop).
pub const NESTED_APP_ID: &str = "org.orchard.nested";
#[cfg(feature = "x11")]
pub mod x11;

pub use state::{OrchardState, ClientState};
