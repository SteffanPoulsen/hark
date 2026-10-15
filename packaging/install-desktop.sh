#!/bin/sh
# Installs the desktop entry and the icon for the current user, so launchers and the
# compositor show hark by its app id.
set -e
here=$(cd "$(dirname "$0")" && pwd)
icons="$here/../crates/hark/assets/icon"
install -Dm644 "$here/hark.desktop" "$HOME/.local/share/applications/hark.desktop"
for s in 16 32 48 64 128 256 512; do
  install -Dm644 "$icons/hark-$s.png" "$HOME/.local/share/icons/hicolor/${s}x${s}/apps/hark.png"
done
install -Dm644 "$icons/hark.svg" "$HOME/.local/share/icons/hicolor/scalable/apps/hark.svg"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
