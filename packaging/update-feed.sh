#!/bin/sh
# Serves a folder of signed downloads as an update feed, to try an update before a release.
# A kuulo started with KUULO_UPDATE_FEED set to the URL this prints offers what the folder
# holds, and installs it only if it carries kuulo's signature, like any update.
#
# Build two test versions that will never be released with the release workflow's version
# input (gh workflow run release.yml --ref <branch> -f version=0.0.2), sign the newer one's
# files (minisign -S -s ~/.minisign/kuulo.key -m <folder>/kuulo-*), install the older one
# and run it against the feed. On macOS: open --env KUULO_UPDATE_FEED=<url> /Applications/kuulo.app
#   packaging/update-feed.sh <folder> [port]
set -eu
dir=$(cd "$1" && pwd)
port=${2:-8765}
host=$(ip -4 route get 1.1.1.1 2>/dev/null | sed -n 's/.* src \([0-9.]*\).*/\1/p')
base="http://${host:-localhost}:$port"
version=$(ls "$dir" | sed -n 's/^kuulo-\([0-9][0-9.]*\)-.*/\1/p' | head -1)
if [ -z "$version" ]; then
  echo "$dir holds no kuulo-<version>-… files" >&2
  exit 1
fi
{
  printf '{\n  "tag_name": "v%s",\n  "html_url": "%s/",\n  "assets": [' "$version" "$base"
  sep=
  for f in "$dir"/kuulo-*; do
    printf '%s\n    {"name": "%s", "browser_download_url": "%s/%s"}' "$sep" "${f##*/}" "$base" "${f##*/}"
    sep=,
  done
  printf '\n  ]\n}\n'
} > "$dir/latest.json"
echo "KUULO_UPDATE_FEED=$base/latest.json"
exec python3 -m http.server "$port" --bind 0.0.0.0 --directory "$dir"
