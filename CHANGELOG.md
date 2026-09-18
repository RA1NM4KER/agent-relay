# Changelog

All notable changes to Agent Relay will be documented here.

## Unreleased

### Added

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

### Security

- Native cross-profile Claude session transfer remains explicitly best-effort and version-gated.
- M1 performs no real Claude authentication or configuration access.
- M1.5 rejects missing, symlinked, externally escaped, wrongly owned, or group/other-accessible Claude profile directories before invoking Claude.
- Real M1.5 validation made no filesystem changes and detected that the Erika and Megan directories currently expose the same pinned Claude identity.
