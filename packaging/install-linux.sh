#!/usr/bin/env bash
# Installs the desktop entry and the themed icon sizes for the current user.
#
# gpui has no window-icon API, so on Linux the icon reaches the window indirectly: the compositor
# matches the window's app_id (Wayland) or WM_CLASS (X11) against StartupWMClass in a desktop
# entry, then draws that entry's Icon. Both halves have to be installed for the icon to appear,
# which is what this script does. Under Wayland there is no way to set a window icon from the
# process itself, so this is the only mechanism.
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
source_icon="$root/art/rusty_sql_icon.png"
entry="$root/packaging/rusty-sql-tool.desktop"
app_id=rusty-sql-tool

data_home=${XDG_DATA_HOME:-$HOME/.local/share}
icon_root="$data_home/icons/hicolor"
applications="$data_home/applications"

[ -f "$source_icon" ] || { echo "missing artwork: $source_icon" >&2; exit 1; }
[ -f "$entry" ] || { echo "missing desktop entry: $entry" >&2; exit 1; }

# ImageMagick 7 renamed the binary; accept either.
if command -v magick >/dev/null 2>&1; then
    resize() { magick "$1" -resize "$2x$2" "$3"; }
elif command -v convert >/dev/null 2>&1; then
    resize() { convert "$1" -resize "$2x$2" "$3"; }
else
    echo "needs ImageMagick (magick or convert) to scale the artwork" >&2
    exit 1
fi

# The desktop entry runs the binary by name, which only works when it is on the PATH the desktop
# environment starts applications with — ~/.cargo/bin usually is not. Resolve it here instead.
if binary=$(command -v "$app_id" 2>/dev/null); then
    :
elif [ -x "$root/target/release/$app_id" ]; then
    binary="$root/target/release/$app_id"
elif [ -x "$root/target/debug/$app_id" ]; then
    binary="$root/target/debug/$app_id"
else
    echo "no $app_id binary found; run 'cargo build --release' or 'cargo install --path .' first" >&2
    exit 1
fi

for size in 16 24 32 48 64 128 256 512; do
    target="$icon_root/${size}x${size}/apps"
    mkdir -p "$target"
    resize "$source_icon" "$size" "$target/$app_id.png"
done

mkdir -p "$applications"
sed "s|^Exec=.*|Exec=$binary|" "$entry" > "$applications/$app_id.desktop"

# Best effort: the caches are a speed-up, and both desktops rescan without them.
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -f -t "$icon_root" >/dev/null 2>&1 || true
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$applications" >/dev/null 2>&1 || true
command -v kbuildsycoca6 >/dev/null 2>&1 && kbuildsycoca6 >/dev/null 2>&1 || true

echo "installed $app_id.desktop -> $applications"
echo "installed icons          -> $icon_root/<size>/apps/$app_id.png"
echo "Exec                     -> $binary"
