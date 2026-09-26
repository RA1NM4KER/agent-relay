#!/usr/bin/env bash
# Deterministic regression tests for scripts/update-pages-stable-version.py. No network or
# gh-pages checkout is involved: each case works against a temp copy of a minimal fixture that
# mirrors the real page's nav badge and hero meta-row structure.
set -euo pipefail

root="$(mktemp -d)"
trap 'rm -rf "$root"' EXIT
here="$(cd "$(dirname "$0")" && pwd)"
updater="$here/update-pages-stable-version.py"

fixture() {
  local out="$1"
  local version="$2"
  cat >"$out" <<EOF
<!doctype html>
<html>
<body>
<nav class="nav">
  <div class="nav-inner">
    <a href="#top" class="wordmark">Agent Relay</a>
    <div class="nav-links">
      <a href="https://github.com/RA1NM4KER/agent-relay">GitHub</a>
      <a class="badge" href="https://github.com/RA1NM4KER/agent-relay/releases/tag/${version}">${version}</a>
    </div>
  </div>
</nav>
<main>
  <div class="meta-row">
    <span>${version}</span>
    <span>macOS</span>
    <span>Open source</span>
  </div>
  <p>unrelated ${version}-looking neighbor text stays untouched below</p>
</main>
</body>
</html>
EOF
}

# 1. v0.4.1 -> v0.4.2 updates both visible labels and the release URL.
page="$root/1.html"
fixture "$page" v0.4.1
python3 "$updater" "$page" v0.4.2 >/dev/null
grep -q 'releases/tag/v0.4.2">v0.4.2</a>' "$page"
grep -q '<span>v0.4.2</span>' "$page"
if grep -q 'releases/tag/v0.4.1\|<span>v0.4.1</span>' "$page"; then
  echo "expected the badge and meta-row to no longer say v0.4.1" >&2
  exit 1
fi

# 2. Unrelated page content (including a version-looking neighbor string) is untouched.
before="$root/2-before.html"
after="$root/2-after.html"
fixture "$before" v0.4.1
cp "$before" "$after"
python3 "$updater" "$after" v0.4.2 >/dev/null
if ! grep -q 'unrelated v0.4.1-looking neighbor text stays untouched below' "$after"; then
  echo "expected untouched neighbor text to survive the update" >&2
  exit 1
fi
if [ "$(grep -c 'GitHub</a>' "$after")" != "$(grep -c 'GitHub</a>' "$before")" ]; then
  echo "expected unrelated nav link to be unaffected" >&2
  exit 1
fi

# 3. An invalid/dev tag is rejected and the file is left unchanged.
page="$root/3.html"
fixture "$page" v0.4.1
before_hash="$(shasum -a 256 "$page")"
if python3 "$updater" "$page" v0.4.2-dev.3 >/dev/null 2>&1; then
  echo "expected a dev tag to be rejected" >&2
  exit 1
fi
after_hash="$(shasum -a 256 "$page")"
[ "$before_hash" = "$after_hash" ]

# 4a. Missing expected markup (no nav badge) fails rather than silently no-op-ing.
page="$root/4a.html"
fixture "$page" v0.4.1
sed -i.bak '/class="badge"/d' "$page"
if python3 "$updater" "$page" v0.4.2 >/dev/null 2>&1; then
  echo "expected a missing nav badge to fail the update" >&2
  exit 1
fi

# 4b. Ambiguous markup (two nav badges) fails rather than producing a partial update.
page="$root/4b.html"
fixture "$page" v0.4.1
badge_line="$(grep -n 'class="badge"' "$page" | head -1 | cut -d: -f1)"
sed -i.bak "${badge_line}p" "$page"
if python3 "$updater" "$page" v0.4.2 >/dev/null 2>&1; then
  echo "expected a duplicated nav badge to fail the update" >&2
  exit 1
fi

# 5. Rerunning with the same version is idempotent.
page="$root/5.html"
fixture "$page" v0.4.2
before_hash="$(shasum -a 256 "$page")"
python3 "$updater" "$page" v0.4.2 >/dev/null
after_hash="$(shasum -a 256 "$page")"
[ "$before_hash" = "$after_hash" ]

echo "OK: update-pages-stable-version.py updates only the badge, link, and meta-row, fails closed on bad input, and is idempotent."
