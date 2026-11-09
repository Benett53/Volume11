#!/bin/sh
# Installs Volume11 from a release archive.
#   ./install.sh               -> for the current user, into ~/.local
#   sudo ./install.sh --system -> system wide, into /usr/local
# Running it again updates an existing installation; settings in
# ~/.config/volume11 are not touched.
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
if [ "${1:-}" = "--system" ]; then
    PREFIX=/usr/local
else
    PREFIX="$HOME/.local"
fi

# A running copy keeps working with the old file; ask it to quit so the
# new version is what runs from here on.
RUNNING=0
if command -v volume11 >/dev/null 2>&1 || [ -x "$PREFIX/bin/volume11" ]; then
    if pgrep -x volume11 >/dev/null 2>&1; then
        RUNNING=1
        "$PREFIX/bin/volume11" --quit 2>/dev/null || volume11 --quit 2>/dev/null || true
        sleep 1
    fi
fi

install -Dm755 "$HERE/volume11" "$PREFIX/bin/volume11"
install -Dm755 "$HERE/uninstall.sh" "$PREFIX/share/volume11/uninstall.sh"
install -Dm644 "$HERE/volume11.desktop" "$PREFIX/share/applications/volume11.desktop"
for size in 32 48 64 128 256; do
    install -Dm644 "$HERE/icons/volume11-$size.png" "$PREFIX/share/icons/hicolor/${size}x${size}/apps/volume11.png"
done

# An existing autostart entry points at wherever Volume11 was before.
AUTOSTART="${XDG_CONFIG_HOME:-$HOME/.config}/autostart/volume11.desktop"
if [ "$PREFIX" = "$HOME/.local" ] && [ -f "$AUTOSTART" ]; then
    sed -i "s|^Exec=.*|Exec=$PREFIX/bin/volume11|" "$AUTOSTART"
fi

command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$PREFIX/share/applications" || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t "$PREFIX/share/icons/hicolor" || true

echo "Volume11 installed to $PREFIX/bin/volume11"
echo "Uninstall with: $PREFIX/share/volume11/uninstall.sh"

if [ "$RUNNING" = 1 ] && [ "$(id -u)" != 0 ]; then
    nohup "$PREFIX/bin/volume11" >/dev/null 2>&1 &
fi
