#!/usr/bin/env bash
# Deterministic regression tests for the dogfood tap path allow-list. No network or existing tap
# checkout is involved: each case uses a fresh local Git repository.
set -euo pipefail

root="$(mktemp -d)"
trap 'rm -rf "$root"' EXIT
guard="$(cd "$(dirname "$0")" && pwd)/check-dogfood-tap-change-scope.sh"

new_tap() {
  local name="$1"
  local tap="$root/$name"
  mkdir -p "$tap/Formula"
  git init -q "$tap"
  git -C "$tap" config user.email test@example.invalid
  git -C "$tap" config user.name test
  printf 'stable\n' >"$tap/Formula/agent-relay.rb"
  printf 'readme\n' >"$tap/README.md"
  git -C "$tap" add Formula/agent-relay.rb README.md
  git -C "$tap" commit -q -m baseline
  printf '%s\n' "$tap"
}

expect_pass() {
  "$guard" "$1" >/dev/null
}

expect_fail() {
  if "$guard" "$1" >/dev/null 2>&1; then
    echo "expected scope guard to reject $1" >&2
    exit 1
  fi
}

# 1. Initial publication: the formula is a new, untracked file.
tap="$(new_tap untracked-dev)"
printf 'dev\n' >"$tap/Formula/agent-relay-dev.rb"
expect_pass "$tap"

# 2. Later publication: it is an ordinary tracked modification.
tap="$(new_tap tracked-dev)"
printf 'old dev\n' >"$tap/Formula/agent-relay-dev.rb"
git -C "$tap" add Formula/agent-relay-dev.rb
git -C "$tap" commit -q -m dev
printf 'new dev\n' >"$tap/Formula/agent-relay-dev.rb"
expect_pass "$tap"

# 3. Stable stays explicitly protected.
tap="$(new_tap stable)"
printf 'changed stable\n' >"$tap/Formula/agent-relay.rb"
expect_fail "$tap"

# 4a. Any unrelated tracked path is rejected.
tap="$(new_tap unrelated-tracked)"
printf 'changed readme\n' >"$tap/README.md"
expect_fail "$tap"

# 4b. Any unrelated untracked path is rejected.
tap="$(new_tap unrelated-untracked)"
printf 'unexpected\n' >"$tap/Formula/unrelated.rb"
expect_fail "$tap"

echo "OK: dogfood tap change scope guard accepts only the dev formula."
