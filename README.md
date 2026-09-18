# Agent Relay

Agent Relay is an early-stage, local-first orchestrator for explicitly handing an active coding-agent workflow from one authenticated profile to another. Its core job is continuity with single-writer safety—not hidden account rotation, request pooling, or credential storage.

The repository has completed milestone M1: provider-neutral profile storage and a FakeProvider CLI. It is still pre-alpha and must not be used for real Claude handoffs yet.

## M1 development commands

```sh
cargo run -p relay-cli -- profile add erika --provider fake
cargo run -p relay-cli -- profile list
cargo run -p relay-cli -- profile status erika
cargo run -p relay-cli -- profile doctor erika
cargo run -p relay-cli -- profile remove erika
```

Add `--json` anywhere after `relay` for a versioned machine-readable envelope. Fake profiles are stored beneath `~/.config/agent-relay/profiles/<name>/fake` by default. Removing a profile unregisters it but deliberately retains its provider directory.

The M1 Claude boundary contained non-executing process plans only. No M1 command authenticated, inspected, launched, or modified a real Claude profile.

M1.5 adds read-only inspection and dry-run planning:

```sh
relay profile inspect-existing --provider claude --config-dir /absolute/profile/path
relay profile adopt NAME --provider claude --config-dir /absolute/profile/path --dry-run
```

Both commands fail closed on unsafe paths, permissions, environment overrides, executable versions, or unknown auth-status schemas. Dry-run never changes Relay or Claude state.

## Automatic handoff

Opt-in, usage-triggered handoff between profiles: see [docs/automatic-handoff.md](docs/automatic-handoff.md).

```sh
relay integration claude install --profile erika
relay watch run --profile erika --fallback megan --project ~/repos/foo --session <id>
```

## Safety principles

- Every active profile and handoff is visible and auditable.
- Only one Relay-managed writer may operate on a project at a time.
- Relay references provider-owned authentication directories; it does not store OAuth tokens.
- Native session continuation is claimed only after the target session is verified.
- When native continuation is unavailable, Relay explicitly labels the result as state continuation.
- Automatic handoff is out of scope until manual handoff is reliable.

See [the research report](docs/research.md), [architecture](docs/architecture.md), and [threat model](docs/security.md).

## Status

Agent Relay is not ready for real-provider use. M1 commands execute only against FakeProvider.

## License

Apache-2.0.
