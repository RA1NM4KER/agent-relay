# Changelog

All notable changes to Agent Relay will be documented here.

## Unreleased

- **Session adoption and in-agent control.** `relay claude --resume [SESSION]` adopts an existing Claude
  conversation through Claude's own resume flow (same session, no fork), proven from Claude's own
  `SessionStart` report, its live-session registry and the profile identity pin before any lease is written.
  `/relay status`, `/relay switch [profile]` and `/relay adopt` work inside Claude (a `UserPromptSubmit` hook
  answers them without a model turn; installed with the usage integration, an existing `commands/relay.md`
  is never touched); `/relay switch` is a request over a private per-project control directory to the
  supervising `relay`, which runs the ordinary switch transaction and follows the conversation. Bare
  `relay switch` opens a terminal picker (shared target model: current/exhausted/disabled/unverifiable
  profiles shown but not selectable). `relay login <new-name>` no longer silently defaults to Claude. Live
  Codex adoption and a Codex `/relay` command are not offered (no way to verify a live thread's identity or
  register the command).

- `relay setup` works with Claude Code and/or Codex (either alone is enough), asks which provider a new
  profile is for only when both are installed, installs the usage hook only for Claude profiles, and ends
  with the start commands for the providers you configured (`relay claude` / `relay codex` as peers +
  `relay resume`). Claude Code 2.1.278 is now a verified version. CLI help and docs no longer use internal
  milestone labels or describe the retired background-launch flow.
- `relay claude` no longer asks for a first message: it starts `claude --session-id <uuid>` directly in
  your terminal (Relay assigns and records the session) and a handoff can stop that interactive session by
  its verified process. One progress indicator now covers every slow provider phase of `relay claude` /
  `relay codex` (no blank gaps); `relay codex` no longer runs the slow `codex doctor` before its usage check.

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
  checkout nearby — a known gap M3 left open. See `M5_FINAL_REPORT.md`.
- **Daily UX (M4):** `relay setup` (interactive first-run wizard; `--non-interactive` for
  scripting), `relay claude` (the daily entry point: resolves project/profile/fallback
  automatically, launches or reattaches, hands you a real interactive terminal via
  `claude attach`, auto-writes Herdr metadata when applicable), `relay status`/`relay profiles`
  (plain-language summaries), `relay login`/`relay logout` (friendly wrappers around Claude's own
  official `auth login`/`auth logout`). A new Relay-owned `preferences.toml` stores only profile
  names and booleans, never credentials. See `docs/getting-started.md` and `M4_FINAL_REPORT.md`.
- **Herdr integration (M3):** optional Herdr plugin (`plugins/herdr/herdr-plugin.toml`) exposing
  status/doctor/recovery/watch/manual-handoff behind Herdr actions and an automatic
  `pane.agent_status_changed` event, plus `relay integration herdr install|status|doctor|uninstall`;
  live-validated against a real Herdr 0.9.0 server including a full controlled bidirectional
  handoff between real adopted profiles — see `docs/herdr-integration.md` and `M3_FINAL_REPORT.md`.
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
