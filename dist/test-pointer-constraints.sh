#!/bin/bash
# Regression test: pointer locks / confinement (games) must never let the
# cursor leave the window, and a lock's cursor hint must land inside it.
#
# Runs a headless Orchard (two outputs, like a dual-monitor desktop) in an
# isolated runtime dir and D-Bus session, with the pointer_constraint_test
# example as the "game". Exit code 0 = pass.
#
#   dist/test-pointer-constraints.sh            (builds first)
set -u
ROOT=$(cd "$(dirname "$0")/.." && pwd)

if [ "${1:-}" != "--inner" ]; then
    { cargo build --release --manifest-path "$ROOT/Cargo.toml" -p orchard &&
        cargo build --release --manifest-path "$ROOT/Cargo.toml" -p orchard-shell --example pointer_constraint_test; } \
        >/dev/null 2>&1 || { echo "build failed"; exit 2; }
    WORK=$(mktemp -d)
    mkdir -m 700 "$WORK/rt"
    # XDG_RUNTIME_DIR must be set BEFORE dbus-run-session (else portals of the
    # test session attach to the real one)
    env -u WAYLAND_DISPLAY -u ORCHARD_SOCKET -u DISPLAY XDG_RUNTIME_DIR="$WORK/rt" \
        dbus-run-session -- "$0" --inner "$WORK"
    rc=$?
    [ $rc -eq 0 ] && rm -rf "$WORK" || echo "logs kept in $WORK"
    exit $rc
fi

WORK=$2
BIN=$ROOT/target/release
CTL=$XDG_RUNTIME_DIR/orchard-ctl
fail=0

run() { # mode
    local mode=$1 out=$WORK/client-$1.txt log=$WORK/orchard-$1.txt
    : >"$CTL"; : >"$out"
    ORCHARD_HEADLESS_OUTPUTS=2 ORCHARD_NO_AUTOSTART=1 ORCHARD_NO_NOTIFICATIONS=1 \
        "$BIN/orchard" --headless >"$log" 2>&1 &
    local pid=$!
    local sock=""
    for _ in $(seq 50); do
        sock=$(grep -o 'Listening on wayland socket name="[^"]*"' "$log" | sed 's/.*name="\(.*\)"/\1/')
        [ -n "$sock" ] && break
        sleep 0.1
    done
    WAYLAND_DISPLAY=$sock "$BIN/examples/pointer_constraint_test" "$mode" 10 10 >>"$out" 2>&1 &
    local cpid=$!
    sleep 1.5
    echo "motion 720 450" >>"$CTL"; sleep 0.3
    echo "MARK constrained" >>"$out"
    for _ in 1 2 3 4 5; do echo "rel 3000 0" >>"$CTL"; sleep 0.1; done
    for _ in 1 2 3 4 5; do echo "rel -3000 -3000" >>"$CTL"; sleep 0.1; done
    kill -USR1 $cpid; sleep 0.3
    echo "MARK released" >>"$out"
    echo "rel 1 0" >>"$CTL"; sleep 0.3
    kill $cpid 2>/dev/null
    echo "action quit" >>"$CTL"; sleep 0.5
    kill $pid 2>/dev/null; wait $pid 2>/dev/null
}

check() { # mode description condition-command...
    local mode=$1 what=$2; shift 2
    if "$@"; then echo "ok   $mode: $what"; else echo "FAIL $mode: $what"; fail=1; fi
}

run lock
f=$WORK/client-lock.txt
check lock "pointer got locked" grep -q '^locked' "$f"
check lock "relative motion delivered while locked" grep -q '^relative 3000 0' "$f"
check lock "no leave/absolute motion while locked" \
    bash -c "! sed -n '/MARK constrained/,/MARK released/p' '$f' | grep -q '^leave\|^motion'"
check lock "after release the cursor is at the hint (11,10), inside the window" \
    bash -c "sed -n '/MARK released/,\$p' '$f' | grep -q '^motion 11.0 10.0'"
check lock "cursor never left the window" bash -c "! grep -q '^leave' '$f'"

run confine
f=$WORK/client-confine.txt
check confine "pointer got confined" grep -q '^confined' "$f"
check confine "slides along the right edge" grep -q '^motion 399.5 ' "$f"
check confine "slides along the top-left corner" grep -q '^motion 0.0 0.0' "$f"
check confine "cursor never left the window" bash -c "! grep -q '^leave' '$f'"

exit $fail
