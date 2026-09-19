# Agent Relay

Agent Relay keeps Claude Code working when one account runs out of quota. It moves your session —
same conversation, same context — to a backup account, automatically, so you don't have to notice.

Status: **v0.1.0, standalone, macOS-first.** The handoff, recovery, usage-detection, and Herdr
integration paths are validated live on macOS with Claude Code 2.1.276–2.1.278. Linux builds and
passes the tests in CI but has not been validated live (some process-scan code is macOS-specific).

Requires [Claude Code](https://docs.claude.com/en/docs/claude-code) 2.1.x (2.1.276–2.1.278
validated; a newer 2.1.x patch works, `relay setup` explains if it isn't verified yet) and `git`
(Relay checkpoints a project's git state before a handoff). [Herdr](https://herdr.dev) is optional.

## Install

```sh
git clone <this repository> && cd agent-relay
cargo build --release
# binary: target/release/relay — copy it onto your PATH, e.g. ~/.local/bin
```

(Rust is required to build — the exact toolchain is pinned in `rust-toolchain.toml`; install
[rustup](https://rustup.rs) and it's fetched automatically. `cargo install --path crates/relay-cli`
also works.)

## Quickstart

```sh
relay setup
```

One interactive wizard: it detects Claude Code and (optionally) [Herdr](https://herdr.dev), walks
you through logging in to one or more **isolated** Claude accounts (Relay opens Claude's own
official login — it never sees your password or token), and asks which is primary and which are
fallbacks.

Then, for daily work:

```sh
cd ~/repos/my-project
relay claude
```

That's it. Relay launches Claude under your primary account, tracks the session, and — if that
account genuinely runs out of quota — automatically hands the exact same conversation off to your
next fallback account. You never see a config path, a pane id, or a session UUID; `relay status`
tells you what's going on if you're curious.

Full walkthrough, including what happens when quota actually runs out: **[docs/getting-started.md](docs/getting-started.md)**.

## Everyday commands

| Command | What it does |
|---|---|
| `relay setup` | First-run wizard; safe to re-run any time (detects and reuses what's already there). |
| `relay claude [message]` | Start/attach to your Relay-managed Claude session in the current project. |
| `relay status` | Plain-language summary: project, session, current profile, fallback, usage, Herdr. |
| `relay profiles` | List registered profiles and which is primary/fallback. |
| `relay login <name>` / `relay logout <name>` | Friendly wrappers around Claude's own official login/logout for one isolated profile. |

## Is Herdr required?

**No.** Herdr is optional. `relay claude` works as a standalone Relay-managed session either way;
inside a [Herdr](https://herdr.dev) pane it additionally auto-registers the pane metadata so
Herdr's own `status`/`doctor`/`watch`/`handoff` actions work without you typing anything. See
[Herdr integration](docs/herdr-integration.md).

## Is the usage integration required?

**No**, but it's what makes automatic handoff possible without spending a real API call to check.
`relay setup` offers to install it (`relay integration claude install` under the hood) for every
profile you choose. Without it, `relay watch run` (which `relay claude` doesn't need you to run
directly) reports `UNKNOWN` unless you pass `--probe`, an explicit diagnostic that **spends a real
API request**. See [docs/automatic-handoff.md](docs/automatic-handoff.md) for exactly what it
installs, the detection policy, reset windows, and uninstall.

## Advanced / manual setup

`relay setup`/`relay claude` are a UX layer over the commands below — everything still works
exactly as before for scripting or when you want explicit control:

```sh
# Create two isolated Claude profiles and log in to each yourself (Relay never touches credentials):
mkdir -p -m 700 ~/.config/agent-relay/profiles/alice/claude ~/.config/agent-relay/profiles/bob/claude
CLAUDE_CONFIG_DIR=~/.config/agent-relay/profiles/alice/claude claude   # then /login
CLAUDE_CONFIG_DIR=~/.config/agent-relay/profiles/bob/claude   claude   # then /login

# Adopt them (reference-only; preview first with --dry-run). Two profiles with the same account
# identity are rejected.
relay profile adopt alice --provider claude --config-dir ~/.config/agent-relay/profiles/alice/claude --dry-run
relay profile adopt alice --provider claude --config-dir ~/.config/agent-relay/profiles/alice/claude
relay profile adopt bob   --provider claude --config-dir ~/.config/agent-relay/profiles/bob/claude
relay profile doctor alice

# (Optional but recommended) usage integration, so Relay can detect a real limit for free:
relay integration claude install --profile alice --dry-run
relay integration claude install --profile alice
relay integration claude install --profile bob

# Start work as a Relay-managed writer, then let Relay watch it:
relay launch --profile alice --project-dir ~/repos/foo "your prompt"      # prints the session id
relay watch run --profile alice --fallback bob --project ~/repos/foo --session <session-id>
```

`watch run` is one evaluation, not a daemon; run it from cron or a shell loop. When alice is truly
exhausted it performs the transactional handoff to bob; otherwise it does nothing.

Manual handoff, no usage detection needed:

```sh
relay handoff run --from alice --to bob --project ~/repos/foo --session <session-id>
```

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
- `relay claude`'s interactive experience is `claude attach <id>` on a session Relay launched with
  `claude --bg` — Claude's own documented mechanism for attaching to a background session, not a
  Relay workaround — rather than a plain `claude` process; the first exchange happens before you
  attach (Relay needs an initial message to start the tracked session).
- Outside Herdr, nothing watches usage automatically in the background (Relay is not a daemon);
  automatic handoff happens when Herdr's event fires or when `relay claude`/`relay watch run` is
  invoked again.

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

[Getting started](docs/getting-started.md) · [Automatic handoff](docs/automatic-handoff.md) ·
[Herdr integration](docs/herdr-integration.md) · [architecture](docs/architecture.md) ·
[threat model](docs/security.md) · [research](docs/research.md) · [status](STATUS.md) ·
[changelog](CHANGELOG.md)

## License

Apache-2.0 (see `LICENSE`; dependency notices in `THIRD_PARTY_NOTICES.md`).
