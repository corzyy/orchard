# Orchard compositor fork

This directory is Orchard's fork of [pop-os/cosmic-comp](https://github.com/pop-os/cosmic-comp),
the COSMIC desktop compositor. It is the base for Orchard's next compositor. The existing
`orchard/` compositor remains the default session until the port reaches parity.

## Provenance

| | |
|---|---|
| Upstream | <https://github.com/pop-os/cosmic-comp> |
| Imported commit | `0681eaf68324d2afd8cac175d1e10a22b7d34cdf` |
| Commit date | 2026-11-10 (`feat: configurable cursor size`) |
| Upstream version | `cosmic-comp 1.0.0` |
| Imported on | 2026-11-21 |
| License | GPL-3.0-only (`LICENSE`, SPDX headers in every source file) |

The tree was imported as a plain copy of the upstream commit, without upstream git history.
This keeps Orchard's history linear. The exact commit above is the base for rebases; compare
against a fresh upstream checkout when updating.

Upstream uses a patched Smithay (`smithay` git rev `8a27023`), `libcosmic`,
`cosmic-protocols`, `cosmic-settings-daemon` and `id-tree` from git. Those pins live in
`Cargo.toml`/`Cargo.lock` and are intentionally independent of the root Orchard workspace.

## Why a separate workspace

`compositor/` has its own `[workspace]` with `cosmic-comp-config` and `cosmic-comp-ipc`.
It is **not** a member of the root workspace:

- cosmic-comp uses `[patch.crates-io]` to replace Smithay with a git revision. Workspace-wide
  patches would silently change the Smithay used by the existing `orchard` compositor.
- cosmic-comp is edition 2024 and pins Rust 1.95 (`rust-toolchain.toml`).
- Keeping the fork separate means `cargo check` at the root still only builds Orchard, and the
  fork can be updated or replaced without touching Orchard's dependency graph.

Build the fork with an explicit manifest:

```sh
cargo check --manifest-path compositor/Cargo.toml
```

## Build dependencies

Rust 1.95 or newer (edition 2024). On Fedora:

```sh
sudo dnf install libdisplay-info-devel libinput-devel libseat-devel systemd-devel \
    wayland-devel mesa-libgbm-devel libxkbcommon-devel mesa-libEGL-devel \
    mesa-libGLES-devel
```

`libdisplay-info-devel` is the one Orchard does not already need. Without it the
`libdisplay-info` build script fails with a pkg-config error.

The upstream Nix packaging is kept for reference; Orchard does not use it. The upstream
`debian/` directory was excluded from the import because Orchard packages through `dist/`.

## Running it today

The fork runs nested inside the current session. `dist/comp-nested.sh` builds it and forces the
winit backend so it always opens in a window, even when `DISPLAY` is set by Xwayland:

```sh
dist/comp-nested.sh
```

Equivalent manual command:

```sh
cd compositor
COSMIC_BACKEND=winit cargo run
```

It creates its own Wayland socket and runs a bare COSMIC compositor: no Orchard menu bar, Dock,
window chrome, Spaces, Mission Control or IPC yet. Use it to test backend, input, output and
protocol work while the port is in progress.

To run on real hardware from a text console, unset `DISPLAY`/`WAYLAND_DISPLAY` and run
`cargo run --release` from the compositor directory. Do not install it over the running Orchard
session during development.

## Divergence from upstream

At import time: none in the source. The only additions are this file and Orchard's top-level
license change. Keep that property where possible; put Orchard-specific code in clearly named
modules so upstream rebases stay reviewable.

Planned Orchard-only areas:

- `src/orchard/` — IPC server, settings adapter, ported window-chrome and Mission Control code.
- `data/orchard-*` — session unit and desktop entry for the Orchard session.
- `orchard-bridge` — optional crate for Orchard's JSON IPC types, shared with `orchard-shell`.

## What cosmic-comp gives us

- Mature KMS/DRM, GBM/EGL and libinput backends, multi-GPU handling, VRR/adaptive sync,
  fractional scaling, output configuration, explicit sync and DRM leasing.
- Tiling and floating layouts, workspaces per output, overview/zoom, window stacks and tabs,
  focus order, move/resize grabs, Xwayland.
- A broad protocol surface: layer-shell, foreign-toplevel, session-lock, output management,
  toplevel info/management, corner radius, image capture, idle notify/inhibit, input methods,
  screencopy and more.
- A settings daemon integration (`cosmic-comp-config`), systemd session support and hooks.

## What has to come from Orchard

Orchard's shell is a separate process that talks to the compositor over the Orchard JSON IPC
(`$ORCHARD_SOCKET`) and relies on compositor-drawn macOS chrome. cosmic-comp has neither. The
port is therefore mainly a state and presentation bridge, not a backend rewrite.

| Orchard piece | cosmic-comp counterpart | Porting work |
|---|---|---|
| `orchard/src/state.rs`, `udev.rs`, `winit.rs`, `x11.rs`, `input_handler.rs`, `cursor.rs` | `src/state.rs`, `src/backend/*`, `src/input/*` | Reuse cosmic-comp. Retire Orchard's backend after parity. |
| `orchard/src/spaces.rs` | `src/shell/workspace.rs` | Map Orchard's per-output Spaces (including `spaces.json`) onto cosmic workspaces; keep Orchard's user-facing numbering and transitions. |
| `orchard/src/wm.rs`, `shell/grabs.rs`, `focus.rs` | `src/shell/focus/*`, `src/shell/grabs/*`, `src/shell/layout/*` | Port Orchard semantics (zoom, genie minimize, hide, click-through title bar, app cycle) onto cosmic's window model. |
| `orchard/src/chrome/*`, `shell/ssd.rs`, `shell/xdg.rs`, `shell/x11.rs` | `src/shell/element/window.rs`, `src/theme.rs` | Replace cosmic server-side decorations with Orchard's title bars, traffic lights, rounded corners and shadows. Port the GLSL/tiny-skia effects to cosmic's glow renderer. |
| `orchard/src/mission_control.rs`, `effects/*` | `src/shell/zoom.rs`, `src/utils/tween.rs` | Orchard's Mission Control is a distinct mode with per-Space previews and drag-to-Space. Add it as an Orchard mode; do not try to bend cosmic's overview into it. |
| `orchard/src/tiling.rs` | `src/shell/layout/tiling/*` | Keep Orchard's macOS edge tiling as an input/gesture layer on top of cosmic's layouts, or translate it into cosmic tile actions. |
| `orchard/src/ipc.rs` | none (cosmic IPC is only session env) | Implement the Orchard JSON IPC server inside cosmic state. This is the bridge that lets `orchard-shell`, Settings and the locker stay unchanged. Start here. |
| `orchard/src/appmenu.rs`, `gtk_shell.rs`, `kde_blur.rs` | partial protocol support | Port the modules and their D-Bus/protocol plumbing. |
| shell-side notifications, Control Center, menu bar | shell-owned | Only the IPC events and blur/backdrop regions need compositor support. |
| `orchard/src/screenshot.rs`, `screencopy.rs` | `src/utils/screenshot.rs`, `wayland/protocols/image_capture_source.rs` | Wire Orchard screenshot requests to cosmic capture; keep `wlr-screencopy`. |
| `orchard/src/effects/night.rs`, `game_mode.rs` | render hooks / session state | Port as independent modules; they only need the render pass and fullscreen detection. |
| `orchard-lock` | session-lock protocols, `session_lock_layer` | Verify `ext-session-lock` coverage; port Orchard's lock supervision/fade. |
| `~/.config/orchard/settings.json` | `cosmic-comp-config` (RON) | Keep Orchard's JSON as the source of truth during migration and map it onto cosmic config values; avoid migrating user settings twice. |
| `orchard-session`, `dist/*` | `cosmic-service`, systemd units | Add an Orchard session that launches the fork, exports `ORCHARD_SOCKET` and keeps Orchard's portal/environment setup. |

## Migration phases

0. **Import and build** (this change). Fork vendored, GPL-3.0 relicensed, build verified.
1. **Nested bring-up.** Run the fork nested; verify input, output hotplug, scaling, Xwayland,
   layer-shell clients, screencopy.
2. **IPC bridge.** Add `$ORCHARD_SOCKET`, the `Request`/`Event` types from `orchard/src/ipc.rs`,
   and enough state mapping to answer `windows`, `outputs`, `subscribe`, `action`, `spawn`,
   `reload_settings` and `set_dock_tiles`. Get `orchard-shell` to connect and render its menu
   bar and Dock over the fork in a degraded mode.
3. **Window chrome and window model.** Port title bars, traffic lights, focus rings, shadows,
   zoom, minimize/genie, hide and app cycling. This is the largest visual-parity step.
4. **Spaces, Mission Control, tiling.** Port Orchard's transitions, previews, drag-to-Space and
   edge tiling.
5. **Feature modules.** Global menus, screenshot tool, Night Shift, Game Mode, lock screen,
   notifications/backdrop, output and Settings integration.
6. **Cut over.** Add the fork as the default `orchard-session` compositor, keep the old
   `orchard` binary behind a fallback for one release, and run the headless/feature tests
   against both.

`orchard/` must not be deleted before phase 6. The two compositors can coexist: the fork has
its own workspace and binary name, and the current session keeps launching `orchard`.
