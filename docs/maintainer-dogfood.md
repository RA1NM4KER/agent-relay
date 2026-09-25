# Maintainer note: dogfood builds of `main`

**Since v0.4.0, the Homebrew tap (`agent-relay` formula) serves tagged stable releases only.**
Before v0.4.0 it tracked the latest green `main` build instead; that tracking was removed because
it raced with, and could silently overwrite, a real tagged release the moment anyone pushed to
`main` again (a real incident — see the v0.4.0 changelog and `.github/workflows/dogfood.yml`'s
OWNERSHIP note). `Formula/agent-relay.rb` in `RA1NM4KER/homebrew-tap` is now written only by a
maintainer deliberately cutting a tagged release (`.github/workflows/release.yml`'s artifacts,
published by hand today — see the release runbook). `scripts/check-no-dogfood-tap-write.sh` runs in
CI and fails the build if the dogfood workflow ever references the tap again.

## Getting a bleeding-edge (post-tag, pre-release) build

There is no `brew upgrade` path to a dev build anymore. To run exactly what's on `main`:

1. A push to `main` runs CI (fmt, clippy, tests on macOS and Linux).
2. Only if that CI run succeeds, the `Dogfood build` workflow builds the macOS artifacts
   (`aarch64` and `x86_64`), each named `agent-relay-<version>-<sha>-<target>.tar.gz` with a
   `.sha256`, and uploads them to the rolling `dogfood` GitHub pre-release (a red CI never
   publishes; if `main` moved on before publish, only the newest green build is kept).
3. Download the tarball for your architecture from that pre-release (`gh release download dogfood
   --repo RA1NM4KER/agent-relay --pattern '*-<your-target>.tar.gz'`), verify its `.sha256`, and run
   the extracted `relay` binary directly — or build from source at that commit. Neither replaces
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

This repository secret (a write-scoped deploy key valid only for `RA1NM4KER/homebrew-tap`) was
used by the old dogfood-to-tap step. No workflow uses it today; a maintainer publishing a tagged
release currently updates the tap formula by hand (see the release runbook) rather than through
CI.
