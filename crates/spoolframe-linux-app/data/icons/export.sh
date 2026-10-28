#!/usr/bin/env bash
# Exports the freedesktop hicolor PNGs from the app icon's sources: the
# full mark from 48 px up, the three-hole reel below 40 px, as the
# Spoolframe Logo design sets them. Run from this directory after changing
# a source; needs rsvg-convert (`nix shell nixpkgs#librsvg`).
set -euo pipefail
cd "$(dirname "$0")"
app=app.spoolframe.relm4
for size in 16 22 24 32 48 64 128 256 512; do
  source=hicolor/scalable/apps/$app.svg
  [ "$size" -lt 40 ] && source=small.svg
  mkdir -p "hicolor/${size}x${size}/apps"
  rsvg-convert -w "$size" -h "$size" -o "hicolor/${size}x${size}/apps/$app.png" "$source"
done
