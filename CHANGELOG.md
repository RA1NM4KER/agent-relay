# Changelog

All notable changes to Agent Relay will be documented here.

## Unreleased

Everything on `main` since v0.2.0 (not yet released; Homebrew still installs v0.2.0):

- **Codex support:** isolated Codex profiles, `relay codex`, Claude ⇄ Codex handoff as state
  continuation, same-profile Codex resume with thread verification, Codex exhaustion detection from
  Codex's structured app-server rate limits (immediate preflight before a Codex terminal plus
  periodic checks while supervised), pre-launch fallback from an exhausted Codex profile.
- **Automatic handoff trigger:** the Claude `StopFailure` hook now starts the evaluation itself, and a
  supervised terminal follows the conversation to the new owner.
- **Provider options after `--`** (`relay claude -- …`, `relay codex -- …`), stored per project and
  kept provider-scoped; flags that would replace something Relay owns or disable its hooks are
  rejected.
- **Status-line badge** `[Relay · <profile>]` for managed Claude sessions.
- **Fixes:** provider-aware liveness (launch, handoff and `relay status`), Claude `--bg` id parsing
  with coloured output, `relay resume` attach-vs-resume, isolated `CLAUDE_CONFIG_DIR` on attach.

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
