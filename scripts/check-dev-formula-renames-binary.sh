#!/usr/bin/env bash
# Regression guard: the rendered dev Homebrew formula (scripts/render-dev-tap-formula.sh) must
# always install the `relay` binary under a distinct name (`relay-dev`). If it ever installed
# plain `relay`, `agent-relay` (stable) and `agent-relay-dev` (dogfood) could not be installed and
# linked at the same time on one machine -- the whole point of Issue #9 (a separate dogfood/dev
# install path that never touches the stable install) would silently break.
set -euo pipefail

file="scripts/render-dev-tap-formula.sh"

if ! grep -q 'bin.install "relay" => "relay-dev"' "$file"; then
  echo "FAIL: $file no longer renames the relay binary to relay-dev." >&2
  echo "The dev formula must never install a plain 'relay' -- it would collide with the stable" >&2
  echo "formula's binary and defeat the coexistence this issue exists to provide." >&2
  exit 1
fi

echo "OK: the dev formula template keeps relay-dev distinct from stable's relay."
