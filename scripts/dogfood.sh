#!/usr/bin/env bash
# Maintainer helper: check the installed (tagged) release against the tap, and against the latest
# green `main` dogfood build.
#
#   scripts/dogfood.sh status   # installed tagged version vs. tap vs. latest green main
#   scripts/dogfood.sh update   # brew update && brew upgrade agent-relay (tagged releases only)
#
# Since v0.4.0 the tap only ever serves tagged releases (see docs/maintainer-dogfood.md) — `update`
# cannot fetch a `main`-only build. To run exactly what's on `main`, download the matching artifact
# from the rolling `dogfood` GitHub pre-release by hand; see docs/maintainer-dogfood.md.
set -euo pipefail

REPO="RA1NM4KER/agent-relay"

installed_version() { relay --version 2>/dev/null | awk '{print $2}'; }
installed_sha()     { installed_version | sed -n 's/.*+\([0-9a-f]\{7,\}\).*/\1/p'; }

latest_green_sha() {
  if command -v gh >/dev/null 2>&1; then
    gh run list --repo "$REPO" --workflow CI --branch main --status success --limit 1 \
      --json headSha -q '.[0].headSha[0:7]' 2>/dev/null || true
  fi
}

status() {
  local bin version sha green
  bin="$(command -v relay || true)"
  [ -n "$bin" ] || { echo "relay is not installed: brew install RA1NM4KER/tap/agent-relay"; return 1; }
  version="$(installed_version)"; sha="$(installed_sha)"
  green="$(latest_green_sha)"
  echo "binary:            $bin"
  echo "installed:         relay $version"
  echo "latest green main: ${green:-unknown (install/authenticate gh to look it up)}"
  case "$bin" in *target/debug*|*target/release*) echo "WARNING: relay resolves to a cargo build, not the packaged binary";; esac
  if [ -z "$sha" ]; then
    echo "state:             installed is a tagged release build; scripts/dogfood.sh update checks"
    echo "                   for a newer tagged release (the tap does not carry main-only builds)."
    echo "                   For the latest green main itself, see 'Getting a bleeding-edge build'"
    echo "                   in docs/maintainer-dogfood.md."
  elif [ -n "$green" ] && [ "$sha" = "$green" ]; then
    echo "state:             CURRENT (installed build is the latest green main)"
  else
    echo "state:             installed is a non-release build at a different commit than the"
    echo "                   latest green main; see docs/maintainer-dogfood.md to refresh it."
  fi
}

case "${1:-status}" in
  status) status ;;
  update)
    brew update
    brew upgrade agent-relay || true
    status
    ;;
  *) echo "usage: $0 status|update" >&2; exit 2 ;;
esac
