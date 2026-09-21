# Maintainer note: dogfooding latest green `main`

Until other people use Agent Relay, **the version the maintainer runs is the latest green `main`**.
There is no separate stable/edge channel and no parallel binary: one command, `relay`, from the
normal Homebrew formula `agent-relay`.

## How it gets there

1. A push to `main` runs CI (fmt, clippy, tests on macOS and Linux).
2. Only if that CI run succeeds, the `Dogfood build` workflow builds the macOS artifacts
   (`aarch64` and `x86_64`), each named `agent-relay-<version>-<sha>-<target>.tar.gz` with a
   `.sha256`, and uploads them to the rolling `dogfood` pre-release (a red CI never publishes).
3. It rewrites `Formula/agent-relay.rb` in `RA1NM4KER/homebrew-tap` to that exact build (version
   `<next patch>-dev.<commit count>`, artifact URLs, pinned SHA-256s). If `main` has moved on
   meanwhile, only the newest green build is published.

## Update and check

```sh
scripts/dogfood.sh update    # brew update && brew upgrade agent-relay, then status
scripts/dogfood.sh status    # installed commit vs latest green main (no network check happens on
                             # normal `relay` runs)
relay --version              # e.g. relay 0.3.1-dev.412+67a815c
```

`relay --version` is `<next patch>-dev.<commit count>+<short sha>` for main builds (`.dirty` is
appended to a local cargo build with uncommitted changes) and a clean `0.3.0`-style version for
tagged releases. The commit id is injected at build time (`RELAY_BUILD_SHA`/`RELAY_BUILD_COUNT`);
`Cargo.toml` is not edited per commit. `latest green main` is
`gh run list --workflow CI --branch main --status success --limit 1`; the installed `+<sha>`
should equal its `headSha` (allow a few minutes for the dogfood build after CI turns green).

## Things that stay stable across updates

- The binary path is `/opt/homebrew/bin/relay`, so the Claude hooks and status line that
  `relay integration claude install` records keep working after every upgrade. Re-run the install
  only if a profile's integration ever points somewhere else.
- Relay state (leases, provider args, ledgers, journals, profiles) is untouched by upgrades; a
  future incompatible schema fails explicitly rather than being wiped.

## One-time setup (already done)

The workflow pushes to the tap with a write-scoped deploy key stored as the `TAP_DEPLOY_KEY`
repository secret (the key is only valid for `RA1NM4KER/homebrew-tap`).
