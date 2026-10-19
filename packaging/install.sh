#!/bin/sh
# Installs kuulo for the current user: the binaries into ~/.local/bin when they sit beside
# this script (a release archive), and the desktop entry and icon, so launchers and the
# compositor show kuulo by its app id. From the repository the binaries are left where
# cargo put them; the entry runs `kuulo` by name, so it must be on the session's PATH.
set -e
here=$(cd "$(dirname "$0")" && pwd)
if [ -d "$here/icons" ]; then
  icons="$here/icons"
else
  icons="$here/../crates/kuulo/assets/icon"
fi
if [ -x "$here/kuulo" ]; then
  install -Dm755 "$here/kuulo" "$HOME/.local/bin/kuulo"
  install -Dm755 "$here/kuulo-cli" "$HOME/.local/bin/kuulo-cli"
fi
install -Dm644 "$here/kuulo.desktop" "$HOME/.local/share/applications/kuulo.desktop"
for s in 16 32 48 64 128 256 512; do
  install -Dm644 "$icons/kuulo-$s.png" "$HOME/.local/share/icons/hicolor/${s}x${s}/apps/kuulo.png"
done
install -Dm644 "$icons/kuulo.svg" "$HOME/.local/share/icons/hicolor/scalable/apps/kuulo.svg"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
