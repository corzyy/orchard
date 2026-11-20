#!/bin/bash
# Live updates for a running Orchard session: automatically rebuild and swap in
# what changed without logging out.
#
#   dist/dev.sh                  build once and apply
#   dist/dev.sh watch            watch for changes in the foreground
#   dist/dev.sh start            start the watcher in the background
#   dist/dev.sh stop             stop the background watcher
#   dist/dev.sh status           show watcher status and recent log
#   dist/dev.sh logs             follow the background watcher log
#   dist/dev.sh reload-shaders   hot-reload GLSL shaders immediately (no build)
#   dist/dev.sh install-service  auto-start the watcher on every Orchard login
#
#   Batching (so the desktop isn't rebuilt on every save):
#   dist/dev.sh hold             collect changes, don't build (shaders/settings stay live)
#   dist/dev.sh go               build + apply everything collected now, keep holding
#   dist/dev.sh auto             back to auto mode (and apply anything pending)
#   dist/dev.sh pending          show what is waiting to be built
#
#   In auto mode a build starts only after ORCHARD_DEV_QUIET seconds without edits
#   (default 20). Builds run at idle CPU/IO priority on ORCHARD_DEV_JOBS cores
#   (default: all but 2), so the desktop stays responsive while compiling.
#
#   GLSL shaders (*.frag/*.vert)     hot-reloaded in place (~20ms, no build)
#   settings.json                    hot-reloaded in place via IPC
#   menu bar + Dock (orchard-shell)  rebuilt (-p) & restarted in place, windows stay open
#   System Settings                  rebuilt (-p) & reopened on the same page if open
#   compositor (orchard)             rebuilt (-p); auto-restarts nested Orchard if open;
#                                    with `dist/install.sh --dev` the new build is used
#                                    at the next login, no reinstall
set -u
cd "$(dirname "$0")/.."
ROOT=$PWD
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
BIN=$TARGET/release
SHADERS_DIR=$ROOT/orchard/src/effects/shaders
RUNTIME=${XDG_RUNTIME_DIR:-/tmp}
STATE_DIR=${XDG_STATE_HOME:-$HOME/.local/state}/orchard
LOG_FILE=$STATE_DIR/dev-watch.log
PID_FILE=$RUNTIME/orchard-dev-watch.pid
LOCK_FILE=$RUNTIME/orchard-dev-watch.lock
NESTED_STATE=$RUNTIME/orchard-nested.sock-path
MODE_FILE=$STATE_DIR/dev-mode          # "hold" or absent (auto)
PENDING_FILE=$STATE_DIR/dev-pending    # categories collected while holding
BUILD_LOCK=$RUNTIME/orchard-dev-build.lock
QUIET=${ORCHARD_DEV_QUIET:-20}
NCPU=$(nproc 2>/dev/null || echo 4)
JOBS=${ORCHARD_DEV_JOBS:-$(( NCPU > 3 ? NCPU - 2 : 1 ))}

if [ "$(id -u)" = 0 ]; then
    echo "run dist/dev.sh as your user, not root" >&2
    exit 1
fi

say()  { printf '\033[1;32m[dev %s]\033[0m %s\n' "$(date +%H:%M:%S)" "$*"; }
warn() { printf '\033[1;33m[dev %s]\033[0m %s\n' "$(date +%H:%M:%S)" "$*"; }

notify() {
    local summary=$1 body=${2:-} urgency=${3:-low}
    if command -v notify-send >/dev/null 2>&1; then
        notify-send -a "Orchard Dev" -u "$urgency" -t 2500 \
            -h string:x-canonical-private-synchronous:orchard-dev \
            "$summary" "$body" >/dev/null 2>&1 || true
    fi
}

# Send one JSON payload to the live compositor (and optionally nested compositor).
# Prints the "event" of the live compositor's reply.
ipc_json() {
    python3 -I -c '
import glob, json, os, socket, sys

payload = sys.argv[1]
target = sys.argv[2] if len(sys.argv) > 2 else "live"
runtime = os.environ.get("XDG_RUNTIME_DIR", "/tmp")
nested_state = os.path.join(runtime, "orchard-nested.sock-path")
nested_sock = None
if os.path.isfile(nested_state):
    try:
        nested_sock = open(nested_state).read().strip() or None
    except OSError:
        pass

def find_live():
    p = os.environ.get("ORCHARD_SOCKET")
    if p and os.path.exists(p):
        return p
    wd = os.environ.get("WAYLAND_DISPLAY")
    if wd:
        cand = os.path.join(runtime, f"orchard-{wd}.sock")
        if os.path.exists(cand):
            return cand
    for cand in sorted(glob.glob(os.path.join(runtime, "orchard-*.sock"))):
        if cand != nested_sock:
            return cand
    return None

def send(path):
    if not path or not os.path.exists(path):
        return "unreachable"
    try:
        s = socket.socket(socket.AF_UNIX)
        s.settimeout(2)
        s.connect(path)
        s.sendall((payload + "\n").encode())
        line = s.makefile().readline()
        return json.loads(line).get("event", "?") if line else "empty"
    except Exception:
        return "unreachable"

if target == "nested":
    print(send(nested_sock))
elif target == "both":
    res = send(find_live())
    if nested_sock:
        send(nested_sock)
    print(res)
else:
    print(send(find_live()))
' "$1" "${2:-live}"
}

ipc() {
    ipc_json "{\"request\":\"$1\"}" "${2:-live}"
}

mtime() { stat -c %Y "$BIN/$1" 2>/dev/null || echo 0; }

reload_shaders() {
    local payload
    payload=$(python3 -I -c 'import json,sys; print(json.dumps({"request":"reload_shaders","dir":sys.argv[1]}))' "$SHADERS_DIR")
    case "$(ipc_json "$payload" both)" in
        ok)
            say "GLSL shaders reloaded in place"
            notify "Shaders reloaded" "GLSL updated live"
            ;;
        error)
            warn "running compositor predates reload_shaders (available after next login, or test in nested)"
            ;;
        unreachable)
            warn "no Orchard session found; shaders not reloaded"
            ;;
        *)
            warn "shader reload returned unexpected reply"
            ;;
    esac
}

reload_user_settings() {
    case "$(ipc reload_settings both)" in
        ok) say "settings.json reloaded" ;;
        *)  ;;
    esac
}

restart_shell() {
    case "$(ipc restart_shell live)" in
        ok)
            say "menu bar + Dock restarted"
            notify "Orchard Shell reloaded" "Menu bar & Dock updated"
            ;;
        unreachable)
            warn "no Orchard session found; shell not restarted"
            ;;
        *)
            pkill -x orchard-shell 2>/dev/null || true
            sleep 0.3
            setsid -f "$BIN/orchard-shell" >/dev/null 2>&1 </dev/null
            say "menu bar + Dock restarted (by hand)"
            notify "Orchard Shell reloaded" "Menu bar & Dock updated"
            ;;
    esac
    # Also refresh nested shell if a nested compositor is open
    if [ -f "$NESTED_STATE" ]; then
        ipc restart_shell nested >/dev/null 2>&1 || true
    fi
}

restart_settings() {
    local pid page=""
    pid=$(pgrep -f '(^|/)orchard-settings( |$)' | head -1)
    [ -z "$pid" ] && return
    page=$(tr '\0' '\n' < "/proc/$pid/cmdline" 2>/dev/null | sed -n 2p)
    pkill -f '(^|/)orchard-settings( |$)' 2>/dev/null || true
    sleep 0.3
    setsid -f "$BIN/orchard-settings" ${page:+"$page"} >/dev/null 2>&1 </dev/null
    say "System Settings reopened"
    notify "System Settings reloaded"
}

restart_nested_if_running() {
    [ -f "$NESTED_STATE" ] || return 1
    if [ "$(ipc_json '{"request":"action","name":"quit"}' nested)" = "ok" ]; then
        say "restarting nested Orchard with new compositor..."
        for _ in $(seq 30); do
            [ ! -f "$NESTED_STATE" ] && break
            sleep 0.1
        done
        setsid -f "$ROOT/dist/nested.sh" run >/dev/null 2>&1 </dev/null
        notify "Nested Orchard restarted" "Compositor changes live in nested window"
        return 0
    fi
    return 1
}

compositor_note() {
    if restart_nested_if_running; then
        say "compositor rebuilt (nested Orchard restarted; live session uses it at next login)"
        return
    fi
    local exe
    exe=$(readlink -f /usr/local/bin/orchard 2>/dev/null)
    if [ "$exe" = "$BIN/orchard" ]; then
        warn "compositor rebuilt: test with 'dist/nested.sh run' or relog to use in main session"
        notify "Compositor rebuilt" "Run 'dist/nested.sh run' to preview, or relog"
    else
        warn "compositor rebuilt, but /usr/local/bin/orchard is a copy;"
        warn "run 'dist/install.sh --dev' once so new builds are used at login"
    fi
}

# Build specified cargo packages (or full workspace if empty) and apply changes.
build_and_apply() {
    local pkgs=("$@")
    local c0 s0 g0
    c0=$(mtime orchard); s0=$(mtime orchard-shell); g0=$(mtime orchard-settings)

    local cargo_args=(build --release --color never -j "$JOBS")
    if [ ${#pkgs[@]} -gt 0 ]; then
        for p in "${pkgs[@]}"; do
            cargo_args+=(-p "$p")
        done
        say "building (${pkgs[*]})..."
    else
        say "building workspace..."
    fi

    local log
    log=$(mktemp)
    # Idle priority: the compositor, shell and your apps always win over rustc.
    if ! nice -n 19 ionice -c 3 cargo "${cargo_args[@]}" >"$log" 2>&1; then
        grep -E -A10 '^error' "$log" | head -60
        rm -f "$log"
        warn "build failed; nothing changed on the desktop"
        notify "Orchard build failed" "Check terminal or 'dist/dev.sh logs'" normal
        return 1
    fi
    grep -E '^\s*Finished' "$log" || true
    rm -f "$log"

    local changed=0
    [ "$(mtime orchard-shell)" != "$s0" ] && { restart_shell; changed=1; }
    [ "$(mtime orchard-settings)" != "$g0" ] && { restart_settings; changed=1; }
    [ "$(mtime orchard)" != "$c0" ] && { compositor_note; changed=1; }
    [ $changed = 0 ] && say "up to date"
    return 0
}

apply_categories() {
    # One build at a time, whether started by the watcher or by `go`.
    exec 8>"$BUILD_LOCK"
    flock 8
    _apply_categories "$@"
    local rc=$?
    flock -u 8
    return $rc
}

_apply_categories() {
    local cats=" $* "
    local pkgs=()

    if [[ "$cats" == *" user_settings "* ]]; then
        reload_user_settings
    fi
    if [[ "$cats" == *" shaders "* ]] && [[ "$cats" != *" compositor "* ]]; then
        reload_shaders
    fi
    if [[ "$cats" == *" config "* ]]; then
        pkgs=(orchard-shell orchard-settings orchard)
    else
        [[ "$cats" == *" shell "* ]]      && pkgs+=(orchard-shell)
        [[ "$cats" == *" settings "* ]]   && pkgs+=(orchard-settings)
        [[ "$cats" == *" compositor "* ]] && pkgs+=(orchard)
    fi

    if [ ${#pkgs[@]} -gt 0 ]; then
        build_and_apply "${pkgs[@]}"
        # If shaders changed alongside Rust code, reload them too
        if [[ "$cats" == *" shaders "* ]]; then
            reload_shaders
        fi
    fi
}

watch_loop() {
    say "watching for changes (Ctrl+C to stop)"
    say "  shaders (*.frag/*.vert) -> instant live reload"
    say "  settings.json           -> instant live reload"
    say "  orchard-shell           -> fast rebuild + live restart"
    say "  orchard-settings        -> fast rebuild + live reopen"
    say "  orchard (compositor)    -> fast rebuild + nested auto-restart"
    say "  Rust builds wait for ${QUIET}s of no edits; 'dist/dev.sh hold' to batch manually"

    # Python watcher emits space-separated categories whenever files settle.
    python3 -u -I -c '
import os, sys, time

root = sys.argv[1]
quiet = float(sys.argv[2])
mode_file = sys.argv[3]
pending_file = sys.argv[4]
INSTANT = {"shaders", "user_settings"}

def holding():
    try:
        return open(mode_file).read().strip() == "hold"
    except OSError:
        return False

def stash(cats):
    try:
        old = set(open(pending_file).read().split())
    except OSError:
        old = set()
    os.makedirs(os.path.dirname(pending_file), exist_ok=True)
    with open(pending_file, "w") as f:
        f.write(" ".join(sorted(old | cats)) + "\n")
settings_file = os.path.join(
    os.environ.get("XDG_CONFIG_HOME") or os.path.join(os.path.expanduser("~"), ".config"),
    "orchard",
    "settings.json",
)
watch_roots = ["orchard", "orchard-shell", "orchard-settings", "orchard-config"]
exts = (".rs", ".frag", ".vert", ".glsl")

def classify(path):
    if path == settings_file:
        return "user_settings"
    rel = os.path.relpath(path, root)
    if rel.endswith((".frag", ".vert", ".glsl")):
        return "shaders"
    top = rel.split(os.sep, 1)[0]
    if top == "orchard-shell":
        return "shell"
    if top == "orchard-settings":
        return "settings"
    if top == "orchard-config":
        return "config"
    if top == "orchard":
        return "compositor"
    return None

def scan():
    snap = {}
    for d in watch_roots:
        base = os.path.join(root, d)
        if not os.path.isdir(base):
            continue
        for dirpath, dirnames, filenames in os.walk(base):
            dirnames[:] = [x for x in dirnames if x != "target" and not x.startswith(".")]
            for fn in filenames:
                if fn == "Cargo.toml" or fn.endswith(exts):
                    p = os.path.join(dirpath, fn)
                    try:
                        st = os.stat(p)
                        snap[p] = (st.st_mtime_ns, st.st_size)
                    except OSError:
                        pass
    try:
        st = os.stat(settings_file)
        snap[settings_file] = (st.st_mtime_ns, st.st_size)
    except OSError:
        pass
    return snap

prev = scan()
pending = set()
last_change = 0.0

while True:
    time.sleep(0.25)
    cur = scan()
    if cur != prev:
        changed = set(cur.keys()) ^ set(prev.keys())
        for k in set(cur.keys()) & set(prev.keys()):
            if cur[k] != prev[k]:
                changed.add(k)
        for p in changed:
            cat = classify(p)
            if cat:
                pending.add(cat)
        prev = cur
        last_change = time.monotonic()
        continue
    if not pending:
        continue
    idle = time.monotonic() - last_change
    # Shaders and settings.json are cheap: apply them right away, always.
    instant = pending & INSTANT
    if instant and idle >= 0.25:
        print(" ".join(sorted(instant)), flush=True)
        pending -= instant
    if pending and holding() and idle >= 0.25:
        stash(pending)
        pending.clear()
    elif pending and idle >= quiet:
        print(" ".join(sorted(pending)), flush=True)
        pending.clear()
' "$ROOT" "$QUIET" "$MODE_FILE" "$PENDING_FILE" | while read -r cats; do
        # Coalesce anything that queued up while the previous build ran.
        while read -r -t 0.2 more; do cats="$cats $more"; done
        [ -n "$cats" ] && apply_categories $cats
    done
}

start_daemon() {
    mkdir -p "$STATE_DIR"
    if [ -f "$PID_FILE" ]; then
        local old
        old=$(cat "$PID_FILE" 2>/dev/null || true)
        if [ -n "$old" ] && kill -0 "$old" 2>/dev/null; then
            say "watcher already running (pid $old); log: $LOG_FILE"
            return 0
        fi
    fi
    setsid -f "$ROOT/dist/dev.sh" _daemon >>"$LOG_FILE" 2>&1 </dev/null
    sleep 0.3
    if [ -f "$PID_FILE" ] && kill -0 "$(cat "$PID_FILE" 2>/dev/null)" 2>/dev/null; then
        say "background watcher started (pid $(cat "$PID_FILE"))"
        say "logs: $LOG_FILE  (stop with 'dist/dev.sh stop')"
    else
        warn "failed to start background watcher; see $LOG_FILE"
        return 1
    fi
}

stop_daemon() {
    if [ ! -f "$PID_FILE" ]; then
        say "watcher is not running"
        return 0
    fi
    local pid
    pid=$(cat "$PID_FILE" 2>/dev/null || true)
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
        # Kill the whole process group started by setsid
        kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
        rm -f "$PID_FILE"
        say "watcher stopped (pid $pid)"
    else
        rm -f "$PID_FILE"
        say "watcher was not running (cleaned up stale pid file)"
    fi
}

status_daemon() {
    if [ -f "$PID_FILE" ]; then
        local pid
        pid=$(cat "$PID_FILE" 2>/dev/null || true)
        if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
            say "watcher running (pid $pid)"
            [ -f "$LOG_FILE" ] && tail -n 10 "$LOG_FILE"
            return 0
        fi
    fi
    say "watcher is not running (start with 'dist/dev.sh start')"
    return 1
}

flush_pending() {
    local cats=""
    [ -f "$PENDING_FILE" ] && cats=$(cat "$PENDING_FILE")
    rm -f "$PENDING_FILE"
    if [ -z "${cats// /}" ]; then
        say "nothing pending (the watcher may still be waiting ${QUIET}s for edits to settle)"
        return 0
    fi
    say "applying batch: $cats"
    # shellcheck disable=SC2086
    apply_categories $cats
}

install_service() {
    local unit_dir=${XDG_DATA_HOME:-$HOME/.local/share}/systemd/user
    mkdir -p "$unit_dir"
    cat >"$unit_dir/orchard-dev-watch.service" <<EOF
[Unit]
Description=Orchard live development auto-reloader
PartOf=orchard-session.target
After=orchard-session.target

[Service]
Type=simple
ExecStart=$ROOT/dist/dev.sh watch
Restart=on-failure
RestartSec=2

[Install]
WantedBy=orchard-session.target
EOF
    say "wrote $unit_dir/orchard-dev-watch.service"
    if command -v systemctl >/dev/null 2>&1; then
        if systemctl --user daemon-reload && systemctl --user enable --now orchard-dev-watch.service; then
            say "enabled and started orchard-dev-watch.service"
        else
            warn "wrote unit file, but systemctl --user failed; enable it from your session with:"
            warn "  systemctl --user daemon-reload && systemctl --user enable --now orchard-dev-watch.service"
        fi
    fi
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
        build_and_apply
        watch_loop
        ;;
    _daemon)
        exec 9>"$LOCK_FILE"
        flock -n 9 || exit 0
        echo $$ >"$PID_FILE"
        trap 'rm -f "$PID_FILE"' EXIT
        watch_loop
        ;;
    start)
        start_daemon
        ;;
    stop)
        stop_daemon
        ;;
    status)
        status_daemon
        ;;
    logs)
        mkdir -p "$STATE_DIR"
        touch "$LOG_FILE"
        tail -f "$LOG_FILE"
        ;;
    reload-shaders)
        reload_shaders
        ;;
    hold)
        mkdir -p "$STATE_DIR"
        echo hold >"$MODE_FILE"
        say "holding: changes are collected; run 'dist/dev.sh go' to build them"
        ;;
    go|apply)
        flush_pending
        ;;
    auto)
        rm -f "$MODE_FILE"
        say "auto mode: builds start after ${QUIET}s without edits"
        flush_pending
        ;;
    pending)
        mode=auto; [ -f "$MODE_FILE" ] && mode=$(cat "$MODE_FILE")
        say "mode: $mode"
        say "pending: $(cat "$PENDING_FILE" 2>/dev/null || echo none)"
        ;;
    install-service)
        install_service
        ;;
    *)
        awk 'NR > 1 && !/^#/ { exit } NR > 1 { print }' "$0"
        exit 2
        ;;
esac
