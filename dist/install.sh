#!/bin/sh
# Build Orchard in release mode and install it as a login session.
# Pick "Orchard" in your display manager's session menu.
#
#   dist/install.sh        copy the binaries to /usr/local/bin
#   dist/install.sh --dev  symlink /usr/local/bin/orchard* to target/release,
#                          so `cargo build` / dist/dev.sh update the session
#                          without reinstalling
#
# Run it as your user; it calls sudo itself where needed.
set -e
cd "$(dirname "$0")/.."

if [ "$(id -u)" = 0 ]; then
    echo "Run dist/install.sh without sudo (it asks for sudo itself)." >&2
    echo "Building as root leaves root-owned files in target/." >&2
    exit 1
fi

# earlier `sudo dist/install.sh` runs left root-owned build files behind
if [ -d target ] && [ -n "$(find target ! -user "$(id -u)" -print -quit)" ]; then
    echo "Fixing ownership of root-owned files in target/ ..."
    sudo chown -R "$(id -u):$(id -g)" target
fi

cargo build --release

for b in orchard orchard-shell orchard-settings orchard-lock; do
    dst="/usr/local/bin/$b"
    if [ "${1:-}" = "--dev" ]; then
        # -f replaces an existing file/symlink in one step, so a failed link
        # never leaves the binary missing.
        sudo ln -sfn "$PWD/target/release/$b" "$dst"
    else
        # install a complete copy next to the destination, then rename it over
        # the old binary: an interrupted install keeps the previous one.
        sudo install -Dm755 "target/release/$b" "$dst.new"
        sudo mv -f "$dst.new" "$dst"
    fi
done
sudo install -Dm644 dist/org.orchard.settings.desktop /usr/share/applications/org.orchard.settings.desktop
sudo install -Dm755 dist/orchard-session          /usr/local/bin/orchard-session
sudo install -Dm644 dist/orchard.desktop          /usr/share/wayland-sessions/orchard.desktop
sudo install -Dm644 dist/orchard-locked.desktop   /usr/share/wayland-sessions/orchard-locked.desktop
sudo install -Dm644 dist/orchard-lock.pam         /etc/pam.d/orchard-lock

if [ "${1:-}" = "--dev" ]; then
    echo "Installed in dev mode (symlinks into $PWD/target/release)."
    echo "Log in again once; afterwards use dist/dev.sh (or dist/dev.sh watch) for live updates."
else
    echo "Installed. Log out and choose 'Orchard' in the session menu."
fi
echo "Logs: ~/.local/state/orchard/orchard.log"
