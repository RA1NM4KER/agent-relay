# Execution authority audit and next implementation boundary

Actual native user-shell execution is now contained in the opt-in RAM-only VM
slice, with exact revoke, delayed interruption and foreground closure evidence.
See [current integration scope](EXECUTOR_INTEGRATION.md). This does not certify
the rest of this inventory or permit transfer.

This is a source audit of pinned native 6ff1e1c, not a coverage pass. The
app-server dispatcher and common tool router are useful enforcement points but
cannot provide a complete execution or quiescence guarantee.

| Native path | Current research coverage | Remaining requirement |
| --- | --- | --- |
| Raw/typed app-server lifecycle and classified mutations | Durable native intent, controller generation, recheck, actual ID/result binding | Protected frontend transport and complete native method compatibility |
| Alternative home/store/history selectors | Cross-home shared-store bypass reproduced; source checks being validated | Alias tests, internal imports, every store mutation, outer filesystem isolation |
| Thread manager spawn, child/ephemeral registration | Reserved actual native ID and exact parent root; explicit background request scope | Immutable root/epoch/realm grant on the session, not just request-local context |
| `core/src/session/turn_input.rs` | External RPC gate only; source audit identifies delegated input, automatic/recovery start, realtime and queued work | Root-specific admission at every start/steer/reservation/activation boundary |
| `core/src/tools/router.rs` | Actual session-root check before common dispatch | Recheck after permission waits and at irrevocable effects/launches |
| `core/src/exec.rs`, unified exec/process manager/oneshot | Normal native execution; not contained | Realm-bound launch broker, cancellation and outstanding-launch inventory |
| Code mode delegate/workers, multi-agent spawn | Common dispatch and parent registration only | Preserve immutable authority across all spawned tasks and worker callbacks |
| Direct user-shell RPC | O: native executor to session VM; scoped cancellation/drain, exact kernel stop; host snapshots skipped for VM route | Host workspace I/O, native macOS/exit/background behavior and other shell paths |
| MCP, dynamic tools, exec-server, SSH/remote capabilities | No proven revocation adapters | Explicit capability inventory, deny unsafe transfer on unknown/irrevocable jobs |
| Approval/error replies | Connection/root checks, host-unique reply IDs | Actual approval integration and revoke-vs-launch adversaries |
| Host filesystem writes, mappings, broker I/O, shared directories | Not covered by the RAM-only VM experiment | Close admission and drain exact outstanding effects before issuing a receipt |

## Decisions retained

1. Do not weaken `ProfileSessionLock` or enable this mode on production profiles.
2. Default unknown native mutation routes to refusal, never silently omit fencing.
3. A controller lease rotation is not an ownership handoff or an executor drain.
4. A fenced logical owner cannot create a fresh root to reacquire execution.
5. Never use a native thread ID stored only in the authority journal as proof of
   native resumability; commit after the native materialization barrier.
6. An old host binary, arbitrary same-uid file access, copied stores, hard links,
   or supervisor-memory access cannot be secured by an RPC allowlist. Protect
   the broker, recovery capabilities, journal and provider home with OS authority.
7. The VM prototype has no external devices and makes no claim about native
   workspace execution. Linux guest commands cannot silently replace supported
   macOS command semantics.
8. A manager crash retains UNKNOWN and ownership reservations, even if the OS
   eventually destroys its VM. No trusted completion means no handoff.

The next integrated eight-TUI suite is gated on both complete native admission
and an actual realm-bound executor. The successful baseline is not rerun to
stand in for either foundation. Linux production testing requires a controlled
outer cgroup authority inaccessible to the workload; native macOS Full Access
requires an OS-enforced principal/capability design that preserves behavior.
Neither deployment has been implemented or certified here.

The direct user-shell row does not cover host startup/maintenance helpers, shell
tools reached through other runtimes, or autonomous capability creation. Those
still require accounting and fail-closed integration before full profile safety.
