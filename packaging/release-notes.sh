#!/bin/sh
# Prints a release's notes for its GitHub page: the version's lines from CHANGELOG.md, the
# same ones kuulo shows after an update, then what each download holds. The release
# workflow puts them on the draft; run it to read them first.
#   packaging/release-notes.sh 0.6.0
set -eu
v=$1
root=$(cd "$(dirname "$0")/.." && pwd)
lines=$(awk -v h="## $v" '$0 == h { on = 1; next } /^## / { on = 0 } on' "$root/CHANGELOG.md" | sed '/./,$!d')
if [ -z "$lines" ]; then
  echo "CHANGELOG.md has no lines under \"## $v\"" >&2
  exit 1
fi
printf '%s\n\n' "$lines"
cat <<EOF
**Linux x86_64:** \`kuulo-$v-linux-x86_64.tar.gz\` holds \`kuulo\`, \`kuulo-cli\`, the desktop entry, the icons and \`install.sh\`, which installs kuulo to \`~/.local/bin\` for the current user. Built on Ubuntu 24.04 (glibc 2.39 or newer); PipeWire (\`libpipewire-0.3\`) or ALSA at runtime. Wayland and X11; dragging out and clipboard copy need Wayland.

**Windows x86_64:** \`kuulo-$v-windows-x86_64-setup.exe\` installs kuulo for your account without admin rights, with a Start menu entry, \`kuulo-cli\` on PATH and an uninstall entry; \`kuulo-$v-windows-x86_64.zip\` holds the same two programs to run from any folder. Not code-signed yet, so Windows may say it protected your PC: choose More info, then Run anyway. Windows 10 or later; WASAPI.

**macOS, universal, 11 or later:** \`kuulo-$v-macos-universal.dmg\`: drag kuulo to Applications. Not notarized, so macOS blocks the first launch: open System Settings → Privacy & Security and choose Open Anyway. \`kuulo-$v-macos-universal.tar.gz\` is the same app, the file kuulo's updater installs. CoreAudio. Option-drag to drag out, Cmd+C to copy.

**Signatures:** each file has a \`.minisig\` made with kuulo's release key, [\`packaging/kuulo.pub\`](https://github.com/SteffanPoulsen/kuulo/blob/master/packaging/kuulo.pub). Check one with \`minisign -Vm <file> -p kuulo.pub\`.
EOF
