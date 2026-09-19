# Agent Relay

Agent Relay is a local-first orchestrator for **explicitly** handing an active Claude Code session
from one authenticated profile to another, with single-writer safety. It is continuity, not hidden
account rotation: it never stores credentials, never pools quota, and never switches profiles
unless you run it.

Status: **v0.1.0, standalone, macOS-first.** The handoff, recovery and usage-detection paths are
validated live on macOS with Claude Code 2.1.276 and 2.1.277. Linux builds and passes the tests in
CI but has not been validated live (some process-scan code is macOS-specific).

## Prerequisites

- Rust (the exact toolchain is pinned in `rust-toolchain.toml`; install [rustup](https://rustup.rs)
  and it is fetched automatically).
- [Claude Code](https://docs.claude.com/en/docs/claude-code) **2.1.x, currently 2.1.276–2.1.277**
  (`claude --version`). Other release lines are refused; a newer 2.1.x patch works for handoff but
  the usage integration asks for `--allow-unverified-version`.
- Two Claude accounts, each logged in inside its **own isolated config directory** (see below).
- `git` (Relay checkpoints a project's git state before a handoff).

## Build

```sh
git clone <this repository> && cd agent-relay
cargo build --release
# binary: target/release/relay   (copy it onto your PATH, e.g. ~/.local/bin)
```

`cargo install --path crates/relay-cli` also works. The installed hook commands embed the absolute
path of the `relay` binary that installed them, so install the integration from the binary you
intend to keep.

## Quickstart

1. **Create two isolated Claude profiles and log in to each** (Relay never touches credentials):

   ```sh
   mkdir -p -m 700 ~/.config/agent-relay/profiles/alice/claude ~/.config/agent-relay/profiles/bob/claude
   CLAUDE_CONFIG_DIR=~/.config/agent-relay/profiles/alice/claude claude   # then /login
   CLAUDE_CONFIG_DIR=~/.config/agent-relay/profiles/bob/claude   claude   # then /login
   ```

2. **Adopt them** (reference-only; preview first with `--dry-run`):

   ```sh
   relay profile adopt alice --provider claude --config-dir ~/.config/agent-relay/profiles/alice/claude --dry-run
   relay profile adopt alice --provider claude --config-dir ~/.config/agent-relay/profiles/alice/claude
   relay profile adopt bob   --provider claude --config-dir ~/.config/agent-relay/profiles/bob/claude
   relay profile doctor alice
   ```

   Two profiles with the same account identity are rejected.

3. **(Optional but recommended) install the usage integration** so Relay can detect a real limit:

   ```sh
   relay integration claude install --profile alice --dry-run
   relay integration claude install --profile alice
   relay integration claude install --profile bob
   ```

4. **Start work as a Relay-managed writer, then let Relay watch it:**

   ```sh
   relay launch --profile alice --project-dir ~/repos/foo "your prompt"      # prints the session id
   relay watch run --profile alice --fallback bob --project ~/repos/foo --session <session-id>
   ```

   `watch run` is one evaluation, not a daemon; run it from cron or a shell loop. When alice is
   truly exhausted it performs the transactional handoff to bob; otherwise it does nothing.

Manual handoff, no usage detection needed:

```sh
relay handoff run --from alice --to bob --project ~/repos/foo --session <session-id>
```

## Is the integration required?

**Optional.** Manual `relay handoff run` and `relay launch` work without it. Without the integration,
`relay watch run` has no free usage signal and reports `UNKNOWN` (never handing off) unless you pass
`--probe`, an explicit diagnostic that **spends a real API request**. Install it per profile if you
want automatic handoff. See [docs/automatic-handoff.md](docs/automatic-handoff.md) for exactly what it
installs, the detection policy, reset windows and uninstall.

## Recovery and conflicts

- `relay lock status --project-dir DIR` shows the writer lease and whether a transaction is running.
- `relay handoff status` and the journal under Relay's state directory record every attempt.
- `relay watch run` recovers an interrupted transaction automatically before doing anything else.
  You can also run it by hand: `relay recover <transaction-id> --project-dir DIR`, and, only after
  confirming no target process is resuming the session, `relay recover <id> --project-dir DIR --acknowledge`.
- If the target profile holds an older or different copy of the transcript, inspect it and resolve it
  through Relay (never delete transcripts by hand):

  ```sh
  relay session conflict inspect  --source-profile A --target-profile B --project-dir DIR --session-id ID
  relay session conflict resolve  --source-profile A --target-profile B --project-dir DIR --session-id ID [--yes]
  relay session conflict rollback --target-profile B --project-dir DIR --session-id ID
  ```

  Resolve previews unless `--yes`; a stale ancestor is replaced (with a backup) after `--yes`; a
  genuinely divergent copy additionally needs `--force-discard-divergent`.

## Limitations

- One writer per project; a handoff is refused while the source profile has any live Claude process.
- The statusline usage snapshot only refreshes in interactive sessions; headless sessions leave it
  stale, and stale means `UNKNOWN` (no handoff).
- No automatic fail-back or quota pooling.
- A handoff spends one small real API turn to verify the target session.
- Herdr's own session detection only reaches the default `~/.claude`; an isolated profile's pane
  needs an explicit `relay_session_id` token until Herdr's built-in Claude integration supports
  `CLAUDE_CONFIG_DIR` (see [Herdr integration](docs/herdr-integration.md)).
- Claude's transcript layout and `--resume` behavior are not a stable public API; Relay gates on
  validated versions and fails closed.

## Herdr integration

An optional, thin plugin (`plugins/herdr/herdr-plugin.toml`) wires Relay's existing `status`,
`doctor`, `recovery`, `watch`, and manual `handoff` behind Herdr actions and an automatic
`pane.agent_status_changed` event, so a Claude pane inside [Herdr](https://herdr.dev) can surface
and trigger them without a separate terminal. Herdr never gains writer authority — the plugin only
maps a pane to a registered profile (via an explicit `relay_profile`/`relay_profile_fallback`
token, never guessed) and invokes the same `relay --json` CLI this README documents. Install with
`relay integration herdr install` (requires `herdr plugin link` support, i.e. running from this
repo checkout). Details, live-validation results, and current limitations:
[docs/herdr-integration.md](docs/herdr-integration.md).

## Security model

Relay references provider-owned config directories; it never reads, copies or stores OAuth tokens,
cookies, API keys or Keychain data, and removes credential-override environment variables before
running Claude. Profile directories must be private (0700), non-symlinked and user-owned. All state
writes are atomic, journals and logs hold only states, ids and filenames (never transcript contents
or provider error bodies), and the usage integration records only closed structured metadata.
Every mutating step is opt-in and previewable. Details: [docs/security.md](docs/security.md).

## More

[Automatic handoff](docs/automatic-handoff.md) · [Herdr integration](docs/herdr-integration.md) ·
[architecture](docs/architecture.md) · [threat model](docs/security.md) ·
[research](docs/research.md) · [status](STATUS.md) · [changelog](CHANGELOG.md)

## License

Apache-2.0 (see `LICENSE`; dependency notices in `THIRD_PARTY_NOTICES.md`).
