#!/usr/bin/env bash
# Renders Formula/agent-relay-dev.rb from the newest build on the rolling `dogfood` GitHub
# pre-release (see .github/workflows/dogfood.yml). Read-only against GitHub — it does not push,
# commit, or touch RA1NM4KER/homebrew-tap. The dogfood workflow invokes this renderer after each
# green `main` build and commits only Formula/agent-relay-dev.rb; this local command is useful for
# inspecting the generated formula. Formula/agent-relay.rb remains a deliberate stable-release
# maintainer action.
#
# Usage: scripts/render-dev-tap-formula.sh [output-path]
#   (defaults to packaging/homebrew/agent-relay-dev.rb)
set -euo pipefail

REPO="${RELAY_REPO:-RA1NM4KER/agent-relay}"
OUT="${1:-packaging/homebrew/agent-relay-dev.rb}"

command -v gh >/dev/null 2>&1 || { echo "gh CLI is required" >&2; exit 1; }

assets_json="$(gh release view dogfood --repo "$REPO" --json assets --jq '.assets')"

# Newest build = the aarch64 archive (not its .sha256 sidecar) with the latest createdAt.
newest_name="$(echo "$assets_json" | python3 -c '
import json, sys
assets = json.load(sys.stdin)
archives = [a for a in assets if a["name"].endswith("-aarch64-apple-darwin.tar.gz")]
archives.sort(key=lambda a: a["createdAt"])
print(archives[-1]["name"])
')"

# "agent-relay-<version>-<short>-aarch64-apple-darwin.tar.gz" -> "<version>-<short>"
stem="${newest_name#agent-relay-}"
stem="${stem%-aarch64-apple-darwin.tar.gz}"
version="${stem%-*}"
short="${stem##*-}"

base_url="https://github.com/$REPO/releases/download/dogfood"
arm_name="agent-relay-${stem}-aarch64-apple-darwin.tar.gz"
intel_name="agent-relay-${stem}-x86_64-apple-darwin.tar.gz"

fetch_sha() {
  curl -fsSL "$base_url/$1.sha256" | awk '{print $1}'
}
arm_sha="$(fetch_sha "$arm_name")"
intel_sha="$(fetch_sha "$intel_name")"

mkdir -p "$(dirname "$OUT")"
cat >"$OUT" <<RUBY
class AgentRelayDev < Formula
  desc "Agent Relay, bleeding-edge dev build (latest green main, commit ${short})"
  homepage "https://github.com/${REPO}"
  # Tracks the rolling dogfood pre-release, not a tagged version. The dogfood workflow renders
  # and updates this dev-only formula after each green main build; Formula/agent-relay.rb remains
  # owned by deliberate stable releases.
  version "${version}"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "${base_url}/${arm_name}"
      sha256 "${arm_sha}"
    end
    on_intel do
      url "${base_url}/${intel_name}"
      sha256 "${intel_sha}"
    end
  end

  # Installed under distinct names so stable (agent-relay) and dev (agent-relay-dev) coexist:
  # both formulas can be installed and linked at the same time, on the same machine, with no
  # PATH ordering games. "relay" always means stable; "relay-dev" always means this dogfood build.
  def install
    bin.install "relay" => "relay-dev"
    bin.install "relay-herdr-plugin" => "relay-herdr-plugin-dev"
  end

  test do
    system "#{bin}/relay-dev", "--version"
  end
end
RUBY

echo "Rendered $OUT from dogfood build ${version}+${short}" >&2
echo "$OUT"
