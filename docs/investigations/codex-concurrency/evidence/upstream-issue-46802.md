# App-server: coordinated per-thread quiesce and scheduled-work drain for lossless fork handoff

https://github.com/openai/codex/issues/46802

State at retrieval: open.

### What feature would you like to see?

A host-supported handoff protocol for replacing one long-running Codex task with a fork without losing user turns or scheduled/event-driven deliveries.

The currently documented App Server exposes `thread/read`, `thread/fork`, `turn/start`, and `thread/archive`, but no per-thread admission fence that all clients honor, and no scheduler pause/drain with acknowledged transfer of queued or in-flight deliveries. A client-side read-idle → fork → reroute → archive sequence has a race.

### Reproduction / failure mode

With two independent app-server clients, client A reads an idle source and forks it. Before route cutover, client B submits and completes a turn on the source. The child lacks that completed turn. Archiving the source earlier rejects the turn but is not a safe substitute: downstream routes and scheduled deliveries have not yet been transferred and verified.

A SessionStart/UserPromptSubmit hook is not a sufficient universal fence: an already-loaded client may retain the prior hook set, hook failure/timeout is not a durable admission guarantee, and rejected input is not a replayable native queue.

### Minimal host contract requested

1. An atomic, per-thread, generation/epoch-bound admission barrier shared by all app-server and desktop clients. It must stop or durably queue new turns and report the exact last accepted/completed turn at the boundary. It should drain any in-flight turn before the fork snapshot or fail without moving the boundary.
2. A host scheduler pause/drain primitive for this source task that reports and durably accounts for queued and in-flight runs, plus an acknowledged transfer/resume to the child generation. A paused but already-dispatched run must not later write to the retired source.
3. A final compare-and-swap cutover/readback contract: fork at the fenced boundary; transfer event/watch delivery; prove child route and backlog; then retire/archive source. On failure, resume the source without dropping or duplicating a user turn or scheduled delivery.
4. A deterministic, no-model test surface for two-client turn races, in-flight scheduled jobs, crash/restart, repeated cutover, and rollback.

An equivalent API design is fine. The essential property is host-wide enforcement and durable evidence, not a particular method name. A cooperative plugin journal can make its own files recoverable, but cannot fence native clients or the host scheduler.

### Related request

#38289 asks for atomic idle-only admission for one proposed turn. This request is different: it needs a temporary source-wide boundary for **all** clients plus scheduled-run draining and cutover during a fork. It should compose with, not duplicate, that API.

### Environment

Reproduced with Codex CLI/App Server 0.142.5 on macOS, using independent clients and disposable tasks. No production task data or credentials are included here.
