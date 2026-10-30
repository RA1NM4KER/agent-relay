# TUI: expose the displayed thread ID per client for external integrations

https://github.com/openai/codex/issues/48880

State at retrieval: closed; updated: 2026-10-02T03:05:03Z.

User report / issue discussion, not independently verified behavior.

### What variant of Codex are you using?

Codex CLI/TUI 0.157.1 on Linux, with multiple TUIs attached to a shared local app-server daemon.

### What feature would you like to see?

Please expose a **supported, client-scoped notification or query for the thread currently displayed by a particular TUI**. A terminal integration needs the complete native thread ID that this client is showing so it can persist an exact `codex resume <thread-id>` target for that terminal pane. A list of loaded threads or the thread that most recently received a turn does not establish the user's current display selection.

A TUI-local `displayed-thread-changed` event is one possible design; a documented app-server interface is also fine if it explicitly associates a client instance with its selected thread. The signal needs a verifiable client/pane origin (for example, delivery from the TUI process) and the full resumable thread ID. It should report initial selection, new/resume/fork, a switch to an already loaded thread and the switch back, including before another prompt. Background or subagent activity must not change the selection unless the user actually displays that thread. Client-generation and ordering information, or an equivalent guarantee, should prevent a delayed report from a previous TUI instance overwriting the current choice.

### Additional information

Our use case is persistent terminal panes with one Codex TUI each. Several TUIs may share a local daemon and may use the same working directory. After restart, the terminal manager must resume the exact conversation that was last visible in *each* pane. `cwd`, recency, loaded-thread lists and background turn activity cannot safely identify that conversation or its client.

We also reproduced the first-client daemon environment problem: a hook for pane B had B's correct `session_id` but inherited pane A's terminal identity. That bug is already tracked in #48500, and #44902 discusses forwarding triggering-client context to lifecycle hooks; #48778 covers broader daemon-environment inheritance. **This issue is scoped to display selection after startup.** Fixing hook provenance alone would still leave no documented way to observe a local switch X → Y → X between already loaded threads when no lifecycle hook or new prompt is guaranteed. Conversely, a client-local display event could provide both the selected ID and its terminal provenance without treating daemon process environment as client identity.

The [TUI source already has `current_displayed_thread_id()`](https://github.com/openai/codex/blob/rust-v0.157.1/codex-rs/tui/src/app/thread_routing.rs#L190-L201), described as the thread whose transcript is currently on screen. The [documented `SessionStart` hook](https://learn.chatgpt.com/docs/hooks#sessionstart) covers lifecycle sources (`startup`, `resume`, `clear`, `compact`), not every display change. We could not find a documented client-scoped display-selection event for external integrations.

**Acceptance example:** Attach two TUIs in different panes (even with the same `cwd`) to one daemon. In pane A, display already loaded thread X, switch to Y and back to X while pane B remains on Z; also start a background thread. An external integration should observe A:X → A:Y → A:X and B:Z, without attributing any A event to B or treating the background thread as selected. After restarting A's TUI, its initial selection should be reportable again. The integration can then persist the exact last visible native ID for each pane without inspecting screen text or guessing from daemon activity.

