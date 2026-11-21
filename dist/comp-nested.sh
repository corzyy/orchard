#!/bin/bash
# Run the forked COSMIC compositor in a window inside the current Orchard
# session. The fork lives in compositor/ and is not the default session yet.
#
#   dist/comp-nested.sh              build, then open the nested fork
#   dist/comp-nested.sh run          open the debug build without building
#   dist/comp-nested.sh release      build optimised, then open
#
# Everything after the mode is passed to the compositor binary. This is a bare
# COSMIC compositor: it has no Orchard menu bar, Dock, chrome or IPC yet.
set -u
cd "$(dirname "$0")/.."
ROOT=$PWD
COMP=$ROOT/compositor
LOG=${XDG_STATE_HOME:-$HOME/.local/state}/orchard/comp-nested.log

if [ -z "${WAYLAND_DISPLAY:-}" ] && [ -z "${DISPLAY:-}" ]; then
    echo "no Wayland or X11 display: run this from inside your Orchard session" >&2
    exit 1
fi

mkdir -p "$(dirname "$LOG")"

# One instance at a time, like dist/nested.sh.
exec 9>"${XDG_RUNTIME_DIR:-/tmp}/orchard-comp-nested.lock"
if ! flock -n 9; then
    echo "nested COSMIC fork is already open"
    exit 0
fi

# winit makes the nested case deterministic: without it the fork would pick the
# X11 backend whenever DISPLAY is set by Xwayland.
export COSMIC_BACKEND=winit

echo "nested COSMIC fork; log: $LOG"

case "${1:-build}" in
    run)
        shift
        BIN=$COMP/target/debug/cosmic-comp
        if [ ! -x "$BIN" ]; then
            echo "missing $BIN; run dist/comp-nested.sh first" >&2
            exit 1
        fi
        exec "$BIN" "$@" >"$LOG" 2>&1 9>&-
        ;;
    release)
        shift
        exec cargo run --release --manifest-path "$COMP/Cargo.toml" -- "$@" >"$LOG" 2>&1 9>&-
        ;;
    build|"")
        exec cargo run --manifest-path "$COMP/Cargo.toml" -- >"$LOG" 2>&1 9>&-
        ;;
    *)
        sed -n 2,9p "$0"
        exit 1
        ;;
esac
