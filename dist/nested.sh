#!/bin/bash
# Run the freshly built Orchard in a window inside the current session, to
# test compositor changes without logging out.
#
#   dist/nested.sh              build, then open nested Orchard
#   dist/nested.sh run          open without building (what the app entry does)
#   dist/nested.sh action NAME  run a named action in the nested instance
#                               (tile-left, tile-right, tile-top, tile-fill,
#                               untile, center, zoom, mission-control, ...)
#                               — needed for ⌥⌘ shortcuts, which the outer
#                               Orchard handles before the nested window sees them
#   dist/nested.sh spawn CMD    start an app inside the nested instance
#   dist/nested.sh stop         quit the nested instance (same as closing its window)
#   dist/nested.sh install-app  add "Orchard (nested)" to the app launcher / Dock
#
# Only one nested instance runs at a time. It shares your D-Bus session; its
# shell does not take over the notification / global-menu names your real
# shell already owns. F11 is swallowed inside it, and the outer Orchard refuses
# to make its window full screen (both break the nested session).
set -u
cd "$(dirname "$0")/.."
ROOT=$PWD
BIN=${CARGO_TARGET_DIR:-$ROOT/target}/release
LOG=${XDG_STATE_HOME:-$HOME/.local/state}/orchard/nested.log
STATE=${XDG_RUNTIME_DIR:-/tmp}/orchard-nested.sock-path

# send one JSON request; fails if no nested instance answers
ipc() {
    [ -f "$STATE" ] || return 1
    python3 -I -c '
import socket, sys
try:
    s = socket.socket(socket.AF_UNIX); s.settimeout(3); s.connect(sys.argv[1])
    s.sendall((sys.argv[2] + "\n").encode()); print(s.makefile().readline().strip())
except OSError:
    sys.exit(1)
' "$(cat "$STATE")" "$1"
}
need_running() { ipc "$1" || { echo "no nested Orchard running" >&2; exit 1; }; }
json() { python3 -I -c 'import json,sys; print(json.dumps(sys.argv[1]))' "$1"; }

open_nested() {
    # one instance: the lock is held for as long as the nested window is open
    # (not an IPC ping: a nested compositor can be slow to answer while it starts)
    exec 9>"${XDG_RUNTIME_DIR:-/tmp}/orchard-nested.lock"
    if ! flock -n 9; then
        echo "nested Orchard is already open"
        exit 0
    fi
    if [ -z "${WAYLAND_DISPLAY:-}" ]; then
        echo "WAYLAND_DISPLAY is not set: run this from inside your Orchard session" >&2
        exit 1
    fi
    mkdir -p "$(dirname "$LOG")"
    local sock="" pid
    # ORCHARD_SOCKET from the outer session must not leak into the nested shell;
    # the compositor must not inherit the lock fd (it would outlive this script)
    env -u ORCHARD_SOCKET ORCHARD_NO_AUTOSTART=1 ORCHARD_SHELL="$BIN/orchard-shell" \
        "$BIN/orchard" --winit >"$LOG" 2>&1 </dev/null 9>&- &
    pid=$!
    for _ in $(seq 100); do
        sleep 0.1
        kill -0 "$pid" 2>/dev/null || break
        sock=$(sed -n 's/.*IPC socket: \(\S*\).*/\1/p' "$LOG" | head -1)
        [ -n "$sock" ] && break
    done
    if [ -z "$sock" ]; then
        echo "nested Orchard did not come up; see $LOG" >&2
        tail -5 "$LOG" >&2
        kill "$pid" 2>/dev/null
        exit 1
    fi
    echo "$sock" >"$STATE"
    echo "nested Orchard open (IPC $sock, log $LOG); close its window to quit"
    # stay in the foreground until the window is closed, then clean up
    wait "$pid"
    rm -f "$STATE" "$sock"
}

install_app() {
    local dir=${XDG_DATA_HOME:-$HOME/.local/share}/applications
    mkdir -p "$dir"
    cat >"$dir/org.orchard.nested.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Orchard (nested)
Name[de]=Orchard (verschachtelt)
GenericName=Orchard test session
Comment=Run the current Orchard build in a window, without logging out
Exec=$ROOT/dist/nested.sh run
Icon=preferences-system-windows
Categories=Development;
Keywords=orchard;compositor;nested;debug;test;
OnlyShowIn=Orchard;
StartupWMClass=org.orchard.nested
Actions=rebuild;

[Desktop Action rebuild]
Name=Rebuild and Open
Name[de]=Neu bauen und öffnen
Exec=$ROOT/dist/nested.sh
EOF
    echo "installed $dir/org.orchard.nested.desktop"
}

case "${1:-build}" in
    action) need_running "{\"request\": \"action\", \"name\": $(json "${2:?action name}")}" ;;
    spawn) shift; need_running "{\"request\": \"spawn\", \"command\": $(json "$*")}" ;;
    stop) need_running '{"request": "action", "name": "quit"}' ;;
    install-app) install_app ;;
    build) cargo build --release -p orchard -p orchard-shell || exit 1; open_nested ;;
    run) open_nested ;;
    *) sed -n 2,20p "$0"; exit 1 ;;
esac
