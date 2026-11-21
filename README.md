# Orchard

A macOS-style Wayland desktop written in Rust on [Smithay](https://github.com/Smithay/smithay).
Orchard uses no Apple assets. Fonts, icons and the wallpaper come from your own system (Inter, the icon and pointer themes you pick, your own wallpaper).

The compositor is being moved onto a fork of COSMIC's `cosmic-comp`, vendored in `compositor/`.
The existing `orchard` compositor stays the default session until the port reaches parity;
the fork can be built and run nested on its own.

```
orchard/        current compositor (window chrome, animations, Spaces, Mission Control, IPC)
compositor/     cosmic-comp fork, the new compositor base (its own Cargo workspace)
orchard-shell/  menu bar, Dock, Cmd+Tab switcher, Spotlight-like launcher
orchard-lock/   lock screen (ext-session-lock + PAM)
dist/           login session files + installer
```

## Compositor

The new compositor lives in `compositor/` as a fork of
[pop-os/cosmic-comp](https://github.com/pop-os/cosmic-comp). It is a separate Cargo workspace so
its GPL-3.0-only sources and its patched Smithay revision stay independent of the current
compositor. `compositor/FORK.md` records the upstream commit, the build dependencies, the
differences from upstream, and the porting plan for Orchard's chrome, Spaces, IPC and shell.

`dist/comp-nested.sh` runs the fork in a window inside the current Orchard session, the same way
`dist/nested.sh` runs the current compositor. The default login session is unchanged.

## Features
- **Window chrome**:
  - Title bars drawn by the compositor, with traffic lights. The ×/−/+ symbols appear when you hover the buttons.
  - Centered title, rounded corners (via a shader), and soft shadows that change with focus.
  - Invisible resize borders with matching resize cursors.
  - Light and dark mode follow `org.gnome.desktop.interface color-scheme`. Override with `ORCHARD_APPEARANCE=light|dark`.
- **Window behavior**:
  - New windows open centered (slightly above the middle), cascading if needed.
  - Double-clicking the title bar zooms the window.
  - **Green button**: maximize (fills the area between menu bar and Dock); click again to restore. Option-click goes full screen (the window covers its display on the current Space).
  - **Leaving full screen**: push the pointer against the top edge of the screen. The menu bar and a title bar slide in; the green button there (or ⌃⌘F) leaves full screen. Games that lock or confine the pointer don't trigger it.
  - **Yellow button**: genie minimize into the Dock. Close and open use spring animations.
- **Spaces**:
  - Separate Spaces per display, with a slide animation between them. At the first or last Space the current one nudges and springs back.
  - The number of Spaces per display is kept across logins (`~/.local/state/orchard/spaces.json`).
  - Three-finger swipes switch Spaces; swipe up for Mission Control, swipe down for App Exposé.
- **Mission Control / App Exposé**: opens on every display at once. Windows scale into a tidy grid that fills the display; the hovered or selected window lifts under a blue outline and shows its title.
  - The translucent bar on top shows a live miniature of each Space plus "+". Previews are rendered after the first frame (so opening is instant) and only Spaces that changed are redrawn.
  - Hover a Space to grow it; click it to go there, or "+" to add one (Mission Control stays open). Hover a Space and click its × to remove it. Its windows move to the Space on its left (the right one for the first Space).
  - Drag a window onto a Space to move it there, onto "+" to move it to a new Space, or onto another display.
  - Arrow keys select a window, Tab / ⇧Tab or the scroll wheel steps through them, Return or Space opens it, Escape leaves. ⌃⌘←/→ and ⌃⌘1…9 switch Spaces without leaving Mission Control.
  - App Exposé shows just the active app's windows, under its name and window count.
- **Global app menus**: KDE/Qt apps (KWrite, Kate, Dolphin, Konsole …) show their own menus in the menu bar after the app name, with shortcuts, checkmarks and submenus. Their in-window menu bar disappears.
  - Wayland apps announce their menu through `org_kde_kwin_appmenu`. X11 apps register with `com.canonical.AppMenu.Registrar`, which the shell provides.
  - GTK 3/4 apps with a GtkApplication menu bar announce it through `gtk_shell1`; Orchard reads it with `org.gtk.Menus` and runs items with `org.gtk.Actions`. Their in-window menu bar disappears.
  - Electron apps (VS Code/VSCodium, …) running natively on Wayland register their menu with the Registrar for their process; Orchard matches it to the app's window, including Flatpak apps (whose D-Bus traffic comes through the sandbox's proxy). VS Code needs `"window.titleBarStyle": "native"` to use its native menu.
  - Menus are read with `com.canonical.dbusmenu`. Apps without an exported menu keep the Window and Help menus.
- **Menu bar**:
  - Left: leaf menu (Sleep, Restart, Lock and so on), the bold name of the active app, and Window and Help menus.
  - Right: network, battery and the clock in macOS format.
  - Frosted-glass background.
- **Dock**:
  - Pinned apps (`~/.config/orchard/dock.json`), magnification, tooltips, running-app dots.
  - A bounce while an app launches.
  - Minimized windows show a picture of the window with the app's icon as a badge. The genie always ends exactly in that picture, and restoring starts from it.
  - Items grow in and shrink out. Magnification fades out after the pointer leaves.
  - The Trash, plus right-click menus.
- **Launchpad** (F4, the Dock tile, or a hot corner): every app as a page grid over the frosted desktop. Type to search, drag or scroll between pages, use the arrow keys, and press Return to open. Escape or a click on empty space closes it. The Dock stays visible on top.
- **Cmd+Tab** app switcher and a **Spotlight-like launcher**.
- **Notifications**: Orchard is the session's notification server (`org.freedesktop.Notifications`).
  - Banners slide in at the top right below the menu bar and stay while you hover them.
  - Clicking a banner runs its default action and brings the app forward.
  - Action buttons and a close button (on hover) are supported. Critical notifications stay until dismissed.
- **Notification Center** (click the clock):
  - Today's date, a Do Not Disturb switch and every notification that arrived. Missed banners land here.
  - Scrollable, with "Clear All". Click a notification to open it, or hover and click × to remove it.
  - Do Not Disturb silences banners (critical ones still show), shows a moon in the menu bar and is remembered across restarts.
- **Control Center** (the switches icon left of the clock):
  - An iOS 18-style grid with wide side margins: 1×1 controls are single small round buttons with just their symbol, 2×1 controls are pills with a leading icon, title and status, and brightness/volume are tall iOS-style vertical capsules. It includes a Connectivity group of Wi-Fi (`nmcli`) and Bluetooth (`bluetoothctl`) pills (plus Ethernet when wired) with a network detail sheet, Focus, Night Shift, Game Mode, Now Playing (`playerctl`) and display details.
  - Fully customizable: right-click or use the pencil to enter edit mode, drag cards to reorder, hover to resize, tap − to remove, add controls or whole pages from the gallery. The layout is kept in `settings.json` (`control_center`), and every edit saves immediately.
  - Pages slide horizontally (trackpad swipe, wheel or the page dots), long pages scroll, and detail sheets slide back into the grid.
  - Opens with a macOS-style spring from the menu bar icon; modules cascade in with a short stagger. Animation frames are rendered into a reused offscreen buffer and only the panel's rectangle is repainted and damaged, so the rest of the screen is never touched.
  - Click outside or press Esc to close; Esc first leaves the gallery, edit mode or a detail sheet.
- **Lock screen** (`orchard-lock`; ⌃⌘Q, Apple menu → Lock Screen, hot corners, idle lock):
  - macOS style: date and a large clock at the top, your account picture (`~/.face`, else a silhouette), name and the password field at the bottom, over the blurred wallpaper. Sleep, Restart and Shut Down buttons sit below.
  - A wrong password shakes the field. A caps-lock badge shows in the field. Return or the arrow button checks the password through PAM (`/etc/pam.d/orchard-lock`, else swaylock's PAM file).
  - On success the lock fades out and the blurred wallpaper dissolves into the desktop.
  - The compositor never draws windows while locked, and only the lock screen gets keys, even from windows that open behind it. If the locker crashes, the session stays locked and Orchard starts a new one. A second locker is refused.
  - **Instead of a login greeter** (`dist/autologin.sh enable`): the PC boots straight into Orchard, which starts at the lock screen (`orchard-session --locked`). The desktop, the menu bar, the Dock and your autostart apps load behind it while you type, so one password brings up a ready desktop. Logging out shows the greeter again; `dist/autologin.sh disable` turns this off. With automatic login, a keyring that PAM would unlock at login (KWallet, GNOME Keyring) is not unlocked.
- **Night Shift** (System Settings → Displays, or Control Center): warmer display colours on a custom schedule (22:00–07:00 by default), or by hand until the next morning. Switching it off during the schedule holds until the schedule starts again. Fades over about two seconds; a warmth slider picks the colour temperature (5500 K – 2700 K). The compositor multiplies the final image on each display, so it costs one blended quad, needs no gamma ioctls, and never shows up in screenshots or screen sharing. `ORCHARD_NO_NIGHT_SHIFT=1` turns it off.
- **Game Mode** (on by default; switch in Control Center, options under Battery → Game Mode): turns itself on while a game runs full screen on any display, including borderless windows that cover their display. Games are recognised by their desktop file (`Categories=Game`), Steam's launch environment, Wine/Proton, a few known titles (Minecraft, Sober/Roblox) and by locking the pointer; a full-screen video or slideshow doesn't count. While it is on, notification banners wait in Notification Center (critical ones still show; the saved Do Not Disturb switch is untouched), VRR is enabled on the game's display, hot corners and live blur pause, the idle lock waits and the shell polls less. A controller in the menu bar shows that it is on.
- **Shake to locate**: wiggle the mouse quickly and the pointer grows to 3× for a moment (a larger cursor-theme size, not blurry upscaling). Toggle under Accessibility → Pointer.
- **Low-battery warnings**: notifications at 20 %, 10 % and 5 % (critical) for the computer's battery, and at 15 % and 5 % for wireless mice, keyboards and headsets. Peripheral batteries never count as the computer's battery in the menu bar or for battery saver.
- **Volume / brightness HUD**: media keys change the volume (`wpctl`), brightness (`brightnessctl`, or DDC/CI via `ddcutil` for external monitors) and playback (`playerctl`).
  - The macOS-style bezel shows the new level in 16 steps.
  - ⇧⌥ + volume/brightness changes in quarter steps.
- **Window tiling** (macOS Sequoia):
  - Drag a window against the left or right screen edge for half the screen. The top edge fills the screen, and the corners give quarters. A preview shows the target.
  - Dragging a tiled or zoomed window away restores its previous size. A plain click on the title bar no longer un-zooms.
- **Screenshots**:
  - ⌘⇧3 captures the screen, ⌘⇧4 an area. Press Space to pick a window instead (captured with its shadow).
  - Add ⌃ to copy to the clipboard. A shutter sound and a notification with a thumbnail confirm the capture.
- **Screen sharing / recording**: Orchard implements `wlr-screencopy` (v3: shm and dmabuf, damage-tracked, optional cursor). Browsers, Discord and OBS share screens through `xdg-desktop-portal-hyprland` and its picker, and `grim` works.
- **Hide apps**: ⌘H / ⌥⌘H, as on macOS. Hidden windows get no Dock thumbnail, and clicking the app (Dock or ⌘Tab) brings them all back.
- **Wallpaper**: chosen in System Settings → Wallpaper (Orchard only; a soft gradient when none is set). Frosted glass (menu bar, Dock, menus, Launchpad, notifications) is a live GPU blur of whatever is behind it, windows included; it is only recomputed when that content changes. `ORCHARD_NO_LIVE_BLUR=1` falls back to a pre-blurred copy of the wallpaper.
- **Supported protocols**: xdg-shell, xdg-decoration, layer-shell, Xwayland, dmabuf, linux-drm-syncobj (NVIDIA explicit sync), fractional-scale, viewporter, presentation-time, xdg-activation, primary selection, data-control, text-input / input-method, pointer constraints and relative pointer, cursor-shape, idle-notify and idle-inhibit, ext-foreign-toplevel-list, ext-session-lock, security-context, and DRM lease.

## Shortcuts (Super = ⌘)
| Keys | Action |
|---|---|
| ⌘Q / ⌘W / ⌘M / ⌥⌘M | quit app / close window / minimize / restore last minimized |
| ⌃⌘F / ⌥⌘F | full screen / zoom |
| ⌘Tab, ⌘⇧Tab | app switcher |
| ⌘^ (⌘` on US layouts) | cycle windows of the current app |
| ⌘Space | launcher |
| ⌘↑ or F3 / ⌘↓ | Mission Control / App Exposé |
| F4 | Launchpad |
| ⌃⌘← / ⌃⌘→ / ⌃⌘1…9 | previous / next / numbered Space |
| ⌃⌘⇧← / ⌃⌘⇧→ / ⌃⌘⇧1…9 | move window to the previous / next / numbered Space (→ past the last one adds a Space) |
| ⌘⇧3 / ⌘⇧4 | screenshot of the screen / an area (Space: a window) to `~/Bilder` (your XDG Pictures folder); add ⌃ for the clipboard |
| ⌥⌘← / ⌥⌘→ / ⌥⌘↑ / ⌥⌘↓ | tile left / right / top / bottom half |
| ⌥⌘C / ⌥⌘R | center the window / return to its size before tiling or zoom |
| ⌘ + left drag / ⌘ + right drag / ⌘ + middle click | move the window under the pointer (anywhere in it) / resize it from the nearest edge or corner / raise it, or lower it if it is already on top (KDE Plasma's Meta + mouse) |
| ⌘H / ⌥⌘H | hide the app / hide all other apps |
| ⌥⇧⌘⎋ / ⌥⌘⎋ | force quit the frontmost app / open the system monitor |
| ⌥⇧⌘Q | log out |
| volume, brightness, play/pause keys | with an on-screen HUD |
| ⌃⌘Q | lock screen (`orchard-lock`; another locker via Settings → Lock Screen or `ORCHARD_LOCKER`) |
| ⌘Return | terminal (`ORCHARD_TERMINAL`, or first installed terminal) |
| Ctrl+Alt+Backspace | quit Orchard |

Spaces use ⌃⌘ rather than macOS's plain ⌃, so Ctrl+arrow keeps jumping by word in Linux apps.

## Running
```sh
cargo run -- --nested     # in a window inside your current session
cargo run -- --headless   # offscreen, no window (automated tests, see below)
cargo run -- --tty        # real session from a text console (Ctrl+Alt+F3)
dist/install.sh           # release build + "Orchard" entry in your display manager's session menu
dist/autologin.sh enable  # optional: boot straight into Orchard's lock screen, no greeter
```

### Live updates while developing
Install once in dev mode (run as your user, not with `sudo`), then log in again:
```sh
dist/install.sh --dev     # /usr/local/bin/orchard* -> symlinks into target/release
```
Afterwards, from a terminal inside the Orchard session:
```sh
dist/dev.sh                  # rebuild and apply once
dist/dev.sh watch            # live-reload shaders/settings; Rust waits for `go` (foreground)
dist/dev.sh start            # run the auto-reloader in the background (stop / status / logs)
dist/dev.sh install-service  # auto-start the watcher on every Orchard login
dist/dev.sh reload-shaders   # hot-reload GLSL shaders without rebuilding
dist/dev.sh go               # build + apply all Rust changes now (= menu bar ▸ Rebuild ▸ Rebuild Desktop)
dist/dev.sh auto             # opt in to rebuilding automatically 20s after the last edit
dist/dev.sh manual           # back to the default: Rust is only built when you ask
```
- GLSL shaders (`orchard/src/effects/shaders/*.frag`): hot-reloaded in place via `reload_shaders` IPC without rebuilding.
- `~/.config/orchard/settings.json`: auto-reloaded by the compositor when edited externally.
- menu bar + Dock (`orchard-shell`): targeted `-p orchard-shell` rebuild and restarted in place via `restart_shell` IPC; windows stay open. The compositor also restarts the shell by itself if it crashes (gives up after 5 exits in a minute).
- System Settings (`orchard-settings`): targeted rebuild and reopened on the same page if it was open.
- compositor (`orchard`): targeted rebuild; if a nested Orchard window (`dist/nested.sh`) is open it is restarted automatically. The live session compositor can't be replaced under running apps, so the new build is used at the next login with no reinstall needed.

The real-hardware backend links against `libinput` and `libseat`. Install their development packages first:
`sudo dnf install libinput-devel libseat-devel`

Screen sharing uses `xdg-desktop-portal-hyprland` (already installed with Hyprland's portal package) through wlr-screencopy.

## Headless testing
`dist/test-pointer-constraints.sh` checks that pointer locks and confinement (games) never let the cursor leave the window.

`orchard --headless` renders on the GPU without a window. To control it, append lines to `$XDG_RUNTIME_DIR/orchard-ctl`:
`motion X Y`, `rel DX DY` (relative mouse motion, honours pointer locks), `down`, `up`, `key <evdev code> [down|up]`, `action <name>`, `shot <file.png>`, `spawn <cmd>`.

`ORCHARD_START_LOCKED=1` starts the compositor locked (with `orchard-lock` as the first client), as `orchard-session --locked` does. `ORCHARD_LOCK_TRACE=1` makes `orchard-lock` print its startup timings.

`ORCHARD_HEADLESS_OUTPUTS=2` adds a second, never-rendered output to test multi-display behaviour (main display). Headless outputs offer fake 60/120 Hz modes.

Action names: space-1…space-9, move-to-space-1…9, mc-left, mc-right, mc-up, mc-down, mc-pick, close, minimize, unminimize, zoom, fullscreen, cycle, cycle-app, space-left, space-right, move-space-left, move-space-right, mission-control, app-expose, switcher-next, switcher-commit, launcher, screenshot, screenshot-area, screenshot-window, screenshot-cancel, tile-left, tile-right, tile-top, tile-bottom, tile-fill, untile, center, hide, hide-others, force-quit, volume-up, volume-down, volume-mute, brightness-up, brightness-down, play-pause, quit.

`ORCHARD_SCREENSHOT_DIR` overrides where screenshots are saved. `ORCHARD_NO_NOTIFICATIONS=1` keeps the shell from taking `org.freedesktop.Notifications`, for example when testing inside Plasma (or run the test under `dbus-run-session`).

## IPC
Newline-delimited JSON on `$ORCHARD_SOCKET` (`$XDG_RUNTIME_DIR/orchard-<display>.sock`). For example:
`{"request":"windows"}`, `{"request":"subscribe"}`, `{"request":"activate_app","app_id":"org.kde.konsole"}`.

## Session integration
When Orchard runs as a login session (`--tty`), it:
- exports `WAYLAND_DISPLAY`, `DISPLAY`, `XDG_CURRENT_DESKTOP`, `XDG_SESSION_TYPE`, `QT_QPA_PLATFORMTHEME` and `ORCHARD_SOCKET` to systemd and D-Bus activation. It waits for Xwayland first.
- starts `orchard-session.target`, which binds `graphical-session.target`. This lets `xdg-desktop-portal` start, so file dialogs, Flatpak and the dark-mode setting work.
- writes the unit to `~/.local/share/systemd/user/` and `~/.config/xdg-desktop-portal/orchard-portals.conf` (GTK backend for file dialogs/appearance, `gnome-keyring` for secrets, Hyprland portal for ScreenCast/Screenshot). Managed portal configs are updated automatically; delete the first line to keep manual edits.
- gives the session its own dconf database (`DCONF_PROFILE` → `~/.config/orchard/dconf-profile`, database `~/.config/dconf/orchard`, seeded once from your user database) and writes Orchard's icon theme, pointer theme/size and Light/Dark into it, so GTK apps and the settings portal follow Orchard without affecting other sessions;
- uses the GTK Qt platform theme (`QT_QPA_PLATFORMTHEME=gtk3`) unless you set your own, so Qt apps read the same settings without requiring any KDE packages;
- exports `XCURSOR_THEME`/`XCURSOR_SIZE` from Orchard's settings.
- undoes the environment export and stops the target on a clean exit.

If another graphical session of yours is already running (for example, Plasma on another VT), Orchard leaves the systemd environment alone.

Apps started from the Dock or the launcher are spawned by the compositor, so they always get `DISPLAY`, and every child process is reaped.

## Not done yet
- Multi-GPU uses the primary GPU only.
- GTK apps that build a plain GtkMenuBar widget (instead of a GtkApplication menu bar) keep their in-window menus, unless `appmenu-gtk-module` exports them.
- Swipes switch once a threshold is reached; they don't follow your fingers (no trackpad in use, so not planned).
- KDE apps that apply a colour scheme themselves still read it from kdeglobals.

## System Settings
`orchard-settings` (Apple menu → System Settings…, or the Dock) edits `~/.config/orchard/settings.json` and then sends `{"request":"reload_settings"}`. It writes back only the fields you changed there, so changes made meanwhile by the Control Center, the menu bar or a text editor are kept, and it picks up outside changes while open. The compositor applies the change right away (appearance, corner radius, keyboard layout/repeat, mouse/trackpad, scroll speed, hot corners, focus follows mouse, title-bar double-click, Dock size/magnification/position/Trash, idle lock, display scale, wallpaper) and broadcasts `settings_changed`, so the shell restyles the menu bar and Dock. Choosing Light or Dark also sets `org.gnome.desktop.interface color-scheme`, so GTK/Qt apps follow. Autostart entries (`~/.config/autostart`, `/etc/xdg/autostart`, including the polkit agent) run once at login unless turned off under General.

## License
Orchard is licensed under **GPL-3.0-only**; see `LICENSE`.

The compositor fork in `compositor/` is derived from
[pop-os/cosmic-comp](https://github.com/pop-os/cosmic-comp), also GPL-3.0-only. Upstream
copyright notices and SPDX headers are kept in that tree.

The original Orchard compositor began from Smithay's MIT-licensed `anvil` example. That MIT
notice is retained in `orchard/LICENSE-smithay.txt`; MIT is compatible with GPL-3.0, and the
combined work is distributed under GPL-3.0-only.
