---
name: relay
description: Check Agent Relay status, handoff readiness, decisions, and history for the current managed conversation. Use for $relay, $relay doctor, $relay status, $relay why, $relay history, or Relay profile-switch questions. Not for developing the Relay repository.
---

Use the installed Relay CLI to answer from actual state. Codex invokes this skill with `$relay`;
`/relay` and `/relay:doctor` are Claude commands, not Codex commands.

Relay-managed Codex terminals supply `RELAY_EXECUTABLE`, `RELAY_CONFIG_ROOT`, `RELAY_STATE_ROOT`,
`RELAY_PROJECT_DIR`, and `RELAY_SESSION_ID`. Check that all five are nonempty before running the
commands below. If absent, explain that the conversation has no verified Relay terminal context
and suggest `relay doctor` in a terminal in the project. Do not guess a session or owner from
the working directory, profile name, or conversation text.

Choose the command matching the request (default: status). Use the shell tool and preserve
the quoted environment variables as individual arguments:

```sh
"$RELAY_EXECUTABLE" --config-root "$RELAY_CONFIG_ROOT" --state-root "$RELAY_STATE_ROOT" doctor --project "$RELAY_PROJECT_DIR"
"$RELAY_EXECUTABLE" --config-root "$RELAY_CONFIG_ROOT" --state-root "$RELAY_STATE_ROOT" status --project "$RELAY_PROJECT_DIR"
"$RELAY_EXECUTABLE" --config-root "$RELAY_CONFIG_ROOT" --state-root "$RELAY_STATE_ROOT" why --project "$RELAY_PROJECT_DIR" --session "$RELAY_SESSION_ID"
"$RELAY_EXECUTABLE" --config-root "$RELAY_CONFIG_ROOT" --state-root "$RELAY_STATE_ROOT" history --project "$RELAY_PROJECT_DIR" --session "$RELAY_SESSION_ID"
```

Report the relevant CLI output concisely. Doctor exit code 1 with a readiness report means
not ready, not an execution failure. Status lists multiple conversations: identify this one
using `RELAY_SESSION_ID`, not merely the most recently active session. Never invent health,
trust acceptance, quota, or a successful handoff. Report a sandbox denial as a denial.

For switching, do not run `relay switch` inside the current agent shell: the handoff can stop
its caller. Show the user a shell-quoted command to run in another terminal, using the actual
environment values for executable, roots, project and session:

```text
<relay> --config-root <config> --state-root <state> switch <requested-profile> --project-dir <project> --session <session> --no-attach
```

Omit the profile when the user wants the interactive picker. The supervising Relay terminal
follows a successful switch. Do not edit trust settings, accept prompts, change permissions,
install integrations, or mutate Relay state as part of a health/status request.
