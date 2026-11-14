# Inside your agent

Canonical: https://ra1nm4ker.github.io/agent-relay/docs/integrations.html

Use Relay inside Claude Code, Codex, and optional Herdr sessions.

Handbook examples are verified against stable v0.5.2.

The spelling depends on where you type. Shell commands, Claude commands, and Codex skill requests are separate interfaces.

## Which command goes where?

| Where you type | Example | How it runs |
| --- | --- | --- |
| Your terminal shell | `relay status` | The Relay CLI runs directly. |
| Inside Claude Code | `/relay status` | Relay’s installed hook answers status locally, without a model turn. |
| Inside managed Codex | `$relay status` | The installed skill uses a Codex model/tool turn to call Relay. |

`/relay` and `/relay:doctor` are not Codex slash commands.

## Inside Claude Code

```
/relay status
/relay switch claude-backup
/relay adopt
```

Status reports the project, provider, owner, and handoff readiness. Switch asks the supervisor to move the current conversation; your supervised terminal follows it. Adopt brings this running, unmanaged Claude conversation under Relay if verification succeeds.

The integration also adds a status-line badge such as `[Relay · claude-work]`. Existing status-line output is preserved.

To change execution intent, use `/relay:mode autonomous` or `/relay:mode interactive`. When a mode change takes effect, the prompt reaches the model with the new intent so the running conversation can respond immediately. This is an exception to the local-only handling of ordinary Relay commands; it does not change provider permissions.

### Manage the Claude integration

Setup normally installs it for you. From a shell, inspect or preview it for one profile:

```
relay integration claude status --profile claude-work
relay integration claude install --profile claude-work --dry-run
```

Install after reviewing the preview:

```
relay integration claude install --profile claude-work
```

Remove it when needed:

```
relay integration claude uninstall --profile claude-work
```

The integration preserves other hooks and backs up settings. Uninstall restores Relay’s changes while preserving unrelated configuration. Removing it disables the normal structured usage path for automatic Claude handoff.

## Inside a managed Codex session

```
$relay status
$relay doctor
$relay why
$relay history
$relay switch claude-work
```

Relay installs a profile-local skill on managed attach. Requests are scoped to the supervisor’s project and session. A switch request goes to the supervising Relay process, which safely performs the handoff. If no verified supervisor can be reached, the skill gives a manual command for a separate terminal.

Name the target profile for a Codex switch; there is no in-skill interactive picker. The skill uses a model/tool turn, unlike Claude’s local hook.

### Manage the Codex skill

```
relay integration codex status --profile codex-work
relay integration codex install --profile codex-work
```

Restart or resume through Relay for Codex to discover an updated skill. Unmodified Relay-managed skills can be upgraded; user-created or edited skills are preserved.

```
relay integration codex uninstall --profile codex-work
```

The Codex skill supplies in-agent commands. Structured Codex usage checks operate independently of that skill.

## Refresh after an upgrade

```
relay refresh
```

This updates installed assets Relay can prove it owns. It preserves user-edited files and does not change accounts or fallback order. Start a fresh managed attach after refreshing so Claude or Codex can discover the changes.

## Optional Herdr integration

Relay works without Herdr. If you already use it, setup can connect the optional adapter so Herdr can expose Relay’s status, doctor, watch, and handoff actions.

```
relay integration herdr status
relay integration herdr doctor
```

The adapter is a thin interface to Relay’s existing safety checks. It does not grant a shortcut around ownership verification. The documented Herdr pane integration recognises Claude panes; Codex panes are not integrated in the same way.

For manual installation and compatibility details, see the [Herdr integration guide](https://github.com/RA1NM4KER/agent-relay/blob/v0.5.2/docs/herdr-integration.md).
