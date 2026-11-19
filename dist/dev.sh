#!/bin/bash
# Live updates for a running Orchard session: rebuild, then swap in what
# changed without logging out.
#
#   dist/dev.sh          build once and apply
#   dist/dev.sh watch    rebuild + apply whenever a source file changes
#
#   menu bar + Dock (orchard-shell)  restarted in place, windows stay open
#   System Settings                  reopened on the same page if it was open
#   compositor (orchard)             can't be swapped under running apps;
#                                    with `dist/install.sh --dev` the new build
#                                    is used at the next login, no reinstall
set -u
cd "$(dirname "$0")/.."
ROOT=$PWD
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
BIN=$TARGET/release

if [ "$(id -u)" = 0 ]; then
    echo "run dist/dev.sh as your user, not root" >&2
    exit 1
fi

say() { printf '\033[1;32m[dev]\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[dev]\033[0m %s\n' "$*"; }

# one JSON request to the compositor; prints the "event" of the reply
ipc() {
    python3 -I -c '
import json, os, socket, sys
path = os.environ.get("ORCHARD_SOCKET") or os.path.join(
    os.environ.get("XDG_RUNTIME_DIR", "/tmp"),
    "orchard-%s.sock" % os.environ.get("WAYLAND_DISPLAY", "wayland-0"))
try:
    s = socket.socket(socket.AF_UNIX)
    s.settimeout(2)
    s.connect(path)
    s.sendall((json.dumps({"request": sys.argv[1]}) + "\n").encode())
    print(json.loads(s.makefile().readline()).get("event", "?"))
except Exception:
    print("unreachable")
' "$1"
}

mtime() { stat -c %Y "$BIN/$1" 2>/dev/null || echo 0; }

restart_shell() {
    case "$(ipc restart_shell)" in
        ok) say "menu bar + Dock restarted" ;;
        unreachable) warn "no Orchard session found (is WAYLAND_DISPLAY set?); shell not restarted" ;;
        *)
            # compositor from before `restart_shell` existed: do it by hand
            pkill -x orchard-shell
            sleep 0.3
            setsid -f "$BIN/orchard-shell" >/dev/null 2>&1 </dev/null
            say "menu bar + Dock restarted (by hand; this compositor predates restart_shell)"
            ;;
    esac
}

restart_settings() {
    local pid page=""
    pid=$(pgrep -x orchard-settings | head -1)
    [ -z "$pid" ] && return
    # keep the page it was opened on (first argument), if any
    page=$(tr '\0' '\n' < "/proc/$pid/cmdline" 2>/dev/null | sed -n 2p)
    pkill -x orchard-settings
    sleep 0.3
    setsid -f "$BIN/orchard-settings" ${page:+"$page"} >/dev/null 2>&1 </dev/null
    say "System Settings reopened"
}

compositor_note() {
    local exe
    exe=$(readlink -f /usr/local/bin/orchard 2>/dev/null)
    if [ "$exe" = "$BIN/orchard" ]; then
        warn "compositor rebuilt: log out and back in to use it (no reinstall needed)"
    else
        warn "compositor rebuilt, but /usr/local/bin/orchard is a copy;"
        warn "run 'dist/install.sh --dev' once so new builds are used at login"
    fi
}

build_and_apply() {
    local c0 s0 g0
    c0=$(mtime orchard); s0=$(mtime orchard-shell); g0=$(mtime orchard-settings)
    say "building..."
    local log
    log=$(mktemp)
    if ! cargo build --release --color never >"$log" 2>&1; then
        grep -E -A12 '^error' "$log" | head -80
        rm -f "$log"
        warn "build failed; nothing changed on the desktop"
        return 1
    fi
    grep -E '^\s*Finished' "$log"
    rm -f "$log"
    local changed=0
    [ "$(mtime orchard-shell)" != "$s0" ] && { restart_shell; changed=1; }
    [ "$(mtime orchard-settings)" != "$g0" ] && { restart_settings; changed=1; }
    [ "$(mtime orchard)" != "$c0" ] && { compositor_note; changed=1; }
    [ $changed = 0 ] && say "up to date"
    return 0
}

if [ -n "$(find "$TARGET" ! -user "$(id -u)" -print -quit 2>/dev/null)" ]; then
    warn "$TARGET has root-owned files; fix once with:"
    warn "  sudo chown -R $(id -u):$(id -g) $TARGET"
    exit 1
fi

case "${1:-}" in
    "")
        build_and_apply
        ;;
    watch)
        stamp=$(mktemp)
        trap 'rm -f "$stamp"' EXIT
        build_and_apply
        touch "$stamp"
        say "watching for changes (Ctrl+C to stop)"
        while sleep 1; do
            if [ -n "$(find orchard orchard-shell orchard-settings orchard-config \
                    \( -name '*.rs' -o -name Cargo.toml -o -name '*.glsl' -o -name '*.frag' -o -name '*.vert' \) \
                    -newer "$stamp" -print -quit 2>/dev/null)" ]; then
                sleep 0.5 # let the editor finish writing
                touch "$stamp"
                build_and_apply
            fi
        done
        ;;
    *)
        sed -n '2,13p' "$0"
        exit 2
        ;;
esac
