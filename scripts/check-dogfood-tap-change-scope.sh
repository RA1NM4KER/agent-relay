#!/usr/bin/env bash
# Validates the complete (tracked, staged, and untracked) worktree delta a dogfood publication is
# allowed to commit to the Homebrew tap. It is deliberately a path allow-list, not merely a
# stable-formula check: the sole permissible changed path is Formula/agent-relay-dev.rb.
set -euo pipefail

tap_root="${1:?usage: check-dogfood-tap-change-scope.sh TAP_ROOT}"
dev_formula="Formula/agent-relay-dev.rb"
stable_formula="Formula/agent-relay.rb"

fail() {
  echo "FAIL: dogfood publication may change only ${dev_formula}: $*" >&2
  exit 1
}

git -C "$tap_root" rev-parse --is-inside-work-tree >/dev/null

# Keep stable explicit even though the complete status allow-list below would reject it too. Check
# both index and worktree so a future caller cannot silently stage it before validation.
git -C "$tap_root" diff --quiet -- "$stable_formula" \
  || fail "stable formula ${stable_formula} has a worktree modification"
git -C "$tap_root" diff --cached --quiet -- "$stable_formula" \
  || fail "stable formula ${stable_formula} has a staged modification"

allowed_changes=0
while IFS= read -r -d '' entry; do
  status="${entry:0:2}"
  path="${entry:3}"
  # Renames/copies have a second NUL-delimited path in porcelain v1 -z. Reject the first record
  # immediately; the remaining pathname will also fail rather than being treated as a filename.
  case "$status" in
    *R*|*C*) fail "rename/copy status is not permitted (${status} ${path})" ;;
  esac
  [ "$path" != "$stable_formula" ] \
    || fail "stable formula ${stable_formula} has status ${status}"
  [ "$path" = "$dev_formula" ] || fail "unexpected changed path ${path}"
  allowed_changes=$((allowed_changes + 1))
done < <(git -C "$tap_root" status --porcelain=v1 -z --untracked-files=all)

if [ "$allowed_changes" -gt 1 ]; then
  fail "${dev_formula} appeared more than once in worktree status"
fi

echo "OK: tap worktree contains only an optional ${dev_formula} change."
