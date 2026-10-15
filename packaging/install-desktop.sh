#!/bin/sh
# Installs the desktop entry and the icon for the current user, so launchers and the
# compositor show hark by its app id.
set -e
here=$(cd "$(dirname "$0")" && pwd)
icons="$here/../crates/hark/assets/icon"
# Launchers run with the session's PATH, which rarely has the user's bin folder, so the entry
# names the binary by its full path.
bin=$(command -v hark || echo "$here/../target/release/hark")
bin=$(readlink -f "$bin")
mkdir -p "$HOME/.local/share/applications"
sed "s|^Exec=hark|Exec=$bin|" "$here/hark.desktop" > "$HOME/.local/share/applications/hark.desktop"
for s in 16 32 48 64 128 256 512; do
  install -Dm644 "$icons/hark-$s.png" "$HOME/.local/share/icons/hicolor/${s}x${s}/apps/hark.png"
done
install -Dm644 "$icons/hark.svg" "$HOME/.local/share/icons/hicolor/scalable/apps/hark.svg"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
