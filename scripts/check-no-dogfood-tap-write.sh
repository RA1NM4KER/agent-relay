#!/usr/bin/env bash
# Regression guard: dogfood.yml must never write to the Homebrew tap. That file
# (Formula/agent-relay.rb in RA1NM4KER/homebrew-tap) is owned exclusively by tagged releases
# (release.yml, published deliberately by a maintainer) — a green push to main must not touch it.
# It briefly did, and raced with a real tagged release, silently reverting the public formula back
# to a dev build. See the OWNERSHIP note in .github/workflows/dogfood.yml.
set -euo pipefail

file=".github/workflows/dogfood.yml"

# Strip full-line comments first so the OWNERSHIP note explaining this policy (which necessarily
# names the forbidden things) doesn't trip its own guard; only executable YAML/shell content below
# is checked.
if grep -v '^[[:space:]]*#' "$file" | grep -qiE 'homebrew-tap|Formula/agent-relay\.rb|TAP_DEPLOY_KEY'; then
  echo "FAIL: $file references the Homebrew tap again." >&2
  echo "dogfood.yml (every green push to main) must never write to the tap formula;" >&2
  echo "that is release.yml's job alone, triggered by a deliberate version tag." >&2
  exit 1
fi

echo "OK: $file does not reference the Homebrew tap."
