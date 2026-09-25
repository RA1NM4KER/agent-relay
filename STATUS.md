# Project status

Agent Relay supervises Claude Code and Codex conversations across isolated (or Claude's own
native-default) profiles, and moves work to the next eligible profile — by hand (`relay switch`)
or automatically on exhaustion — without losing the conversation. It is in daily dogfood use by
its maintainer; `v0.4.0` is the latest tagged release, but `main` is kept green and may be ahead of
it — see [docs/maintainer-dogfood.md](docs/maintainer-dogfood.md) for how the maintainer runs a
bleeding-edge build day to day.

For what each past milestone actually delivered and how it was live-validated, see
`docs/history/` (`STATUS_through_M4.md` for M0–M4, `M3_FINAL_REPORT.md`, `M3_OVERNIGHT_REPORT.md`,
`M4_FINAL_REPORT.md`, `M5_FINAL_REPORT.md`, `M6_FINAL_REPORT.md`). This file only describes the
*current* state.

## What works today

- **Multi-provider, one priority order**: isolated Claude and Codex profiles (`relay login`,
  `relay setup`), plus Claude's own native-default account (`~/.claude`, adopted with
  `--native-default` — never treated as equivalent to an explicit `CLAUDE_CONFIG_DIR` pointed at
  the same path, since Claude's own auth lookup differs by mode).
- **Relay Sessions, not one writer per project**: a project can hold many Relay Sessions; each
  active one has exactly one owner; a session is ACTIVE while Relay supervises a live provider
  process and DORMANT afterwards (history, not an owner). The same profile may own several
  sessions side by side.
- **Daily commands**: `relay claude` / `relay codex` (start a new session), `relay claude --resume
  [id]` (adopt an existing Claude conversation through Claude's own resume flow; an exact id with
  no `--profile` is found by searching every registered profile's own transcripts), `relay adopt
  --session <id>` (bring an already-running conversation under Relay from outside it — for a
  process that predates `/relay` being installed), `relay resume` (reopen a dormant session),
  `relay switch` (manual handoff, terminal picker or `--session`), `relay status` / `relay
  profiles` (read-only summaries), `/relay status|switch|adopt` (in-agent Claude, answered by a
  hook, no model turn). A Relay-managed Codex session gets the equivalent `$relay
  status|doctor|why|history|switch [profile]` through an installed skill (a model/tool turn, not a
  hook — Codex has no hook-free slash-command mechanism); `$relay switch` asks the supervising
  Relay process to perform the switch over the same control channel Claude's `/relay switch` uses,
  falling back to printing the manual command only when no verified supervisor can be reached.
- **Handoff**: same-profile and cross-profile Claude → Claude is SESSION_CONTINUATION (the same
  native conversation, no fork); anything touching Codex is STATE_CONTINUATION (a new session
  seeded from a captured state bundle, never presented as the same native conversation). Every
  transfer is one journaled, recoverable transaction (`relay recover`) under a per-project
  orchestration lock.
- **Automatic handoff**: Claude's `StopFailure` hook and structured usage signals
  (statusline/`rate_limit_event`), and Codex's own structured app-server rate limits, trigger a
  one-shot evaluation (`relay watch run`); a supervised terminal follows the conversation to the
  new owner. Never fires from raw text matching alone.
- **Herdr integration**: an optional, thin adapter (`plugins/herdr`) exposing Relay's
  status/doctor/recovery/watch/handoff behind Herdr actions; Relay owns no Herdr-specific policy.
- **Provider-scoped arguments**, a status-line badge, actionable provider-error diagnostics
  (sanitized stderr, no secrets), and permission/identity/symlink safety checks on every profile
  directory.

See `README.md` for the command reference and `CHANGELOG.md` for what changed release to release
(the `Unreleased` section is the most current).

## Known limitations

- Compatibility is validated against the Claude Code and Codex CLI versions named in
  `relay_provider_claude::VERIFIED_VERSIONS` / the Codex capability gate; a newer CLI is accepted
  with a warning (fail-open on version, fail-closed on structural signals), not silently trusted.
- `ps -Eww` environment scanning (used for writer-conflict detection) is unavailable on some CI
  runners (notably some hosted Linux images); those checks fail closed and the affected tests skip
  there rather than fail. macOS is the actively dogfooded platform.
- No automatic fail-back once a handoff has moved a conversation off an exhausted profile — the
  new owner stays sticky until another handoff moves it again.
- No quota pooling, no credential vault, no cross-account aliasing.
- Live proof of Herdr automatic handoff for an *externally adopted* native-default session (one
  that predates `/relay` install) has not been performed against a real Herdr workspace in this
  pass; the mechanism is the same one already live-validated for isolated profiles, and is covered
  by automated tests against a realistic fake-provider fixture.

## Repository structure

See `README.md`'s "Repository structure" section for the crate map and, within `relay-cli`, where
each command lives.
