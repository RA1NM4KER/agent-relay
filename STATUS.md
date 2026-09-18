# Project status

## Current milestone

M1.5 — real adoption approved and implemented; blocked on a pre-existing permission issue on
Relay's own config directories before it can be performed against Erika and Megan.

## Completed

- Initialized a standalone Git repository.
- Inspected the current Claude Code and Herdr CLIs without changing authentication or configuration.
- Performed a temporary-directory isolation probe for `CLAUDE_CONFIG_DIR`.
- Inspected source and licensing for all required upstream projects.
- Selected Apache-2.0 for original Agent Relay code.
- Documented the M0 research, architecture, dependency decisions, threat model, and implementation plan.
- Scaffolded the Rust workspace and pinned a repository-local development toolchain.
- Implemented provider-neutral profile, identity, authentication, and availability models.
- Implemented canonical OS config/state paths, private profile directories, permission checks, and atomic state writes.
- Implemented FakeProvider and profile add/list/status/remove/doctor commands with JSON output.
- Defined non-executing Claude create/adopt/status/identity/launch interfaces.
- Added the optional Herdr module boundary without an integration dependency.
- Added corruption, identity, auth, path, symlink, permissions, secret-redaction, adoption, and atomic-failure tests.
- Added macOS/Linux CI configuration.
- Recorded the owner's separate successful Megan profile-isolation validation without inspecting the profile.
- Implemented `profile inspect-existing --provider claude` with executable/version validation, environment conflict detection, bounded command execution, and closed schema parsing.
- Implemented reference-only `profile adopt --dry-run` with an exact Relay-owned write plan.
- Defined a versioned non-secret Claude identity pin and fail-closed compatibility policy.
- Added zero-write, malformed-output, secret-redaction, environment override, executable, version, identity, ownership, and dry-run tests.
- Revalidated Erika and Megan as owned, non-symlinked, mode-0700 profile directories.
- Added fixture-tested support for the Claude Code 2.1.276 auth-status schema and verification of its reported config/project paths.
- Completed read-only inspection and zero-write adoption dry-runs for Erika and Megan.
- Verified recursive filesystem metadata for both Claude profile trees and the default `~/.claude` tree remained unchanged.
- Confirmed distinct Erika and Megan non-secret identity pins after Erika's authentication correction.
- Added default rejection of duplicate provider-scoped identity pins in core registration and Claude adoption dry-run.
- Implemented `ClaudeAdoptionProvider`, a real (non-fake) `Provider` that re-inspects immediately
  before Relay's registry write; gating (authentication, identity presence) stays in
  `ProfileService::add`, matching `FakeProvider`'s contract rather than duplicating checks.
- Enabled `relay profile adopt` to perform real, non-dry-run registration by default (`--dry-run`
  still only previews); `profile status`/`doctor` now select the provider matching the profile's
  own stored provider kind instead of always using `FakeProvider`, and gained a matching
  `--claude-executable` override.
- Added tests for real registration success, duplicate-name rejection, duplicate-identity
  rejection, unauthenticated fail-closed behavior, zero writes to any Claude directory, and no
  stray atomic-write temp files; 52 tests pass (was 45); fmt and Clippy pass.
- Confirmed via `--dry-run` against the real Erika and Megan profile directories that both would
  still succeed with distinct identity pins under the new code path (zero writes; matches prior
  M1.5 validation).

## In progress

- Real reference-only adoption of Erika and Megan is implemented and approved but not yet
  performed, pending the blocker below.

## Blockers

- `~/.config/agent-relay` and `~/.config/agent-relay/profiles` are mode 0755 (owner rwx, group/
  other rx), not the 0700 that `ProfileDirectory::prepare_root` requires of an existing Relay
  config/profiles root. Only the leaf `.../erika/claude` and `.../megan/claude` directories are
  correctly 0700. Real adoption will fail closed with `unsafe_permissions` on first write until
  these two ancestor directories are tightened to 0700; Relay intentionally never repairs
  permissions on directories it did not create, so this needs an explicit decision/action before
  proceeding. No Claude directory is affected; this is Relay's own directory tree.
- Cross-profile session transfer remains unverified, best-effort, version-gated, and outside M1.

## Unresolved architecture questions

- What explicit confirmation and UX should a future alias override require, if aliases are supported at all?
- Which exact Claude Code versions should be allowed for the first native-transfer compatibility matrix?
- Can Relay install its identity/session hook through additive launch settings without mutating profile settings, or should profile-local hook installation be an explicit setup step?
- Should the first Herdr plugin require Herdr 0.9.0 or a narrower feature-detected minimum?

## Next exact action

Resolve the ~/.config/agent-relay / .../profiles permission blocker (owner decision required:
authorize tightening those two directories to 0700, or investigate why they are 0755), then run
real `profile adopt` for Erika and Megan, verify stored identity pins, rerun `profile status`/
`doctor` for both, and verify both Claude directories and ~/.claude remain unchanged. Do not begin
M2 or test cross-profile session transfer until real adoption succeeds and the repository is
clean.
