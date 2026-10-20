#!/bin/sh
# Installs kuulo for the current user: the binaries into ~/.local/bin when they sit beside
# this script (a release archive), and the desktop entry and icons, so launchers and the
# compositor show kuulo by its app id. ~/.local/bin is not on every graphical session's
# PATH, so an archive install points the entry at the installed binary; from the repository
# the binaries stay where cargo put them and the entry runs `kuulo` by name.
# An archive install also leaves a marker naming the binaries' folder: kuulo installs its
# own updates only when it runs from there, by running the next archive's install.sh.
# `install.sh --uninstall` removes what this installs and leaves the library, settings and
# copies alone.
set -e
here=$(cd "$(dirname "$0")" && pwd)
bin="$HOME/.local/bin"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
apps="$data/applications"
hicolor="$data/icons/hicolor"
marker="$data/kuulo/installed"
sizes="16 32 48 64 128 256 512"

refresh() {
  update-desktop-database "$apps" 2>/dev/null || true
  gtk-update-icon-cache -q "$hicolor" 2>/dev/null || true
}

if [ "$1" = --uninstall ]; then
  rm -f "$bin/kuulo" "$bin/kuulo-cli" "$marker" "$apps/kuulo.desktop" "$hicolor/scalable/apps/kuulo.svg"
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
  # Both binaries are copied in beside their places first and then renamed over them, so a
  # copy that fails, on a full disk say, leaves the installed kuulo whole.
  mkdir -p "$bin"
  trap 'rm -f "$bin/.kuulo.new" "$bin/.kuulo-cli.new"' EXIT
  install -m755 "$here/kuulo" "$bin/.kuulo.new"
  install -m755 "$here/kuulo-cli" "$bin/.kuulo-cli.new"
  mv -f "$bin/.kuulo.new" "$bin/kuulo"
  mv -f "$bin/.kuulo-cli.new" "$bin/kuulo-cli"
  mkdir -p "$data/kuulo"
  echo "$bin" > "$marker"
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
