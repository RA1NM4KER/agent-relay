# Changelog

All notable changes to Agent Relay will be documented here.

## Unreleased

## v0.4.1 — 2026-09-25

A CLI-UX and polish release. No handoff, exhaustion, or provider-auth semantics changed — the
durable provider-account exhaustion tracking, fresh-evidence supersession of stale exhaustion after
an out-of-band provider reset, and explicit "no eligible fallback" reporting already shipped in
v0.4.0 (see below); this patch is about visibility into what Relay is doing while it works.

### CLI UX

- **Cached, non-blocking update-available hints.** `relay status`, `relay doctor`, and `relay
  setup` now show a small hint (`Update available: vX.Y.Z` / `Run: brew upgrade agent-relay`) when
  a newer *stable* release exists. The check reads a local cache refreshed at most once every 24
  hours, so a normal command never waits on GitHub; a stale or missing cache triggers a detached,
  best-effort background refresh (a short-timeout `curl` against GitHub's stable-latest-release
  endpoint — prereleases and drafts are never treated as an update target) instead of blocking the
  foreground command, and any network or parse failure is silent with no retries on the critical
  path. Never shown in `--json` output or when the terminal isn't interactive. Several commands
  hitting a stale cache at the same time no longer spawn duplicate refresh processes: the same
  OS-backed lock `relay status`'s own session lock already uses now serializes the background
  refresh too, and a refresh process dying cannot wedge future ones — there is no daemon, and the
  internal refresh command cannot itself schedule another refresh or show a hint.
- **Consistent, honest progress feedback for slow commands.** `relay status` previously gave no
  indication at all while it spawned provider subprocesses; `relay doctor` and `relay setup` had a
  spinner stuck on a static label throughout. All three (plus `relay profile status`/`relay profile
  doctor`) now share one progress indicator whose label updates through each real phase ("Checking
  claude-main…", "Checking provider usage…", …), appears only past a ~250ms delay so a fast path
  never flickers a spinner on and off, and clears cleanly on success, error, or panic. Never drawn
  in `--json` mode or a non-interactive terminal.

## v0.4.0 — 2026-09-25

### Multi-session Relay supervision

- **Relay supervises conversations, not repositories.** A project can now hold many Relay Sessions
  (each with a stable Relay id, its own lease, journals, provider arguments and control endpoint);
  the invariant is one active owner per Relay Session, not one writer per project. Two sessions may
  run on the same profile, on different profiles or providers, side by side. A session is ACTIVE
  while Relay supervises a live provider process and DORMANT afterwards (its last profile is
  history, not an owner); a provider that exits before any conversation existed leaves nothing
  behind. `relay claude` / `relay codex` always start a new session and never stop or block on
  another (`--new` is a deprecated no-op); `relay claude --resume` works while other sessions are
  active and only refuses a conversation that is already active; `relay resume` reopens a dormant
  session (a picker with several, `--session <id>` for scripts); `relay switch` and `/relay switch`
  act on one session (`--session <id>`, or a picker) and never touch another; `relay status` lists
  active and dormant sessions. Hooks, the status-line badge, automatic handoff and Herdr resolve the
  session from the exact provider conversation, never from "the project's lease". The old
  one-lease-per-project state is migrated automatically into one Relay Session. The Claude process
  check for handoffs is now conversation-scoped: only another process serving *the same
  conversation* blocks.

### Claude and Codex parity

- `relay setup` works with Claude Code and/or Codex (either alone is enough), asks which provider a
  new profile is for only when both are installed, installs the usage hook only for Claude
  profiles, and ends with the start commands for the providers you configured (`relay claude` /
  `relay codex` as peers + `relay resume`). `relay login <new-name>` no longer silently defaults to
  Claude, Claude Code 2.1.278 is now a verified version, and CLI help/docs no longer use internal
  milestone labels or describe the retired background-launch flow.
- Investigated whether Codex can show a live Relay-ownership indicator the way Claude's status-line
  badge does. It cannot today: Codex's TUI status line only accepts a fixed set of built-in items
  (no custom-command item type), and its hook system fires on lifecycle events (`SessionStart`,
  `PreToolUse`, ...) rather than on a render tick, so nothing can keep a footer honest across a
  handoff. The existing one-shot launch banner stays; see `docs/codex-status-line.md` for the
  live-verified findings.

### Seamless in-agent switching

- `/relay status`, `/relay switch [profile]` and `/relay adopt` work inside Claude (a
  `UserPromptSubmit` hook answers them without a model turn; installed with the usage integration,
  an existing `commands/relay.md` is never touched); `/relay switch` is a request over a private
  per-project control directory to the supervising `relay`, which runs the ordinary switch
  transaction and follows the conversation. Bare `relay switch` opens a terminal picker (shared
  target model: current/exhausted/disabled/unverifiable profiles shown but not selectable).
- **`$relay switch <profile>` now performs the switch itself**, instead of only printing a command
  for another terminal. It asks the Relay process supervising the Codex terminal to do it, over the
  same private control channel Claude's `/relay switch` already uses (`switch-request`) — the
  requesting Codex tool shell is never the same process as the one Relay supervises, so the request
  is verified by process ancestry (a bounded parent-chain walk from the supervised Codex process to
  the requester) plus a matching Relay session id, owner profile, live supervisor identity and an
  active lease, failing closed on any ambiguity; only the supervisor itself ever executes the
  switch. Falls back to printing the manual command only when no verified supervisor can be reached
  (`no_supervisor`, `stale_session`, `unreachable`, `timeout`). Claude's own switch path is
  unchanged. Live Codex adoption is still not offered (no way to verify a live thread's identity);
  Codex now has its own in-agent mechanism instead — `$relay status|doctor|why|history|switch`, an
  installed skill executed through a model/tool turn rather than a hook.

### Codex official app-server context capture

- **Codex → \* handoffs can now capture real recent conversation context**, not just repository
  facts, for the `STATE_CONTINUATION` bundle a target profile bootstraps from. Sourced from
  `codex app-server`'s official `thread/items/list` method (the same structured, schema-generated
  protocol Relay already uses for rate limits and thread identity) — never Codex's undocumented,
  version-fragile local rollout storage. Best-effort like every other app-server read: a missing
  thread, an old server, or any protocol error degrades to no captured context rather than failing
  the handoff.

### Automatic exhaustion detection/handoff

- `relay claude` no longer asks for a first message: it starts `claude --session-id <uuid>`
  directly in your terminal (Relay assigns and records the session) and a handoff can stop that
  interactive session by its verified process.

### Claude pre-modal exhaustion evidence

- Exhaustion now also fires on a named `StopFailure` for the account's `session` or `weekly` limit
  corroborated by a fresh, same-native-session pre-failure statusline showing the matching 5-hour
  or 7-day window at ≥90% with its reset still in the future — narrowly covering Claude's limit
  modal, which can redraw the current statusline with null usage windows. `NEAR_LIMIT` (≥90%) alone,
  `AVAILABLE`, and anything stale or ambiguous (`UNKNOWN`) never trigger a handoff on their own; a
  bare `rate_limit` can be a transient 429 capacity error, so it is never enough alone. Relay stores
  at most 20 sanitized statusline snapshots per Claude profile, and only ever matches a
  `StopFailure` against a snapshot from the *same* native Claude session ID. **This exact path fired
  for real during dogfooding**: a genuine weekly-limit exhaustion, independently corroborated by a
  second native session reading the identical 100% usage minutes earlier — see "fresh evidence
  superseding stale exhaustion" below for the gap that real incident actually exposed.

### Durable provider-account exhaustion tracking

- When strong exhaustion evidence includes a future reset, Relay now also records a bounded durable
  provider-account fact under its state root, keyed by provider plus the registered profile's
  currently verified stable identity. A new Relay Session using that exact identity reads it as
  `RESET_PENDING` until the reset passes; another profile/account sharing the same project is
  unaffected. `relay watch clear` still clears only project-session automation state; to explicitly
  clear one durable account record, use `relay watch clear --project <path> --provider-account
  <profile>`.

### Fresh evidence superseding stale exhaustion after provider-side resets

- The durable record's reset time is Relay's own clock-based estimate, not a live guarantee — the
  same real incident above was invalidated less than a minute after detection by an out-of-band,
  mid-window provider-side usage reset the record's own clock had no way to know about. With no
  automatic supersession, Relay kept treating the now-healthy account as `RESET_PENDING` for three
  more days. `apply_provider_exhaustion` now checks the *fresh* observation first: a real,
  positively-read `AVAILABLE` or `NEAR_LIMIT` statusline snapshot clears the stale durable record
  for that identity and wins outright, instead of being silently overridden by the older stored
  fact. A fresh `UNKNOWN` reading (no real signal at all) still defers to the durable record exactly
  as before — this is scoped to genuine, positive contradicting evidence, never a general weakening
  of the fail-closed default.

### Explicit no-eligible-fallback diagnostics

- When the writer is exhausted and every configured fallback is also exhausted, disabled,
  unhealthy, or shares the writer's own identity, automatic evaluation now reaches an explicit
  terminal outcome instead of retrying silently forever (which is what actually happened in the
  incident above, once its independently-exhausted fallback profile compounded with the stale
  record — the standoff produced no visible signal at all). It names why each candidate was
  rejected, is recorded in the session's own automation ledger so a later evaluation can tell "the
  same standoff as last time" apart from "something changed," and a supervised terminal prints it
  once the first time it's reached:
  ```
  [Relay] codex-main is exhausted, but no fallback is currently eligible.
    claude-main: known exhausted
    claude-backup: known exhausted
  ```
  It does not repeat on every later poll while the same standoff holds, and reports again only if
  the facts actually change. `--json` output carries the same facts structurally instead of the
  printed line. `relay why` explains the same decision on demand at any time. The writer keeps the
  lease throughout — nothing here weakens single-writer ownership.

### Handoff latency/progress improvements

- One progress indicator now covers every slow provider phase of `relay claude` / `relay codex` (no
  blank gaps); `relay codex` no longer runs the slow `codex doctor` before its usage check.
- **Tasteful progress indicators for slow, previously-silent phases** of `relay resume`, `relay
  doctor`, `relay setup`, `relay integration claude/herdr install|status|doctor`, and `relay watch
  run`'s usage/fallback evaluation. TTY-only, `--json`-silent, delayed ~250ms so a fast path never
  flickers a spinner on and off.
- When an interactive terminal is waiting for a detached automatic handoff that has acquired its
  orchestration lock, Relay now shows a terminal-safe progress indicator: it names the source while
  the transaction is in progress, names the target only after the completed lease proves ownership,
  and then reports that it is continuing there. `--json` remains silent.

### Safety and identity verification improvements

- **More precise switch safety.** The check before a Claude → Claude move no longer refuses because
  *some* Claude process runs under the source profile (background daemons, helpers and sessions in
  other projects did): each process is classified by pid + start time, Claude's session registry
  and working directory, and only a possible writer for *this* project — or something that cannot
  be placed — blocks. Foreseeable refusals now happen before the source is stopped, and a
  supervised terminal reopens the same session when a switch fails after the stop but before
  ownership moved. Identity of the target is also checked up front.
- **More actionable provider errors.** A failed provider inspection now reports which operation
  failed and a sanitized reason (`Claude \`auth status\` failed: ...`) instead of a bare "provider
  command failed"; stderr is captured, secret-shaped tokens are redacted, and raw output is never
  leaked. An unsafe profile directory's error now names the exact mode and the fix (`... is mode
  755; ... Run: chmod 700 <path>`), and `relay profile doctor` surfaces that same actionable text
  instead of a generic "failed safety validation".

### Native-default Claude profile support

- **Claude's own native-default account is now a first-class profile.** Live-confirmed: `claude
  auth status --json` with `CLAUDE_CONFIG_DIR` unset reports the logged-in native account
  (`configDirectory: ~/.claude`); the identical command with `CLAUDE_CONFIG_DIR=~/.claude`
  explicitly set reports logged **out**. These are never treated as equivalent. `relay profile
  inspect-existing --provider claude --native-default` and `relay profile adopt <name> --provider
  claude --native-default` register `~/.claude` by reference (no credential copy, no
  reauthentication) with the same identity pinning, permission checks and duplicate-identity
  rules as any isolated profile; `relay integration claude install/status/uninstall
  --native-default` installs the usage hooks there too. A native-default profile can be the
  source or target of a normal `relay switch`/automatic handoff, side by side with isolated
  profiles.

### Adoption/resume improvements

- **`relay claude --resume <exact-uuid>` finds its real owner.** With no `--profile`, every
  registered Claude profile (native-default included) is searched structurally — via its own
  saved transcripts, never model output — for the one that actually has that conversation: a
  single owner is used automatically (and named), zero owners gives an actionable next step, and
  more than one refuses and lists the candidates. An explicit `--profile` still pins the search,
  but a wrong pin now names the real owner when one is provable instead of a bare "not found".
  `relay claude --resume [SESSION]` adopts through Claude's own resume flow (same session, no
  fork), proven from Claude's own `SessionStart` report, its live-session registry, and the
  profile identity pin before any lease is written.
- **`relay adopt --session <id>`** brings an already-running Claude conversation under Relay from
  outside it — no in-session `/relay adopt` required. This matters because a Claude process
  started before Relay's `/relay` command was installed never sees it (Claude only loads custom
  commands at session start), so the in-session hook path is structurally unreachable for a
  conversation that predates the install. Every fact still comes from Claude's own live session
  registry and transcript layout; every registered Claude profile is checked (or one is pinned
  with `--profile`), the process is never restarted, and the exact-session safety invariants
  (one live owner per native conversation, atomic lease creation) apply unchanged. `relay status`
  shows it ACTIVE immediately, and an external `relay switch --session <id> <target>` can act on
  it like any other session.

### Benchmark/instrumentation work

- Every handoff journal now persists a `timings` array with monotonic elapsed milliseconds per
  phase — `project_git_checkpoint`, `source_liveness`, `source_stop_and_verification`,
  `target_process_launch`, `target_verification`, `total_handoff`, and (session continuation only)
  `source_preflight`/`session_staging`, or (state continuation only) `context_capture`,
  `continuation_bundle_serialization`, `bootstrap_continuation_setup`. Diagnostics only — never
  bundle content, provider output, or credentials — inspectable via `relay handoff status --json`
  or the journal itself. A new `state_continuation_latency` benchmark (§4b in
  [docs/benchmarks.md](docs/benchmarks.md#per-handoff-phase-diagnostics)) isolates Relay's own
  state-continuation orchestration overhead with deterministic fake ports, separately from real
  provider latency. For an automatic Claude handoff, the existing `auto-handoff.log` also records
  sanitized wall-clock correlation points (hook receipt, detached watcher spawn, first evaluation,
  corroboration, foreground supervisor noticing Claude exit); the corroboration retry loop itself
  is now a short 300ms/1s/2s burst followed by the normal 20-second cadence, bounded to roughly two
  minutes, rather than a flat two-minute retry. None of this alters ownership or recovery
  decisions.

## v0.3.0 — 2026-09-21

Agent Relay becomes multi-provider: Claude Code and Codex profiles share one priority order, one
writer per project, and automatic handoff in both directions.

### Added

- **Codex support:** isolated Codex profiles (`relay login`, `relay setup`), `relay codex` (a new
  Relay-managed Codex conversation, symmetric with `relay claude`), same-profile Codex resume with
  thread verification, and Claude ⇄ Codex handoff as *state continuation* (a new session seeded from
  a Relay state bundle — never presented as the same native conversation).
- **Automatic Codex handoff:** exhaustion is read from Codex's own structured app-server rate limits
  (`ordinaryUsageAllowed`) — immediately before Relay enters a Codex terminal, then periodically
  while that terminal is supervised. A fresh `relay codex` on an exhausted profile skips Codex and
  starts on the next eligible profile; `relay resume` on an exhausted Codex thread hands off at
  once; `relay switch` refuses an exhausted or unverifiable Codex target. Unknown never moves
  anything.
- **Automatic Claude handoff that actually fires:** the Claude `StopFailure` hook starts a one-shot
  evaluation itself, and a supervised terminal follows the conversation to the new owner
  (`relay claude` / `relay codex` / `relay resume`).
- **Provider options after `--`:** `relay claude -- --model opus`, `relay codex -- --sandbox
  workspace-write`, forwarded verbatim, stored per project and never translated between providers.
  Flags that would replace something Relay owns — or disable its hooks (`--bare`, `--safe-mode`,
  `--restricted`, `--setting-sources` without `user`) — are rejected with a clear error.
- **Status-line badge** `[Relay · <profile>]` (`switching → <profile>` mid-handoff) for managed Claude
  sessions, composed with your own status line and colour-aware (`NO_COLOR` respected).
- One global priority order (primary + fallbacks) reconsidered at every exhaustion; the current
  writer stays sticky (no eager fail-back).

### Fixed

- `relay status`, launch, handoff and session-conflict paths now ask the *lease owner's* provider
  whether a session is live (Claude or Codex), and report `unknown` instead of guessing `active`.
- Codex liveness/stop cover the interactive `codex resume` process, not only the short-lived
  `codex exec` that created the thread.
- Claude `--bg` job ids with coloured output, `relay resume` attach-vs-resume for a live background
  job, and attaching under the lease owner's isolated `CLAUDE_CONFIG_DIR`.
- `relay switch --no-attach` now points at `relay resume`.

### Notes

- macOS is the validated platform; Linux builds and passes CI but is not yet live-supported.
- Automatic handoff from a real Codex exhaustion mid-session is covered by automated tests and has
  not yet been observed live; a real exhausted Codex profile has been live-checked for pre-launch
  fallback.
- After upgrading, re-run `relay integration claude install --profile <name>` so the Claude hooks
  and status line point at the new `relay` binary.

## v0.2.0

The release that makes Agent Relay installable rather than only buildable, and easy to run day to
day rather than only correct.

### Added

- **Packaging (M5):** GitHub Releases with prebuilt `macOS` binaries (Apple Silicon primary,
  Intel where cross-build validation allows), SHA-256 checksums, and a Homebrew tap
  (`brew install RA1NM4KER/tap/agent-relay`) as the primary macOS install path — no Rust, no
  `git clone` required. The Herdr plugin manifest is now embedded in the `relay` binary and
  materialized into a Relay-owned directory at install time, so `relay integration herdr install`
  (and `relay setup`'s Herdr step) work from a packaged install with no `agent-relay` source
  checkout nearby — a known gap M3 left open. See `docs/history/M5_FINAL_REPORT.md`.
- **Daily UX (M4):** `relay setup` (interactive first-run wizard; `--non-interactive` for
  scripting), `relay claude` (the daily entry point: resolves project/profile/fallback
  automatically, launches or reattaches, hands you a real interactive terminal via
  `claude attach`, auto-writes Herdr metadata when applicable), `relay status`/`relay profiles`
  (plain-language summaries), `relay login`/`relay logout` (friendly wrappers around Claude's own
  official `auth login`/`auth logout`). A new Relay-owned `preferences.toml` stores only profile
  names and booleans, never credentials. See `docs/getting-started.md` and `docs/history/M4_FINAL_REPORT.md`.
- **Herdr integration (M3):** optional Herdr plugin (`plugins/herdr/herdr-plugin.toml`) exposing
  status/doctor/recovery/watch/manual-handoff behind Herdr actions and an automatic
  `pane.agent_status_changed` event, plus `relay integration herdr install|status|doctor|uninstall`;
  live-validated against a real Herdr 0.9.0 server including a full controlled bidirectional
  handoff between real adopted profiles — see `docs/herdr-integration.md` and `docs/history/M3_FINAL_REPORT.md`.
  Profile↔pane binding is now written automatically by `relay claude` (M4) instead of requiring a
  manual `herdr pane report-metadata` call.
- M2C.1: structured Claude usage detection (StopFailure hook, statusline `rate_limits`, stream-json `rate_limit_event`), `relay integration claude install|status|uninstall`, automatic startup recovery in `relay watch run`, `--workload-model`, and Claude Code version/capability gating; see `docs/automatic-handoff.md`.
- M2C.1 automatic handoff via `relay watch run` is now live-validated in both directions (Profile A -> Profile B and Profile B -> Profile A); see `STATUS.md`.
- Fixed: `relay watch run`'s "Claude Code version not verified" warning, and two new M4 informational messages, no longer print under `--json` — they were contaminating the stable stdout/stderr JSON contract for machine consumers (including Relay's own Herdr plugin), live-found during M4 dogfooding.
- Fixed: the usage phrase matcher now recognizes real Claude limit messages.
- Rust workspace with provider-neutral core, CLI, Claude boundary, Herdr boundary, and testkit crates.
- Secure profile-directory creation, permission validation, canonical path handling, and atomic state writes.
- FakeProvider-backed profile add, list, status, remove, and doctor commands.
- Versioned JSON command output and redacted structured errors.
- Reference-only existing-profile adoption model with mandatory identity pinning.
- Non-executing Claude command plans for isolated login, auth status, and launch.
- macOS/Linux CI for formatting, Clippy, and tests.
- Read-only Claude existing-profile inspection and reference-only adoption dry-run commands.
- Closed Claude auth-status parser, identity pin, environment override inventory, executable validation, timeout, and output bounds.
- Claude Code 2.1.276 auth-status schema support with reported profile-path verification.
- Provider-scoped duplicate identity-pin rejection in core registration and Claude adoption dry-run.

### Security

- Native cross-profile Claude session transfer remains explicitly best-effort and version-gated.
- M1 performs no real Claude authentication or configuration access.
- M1.5 rejects missing, symlinked, externally escaped, wrongly owned, or group/other-accessible Claude profile directories before invoking Claude.
- Real M1.5 validation made no filesystem changes and confirmed distinct Profile A and Profile B identity pins after the Profile A authentication correction.
