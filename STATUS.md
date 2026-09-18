# Project status

## Current milestone

M1.5 — read-only real Claude profile adoption, implementation complete; real validation blocked by profile preflight.

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
- Ran the authorized real preflight: Erika's supplied directory was absent; Megan's directory was mode 0755 and rejected before Claude launch.

## In progress

- Awaiting correction or clarification of the Erika path and explicit authorization for any Megan permission change.

## Blockers

- Erika inspection is blocked because `~/.config/agent-relay/profiles/erika/claude` does not exist on the observed filesystem.
- Megan inspection is blocked because `~/.config/agent-relay/profiles/megan/claude` is mode 0755; Relay requires 0700 and will not change it implicitly.
- The real Claude auth-status schema and identity fields remain unobserved because both preflights stopped before process launch.
- Cross-profile session transfer remains unverified, best-effort, version-gated, and outside M1.

## Unresolved architecture questions

- Does Claude Code 2.1.276 expose account ID, email, and organization ID using the fixture-tested auth-status schema?
- Which exact Claude Code versions should be allowed for the first native-transfer compatibility matrix?
- Can Relay install its identity/session hook through additive launch settings without mutating profile settings, or should profile-local hook installation be an explicit setup step?
- Should the first Herdr plugin require Herdr 0.9.0 or a narrower feature-detected minimum?

## Next exact action

Resolve Erika's missing directory and Megan's broad permissions without Relay modifying either profile. Then rerun inspection and dry-run. Do not perform adoption, begin M2, or test cross-profile session transfer.
