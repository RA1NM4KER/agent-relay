# Remote Control: threads viewed in the iOS app stay locked in the daemon, desktop fails with "already has an active writer"

https://github.com/openai/codex/issues/44449

State at retrieval: open; updated: 2026-10-19T16:23:30Z.

User report / issue discussion, not independently verified behavior.

### What version of the Codex App are you using (From "About Codex" dialog)?

26.901.20858 (macOS), embedded app-server 0.153.x; Remote Control daemon `codex app-server --remote-control` 0.153.1; terminal CLI 0.153.4.

### What subscription do you have?

ChatGPT Pro

### What platform is your computer?

macOS 26.2 (25C56), Apple Silicon

### What issue are you seeing?

Threads that I last opened in the ChatGPT iOS app (Codex tab, Remote Control) become unopenable in the Codex desktop app on the same Mac. Clicking them in the sidebar fails with:

```
thread 01a0831e-aec8-77c2-9295-7beeddf97692 already has an active writer
```

The desktop shows the thread as open elsewhere. Nothing is visibly open on the phone; the app is in the background. The model picker on those threads is also disabled, so they cannot be moved from GPT-5.6 Sol to GPT-6 Astra until the lock clears.

The cause is visible in `logs_2.sqlite`. When the iOS client (`codex_chatgpt_ios_remote` 1.2026.237) reconnects, the backend sends `__slingshot_backend_notification_thread_resume__` for every thread the phone had touched, and the Remote Control daemon resumes all of them. Each resumed thread takes a writer lock in `~/.codex/thread-writer-locks/<thread>.lock`. The desktop's own app-server then cannot resume them until the daemon tears the threads down, which happened about 50 minutes later.

Timeline from one occurrence (UTC, 2026-09-09):

```
20:35:23-20:35:26  daemon (pid 40273): 8 thread/resume requests, request_id
                   __slingshot_backend_notification_thread_resume__:<id>, connection 556
                   threads incl. 01a0802c…, 019fce14…, 019feb04…, 01a0831e…
21:07:50           desktop app-server: thread/resume 01a0831e… -> -32600 already has an active writer
21:08:23-21:13:01  same for 019fce14… (12 attempts)
21:23:55-21:26:23  same for 01a0802c… (8 attempts)
21:28:24           daemon: "clearing thread listener during thread-state teardown" for all three,
                   op: Shutdown, "Agent loop exited"; desktop can open them again
```

`lsof` on `~/.codex/thread-writer-locks/` during the window showed only the desktop's app-server holding locks for threads it had itself opened. The failing threads' locks were held by the daemon process. No turn was running in those threads on the phone.

### What steps can reproduce the bug?

1. Enable Remote Control in Codex desktop on a Mac.
2. In the ChatGPT iOS app, open the Codex tab and view three or four threads hosted on that Mac. Background the app.
3. Later, bring the iOS app back to the foreground (or let it reconnect).
4. In Codex desktop on the Mac, click any of those threads in the sidebar.

Observed: `already has an active writer`, thread cannot be resumed or have its model changed. Waiting until the daemon tears the threads down (tens of minutes) clears it. Closing the Codex tab on the phone also clears it.

### What is the expected behavior?

Viewing a thread on the phone should not lock it against the desktop on the machine that hosts it, at least while no turn is running. Either the daemon should release idle threads when the phone is backgrounded, or the desktop should be able to take the thread over from the daemon (or open it read-only, as #43253 did for the TUI) instead of failing.

### Additional information

Related but different: #43374 and #44063 are stale locks left by an exited process. Here the lock holder is alive and legitimate; the problem is that a backgrounded phone client keeps every recently viewed thread loaded in the daemon. #43253 added a read-only fallback for the TUI only.

