#!/bin/sh
# Skip the login greeter: boot straight into Orchard, which starts at its own
# lock screen. The desktop loads behind the lock while you type, so one
# password unlocks a ready desktop (like macOS).
#
#   dist/autologin.sh enable    log in automatically, start locked
#   dist/autologin.sh disable   show the greeter again
#   dist/autologin.sh status
#
# Works with SDDM, GDM, and LightDM. Run dist/install.sh first.
# Run it as your user; it calls sudo itself.
set -e

if [ "$(id -u)" = 0 ]; then
    echo "Run dist/autologin.sh as your user (it asks for sudo itself)." >&2
    exit 1
fi

DM=""
CONF=""
if [ -d /etc/sddm.conf.d ] || systemctl is-enabled -q sddm.service 2>/dev/null; then
    DM=sddm
    CONF=/etc/sddm.conf.d/zz-orchard-autologin.conf
elif [ -d /etc/lightdm/lightdm.conf.d ] || systemctl is-enabled -q lightdm.service 2>/dev/null; then
    DM=lightdm
    CONF=/etc/lightdm/lightdm.conf.d/zz-orchard-autologin.conf
elif [ -d /etc/gdm ] || systemctl is-enabled -q gdm.service 2>/dev/null; then
    DM=gdm
    CONF=/etc/gdm/custom.conf
else
    echo "No supported display manager (SDDM, GDM, LightDM) found; set up automatic login yourself" >&2
    echo "with the session /usr/share/wayland-sessions/orchard-locked.desktop." >&2
    exit 1
fi

case "${1:-}" in
    enable)
        for f in /usr/local/bin/orchard-lock /usr/share/wayland-sessions/orchard-locked.desktop /etc/pam.d/orchard-lock; do
            if [ ! -e "$f" ]; then
                echo "$f is missing: run dist/install.sh (or dist/install.sh --dev) first." >&2
                exit 1
            fi
        done
        sudo mkdir -p "$(dirname "$CONF")"
        case "$DM" in
            sddm)
                printf '%s\n' \
                    "# Written by orchard/dist/autologin.sh; remove with 'dist/autologin.sh disable'." \
                    "# Logs $USER into Orchard at boot. Orchard starts locked (orchard-session --locked)," \
                    "# so the password is still needed; after logging out the greeter is shown." \
                    "[Autologin]" \
                    "User=$USER" \
                    "Session=orchard-locked.desktop" \
                    "Relogin=false" | sudo tee "$CONF" >/dev/null
                ;;
            lightdm)
                printf '%s\n' \
                    "# Written by orchard/dist/autologin.sh; remove with 'dist/autologin.sh disable'." \
                    "[Seat:*]" \
                    "autologin-user=$USER" \
                    "autologin-session=orchard-locked" | sudo tee "$CONF" >/dev/null
                ;;
            gdm)
                printf '%s\n' \
                    "# Written by orchard/dist/autologin.sh; remove with 'dist/autologin.sh disable'." \
                    "[daemon]" \
                    "AutomaticLoginEnable=True" \
                    "AutomaticLogin=$USER" | sudo tee "$CONF" >/dev/null
                echo "Note: for GDM, select 'Orchard' once or set XSession=orchard-locked in /var/lib/AccountsService/users/$USER."
                ;;
        esac
        echo "Automatic login into Orchard's lock screen enabled ($CONF)."
        echo "Takes effect at the next boot (or after 'sudo systemctl restart $DM', which ends running sessions)."
        ;;
    disable)
        if [ "$DM" = "gdm" ]; then
            sudo sed -i '/^AutomaticLoginEnable=/d;/^AutomaticLogin=/d' "$CONF" 2>/dev/null || true
        else
            sudo rm -f "$CONF"
        fi
        echo "Automatic login disabled; the greeter is shown again at the next boot."
        ;;
    status)
        if [ -f "$CONF" ] && { [ "$DM" != "gdm" ] || grep -q '^AutomaticLoginEnable=True' "$CONF"; }; then
            echo "enabled ($CONF):"
            cat "$CONF"
        else
            echo "disabled"
        fi
        ;;
    *)
        sed -n '2,12p' "$0"
        exit 2
        ;;
esac
