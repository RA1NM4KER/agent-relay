# Agent Relay

Keep your coding agent moving when one account hits its usage limit. Agent Relay supervises
isolated Claude and Codex profiles, keeps exactly one owner per conversation, and moves the work to the next eligible
profile in one priority order. Claude → Claude continues the same native Claude session; anything
involving Codex continues from a Relay state bundle in a new session — never the same native
conversation across the two CLIs.

Status: **v0.4.1, macOS-first.** The handoff, recovery, and usage-detection paths are validated
live on macOS with Claude Code 2.1.276–2.1.278; Codex support was checked live against Codex CLI
0.155.0 (structured usage reads, thread verification, Codex → Claude handoff by hand, pre-launch
fallback from an exhausted Codex profile). Automatic handoff *from a real Codex exhaustion
mid-session* has now been observed live: detection and evaluation ran correctly against a genuine
exhaustion, though that specific incident had no eligible fallback available to hand off to (every
configured fallback was independently exhausted at the same time) — see
[docs/automatic-handoff.md](docs/automatic-handoff.md#no-eligible-fallback) for the diagnostics
that case now produces. A full live handoff away from a real Codex exhaustion (to a healthy
fallback) is still covered by fake-provider tests rather than observed live. Linux builds and
passes the tests in CI but is not yet live-supported or validated (some process-scan code is
macOS-specific). Relay runs entirely standalone — no separate tool required; an optional
[Herdr](https://herdr.dev) integration exists for people already using Herdr (see
["Is Herdr required?"](#is-herdr-required)), also validated live.

## Install

```sh
brew install RA1NM4KER/tap/agent-relay
```

This installs the latest tagged release. To run the bleeding-edge `main` branch instead, build it
from source (see [below](#advanced--manual-setup--building-from-source)), or see
[docs/maintainer-dogfood.md](docs/maintainer-dogfood.md) for the maintainer's own dogfood-build
workflow.

## Use

```sh
relay setup                 # once: profiles for the providers you use, one priority order
cd ~/repos/my-project

relay claude                # start a new managed conversation with Claude ...
relay codex                 # ... or with Codex (peer entry points — use whichever you want)

relay claude --resume       # adopt an OLD Claude conversation (pick it in Claude's own picker)
relay resume                # reopen a closed conversation of this project
relay switch                # choose a profile from a list ...
relay switch <profile>      # ... or name it: move a conversation to another profile

relay status                # fast local snapshot: what is going on in this project
relay status --live         # the same, plus a fresh provider auth/usage check (slower)
relay profiles              # your profiles, their order and login state
```

`relay setup` is a one-time wizard: it detects the coding-agent CLIs you have (Claude Code and/or
the Codex CLI — either one is enough), walks you through logging in to one or more **isolated**
profiles (Relay opens each provider's own official login — it never sees your password or token),
and asks for **one global priority order**: which profile is primary and which are fallbacks.
(If you already use [Herdr](https://herdr.dev), setup also offers to wire in that optional
integration — see ["Is Herdr required?"](#is-herdr-required); most people don't need it.)
Any provider can be primary; a Codex primary with a Claude
fallback is as natural as the reverse.

The mental model is simple:

```
relay claude = Claude + Relay supervision     (new managed conversation)
relay codex  = Codex  + Relay supervision     (new managed conversation)
relay claude --resume = bring an existing Claude conversation under Relay (same session, in place)
relay resume = reopen a closed (dormant) conversation of this project
relay switch [<profile>] = explicitly move a conversation (bare: pick from a list)
```

**Start through Relay, or bring an existing conversation under Relay later.** Switch accounts and
providers without leaving your coding workflow:

- `relay claude` / `relay codex` start through Relay.
- `relay claude --resume` adopts an **old** Claude conversation: Relay opens Claude's own resume
  picker (or resumes the id you give it), then manages exactly the conversation you chose — no fork,
  no second conversation. It is not the same as `relay resume`, which continues a conversation Relay
  *already* manages.
- Inside a running Claude session, `/relay status`, `/relay switch [profile]` and `/relay adopt`
  are answered by Relay itself (no model turn, nothing typed into the conversation).
  `/relay adopt` brings a **live** unmanaged Claude conversation under Relay without restarting it;
  `/relay switch` moves the conversation and your terminal follows it.
- Inside a Relay-managed Codex session, use **`$relay status`**, **`$relay doctor`**,
  **`$relay why`**, **`$relay history`**, or **`$relay switch <profile>`**. Relay installs a
  profile-local skill when attaching Codex; Codex executes the CLI through a model/tool turn.
  `/relay` and `/relay:doctor` are not Codex slash commands. The skill scopes queries to the
  supervisor's project and session; `$relay switch <profile>` asks the supervising Relay process
  itself, over the same control channel Claude's in-agent switch uses, to safely stop Codex and
  continue the conversation on the new profile — no copy-pasting a command into another terminal.
  It falls back to printing that manual command only when no verified Relay supervisor can be
  reached. Live Codex adoption is not offered — use `relay codex` to start through Relay.

- **Relay supervises conversations, not repositories.** A project can hold many *Relay sessions*
  — each one a conversation with its own stable id, running on one profile at a time. Two sessions
  may run on the same profile; one may be Claude and another Codex; some may be closed. Each *live*
  session has exactly one owner; a closed one has none (it is remembered, not owned).
  Relay does not serialize edits to your files: two agents in one working tree can touch the same
  files, so it is your call when running several at once makes sense.
- `relay claude` always starts a **new** Relay session under your highest-priority Claude profile,
  tracked by Relay from the start; Relay drops you straight into Claude (you type the first message
  there — Relay never prompts for it). Other sessions of the project, active or not, are none of its
  business: it never stops, replaces or blocks on them (`--new` is a deprecated no-op).
- `relay codex` is the same thing for Codex: a new Relay session under your highest-priority
  configured *Codex* profile. `--profile <name>` overrides the choice and must name a profile of
  that provider; `--no-attach` behaves as it does for `relay claude`.
- `relay resume` reopens a **closed** session of this project on the profile it last ran on. With
  one it just opens it; with several you choose from a list (or pass `--session <id>`); one that
  is active in another terminal is shown as active and never started a second time.
- Once a session is running, Relay still does the thing this project exists for: if the account
  genuinely runs out of quota, it hands the work to the next eligible profile in your priority order
  automatically — Claude profile A → Claude profile B → Codex → back to Claude A once it has reset,
  however your order is configured. Claude → Claude keeps the same session; any move to or from
  Codex continues from a Relay state bundle in a new session. You never see a config path, a pane id, or a session UUID; `relay status`
  tells you what's going on if you're curious.

Full walkthrough: **[docs/getting-started.md](docs/getting-started.md)**.

## What happens when quota runs out?

If the current writer genuinely, verifiably runs out of quota (not a transient rate-limit blip),
Agent Relay safely stops the session, re-checks your whole priority order, and continues on the
first eligible profile. **Claude → Claude** resumes the same native Claude session under the new
account. **Any move involving Codex** starts a new session on the other provider seeded from a Relay
state bundle (project state, git checkpoint, recent context) — it is a continuation, not the same
native conversation. The writer is sticky: a profile that has reset does not take work back until
the current writer blocks. There is no daemon, and an unknown or ambiguous reading never moves
anything.

- **Claude** is event-driven: the usage integration `relay setup` installs runs a hook the instant
  Claude reports a rate limit, and for the session Relay manages it starts one short-lived
  evaluation for that project.
- **Codex** has no such event, so Relay reads Codex's own structured rate-limit state
  (`ordinaryUsageAllowed`) immediately before it enters a Codex terminal, and then periodically
  while that terminal is supervised — an already-exhausted Codex profile is skipped or handed off at
  once instead of after a failed launch.
- If you are attached through `relay claude` / `relay codex` / `relay resume`, the terminal carries
  you onto the new owner by itself ("continuing this conversation on '<profile>'"). Herdr's status
  event is a second, best-effort trigger, and `relay watch run` runs an evaluation by hand.

## Security

Authentication remains provider-owned: Relay only launches each provider's official login/logout
inside an isolated config home per profile (`CLAUDE_CONFIG_DIR` / `CODEX_HOME`), and never reads,
copies, or stores an OAuth token, cookie, API key, or password. Everything runs locally — no
Relay-operated server, no data leaves your machine except what the Claude and Codex CLIs already send.
Details: [docs/security.md](docs/security.md).

## Requires

- [Claude Code](https://docs.claude.com/en/docs/claude-code) 2.1.x for Claude profiles (2.1.276–2.1.278
  validated; a newer 2.1.x patch works, `relay setup` explains if it isn't verified yet).
- The [Codex CLI](https://developers.openai.com/codex) for Codex profiles (0.155.0 validated; other
  versions are treated as unverified).
- `git` — Relay checkpoints a project's git state before a handoff.
- [Herdr](https://herdr.dev) is optional.
- macOS is the live-validated platform. Linux currently builds and passes the tests in CI but is not
  yet live-supported or validated.

## Updating

```sh
brew update && brew upgrade agent-relay
```

`relay status`/`relay doctor`/`relay setup` may show a small "Update available" hint when a newer
stable release exists — based on a cached, at-most-daily check of GitHub's releases, never a
network call on the command's own critical path. Relay never auto-updates itself.

## Everyday commands

| Command | What it does |
|---|---|
| `relay setup` | First-run wizard; safe to re-run any time (detects and reuses what's already there). |
| `relay claude [message]` | Start a **new** Relay session (Claude) in the current project and open Claude directly (an optional message is passed to Claude as the opening prompt). Other sessions in the project are left alone. |
| `relay codex [message]` | Start a **new** Relay session (Codex) and open Codex directly — the peer of `relay claude` (`--profile`, supervised terminal). |
| `relay resume [profile] [--session ID]` | Reopen a closed Relay session of this project (a picker when there are several; the profile is only a filter). |
| `relay switch [profile] [--session ID]` | Explicitly hand a conversation to a different profile/provider (state continuation across providers). With several active sessions you choose which; refuses an exhausted or unverifiable Codex target rather than rerouting your choice. |
| `relay claude -- …` / `relay codex -- …` | Arguments after `--` go straight to that provider's CLI and stay provider-scoped (see below). |
| `relay status [--live]` | Plain-language summary: the project's active and dormant Relay sessions (provider, profile, ids), primary/fallback order, automatic-handoff status, Herdr. Default reads only local Relay state (fast; provider auth/usage are reported as last-known, not fresh). `--live` also performs a current provider auth/usage/version check (slower — spawns the provider CLI). |
| `relay doctor [--project PATH]` | Check automatic-handoff readiness, including each configured profile's recorded trust for the project. Missing or unverifiable trust is blocking (exit 1). |
| `relay profiles` | List registered profiles and which is primary/fallback. |
| `relay login <name>` / `relay logout <name>` | Friendly wrappers around the provider's own official login/logout (Claude or Codex) for one isolated profile. |

### Status-line badge

Inside a Relay-managed Claude session the status line ends with a small badge (muted amber; plain
text if `NO_COLOR` is set),
`[Relay · <profile>]`, showing who owns the conversation right now (and `[Relay · switching →
<profile>]` for the moment a handoff is in progress). It is read from Relay's own project state on
every refresh, so it changes owner after a handoff without a new terminal, and it never appears in an
ordinary Claude session, or in one whose conversation has since moved to another provider. It rides on
the status line the usage integration already installs, which wraps — never replaces — your own
status line (your command's output is kept byte for byte and the badge is appended to its last
line; `relay integration claude uninstall` restores your original settings).

Codex gets the same amber owner badge plus the Relay session id and `$relay` command hints in
the terminal on each attach, including resume and handoff. This is a launch banner, not a live
footer: Codex's status line accepts built-in items rather than a custom status-line command.
Use `$relay status` for current ownership. The skill can also be managed explicitly:

```sh
relay integration codex install --profile codex-work
relay integration codex status --profile codex-work
relay integration codex uninstall --profile codex-work
```

Existing or user-edited `skills/relay/SKILL.md` files are preserved — Relay tracks a hash of
exactly what it last wrote (`skills/relay/.relay-managed.json`, next to the skill itself), so a
plain `relay integration codex install` (also run automatically on a new managed Codex attach)
upgrades an older, *unmodified* Relay-installed skill to the current one on its own; anything else
(no marker, or a hash that no longer matches) is left untouched and reported, never guessed past.
Already-running Codex sessions need a fresh managed attach to discover an upgraded skill and
receive its session context.

### Project trust before unattended use

Run `relay doctor` in the project before relying on unattended handoffs. Doctor, setup's completion
screen, and status share the same readiness checks: a configured Claude or Codex profile without
verified project trust makes the verdict **not ready**, even when login and usage hooks are healthy.
Doctor reads Claude's `hasTrustDialogAccepted` or Codex's `projects.<path>.trust_level` for the exact
canonical project directory. It conservatively requires an explicit record for that directory;
it does not infer inherited trust. Missing, unreadable, or malformed state cannot pass.

Use the command printed beside each blocker to open that exact profile in the project, review
the provider's trust prompt, then rerun doctor. Relay never writes trust acceptance. This is a
readiness preflight, not a change to the handoff transaction or a guarantee against other runtime
approval prompts. `/relay:doctor` checks the Claude conversation's project; the Codex skill uses
the managed terminal's project, even if the agent has changed its working directory.

### Provider options: `--` separates Relay's options from the provider's

Everything **before** `--` is Relay's; everything **after** it is forwarded, verbatim and as exact
arguments, to the provider CLI. Relay does not mirror either CLI's flags, so new provider flags
just work:

```sh
relay claude -- --dangerously-skip-permissions
relay claude --profile claude-backup -- --model opus --add-dir ../shared
relay codex -- --sandbox workspace-write
relay codex --profile codex-work "fix the failing test" -- --model o3
```

Provider options stay **provider-scoped**. Relay remembers them per project (structured, no shell
strings, no credentials) and reuses them only where they mean the same thing: the Claude ones for
Claude launches and resumes, the Codex ones for Codex — including after an automatic handoff to
another profile of the same provider. A Claude → Codex (or Codex → Claude) handoff never translates
flags between the two CLIs: the new provider is continued with *its own* stored options, if any.
`relay resume -- …` and `relay switch <profile> -- …` replace the stored options for the provider
they continue. Relay's own internal turns (the headless verification/bootstrap turns) never carry
user options.

A handful of flags are refused, because they would replace something Relay must own for a managed
session — the working directory (`-C`/`--cd`, `--worktree`), session/thread identity
(`--resume`, `--continue`, `--session-id`, `--fork-session`, `--last`, …), headless or
machine-readable output (`-p`, `--output-format`, `--json`, …), detaching (`--bg`, `--tmux`) and
non-resumable sessions (`--ephemeral`, `--no-session-persistence`). Claude flags that would switch
off the hooks Relay's automatic handoff depends on are refused too — `--bare`, `--safe-mode`,
`--restricted`, and `--setting-sources` without `user` — with a message saying so; Relay never
silently strips a flag or quietly downgrades a session to "not automatic". Everything else is yours.

## Is Herdr required?

**No.** Herdr is optional. `relay claude` works as a standalone Relay-managed session either way;
inside a [Herdr](https://herdr.dev) pane it additionally auto-registers the pane metadata so
Herdr's own `status`/`doctor`/`watch`/`handoff` actions work without you typing anything. See
[Herdr integration](docs/herdr-integration.md).

## Is the usage integration required?

**For Claude, effectively yes** — it is what makes automatic Claude handoff possible without
spending a real API call to check. `relay setup` offers to install it
(`relay integration claude install` under the hood) for every Claude profile you choose: a
`StopFailure` hook records a real rate limit and a status-line snapshot corroborates it (the same
status line also carries the `[Relay · <profile>]` badge). Without it, `relay watch run` reports
`UNKNOWN` for Claude unless you pass `--probe`, an explicit diagnostic that **spends a real API
request**.

**Codex needs no installed integration.** Relay asks Codex's own app-server for structured rate-limit
state, read-only, under the profile's isolated `CODEX_HOME`, immediately before it enters a Codex
terminal and periodically while that terminal is supervised — no hooks, no config edits, no daemon.
In both cases an unknown or ambiguous state never moves anything. See
[docs/automatic-handoff.md](docs/automatic-handoff.md) for exactly what it installs, the detection
policy, reset windows, and uninstall.

## Advanced / manual setup / building from source

`relay setup`/`relay claude` are a UX layer over the commands below — everything still works
exactly as before for scripting or when you want explicit control. Building from source (for
contributors, or if you'd rather not use Homebrew):

```sh
git clone https://github.com/RA1NM4KER/agent-relay && cd agent-relay
cargo build --release
# binaries: target/release/relay, target/release/relay-herdr-plugin — copy both onto your PATH,
# e.g. ~/.local/bin (relay-herdr-plugin is only needed for the optional Herdr integration, but
# must sit next to relay if you want relay setup to offer it)
```

(Rust is required to build — the exact toolchain is pinned in `rust-toolchain.toml`; install
[rustup](https://rustup.rs) and it's fetched automatically. `cargo install --path crates/relay-cli`
builds `relay` alone, without `relay-herdr-plugin`.)

```sh
# Create two isolated Claude profiles and log in to each yourself (Relay never touches credentials):
mkdir -p -m 700 ~/.config/agent-relay/profiles/claude-primary/claude ~/.config/agent-relay/profiles/claude-backup/claude
CLAUDE_CONFIG_DIR=~/.config/agent-relay/profiles/claude-primary/claude claude   # then /login
CLAUDE_CONFIG_DIR=~/.config/agent-relay/profiles/claude-backup/claude   claude   # then /login

# Adopt them (reference-only; preview first with --dry-run). Two profiles with the same account
# identity are rejected.
relay profile adopt claude-primary --provider claude --config-dir ~/.config/agent-relay/profiles/claude-primary/claude --dry-run
relay profile adopt claude-primary --provider claude --config-dir ~/.config/agent-relay/profiles/claude-primary/claude
relay profile adopt claude-backup --provider claude --config-dir ~/.config/agent-relay/profiles/claude-backup/claude
relay profile doctor claude-primary

# Already logged in to Claude's own default account (no isolated profile, ~/.claude) with a real
# terminal running? Adopt it by reference too — never as a fake explicit CLAUDE_CONFIG_DIR:
relay profile adopt erika-default --provider claude --native-default --dry-run
relay profile adopt erika-default --provider claude --native-default
# ...and if that account's Claude session is already running, bring it under Relay without
# restarting it (works even if `/relay adopt` wasn't installed when that session started):
relay adopt --session <claude-session-id>

# (Optional but recommended) usage integration, so Relay can detect a real limit for free:
relay integration claude install --profile claude-primary --dry-run
relay integration claude install --profile claude-primary
relay integration claude install --profile claude-backup
# ...or every registered Claude profile at once: relay integration claude install --all

# Start work as a Relay-managed writer, then let Relay watch it:
relay launch --profile claude-primary --project-dir ~/repos/foo "your prompt"      # prints the session id
relay watch run --profile claude-primary --fallback claude-backup --project ~/repos/foo --session <session-id>
```

`watch run` is one evaluation, not a daemon; run it from cron or a shell loop. When claude-primary is truly
exhausted it performs the transactional handoff to claude-backup; otherwise it does nothing.

Manual handoff, no usage detection needed:

```sh
relay handoff run --from claude-primary --to claude-backup --project ~/repos/foo --session <session-id>
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

- One active owner per Relay session — not one writer per project. Several sessions of a project
  (even on one profile) run side by side and Relay does not serialize edits to your working tree.
  A handoff is refused only while another live process serves the very conversation being moved.
- The statusline usage snapshot only refreshes in interactive sessions; headless sessions leave it
  stale, and stale means `UNKNOWN` (no handoff).
- No automatic fail-back or quota pooling.
- A handoff spends one small real API turn to verify the target session.
- Herdr's own session detection only reaches the default `~/.claude`; an isolated profile's pane
  needs an explicit `relay_session_id` token until Herdr's built-in Claude integration supports
  `CLAUDE_CONFIG_DIR` (see [Herdr integration](docs/herdr-integration.md)).
- Claude's transcript layout and `--resume` behavior are not a stable public API; Relay gates on
  validated versions and fails closed.
- `relay claude` runs `claude --session-id <uuid>` in your terminal: Relay assigns and records the
  session before Claude starts, so it knows the native session, and you type your first message
  inside Claude. (Until you send a first message Claude has no transcript yet, so a `relay resume` of
  a session you never wrote in reports Claude's own "no conversation" error.) `relay resume` runs
  an interactive `claude --resume <id>` (or `codex resume <thread-id>`) under the session's actual
  owner profile. `relay claude --no-attach` is the scripting form: it starts a background Claude
  session, which needs its first message on the command line.
- Codex can be an automatic *source* as well as a target. Its exhaustion is read from Codex's own
  typed interface (`codex app-server` → `account/rateLimits/read`, `ordinaryUsageAllowed`), never
  from error text; anything unavailable or ambiguous is `UNKNOWN` and moves nothing. Codex has no
  limit event to hook, so while you are inside a supervised `relay resume`/`relay switch` Codex
  session Relay checks that interface every 2 minutes (`RELAY_CODEX_POLL_SECS`, `0` disables) — only
  for as long as that terminal is open. Codex → Claude (and Codex → another Codex profile, tested with fakes only) hand over as state
  continuation, never as the same native conversation; only same-profile Codex resume is native, and
  Relay confirms the thread with Codex before resuming it.
- Before Relay enters a Codex terminal it checks Codex's structured usage immediately, so an already
  exhausted account never has to wait for the periodic check: `relay resume` on an exhausted Codex
  thread hands off at once (a real state-continuation handoff, since a thread exists); a fresh
  `relay codex` on an exhausted profile skips Codex entirely and starts the new conversation on the
  next eligible profile in your priority order (pre-launch routing — there is no Codex conversation
  yet, so nothing is "handed off"); `relay switch <codex-profile>` refuses an exhausted target
  (`target_profile_exhausted`) rather than rerouting your explicit choice. If the usage interface
  can't be read (`unknown`), Relay starts or switches nothing on a guess.
- Relay is not a daemon. Claude is never polled. Automatic handoff needs a *trigger*: the
  usage-integration `StopFailure` hook (installed by `relay setup`, per profile — a fallback
  profile needs it installed too for a *second* hop), Herdr's status event, or a manual
  `relay watch run`. A profile without the integration installed cannot start an automatic
  handoff. Hooks record `relay`'s own path at install time, so after upgrading Relay re-run
  `relay integration claude install` (or `--all`, for every registered profile in one command) to
  point them at the new binary. See `docs/maintainer-dogfood.md` for the separate `relay-dev`
  install path if you're testing an unreleased build rather than upgrading stable.
- Automatic continuation into the fallback happens in the terminal `relay claude`/`relay resume`
  is running in. A terminal that wasn't started through Relay is still handed off correctly, but
  it has to run `relay resume` itself.

## Herdr integration

An optional, thin plugin (`plugins/herdr/herdr-plugin.toml`) wires Relay's existing `status`,
`doctor`, `recovery`, `watch`, and manual `handoff` behind Herdr actions and an automatic
`pane.agent_status_changed` event, so a Claude pane inside [Herdr](https://herdr.dev) can surface
and trigger them without a separate terminal. Herdr never gains writer authority — the plugin only
maps a pane to a registered profile (via an explicit `relay_profile`/`relay_profile_fallback`
token, never guessed) and invokes the same `relay --json` CLI this README documents. Install with
`relay integration herdr install` (or via `relay setup`) — works from a packaged install (Homebrew
or a release tarball) with no source checkout: the plugin manifest is embedded in the `relay`
binary and materialized alongside the `relay-herdr-plugin` binary installed next to it. Details,
live-validation results, and current limitations:
[docs/herdr-integration.md](docs/herdr-integration.md).

## Security model

Relay references provider-owned config directories; it never reads, copies or stores OAuth tokens,
cookies, API keys or Keychain data, and removes credential-override environment variables before
running Claude. Profile directories must be private (0700), non-symlinked and user-owned. All state
writes are atomic, journals and logs hold only states, ids and filenames (never transcript contents
or provider error bodies), and the usage integration records only closed structured metadata.
Every mutating step is opt-in and previewable. Details: [docs/security.md](docs/security.md).

## Repository structure

```text
crates/
├── relay-core/             provider-neutral domain/safety primitives: Profile, ClaudeConfigMode,
│                            RelaySessionId, ContinuityType, HandoffState, ProcessIdentity, the
│                            HandoffCoordinator state machine, typed errors.
├── relay-provider-claude/  Claude-specific capabilities: session/process inspection, usage
│                            detection, the settings.json/statusline integration.
├── relay-provider-codex/   Codex-specific capabilities: structured usage, state continuation.
├── relay-cli/              the `relay` binary — user-facing orchestration only; owns no domain
│                            invariant that belongs in relay-core.
│   └── src/
│       ├── main.rs           parse CLI, dispatch, exit code — nothing else.
│       ├── cli.rs            every clap argument type (`Cli`, `Command`, each subcommand's args).
│       ├── commands/         one module per command family — this is where `relay <name>`
│       │                      behavior lives (e.g. `commands/adopt.rs` is `relay adopt`,
│       │                      `commands/profile.rs` is `relay profile *` and `relay login/logout`,
│       │                      `commands/resume.rs` is `relay resume`).
│       ├── auth.rs           profile authentication/inspection shared by several commands.
│       ├── launch.rs         writer-lease creation shared by `relay launch` and `relay claude`.
│       ├── terminal_session.rs  supervising an interactive terminal across a mid-session handoff.
│       ├── hook.rs           internal hook entry points a live Claude session invokes.
│       ├── output.rs         the human/JSON result envelope every command returns.
│       └── sessions.rs, target.rs, live.rs, providers.rs, control.rs, terminal.rs, …
│                              provider-neutral supporting modules, each scoped to one concern.
├── relay-herdr/             thin, optional Herdr adapter (a different wire contract from
│                            relay-cli's own provider clients — not merged with them).
└── relay-testkit/           reusable deterministic testing support (FakeProvider, fixtures).
```

To find where a command lives: `relay-cli/src/commands/<name>.rs` almost always matches the
command name directly (`relay switch` → `commands/switch.rs`, `relay watch run` →
`commands/watch.rs`). `cli.rs` has every flag; `commands/mod.rs` has the one dispatch match.

## More

[Getting started](docs/getting-started.md) · [Automatic handoff](docs/automatic-handoff.md) ·
[Herdr integration](docs/herdr-integration.md) · [architecture](docs/architecture.md) ·
[threat model](docs/security.md) · [research](docs/research.md) · [status](STATUS.md) ·
[changelog](CHANGELOG.md) · [maintainer dogfooding](docs/maintainer-dogfood.md) ·
[past milestone reports](docs/history/)

## License

Apache-2.0 (see `LICENSE`; dependency notices in `THIRD_PARTY_NOTICES.md`).
