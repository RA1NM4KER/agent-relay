# Project status

## Current milestone

M1.5 — read-only real Claude profile adoption validation complete; distinct-account identity issue found.

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
- Detected that both directories report the same non-secret identity pin (`megan@schoolscape.co.za` in the same organization).

## In progress

- Awaiting resolution or explicit acknowledgement that Erika and Megan currently authenticate the same Claude identity.

## Blockers

- The two directories do not currently prove a two-account setup: their identity pins are identical.
- Actual adoption remains unapproved and was not performed.
- Cross-profile session transfer remains unverified, best-effort, version-gated, and outside M1.

## Unresolved architecture questions

- Should intentionally duplicated identities ever be admitted as explicit aliases, or should adoption always reject a pin already registered under another name?
- Which exact Claude Code versions should be allowed for the first native-transfer compatibility matrix?
- Can Relay install its identity/session hook through additive launch settings without mutating profile settings, or should profile-local hook installation be an explicit setup step?
- Should the first Herdr plugin require Herdr 0.9.0 or a narrower feature-detected minimum?

## Next exact action

Resolve the matching Erika/Megan identity before approving them as distinct profiles. Do not perform adoption, begin M2, or test cross-profile session transfer.
