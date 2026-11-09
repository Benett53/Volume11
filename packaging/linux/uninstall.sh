#!/bin/sh
# Removes Volume11.
#   uninstall.sh          -> program, menu entry, icons and autostart entry
#   uninstall.sh --purge  -> also the saved volumes and settings
set -eu

case "$(cd "$(dirname "$0")" && pwd)" in
    /usr/local/*) PREFIX=/usr/local ;;
    *) PREFIX="$HOME/.local" ;;
esac

"$PREFIX/bin/volume11" --quit 2>/dev/null || true
sleep 1

rm -f "$PREFIX/bin/volume11" \
      "$PREFIX/share/applications/volume11.desktop"
for size in 32 48 64 128 256; do
    rm -f "$PREFIX/share/icons/hicolor/${size}x${size}/apps/volume11.png"
done

CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}"
rm -f "$CONFIG/autostart/volume11.desktop"
if [ "${1:-}" = "--purge" ]; then
    rm -rf "$CONFIG/volume11"
fi

command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$PREFIX/share/applications" || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t "$PREFIX/share/icons/hicolor" || true

rm -rf "$PREFIX/share/volume11"
echo "Volume11 removed."
