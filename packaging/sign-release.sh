#!/bin/sh
# Signs a release's downloads with kuulo's release key and attaches the signatures. Run it on
# the draft, before publishing: an immutable release's files cannot change once it is out.
# minisign names each file in its signature's trusted comment, and the updater installs a
# file only under the name it was signed with.
#   packaging/sign-release.sh v0.6.0 [secret key, by default ~/.minisign/kuulo.key]
set -eu
tag=$1
key=${2:-$HOME/.minisign/kuulo.key}
here=$(cd "$(dirname "$0")" && pwd)
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
gh release download "$tag" --dir "$dir" --pattern 'kuulo-*'
rm -f "$dir"/*.minisig
minisign -S -s "$key" -m "$dir"/kuulo-*
# The public keys beside this script are the ones kuulo is built with, so a signature that
# none of them accepts would be refused by every kuulo that updates itself.
for f in "$dir"/kuulo-*; do
  case $f in *.minisig) continue ;; esac
  ok=
  for pub in "$here"/kuulo*.pub; do
    if minisign -V -q -p "$pub" -m "$f" 2>/dev/null; then ok=1; break; fi
  done
  if [ -z "$ok" ]; then
    echo "$(basename "$f"): no key kuulo is built with accepts this signature; nothing was uploaded" >&2
    exit 1
  fi
done
gh release upload "$tag" "$dir"/*.minisig --clobber
echo "Signed and attached: $(cd "$dir" && ls ./*.minisig | sed 's|^\./||' | tr '\n' ' ')"
