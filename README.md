# Agent Relay

Agent Relay is an early-stage, local-first orchestrator for explicitly handing an active coding-agent workflow from one authenticated profile to another. Its core job is continuity with single-writer safety—not hidden account rotation, request pooling, or credential storage.

The repository is currently at milestone M0: upstream research and architecture validation. There is no usable CLI yet.

## Safety principles

- Every active profile and handoff is visible and auditable.
- Only one Relay-managed writer may operate on a project at a time.
- Relay references provider-owned authentication directories; it does not store OAuth tokens.
- Native session continuation is claimed only after the target session is verified.
- When native continuation is unavailable, Relay explicitly labels the result as state continuation.
- Automatic handoff is out of scope until manual handoff is reliable.

See [the research report](docs/research.md), [architecture](docs/architecture.md), and [threat model](docs/security.md).

## Status

Agent Relay is not ready for use. No real Claude authentication or configuration has been modified during M0 research.

## License

Apache-2.0.

