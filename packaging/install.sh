#!/bin/sh
# Installs hark for the current user: the binaries into ~/.local/bin when they sit beside
# this script (a release archive), and the desktop entry and icon, so launchers and the
# compositor show hark by its app id. From the repository the binaries are left where
# cargo put them; the entry runs `hark` by name, so it must be on the session's PATH.
set -e
here=$(cd "$(dirname "$0")" && pwd)
if [ -d "$here/icons" ]; then
  icons="$here/icons"
else
  icons="$here/../crates/hark/assets/icon"
fi
if [ -x "$here/hark" ]; then
  install -Dm755 "$here/hark" "$HOME/.local/bin/hark"
  install -Dm755 "$here/hark-cli" "$HOME/.local/bin/hark-cli"
fi
install -Dm644 "$here/hark.desktop" "$HOME/.local/share/applications/hark.desktop"
for s in 16 32 48 64 128 256 512; do
  install -Dm644 "$icons/hark-$s.png" "$HOME/.local/share/icons/hicolor/${s}x${s}/apps/hark.png"
done
install -Dm644 "$icons/hark.svg" "$HOME/.local/share/icons/hicolor/scalable/apps/hark.svg"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
