# Actual shared native validation and remaining counterexamples

All runtime stages use the existing source-built rust-v0.166.0 package, SHA256
`dfc5fa0a2e9b3522665ad8d8db49dd98898f174f2306f8f6203ab3f2a2ee173e`.
No production home, credentials or installed production binaries are used.

| Stage / root | Observed result |
| --- | --- |
| approved-capacity / o4fbcor9 | Failed observer initialize: newline JSON was sent over a WebSocket transport. Native daemon cleanup succeeded. |
| native-websocket-read-observer-smoke / o4fbcor9 | Correct read-only Unix WebSocket handshake, empty in-memory inventory, mutation refusal, socket closure, daemon cleanup. |
| native-websocket / wvr8ddcp | Eight real shared roots and concurrent foreground streams; Ctrl-C opens native task-exit menu. Exact interruption gate fails. Cleanup retained. |
| native-escape / j2t4xjw9 | Sixteen checks pass through exact-root resume. Third prompt remains a composer draft; no accepted/started native turn. Cleanup retained. |
| bracketed-paste / zqnciiy7 | Twenty-six behavioral checks pass, including A cancellation/crash isolation. Native stop escalates after 60 seconds; PID absence/socket refusal verified. |
| host-adversaries / l170k06y | Seven counterexample checks confirm raw native peer bypass and detached effects after turn completion/host death. |
| host-recovery-adversaries / _1igdmfs | Ten checks additionally demonstrate committed replay and the contract service's global-generation recovery limitation. |
| host-adversaries-final / g3ok82gy | Ten checks with exact driver snapshot and reserve-signal cleanup handler; all counterexamples reproduced. |
| native-containment-environment.json | BLOCKED, exit 2: macOS has no supplied delegated Linux cgroup test environment. |

Raw terminal/native-event/provider evidence is gzip compressed without content
changes; each TUI root inventory records original sizes and SHA256. Daemon stderr
is retained, including shell-snapshot and loopback MCP session-delete errors.
Every failed run remains evidence. The proxy mistake and input heuristics are
harness corrections, not claimed upstream native fixes.

The foreground Responses fixture holds actual native HTTP requests and emits
heartbeat SSE. It performs no inference or model-requested tools, logs no request
body or credentials, and identifies streams from native thread-id headers. The
read-only observer never resumes roots or mutates backend state. Its isolated
Python dependency is exact/hash pinned in feasibility/observer-requirements.txt.

Storage evidence includes decoded artifact hashes for lossless APFS compression,
real Rust metadata/rlib consumption checks, package clone reports, and dependency
installation provenance. Cache deletion approval/inventory remains local in the
original checkout's checkpoints; one active browser-cache directory was retained.

These results certify only the stated shared frontend behavior. They do not
certify native host authority, execution quiescence, scoped recovery, handoff,
credentialed/real-world parity, or a clean production workspace baseline.
