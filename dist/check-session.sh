#!/bin/sh
# Run inside a real Orchard login session: checks that the session plumbing works
# (portals, notifications, screencopy, autostart, crashes) and says what to fix.
#
#   dist/check-session.sh
set -u

ok=0
bad=0
pass() { printf '  \033[32m✓\033[0m %s\n' "$*"; ok=$((ok + 1)); }
fail() { printf '  \033[31m✗\033[0m %s\n' "$*"; bad=$((bad + 1)); }
info() { printf '    %s\n' "$*"; }

LOG="${XDG_STATE_HOME:-$HOME/.local/state}/orchard/orchard.log"

echo "Orchard session check"

[ "${XDG_CURRENT_DESKTOP:-}" = "Orchard" ] && pass "running in Orchard ($WAYLAND_DISPLAY)" \
    || fail "XDG_CURRENT_DESKTOP is '${XDG_CURRENT_DESKTOP:-}', not Orchard - run this inside the Orchard session"

if pgrep -x orchard >/dev/null; then pass "compositor running"; else fail "compositor not running"; fi
if pgrep -x orchard-shell >/dev/null; then pass "menu bar / Dock running"; else fail "orchard-shell not running"; fi

# session target + environment
if systemctl --user is-active --quiet orchard-session.target; then
    pass "orchard-session.target active"
else
    fail "orchard-session.target not active"
    grep 'session:' "$LOG" 2>/dev/null | tail -5 | while read -r l; do info "$l"; done
fi
envwd=$(systemctl --user show-environment | sed -n 's/^WAYLAND_DISPLAY=//p')
[ "$envwd" = "${WAYLAND_DISPLAY:-}" ] && pass "systemd knows WAYLAND_DISPLAY=$envwd" \
    || fail "systemd has WAYLAND_DISPLAY='$envwd', session has '${WAYLAND_DISPLAY:-}'"
envx=$(systemctl --user show-environment | sed -n 's/^DISPLAY=//p')
[ -n "$envx" ] && pass "systemd knows DISPLAY=$envx (Xwayland)" || fail "DISPLAY not exported (Xwayland not up?)"

# portals
if busctl --user status org.freedesktop.portal.Desktop >/dev/null 2>&1; then
    pass "xdg-desktop-portal running"
    scheme=$(busctl --user call org.freedesktop.portal.Desktop /org/freedesktop/portal/desktop \
        org.freedesktop.portal.Settings ReadOne ss org.freedesktop.appearance color-scheme 2>/dev/null)
    [ -n "$scheme" ] && pass "appearance portal answers ($scheme; 1 = dark)" || fail "appearance portal does not answer"
    if busctl --user introspect org.freedesktop.portal.Desktop /org/freedesktop/portal/desktop 2>/dev/null | grep -q 'org.freedesktop.portal.ScreenCast'; then
        pass "ScreenCast portal available"
    else
        fail "no ScreenCast portal (check ~/.config/xdg-desktop-portal/orchard-portals.conf)"
    fi
else
    fail "xdg-desktop-portal not running"
    systemctl --user status xdg-desktop-portal.service --no-pager 2>/dev/null | tail -5 | while read -r l; do info "$l"; done
fi

# notifications
owner=$(busctl --user status org.freedesktop.Notifications 2>/dev/null | sed -n 's/^Comm=//p')
[ "$owner" = "orchard-shell" ] && pass "notifications served by orchard-shell" \
    || fail "org.freedesktop.Notifications owned by '${owner:-nobody}'"

# global menus
reg=$(busctl --user status com.canonical.AppMenu.Registrar 2>/dev/null | sed -n 's/^Comm=//p')
[ "$reg" = "orchard-shell" ] && pass "global menus: registrar served by orchard-shell" \
    || fail "com.canonical.AppMenu.Registrar owned by '${reg:-nobody}' (Qt apps won't export menus)"

# screencopy
if command -v grim >/dev/null; then
    f=$(mktemp --suffix=.png)
    if grim "$f" 2>/dev/null && [ -s "$f" ]; then pass "screencopy works (grim)"; else fail "grim failed"; fi
    rm -f "$f"
fi

# app theming from Orchard's own settings
[ "$(systemctl --user show-environment | sed -n 's/^DCONF_PROFILE=//p')" = "${XDG_CONFIG_HOME:-$HOME/.config}/orchard/dconf-profile" ] \
    && pass "apps use Orchard's dconf database (icon-theme: $(gsettings get org.gnome.desktop.interface icon-theme))" \
    || info "DCONF_PROFILE is not Orchard's (fine if you set your own)"
[ "$(systemctl --user show-environment | sed -n 's/^QT_QPA_PLATFORMTHEME=//p')" = "gtk3" ] \
    && pass "Qt apps follow Orchard's theme (gtk3 platform theme)" || info "QT_QPA_PLATFORMTHEME is not 'gtk3' (fine if you set your own)"

# log health
if [ -f "$LOG" ]; then
    n=$(grep -c ' WARN \| ERROR ' "$LOG")
    p=$(grep -c 'PANIC\|panicked' "$LOG")
    r=$(grep -c 'orchard-shell exited' "$LOG")
    [ "$p" -eq 0 ] && pass "no panics in this session's log" || fail "$p panic(s) in $LOG"
    [ "$r" -eq 0 ] && pass "shell never restarted" || fail "orchard-shell restarted $r time(s)"
    info "$n warnings/errors in $LOG (most recent below)"
    grep ' WARN \| ERROR ' "$LOG" | grep -v 'mode property blob\|drm master' | tail -5 | cut -c1-160 | while read -r l; do info "$l"; done
fi
ls "${XDG_STATE_HOME:-$HOME/.local/state}"/orchard/crash-*.txt >/dev/null 2>&1 \
    && info "crash reports: $(ls "${XDG_STATE_HOME:-$HOME/.local/state}"/orchard/crash-*.txt | tr '\n' ' ')"

echo
echo "$ok ok, $bad problem(s)"
[ "$bad" -eq 0 ]
