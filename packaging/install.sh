#!/bin/sh
# Installs kuulo for the current user: the binaries into ~/.local/bin when they sit beside
# this script (a release archive), and the desktop entry and icons, so launchers and the
# compositor show kuulo by its app id. ~/.local/bin is not on every graphical session's
# PATH, so an archive install points the entry at the installed binary; from the repository
# the binaries stay where cargo put them and the entry runs `kuulo` by name.
# `install.sh --uninstall` removes what this installs and leaves the library, settings and
# copies alone.
set -e
here=$(cd "$(dirname "$0")" && pwd)
bin="$HOME/.local/bin"
apps="$HOME/.local/share/applications"
hicolor="$HOME/.local/share/icons/hicolor"
sizes="16 32 48 64 128 256 512"

refresh() {
  update-desktop-database "$apps" 2>/dev/null || true
  gtk-update-icon-cache -q "$hicolor" 2>/dev/null || true
}

if [ "$1" = --uninstall ]; then
  rm -f "$bin/kuulo" "$bin/kuulo-cli" "$apps/kuulo.desktop" "$hicolor/scalable/apps/kuulo.svg"
  for s in $sizes; do
    rm -f "$hicolor/${s}x${s}/apps/kuulo.png"
  done
  refresh
  echo "kuulo is removed. Your library, settings and copies are where they were."
  exit 0
fi

if [ -d "$here/icons" ]; then
  icons="$here/icons"
else
  icons="$here/../crates/kuulo/assets/icon"
fi
exec_line="Exec=kuulo %f"
if [ -x "$here/kuulo" ]; then
  install -Dm755 "$here/kuulo" "$bin/kuulo"
  install -Dm755 "$here/kuulo-cli" "$bin/kuulo-cli"
  exec_line="Exec=\"$bin/kuulo\" %f"
fi
mkdir -p "$apps"
sed "s|^Exec=.*|$exec_line|" "$here/kuulo.desktop" > "$apps/kuulo.desktop"
for s in $sizes; do
  install -Dm644 "$icons/kuulo-$s.png" "$hicolor/${s}x${s}/apps/kuulo.png"
done
install -Dm644 "$icons/kuulo.svg" "$hicolor/scalable/apps/kuulo.svg"
refresh
if [ -x "$here/kuulo" ]; then
  case ":$PATH:" in
    *":$bin:"*) ;;
    *) echo "$bin is not on your PATH yet, so a terminal will not find kuulo-cli until it is." ;;
  esac
fi
