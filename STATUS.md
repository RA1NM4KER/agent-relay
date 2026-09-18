# Project status

## Current milestone

M0 — research and architecture.

## Completed

- Initialized a standalone Git repository.
- Inspected the current Claude Code and Herdr CLIs without changing authentication or configuration.
- Performed a temporary-directory isolation probe for `CLAUDE_CONFIG_DIR`.
- Inspected source and licensing for all required upstream projects.
- Selected Apache-2.0 for original Agent Relay code.

## In progress

- Recording research findings and architecture decisions.
- Defining the initial threat model and M1 acceptance gates.

## Blockers

- None for M0.
- Real two-account transfer validation is intentionally blocked on architecture approval and explicit authorization to use real authentication state.

## Unresolved architecture questions

- Which exact Claude Code versions should be allowed for the first native-transfer compatibility matrix?
- Can Relay install its identity/session hook through additive launch settings without mutating profile settings, or should profile-local hook installation be an explicit setup step?
- Should the first Herdr plugin require Herdr 0.9.0 or a narrower feature-detected minimum?

## Next exact action

Complete and commit the M0 research, architecture, security, and implementation-plan documents; then request architecture approval before beginning M1.

