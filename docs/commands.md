# Command reference

Canonical: https://ra1nm4ker.github.io/agent-relay/docs/commands.html

Find Agent Relay commands, options, and copyable examples.

Handbook examples are verified against stable v0.5.2.

Commands on this page are checked against **stable v0.5.2**. Use `relay <command> --help` for every option supported by your installed version. Angle brackets mark values you replace; square brackets indicate optional arguments.

**Find a command**

[setup](https://ra1nm4ker.github.io/agent-relay/docs/commands.html#setup)[claude](https://ra1nm4ker.github.io/agent-relay/docs/commands.html#claude)[codex](https://ra1nm4ker.github.io/agent-relay/docs/commands.html#codex)[resume](https://ra1nm4ker.github.io/agent-relay/docs/commands.html#resume)[switch](https://ra1nm4ker.github.io/agent-relay/docs/commands.html#switch)[status](https://ra1nm4ker.github.io/agent-relay/docs/commands.html#status)[doctor](https://ra1nm4ker.github.io/agent-relay/docs/commands.html#doctor)[why](https://ra1nm4ker.github.io/agent-relay/docs/commands.html#why)[history](https://ra1nm4ker.github.io/agent-relay/docs/commands.html#history)[profiles](https://ra1nm4ker.github.io/agent-relay/docs/commands.html#profiles)[login / logout](https://ra1nm4ker.github.io/agent-relay/docs/commands.html#login)[adopt](https://ra1nm4ker.github.io/agent-relay/docs/commands.html#adopt)[mode](https://ra1nm4ker.github.io/agent-relay/docs/commands.html#mode)[state](https://ra1nm4ker.github.io/agent-relay/docs/commands.html#state)[refresh](https://ra1nm4ker.github.io/agent-relay/docs/commands.html#refresh)[update](https://ra1nm4ker.github.io/agent-relay/docs/commands.html#update)[advanced](https://ra1nm4ker.github.io/agent-relay/docs/commands.html#advanced)

## relay setup

Run the first-time wizard, add profiles, or change your primary and fallback order. It detects existing configuration and reuses it.

```
relay setup
```

`--verbose` includes technical detail. Scripted setup uses `--non-interactive` with explicit configuration; profiles must already exist. Inspect `relay setup --help` before scripting it.

## relay claude

relay claude [message] [--profile NAME] [--resume [SESSION_ID]] [--autonomous] [-- PROVIDER_ARGS]

Start a fresh managed Claude conversation. Omit the message to type it in Claude’s terminal.

```
relay claude
relay claude --profile claude-work "Review the failing tests"
relay claude --resume
relay claude --profile claude-work -- --model opus
```

| Option | Use |
| --- | --- |
| `--profile NAME` | Choose a registered Claude profile for this launch. |
| `--fallback NAME` | Override the fallback order for this run; repeat for multiple profiles. |
| `--project-dir PATH` | Use a specific project directory. |
| `--resume [SESSION_ID]` | Adopt an existing native Claude conversation through its picker or exact ID. |
| `--no-attach` | Scripting form: create the managed session without attaching. Supply a message. |

`--resume` cannot be combined with an opening message or `--no-attach`. To reopen an already-managed dormant session, use [relay resume](https://ra1nm4ker.github.io/agent-relay/docs/commands.html#resume).

## relay codex

relay codex [message] [--profile NAME] [--autonomous] [-- PROVIDER_ARGS]

Start a fresh managed Codex conversation using the highest-priority Codex profile, or a profile you select. Before creating the conversation, Relay checks structured usage. Verified exhaustion can route this fresh start to the next eligible profile; unknown usage is refused. An explicit `relay switch` to an exhausted Codex target is refused rather than rerouted.

```
relay codex
relay codex --profile codex-work
relay codex -- --sandbox workspace-write
```

Useful options: `--profile NAME`, `--project-dir PATH`, and `--no-attach`. The no-attach form creates the thread and prints a summary; `relay resume` opens it later.

There is no equivalent of Claude’s unmanaged conversation adoption here. Use `relay resume` for a Codex session Relay already manages.

## relay resume

relay resume [PROFILE] [--session ID] [--project-dir PATH] [--autonomous | --interactive] [-- PROVIDER_ARGS]

Reopen a dormant Relay session on its last profile. With several sessions, a terminal picker lets you choose.

```
relay resume
relay resume claude-work
relay resume --session <relay-session-id>
```

The optional profile is a **filter**, not a new destination. To move work to another account, use `relay switch`. Options after `--` replace the stored options for the resumed provider.

## relay switch

relay switch [PROFILE] [--session ID] [--project-dir PATH] [--no-attach] [-- PROVIDER_ARGS]

Move an active Relay conversation to a different profile or provider. With no target in an interactive terminal, choose from the profile list.

```
relay switch
relay switch claude-backup
relay switch codex-work --session <relay-session-id>
```

`--no-attach` stops after the transaction and prints a summary. Without a terminal, or with `--json`, omitting the target prints choices instead of opening a picker. Unavailable explicit targets are refused.

[Continuation depends on the providers](https://ra1nm4ker.github.io/agent-relay/docs/handoffs.html#continuity): Claude → Claude retains the native conversation; every handoff involving Codex creates a new native session from state.

## relay status

relay status [--live] [--project PATH]

Read local Relay state: current ownership, active sessions, the three most recent dormant sessions, and configured fallback order. Older dormant sessions remain available in the resume picker and JSON. Without `--live`, provider health is not checked this run.

```
relay status
relay status --live
relay status --project ~/repos/my-project
relay status --json
```

**Configured does not mean checked.**`--live` spawns provider CLIs for current authentication, usage, and version checks. The configured fallback chain is an order, not a guarantee that every profile is eligible.

## relay doctor

relay doctor [--project PATH]

Check whether your profiles and project are ready for automatic handoff. It checks authentication, project trust, integration status, provider support, and preferences.

```
relay doctor
relay doctor --project ~/repos/my-project
```

Doctor separates blockers, warnings, passed checks, and recovery actions. An eligible configured profile is enough for readiness; other profiles’ problems remain warnings. No eligible profile or a remaining global blocker produces a nonzero exit status. Run the suggested command for each profile, review any provider prompts, then rerun doctor.

## relay why

relay why [--project PATH] [--session ID]

Explain Relay’s current automatic-handoff decision from recorded usage and eligibility evidence. Start here when “Why didn’t it switch?” is the question.

```
relay why
relay why --session <relay-session-id>
```

This explains evidence; it does not force a handoff.

## relay history

relay history [--project PATH] [--session ID] [--limit N]

Read the operational timeline derived from Relay’s durable session and handoff state. This is not your provider’s conversation transcript.

```
relay history
relay history --session <relay-session-id> --limit 20
```

The default limit is 200 recent observations. With no `--session`, history selects the most recently active Relay session. It reads local durable state without contacting a provider or changing ownership, recovery, or automation state.

## relay profiles

List registered profiles, their providers, priority roles, and login state.

```
relay profiles
```

For deeper inspection of one profile:

```
relay profile doctor claude-work
```

## relay login / logout

relay login NAME [--provider claude|codex] relay logout NAME

Use the provider’s official login or logout inside the named profile. A new profile needs a provider selection; an existing one already knows its provider.

```
relay login claude-work --provider claude
relay login codex-work --provider codex
relay logout claude-work
```

Run `relay setup` to include a newly added profile in your priority order.

## relay adopt

relay adopt --session CLAUDE_SESSION_ID [--profile NAME]

Bring an already-running Claude conversation under Relay from a separate terminal. The ID is the **native Claude session ID**. Relay must verify the process, profile, and conversation before changing ownership.

```
relay adopt --session <claude-session-id>
```

Use this when Claude started before the in-agent integration was installed. It does not restart Claude. See the [adoption workflow](https://ra1nm4ker.github.io/agent-relay/docs/workflows.html#existing) for the other entry points.

## relay mode

relay mode [interactive|autonomous] [--session ID] [--project PATH]

Show or change a session’s execution intent. Interactive is the default; autonomous asks the agent to continue the current task until a real blocker rather than asking merely whether to proceed.

```
relay mode
relay mode autonomous
relay mode interactive --session <relay-session-id>
```

Launch with `--autonomous` on `relay claude` or `relay codex`, or use `relay resume --autonomous` / `--interactive`. Handoffs preserve the selected mode. This changes behavioral intent only; it never grants provider permissions or changes approval or sandbox settings.

## relay state

relay state show [--session ID] [--project PATH] relay state update JSON_OR_- [--session ID] [--project PATH]

Inspect or update bounded, advisory working notes: goal, current subtask, decisions, failed attempts, relevant files, and next actions. Only the current active writer may update them.

```
relay state show
relay state update '{"goal":"Fix the failing tests","next_actions":["Run the focused test suite"]}'
```

Use `-` to read the update JSON from stdin. Omitted fields stay unchanged; `next_actions` replaces the list when supplied. Notes can accompany state continuation but never override user instructions, permissions, or Relay’s safety decisions. [Working-state details](https://ra1nm4ker.github.io/agent-relay/docs/advanced.html#working-state).

## relay refresh

Reconcile installed Relay-owned integration assets with the running binary after an upgrade. This does not run onboarding or change profiles, authentication, or fallback preferences. User-created or edited assets are preserved.

```
relay refresh
```

Reopen managed conversations to discover updated commands and skills.

## relay update

relay update [--check]

Check for a newer stable release, or explicitly request an upgrade. Supported Homebrew installations delegate the upgrade to Homebrew; other installations receive manual release instructions.

```
relay update --check
relay update
```

The check form never installs anything. An unavailable fresh release check is reported as unknown.

## Global options and help

| Option | Meaning |
| --- | --- |
| `--help` | Show usage for the command or subcommand. |
| `--version` | Print the installed Relay version at the top level. |
| `--json` | Emit a stable machine-readable JSON envelope. |
| `--config-root PATH` | Override Relay’s configuration root. |
| `--state-root PATH` | Override Relay’s state root. |

Relay’s `--json` belongs before the provider separator. After `--`, arguments belong to the provider and can be rejected if they interfere with managed session behaviour.

## Advanced command families

These are for scripting, inspection, or deliberate recovery. Prefer the everyday commands above for routine work.

| Command | Purpose | Read more |
| --- | --- | --- |
| `relay profile` | Manage explicit provider profiles and inspect identity. | [Profiles](https://ra1nm4ker.github.io/agent-relay/docs/profiles.html) |
| `relay integration` | Install, inspect, or remove Claude, Codex, and Herdr integrations. | [Integrations](https://ra1nm4ker.github.io/agent-relay/docs/integrations.html) |
| `relay launch` | Start a background Claude session for scripting. | [Background launch](https://ra1nm4ker.github.io/agent-relay/docs/advanced.html#launch) |
| `relay watch` | Evaluate automatic handoff once or inspect its ledger. | [Watch](https://ra1nm4ker.github.io/agent-relay/docs/advanced.html#watch) |
| `relay handoff` | Run a manual Claude transfer or read a transaction journal. | [Transactions](https://ra1nm4ker.github.io/agent-relay/docs/advanced.html#handoff) |
| `relay recover` | Reconcile an interrupted handoff. | [Recovery](https://ra1nm4ker.github.io/agent-relay/docs/troubleshooting.html#recovery) |
| `relay lock` | Inspect writer lease and orchestration lock. | [Inspection](https://ra1nm4ker.github.io/agent-relay/docs/advanced.html#inspect) |
| `relay session` | Stage Claude session artifacts or inspect transcript conflicts. | [Conflicts](https://ra1nm4ker.github.io/agent-relay/docs/advanced.html#conflicts) |

`relay task` is a repository-specific GitHub workflow shipped in this release, separate from conversation supervision. See [its scope and remote effects](https://ra1nm4ker.github.io/agent-relay/docs/advanced.html#tasks) before using it.
