# Changelog

All notable changes to Agent Relay will be documented here.

## Unreleased

### Added

- M2C.1: structured Claude usage detection (StopFailure hook, statusline `rate_limits`, stream-json `rate_limit_event`), `relay integration claude install|status|uninstall`, automatic startup recovery in `relay watch run`, `--workload-model`, and Claude Code version/capability gating; see `docs/automatic-handoff.md`.
- M2C.1 automatic handoff via `relay watch run` is now live-validated in both directions (Profile A -> Profile B and Profile B -> Profile A); see `STATUS.md`.
- M3: optional Herdr plugin (`plugins/herdr/herdr-plugin.toml`) exposing status/doctor/recovery/watch/manual-handoff behind Herdr actions and an automatic `pane.agent_status_changed` event, plus `relay integration herdr install|status|doctor|uninstall`; live-validated against a real Herdr 0.9.0 server including a full controlled bidirectional handoff between the real adopted profiles — see `docs/herdr-integration.md` and `M3_FINAL_REPORT.md`. No `relay-core` changes.
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
