# Contributing to Agent Relay

Thank you for considering a contribution. Agent Relay supervises coding-agent conversations and
can start, stop, and transfer local provider processes. Small, well-evidenced changes are the
easiest to review and the safest to ship.

## Before you begin

- Read the [README](README.md) and [current project status](STATUS.md) for the supported behavior
  and known limitations.
- Read [the architecture](docs/architecture.md), [automatic-handoff design](docs/automatic-handoff.md),
  and [security model](docs/security.md) before changing lifecycle, provider, persistence, locking,
  or recovery code.
- Search existing issues before opening a new one. For a non-trivial proposal, open an issue first
  so we can agree on the problem and scope before implementation.
- Report vulnerabilities privately under the process in [SECURITY.md](SECURITY.md), not in a public
  issue or pull request.

Please follow the [Code of Conduct](CODE_OF_CONDUCT.md) in all project spaces.

## Safety constraints

These properties are deliberate design boundaries, not implementation details:

- A Relay Session has exactly one active Relay-managed owner. Ambiguous ownership, process
  identity, lock state, or target verification must fail closed.
- Provider authentication belongs to the provider. Do not read, copy, log, or route credentials;
  preserve the isolation of `CLAUDE_CONFIG_DIR` and `CODEX_HOME` profiles.
- Handoffs are journaled, transactional, and recoverable. Do not introduce an unlock/relock gap or
  leave an unverified target running.
- A `SESSION_CONTINUATION` is a proven native resume. A `STATE_CONTINUATION` starts a new native
  session from a provider-neutral bundle and must never be described as the same conversation.
- Do not bypass usage limits, add quota pooling, or make safety decisions from untrusted display
  text.

## Development workflow

1. Fork the repository and create a focused branch from `main`.
2. Make the smallest change that addresses the agreed problem. Keep unrelated formatting or
   refactors out of the pull request.
3. Add or update tests for behavior changes. Update the relevant current documentation when a
   user-visible, architectural, or safety behavior changes.
4. Run the required checks from the repository root:

   ```sh
   cargo fmt --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   ```

   `scripts/benchmark.sh` is useful for performance-sensitive work; `--with-codex` requires an
   authenticated, isolated `CODEX_HOME` and is not expected for ordinary contributions.

5. Open a pull request with a clear description of the problem, the change, tests run, and any
   remaining limitations or follow-up work. Include command output or a small reproduction when it
   makes review easier, but never include credentials, provider configuration, transcripts, or
   sensitive logs.

## Pull-request expectations

Review prioritizes correctness and evidence over size. In particular, explain how a change affects
ownership, locks, process identity, provider isolation, handoff recovery, or continuation semantics
when any of those are in scope. Preserve existing failure-closed behavior unless the pull request
explicitly changes it and supplies adequate tests and documentation.

By contributing, you agree that your contributions are licensed under the repository's
[Apache-2.0 license](LICENSE).
