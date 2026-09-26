#!/usr/bin/env bash
# Regression guard: dogfood.yml may publish the rolling dev formula, but must never touch the
# stable formula. `Formula/agent-relay.rb` is owned exclusively by tagged releases. It briefly
# was written by dogfood builds and raced with a real release, silently reverting the public
# formula to a dev build. See the OWNERSHIP note in .github/workflows/dogfood.yml.
set -euo pipefail

file=".github/workflows/dogfood.yml"

# Strip full-line comments first so the ownership note itself does not trip the guard. Runtime
# stable-path protection lives in check-dogfood-tap-change-scope.sh; dogfood.yml itself must not
# name the stable formula in executable content.
if grep -v '^[[:space:]]*#' "$file" \
  | grep -E 'Formula/agent-relay\.rb' \
  | grep -q .; then
  echo "FAIL: $file references Formula/agent-relay.rb." >&2
  echo "dogfood.yml may update only Formula/agent-relay-dev.rb; stable remains release-owned." >&2
  exit 1
fi

echo "OK: $file does not reference the stable Homebrew formula."
