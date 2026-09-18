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

### Security

- Native cross-profile Claude session transfer remains explicitly best-effort and version-gated.
- M1 performs no real Claude authentication or configuration access.

