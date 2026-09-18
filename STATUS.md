# Project status

## Current milestone

M1 — profiles and isolation, implementation complete pending owner review.

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

## In progress

- Awaiting M1 review and authorization for a separately planned Megan-profile adoption experiment.

## Blockers

- No M1 implementation blockers.
- Megan adoption is intentionally blocked on explicit authorization to inspect the existing profile through the future read-only Claude adapter.
- Cross-profile session transfer remains unverified, best-effort, version-gated, and outside M1.

## Unresolved architecture questions

- Which stable, non-secret identity fields from `claude auth status --json` should form the adoption pin?
- Which exact Claude Code versions should be allowed for the first native-transfer compatibility matrix?
- Can Relay install its identity/session hook through additive launch settings without mutating profile settings, or should profile-local hook installation be an explicit setup step?
- Should the first Herdr plugin require Herdr 0.9.0 or a narrower feature-detected minimum?

## Next exact action

After approval, implement the read-only Claude auth-status/identity adapter and present the exact Megan adoption experiment commands before executing them. Do not begin M2 or cross-profile transfer testing.
