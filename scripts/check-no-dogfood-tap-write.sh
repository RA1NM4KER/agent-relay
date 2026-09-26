#!/usr/bin/env bash
# Regression guard: dogfood.yml may publish the rolling dev formula, but must never touch the
# stable formula. `Formula/agent-relay.rb` is owned exclusively by tagged releases. It briefly
# was written by dogfood builds and raced with a real release, silently reverting the public
# formula to a dev build. See the OWNERSHIP note in .github/workflows/dogfood.yml.
set -euo pipefail

file=".github/workflows/dogfood.yml"

# Strip full-line comments first so the ownership note itself does not trip the guard. The one
# permitted stable-path reference is the read-only `git diff` assertion immediately before the
# dev-only commit; no writer command can name it.
if grep -v '^[[:space:]]*#' "$file" \
  | grep -E 'Formula/agent-relay\.rb' \
  | grep -vFx '          test -z "$(git diff -- Formula/agent-relay.rb)"' \
  | grep -q .; then
  echo "FAIL: $file references Formula/agent-relay.rb." >&2
  echo "dogfood.yml may update only Formula/agent-relay-dev.rb; stable remains release-owned." >&2
  exit 1
fi

echo "OK: $file does not reference the stable Homebrew formula."
