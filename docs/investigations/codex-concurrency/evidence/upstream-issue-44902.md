# Hook-scoped client env allowlist for app-server launched hooks

https://github.com/openai/codex/issues/44902

State at retrieval: open; updated: 2026-10-21T10:00:17Z.

User report / issue discussion, not independently verified behavior.

### What variant of Codex are you using?

CLI

### What feature would you like to see?

I’d like to propose a narrow way for hooks launched by the app-server daemon to receive explicitly selected environment values from the originating Codex client.

Today, when hooks are spawned by the central app-server daemon, they inherit the daemon/server context rather than the shell context of the codex client process. That is the right default, but it makes some hook use cases difficult or impossible: terminal/tab targeting, terminal notifications, status integrations, or client-local routing based on values such as WT_SESSION, TERM_PROGRAM, TMUX, SSH_TTY, etc.

This is adjacent to issues like #37537 and #24638, and related to the broader environment-contract discussion in #26901. I am proposing something narrower than general env inheritance or hook-produced env overlays.

  ## Proposal

Allow user or managed config to define an explicit env-var allowlist scoped by hook event type, e.g.:

```toml
  [hooks.client_env.SessionStart]
  include = ["WT_SESSION", "TERM_PROGRAM", "TMUX"]

  [hooks.client_env.SessionEnd]
  include = ["WT_SESSION"]

  [hooks.client_env.PreToolUse]
  include = ["TERM_PROGRAM"]
```

  For matching hook events, Codex would pass the captured values as structured hook input:

```json
  {
    "hook_event_name": "SessionStart",
    "cwd": "/path/to/repo",
    "client_env": {
      "WT_SESSION": "...",
      "TERM_PROGRAM": "Apple_Terminal",
      "TMUX": "..."
    }
  }
```

  The values would not be merged into the hook process environment by default. Hook commands would read client_env from stdin like other hook payload fields.

  ## Security / behavior constraints

  Suggested constraints:

  - Only user or managed config may request client env forwarding.
  - Repo-local config should not be able to request arbitrary client env vars.
  - Explicit names only; no wildcards initially.
  - Values are exposed only to hook payloads for the configured hook event type.
  - This should not affect model-run shell command environments.
  - This should not replace shell_environment_policy; it solves a different provenance problem.
  - Values should be treated as hook input, not transcript/model-visible context unless hooks explicitly emit them.

  ## Optional per-matcher form

  If finer scoping is useful, this could also support rules:

```toml
  [[hooks.client_env.SessionStart.rules]]
  matcher = "^startup$"
  include = ["WT_SESSION", "TERM_PROGRAM", "TMUX"]

  [[hooks.client_env.SessionStart.rules]]
  matcher = "^resume$"
  include = ["WT_SESSION"]
```

  ## Motivation

  The daemon architecture means hook process ancestry no longer identifies the originating terminal/client. Some terminal-targeted hooks worked when the TUI process was in the terminal process tree, but now hooks are spawned by the daemon. Passing a small, explicitly configured set of client env values gives hooks a stable, documented way to recover client-local context without broad env inheritance.

  ## Open questions

  - Should this be app-server protocol data like client_env_by_hook, or a more general client_context field?

### Additional information

_No response_
