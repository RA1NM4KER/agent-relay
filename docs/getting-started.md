# Get started

Canonical: https://ra1nm4ker.github.io/agent-relay/docs/getting-started.html

Install Relay, set up profiles, and start your first managed conversation.

Handbook examples are verified against stable v0.5.2.

- [01 Install](https://ra1nm4ker.github.io/agent-relay/docs/getting-started.html#install)
- [02 Configure](https://ra1nm4ker.github.io/agent-relay/docs/getting-started.html#setup)
- [03 Start](https://ra1nm4ker.github.io/agent-relay/docs/getting-started.html#start)
- [04 Verify](https://ra1nm4ker.github.io/agent-relay/docs/getting-started.html#readiness)

## What you need

- **macOS**, the platform validated in live use. Linux builds and passes CI tests, but is not yet live-supported. Windows is not currently supported.
- **Claude Code or the Codex CLI**, installed separately. Either provider is enough; you do not need both.
- **Git** and a project to work in. Relay captures repository and git facts for handoffs.
- **One or more provider accounts.** Add another eligible profile to make fallback possible.

Herdr is optional. Relay works in a regular terminal.

## 1. Install Relay

```
brew install RA1NM4KER/tap/agent-relay
relay --version
```

Homebrew installs the latest tagged stable release. Prefer a source build? See [Building from source](https://ra1nm4ker.github.io/agent-relay/docs/advanced.html#source).

## 2. Set up your profiles

```
relay setup
```

The wizard detects your installed providers, walks you through each provider’s official login, and asks for a primary profile and fallback order. Use memorable names such as `claude-work`, `claude-backup`, and `codex-work`.

Accept the recommended Claude usage integration if you want automatic handoff from Claude. Codex usage checks work through its structured provider interface without Claude hooks.

Already configured? You can rerun setup to add accounts or change the order. It reuses existing profiles.

## 3. Open your project and start

```
cd ~/repos/my-project
relay claude
```

Or start with Codex:

```
relay codex
```

Relay opens your provider’s interactive terminal. Type your first message there. Each command starts a **new managed conversation**, normally using the highest-priority configured profile of that provider. If the selected Codex account is already verified exhausted, Relay can start the new conversation on the next eligible profile instead; no Codex conversation exists yet to hand off. Unknown usage is refused.

Complete the provider’s project trust prompt when it appears. To choose an account explicitly, use `relay claude --profile claude-work` or `relay codex --profile codex-work`.

## 4. Check that fallback is ready

In another terminal, open the same project and run:

```
relay doctor
```

Doctor checks authentication, verified Codex usage, provider compatibility, project trust, integrations, and preferences. At least one configured profile must be eligible; problems with other profiles remain visible as warnings. Follow the recovery commands for any blockers, then rerun the check. A ready verdict does not guarantee that a particular session has an eligible fallback.

**Trust is per profile and project.** A fallback may be logged in but still need you to review the provider’s trust prompt for this project. Relay never accepts trust on your behalf.

## Next time you sit down

Start a new conversation with `relay claude` or `relay codex`. To reopen a closed conversation that Relay already manages:

```
relay resume
```

With several closed sessions, Relay offers a picker. Use `relay status` for local session and ownership information. Use `relay status --live` when you need fresh provider checks.

| I want to… | Use |
| --- | --- |
| Start fresh with Claude | `relay claude` |
| Start fresh with Codex | `relay codex` |
| Reopen a closed managed conversation | `relay resume` |
| Move the current conversation | `relay switch` |
| See what is happening | `relay status` |
| Check unattended readiness | `relay doctor` |

## Keep Relay up to date

```
brew update
brew upgrade agent-relay
```

Relay may display a cached update notice. Use `relay update --check` to check for a newer stable release, or `relay update` to delegate a supported Homebrew installation’s upgrade to Homebrew. Other installations receive manual update instructions. Read the [release notes](https://github.com/RA1NM4KER/agent-relay/releases) when upgrading. After upgrading, run `relay refresh` to reconcile installed Relay-owned integrations, then reopen managed sessions to load the updated assets. The installed CLI’s `--help` describes the flags your version supports.
