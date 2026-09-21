#!/usr/bin/env bash
# Maintainer helper: run the packaged build of the latest GREEN main.
#
#   scripts/dogfood.sh status   # which commit is installed vs the latest green main
#   scripts/dogfood.sh update   # brew update && brew upgrade agent-relay, then show status
#
# Nothing is compiled locally and nothing here runs on a normal `relay` invocation: updates go
# through the normal Homebrew flow (which verifies the SHA-256 pinned in the formula).
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

formula_sha() {
  # The formula's artifact names carry the commit: agent-relay-<version>-<sha>-<target>.tar.gz
  brew cat agent-relay 2>/dev/null | sed -n 's/.*agent-relay-[^"]*-\([0-9a-f]\{7\}\)-[a-z0-9_]*-apple-darwin\.tar\.gz.*/\1/p' | head -1
}

status() {
  local bin version sha green formula
  bin="$(command -v relay || true)"
  [ -n "$bin" ] || { echo "relay is not installed: brew install RA1NM4KER/tap/agent-relay"; return 1; }
  version="$(installed_version)"; sha="$(installed_sha)"
  green="$(latest_green_sha)"; formula="$(formula_sha)"
  echo "binary:            $bin"
  echo "installed:         relay $version"
  echo "latest green main: ${green:-unknown (install/authenticate gh to look it up)}"
  echo "tap formula:       ${formula:-unknown}"
  case "$bin" in *target/debug*|*target/release*) echo "WARNING: relay resolves to a cargo build, not the packaged binary";; esac
  if [ -z "$sha" ]; then
    echo "state:             this binary carries no commit id (a tagged release build)"
  elif [ -n "$green" ] && [ "$sha" = "$green" ]; then
    echo "state:             CURRENT (installed build is the latest green main)"
  elif [ -n "$formula" ] && [ "$sha" = "$formula" ]; then
    echo "state:             installed matches the formula; main may have moved (dogfood build pending)"
  elif [ -n "$formula" ]; then
    echo "state:             UPDATE AVAILABLE -> scripts/dogfood.sh update"
  else
    echo "state:             unknown"
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
