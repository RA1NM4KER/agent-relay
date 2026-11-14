# Advanced use

Canonical: https://ra1nm4ker.github.io/agent-relay/docs/advanced.html

Inspect transactions, script Relay, and work with development builds.

Handbook examples are verified against stable v0.5.2.

Use these tools when you need explicit inspection or scripting. For day-to-day sessions, `relay claude`, `relay codex`, `relay resume`, and `relay switch` are the simpler entry points.

## Inspect state without moving work

```
relay lock status --project-dir /path/to/project
relay watch status --project /path/to/project
relay handoff status <transaction-id> --project-dir /path/to/project
```

Lock status reports the observed lease and orchestration lock. Watch status reads the automation ledger. Handoff status reads a specific transaction journal. Observations are snapshots; they do not authorise bypassing a live owner.

Add `--json` when consuming the CLI from a script. Run `--help` at each subcommand level for the installed schema of arguments.

## Launch a background Claude session

```
relay launch --profile claude-work --project-dir /path/to/project "Review the failing tests"
```

This is the lower-level background Claude launch path. It prints session information rather than attaching your terminal. Use a supervised interactive session for the usual automatic handoff experience; stale headless Claude usage snapshots cannot establish exhaustion.

## Evaluate automatic handoff once

```
relay watch run --profile claude-work --fallback claude-backup --project /path/to/project --session <session-id> --dry-run
```

This evaluates the configured evidence once. The dry run reports the decision without performing a handoff or persisting ledger state. Without `--dry-run`, it can transfer an exhausted session through the normal transaction.

It is not a background daemon. The optional `--probe` diagnostic can spend a real API request; it is not required for normal structured detection.

## Run an explicit Claude handoff

```
relay handoff run --from claude-work --to claude-backup --project /path/to/project --session <claude-session-id>
```

The lower-level command performs the journaled source-stop, transfer, target-launch, and verification flow. For everyday switching, including cross-provider moves, use `relay switch`.

Read a transaction afterward with `relay handoff status`. For an interruption, follow the [recovery steps](https://ra1nm4ker.github.io/agent-relay/docs/troubleshooting.html#recovery).

## Inspect a Claude transcript conflict

```
relay session conflict inspect --source-profile claude-work --target-profile claude-backup --project-dir /path/to/project --session-id <claude-session-id>
```

The inspection classifies the target copy: missing, identical, stale, divergent, or active. To preview a resolution:

```
relay session conflict resolve --source-profile claude-work --target-profile claude-backup --project-dir /path/to/project --session-id <claude-session-id>
```

Resolve previews unless `--yes` is supplied. Replacing a proven stale ancestor preserves a backup. Discarding a genuinely divergent copy requires an additional explicit flag because it can contain unique turns. Review `relay session conflict resolve --help` and the preview before acting.

`relay session stage-transfer` is also available for deliberately staging a stopped Claude conversation. It does not replace the complete handoff transaction or copy credentials. Consult its help before use.

## Carry advisory working notes

```
relay state show --session <relay-session-id>
relay state update '{"current_subtask":"Verify the fix","next_actions":["Run the focused tests"]}'
```

Agents update working state explicitly at meaningful checkpoints. Relay does not generate notes through an extra model call. Use `relay state update -` to read JSON from stdin; only the current active writer may update a session’s notes.

Decisions, failed attempts, and relevant files are appended within fixed bounds. Supplied next actions replace the old list. Oversized updates are rejected. These notes are advisory context for state continuation; they never determine ownership, provider eligibility, exhaustion, or permission. Missing or corrupt notes do not block the ownership transaction.

## Repository-specific GitHub tasks

`relay task` is an explicit GitHub workflow currently bound to `RA1NM4KER/agent-relay`, not a general queue for arbitrary repositories and not a multi-agent scheduler. It does not start a provider.

| Subcommand | Effect |
| --- | --- |
| `task next` | Inspect open issues explicitly labelled `relay:ready`. |
| `task claim` | Create an exclusive remote claim ref, then publish claim visibility on GitHub. |
| `task result` | Record bounded local result evidence for an existing claim. |
| `task apply-result` | Apply the supported GitHub lifecycle transition after verifying the claim and result evidence. |

Claiming and applying a result modify remote GitHub state. These commands do not close issues, merge pull requests, or delete claim refs. Consult `relay task <subcommand> --help` and the [tagged implementation](https://github.com/RA1NM4KER/agent-relay/blob/v0.5.2/crates/relay-cli/src/commands/task.rs) before using this repository-specific workflow.

## Build from source

Install Rust through rustup; the repository pins its toolchain. To build the version this handbook documents:

```
git clone https://github.com/RA1NM4KER/agent-relay
cd agent-relay
git checkout v0.5.2
cargo build --release
```

The binaries are `target/release/relay` and `target/release/relay-herdr-plugin`. Put the Relay binary on your PATH. The optional Herdr helper must sit beside it if you want setup to offer that integration.

## Stable and development builds

Homebrew’s `agent-relay` formula follows tagged stable releases. The repository’s `main` branch can contain newer behaviour that has not shipped in that release.

The development channel uses a separate formula and executable:

```
brew install RA1NM4KER/tap/agent-relay-dev
relay-dev --version
```

Use `relay-dev --help` to discover the current development command set. Stable v0.5.2 already includes integration refresh, execution modes, advisory working state, and the repository-specific task commands. A development build may change their behavior; this handbook does not treat unreleased work as stable.

**Setup binds integrations to the binary you run.**`relay-dev setup` points managed integrations at the development binary; `relay setup` points them back at stable. Existing provider processes need a new attach to pick up changed integration assets. Review the maintainer guide before switching a daily workflow.

[Development build and channel guide ↗](https://github.com/RA1NM4KER/agent-relay/blob/main/docs/maintainer-dogfood.md)

## Read the implementation guides

- [Architecture and ownership](https://github.com/RA1NM4KER/agent-relay/blob/v0.5.2/docs/architecture.md) — locks, journal phases, and provider boundaries.
- [Automatic handoff](https://github.com/RA1NM4KER/agent-relay/blob/v0.5.2/docs/automatic-handoff.md) — structured signals, eligibility, and reset policy.
- [Security model](https://github.com/RA1NM4KER/agent-relay/blob/v0.5.2/docs/security.md) — process identity, credential isolation, and recovery.
- [Changelog](https://github.com/RA1NM4KER/agent-relay/blob/main/CHANGELOG.md) — released changes and work in progress.
