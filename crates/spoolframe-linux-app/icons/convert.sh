#!/usr/bin/env bash
# Fetches the Lucide icons the app uses and makes them GTK symbolic icons.
#
# Lucide draws with 2 px strokes; GTK recolours a symbolic icon by forcing
# its fills to the text colour, so each stroke is outlined into a filled
# path first (Inkscape's stroke-to-path). The results are committed: the
# build needs neither the network nor Inkscape. To add an icon, add its
# Lucide name below and run this from anywhere.
set -euo pipefail

VERSION=1.50.0
ICONS=(
  play pause rotate-ccw rotate-cw captions activity maximize minimize
  volume-2 chevron-down chevron-up chevron-left x
  skip-back skip-forward shuffle repeat repeat-1
  house search film tv music
  eye check plus menu
  chevron-right server circle-alert palette
)

here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

curl -fsSL "https://cdn.jsdelivr.net/npm/lucide-static@${VERSION}/LICENSE" -o "$here/LICENSE"
actions=""
for name in "${ICONS[@]}"; do
  curl -fsSL "https://cdn.jsdelivr.net/npm/lucide-static@${VERSION}/icons/${name}.svg" -o "$work/${name}.svg"
  out="$here/lucide-${name}-symbolic.svg"
  actions+="file-open:$work/${name}.svg;select-all:all;object-stroke-to-path;"
  actions+="export-plain-svg;export-filename:${out};export-do;file-close;"
done
nix shell nixpkgs#inkscape --command inkscape --actions="$actions"
# The outlined paths carry their own fill; Lucide's stroke and fill
# attributes on the root would only mislead whoever reads the file next.
for name in "${ICONS[@]}"; do
  sed -i -E '/^ *(fill="none"|stroke="currentColor"|stroke-width="[^"]*"|stroke-linecap="[^"]*"|stroke-linejoin="[^"]*")$/d' \
    "$here/lucide-${name}-symbolic.svg"
done
echo "converted ${#ICONS[@]} icons from Lucide ${VERSION}"
