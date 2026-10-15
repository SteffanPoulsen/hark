#!/usr/bin/env bash
# Builds the release archives into dist/: a tarball for Linux x86_64 built on this machine,
# and a zip for Windows x86_64 cross-built with the GNU toolchain (mingw-w64). The version
# is the workspace's. Run from any directory; needs cargo, the x86_64-pc-windows-gnu target
# and x86_64-w64-mingw32-gcc.
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo/Cargo.toml" | head -1)
dist="$repo/dist"
icons="$repo/crates/hark/assets/icon"

cd "$repo"
cargo build --release
cargo build --release --target x86_64-pc-windows-gnu

mkdir -p "$dist"
linux="hark-$version-linux-x86_64"
rm -rf "${dist:?}/$linux" "$dist/$linux.tar.gz"
mkdir -p "$dist/$linux/icons"
cp target/release/hark target/release/hark-cli "$dist/$linux/"
cp packaging/hark.desktop packaging/install.sh README.md LICENSE "$dist/$linux/"
cp "$icons"/hark-*.png "$icons/hark.svg" "$dist/$linux/icons/"
tar -C "$dist" -czf "$dist/$linux.tar.gz" "$linux"

windows="hark-$version-windows-x86_64"
rm -rf "${dist:?}/$windows" "$dist/$windows.zip"
mkdir -p "$dist/$windows"
cp target/x86_64-pc-windows-gnu/release/hark.exe target/x86_64-pc-windows-gnu/release/hark-cli.exe "$dist/$windows/"
cp README.md LICENSE "$dist/$windows/"
(cd "$dist" && zip -qr "$windows.zip" "$windows")

ls -l "$dist"/*.tar.gz "$dist"/*.zip
