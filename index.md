# Agent Relay

Canonical: https://ra1nm4ker.github.io/agent-relay/

A local supervisor for Claude Code and Codex. Verified usage exhaustion, isolated profiles, and one active owner per Relay session.

Handbook examples are verified against stable v0.5.2.

## Your agent hits a limit. Your work carries on.

Agent Relay moves work between your Claude Code and Codex accounts when a usage limit gets in the way.

```
brew install RA1NM4KER/tap/agent-relay
```

Open source. Built for macOS. [Setup guide](https://ra1nm4ker.github.io/agent-relay/docs/getting-started.html)

```
$ relay status --live

Current
  claude-main · Claude
  Session 9cc4d728

Configured fallback chain
  1. claude-backup
  2. codex-main
```

Your profiles, readiness, and session ownership, in one place. [Explore the commands](https://ra1nm4ker.github.io/agent-relay/docs/commands.html)

## A change of account. Less starting over.

Choose your profiles once. Relay supervises the conversation and checks the next account before handing over.

 [How handoffs work](https://ra1nm4ker.github.io/agent-relay/docs/handoffs.html)

1. **Set your order**

   Sign in to isolated profiles and choose which account comes next.
2. **Work with your usual agent**

   Start with `relay claude` or `relay codex`. Relay keeps track of the active session.
3. **Hand over when needed**

   Switch yourself, or enable automatic handoff on verified usage exhaustion.

Under your control

## Your agents. Your accounts. Your machine.

[Read the security model](https://github.com/RA1NM4KER/agent-relay/blob/v0.5.2/docs/security.md)

### It stays local

Relay runs on your machine. Your installed Claude Code and Codex tools handle provider communication.

### Your logins stay separate

Each profile uses its provider’s own sign in. Relay never reads, copies, or routes your credentials.

### One owner at a time

Every handoff verifies the next owner. Uncertain usage or identity stops the transfer.

## Before you start.

**What happens to my conversation?**

Claude to Claude resumes the same native conversation. Any handoff involving Codex starts a new native session with repository facts, git state, and bounded recent conversation context. Provider prompt caches are not transferred. [See what continues.](https://ra1nm4ker.github.io/agent-relay/docs/handoffs.html)

**Does it bypass usage limits?**

No. Relay respects each provider’s limits. It can move work to another eligible account you have configured after verifying exhaustion. Unknown usage never triggers automatic handoff.

**Can I choose when to switch?**

Yes. Use `relay switch` to choose a handoff yourself. Automatic handoff is configurable, and the new owner stays active until another handoff. [Read the switching guide.](https://ra1nm4ker.github.io/agent-relay/docs/workflows.html#switch)

**What do I need installed?**

Claude Code or Codex, an account for each profile you want to use, and Homebrew for the install command above. macOS is the actively validated platform. You can also [build from source](https://ra1nm4ker.github.io/agent-relay/docs/advanced.html#source).

Keep building

## Good work deserves to keep going.

 [Get started](https://ra1nm4ker.github.io/agent-relay/docs/getting-started.html)
