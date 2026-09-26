# Maintainer note: dogfood builds of `main`

**Since v0.4.0, the Homebrew tap (`agent-relay` formula) serves tagged stable releases only.**
Before v0.4.0 it tracked the latest green `main` build instead; that tracking was removed because
it raced with, and could silently overwrite, a real tagged release the moment anyone pushed to
`main` again (a real incident — see the v0.4.0 changelog and `.github/workflows/dogfood.yml`'s
OWNERSHIP note). `Formula/agent-relay.rb` in `RA1NM4KER/homebrew-tap` is now written only by a
maintainer deliberately cutting a tagged release (`.github/workflows/release.yml`'s artifacts,
published by hand today — see the release runbook). `scripts/check-no-dogfood-tap-write.sh` runs in
CI and fails the build if the dogfood workflow ever references the stable formula.

## Getting a bleeding-edge (post-tag, pre-release) build

A push to `main` runs CI (fmt, clippy, tests on macOS and Linux); only if that succeeds does the
`Dogfood build` workflow build the macOS artifacts (`aarch64` and `x86_64`), each named
`agent-relay-<version>-<sha>-<target>.tar.gz` with a `.sha256`, and upload them to the rolling
`dogfood` GitHub pre-release (a red CI never publishes; if `main` moved on before publish, only the
newest green build is kept). Two ways to run that build:

### Via Homebrew (`relay-dev`)

```sh
brew install RA1NM4KER/tap/agent-relay-dev   # first time
brew upgrade agent-relay-dev                 # later, to pick up a newer green main
relay-dev setup                              # or: relay-dev integration claude install --all
```

`agent-relay-dev` is a **separate** formula from stable `agent-relay` and installs a **separate**
binary, `relay-dev` (never `relay`) — installing, upgrading, or running one never touches the
other, no PATH ordering needed. After each successful `main` build, the dogfood workflow publishes
the immutable artifacts, computes their checksums, renders `Formula/agent-relay-dev.rb`, and
commits **only that path** to the tap. It asserts that `Formula/agent-relay.rb` is unchanged before
committing; `scripts/check-no-dogfood-tap-write.sh` and
`scripts/check-dev-formula-renames-binary.sh` guard the same stable-ownership and distinct-binary
invariants in CI. Therefore, once that green workflow completes, `brew upgrade agent-relay-dev`
receives that build — no maintainer formula-render/commit/push step is hidden in the user flow.

Since `relay setup`/`relay integration claude install` always act on whichever executable is
actually running (`std::env::current_exe()`), `relay-dev setup` refreshes every registered
Relay-managed Claude profile's hooks and status line to `relay-dev`; plain `relay setup` moves all
of them back to stable. `relay-dev integration claude status --profile <name>` (or plain `relay ...
status`) reports the mismatch honestly either way — no separate "channel" concept exists in
`relay-core`, it's just whichever executable is currently running. `--all` (on
`install`/`status`/`uninstall`) likewise targets every registered Claude profile. Currently-running
Claude/Codex sessions are unaffected until they're restarted or resumed — this never hot-patches a
live process.

### Manual download (no new Homebrew tap entry needed)

Download the tarball for your architecture from the `dogfood` pre-release (`gh release download
dogfood --repo RA1NM4KER/agent-relay --pattern '*-<your-target>.tar.gz'`), verify its `.sha256`, and
run the extracted `relay` binary directly — or build from source at that commit. Neither replaces
your Homebrew-installed `relay`; keep them at different paths if you want both.

```sh
scripts/dogfood.sh status    # installed (tagged) version vs. latest green main's dogfood build
relay --version               # e.g. relay 0.4.0 (tagged) or 0.4.1-dev.412+67a815c (dev/local build)
```

`relay --version` is `<next patch>-dev.<commit count>+<short sha>` for non-release builds (`.dirty`
is appended to a local cargo build with uncommitted changes) and a clean `0.4.0`-style version only
for a build made with `RELAY_RELEASE_BUILD=1` (what `release.yml` sets). The commit id is injected
at build time (`RELAY_BUILD_SHA`/`RELAY_BUILD_COUNT`); `Cargo.toml` is not edited per commit.

## Things that stay stable across updates

- The binary path is `/opt/homebrew/bin/relay`, so the Claude hooks and status line that
  `relay integration claude install` records keep working after every upgrade. Re-run the install
  only if a profile's integration ever points somewhere else.
- Relay state (leases, provider args, ledgers, journals, profiles) is untouched by upgrades; a
  future incompatible schema fails explicitly rather than being wiped.

## `TAP_DEPLOY_KEY`

This repository secret is a write-scoped deploy key valid only for `RA1NM4KER/homebrew-tap`. The
dogfood workflow uses it only after artifact publication, then allows and commits only
`Formula/agent-relay-dev.rb`. If it is missing or invalid, that workflow fails rather than claiming
the build is Homebrew-consumable; configure it before relying on the automatic dogfood path. Stable
releases still update `Formula/agent-relay.rb` deliberately and manually.
