#!/usr/bin/env bash
# Runs the client in plain libadwaita, as on a desktop with no theme of
# Spoolframe's chosen: Preferences' "Follow system", with the desktop's
# light or dark preference faked by libadwaita's own debug variable. Your
# settings and your desktop's theme are untouched: the app reads a
# throwaway config folder, deleted when it quits. Saved servers and
# sign-ins are in the data folder and keyring, so they carry over; the
# server used last does not, so it opens on Choose a server.
#
#   scripts/adwaita.sh          light
#   scripts/adwaita.sh dark     dark
#   scripts/adwaita.sh light -v any further arguments go to the app
#
# The binary is target/release/spoolframe-linux, else spoolframe-linux on
# the PATH; SPOOLFRAME_LINUX overrides it.
set -euo pipefail

case "${1:-light}" in
  light) scheme=prefer-light ;;
  dark) scheme=prefer-dark ;;
  *)
    echo "usage: $0 [light|dark] [app arguments…]" >&2
    exit 2
    ;;
esac
shift || true

here="$(cd "$(dirname "$0")/.." && pwd)"
app="${SPOOLFRAME_LINUX:-}"
if [ -z "$app" ]; then
  if [ -x "$here/target/release/spoolframe-linux" ]; then
    app="$here/target/release/spoolframe-linux"
  else
    app="$(command -v spoolframe-linux)" || {
      echo "no spoolframe-linux: build it (cargo build --release) or set SPOOLFRAME_LINUX" >&2
      exit 1
    }
  fi
fi

cfg="$(mktemp -d)"
trap 'rm -rf "$cfg"' EXIT
mkdir -p "$cfg/spoolframe-relm4"
printf '[appearance]\ntheme=system\n' > "$cfg/spoolframe-relm4/settings.ini"

XDG_CONFIG_HOME="$cfg" ADW_DEBUG_COLOR_SCHEME="$scheme" "$app" "$@"
