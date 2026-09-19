# Getting started with Agent Relay

This is for someone who has never used Agent Relay before and doesn't need to know how it works
internally. If you want the internals — writer leases, transaction states, Herdr tokens — those
live in [`docs/architecture.md`](architecture.md) and [`docs/automatic-handoff.md`](automatic-handoff.md).
Here, you only need three ideas:

- You have one or more Claude accounts.
- One is your **primary**; the others are **fallbacks**.
- You run Claude *through* Agent Relay, and Relay moves your session to a fallback if your primary
  genuinely runs out of quota.

## 1. Install

```sh
git clone <this repository> && cd agent-relay
cargo build --release
# copy target/release/relay onto your PATH, e.g. ~/.local/bin
```

You also need [Claude Code](https://docs.claude.com/en/docs/claude-code) itself installed
(`claude --version` should print something in the `2.1.x` range) and `git`.
[Herdr](https://herdr.dev) is optional — everything below works without it.

## 2. `relay setup`

Run:

```sh
relay setup
```

This is a one-time interactive wizard. It walks through six things:

1. **Environment check.** It looks for Claude Code, Herdr (if you have it), and Agent Relay's own
   config directory, and shows a simple checklist — no raw JSON unless you pass `--verbose`.
2. **Claude accounts.** For each account you want Relay to manage, you can either:
   - **Authenticate a new one.** You give it a short name (e.g. `work`); Relay creates a private,
     isolated directory for it and opens Claude's own official login (a browser window, or
     whatever Claude's login flow normally shows you). **Relay never sees your password or your
     Claude token** — it only waits for the login to finish and then asks Claude "are you
     authenticated now?" the same way `relay profile doctor` already does.
   - **Reuse one that's already logged in.** If you've already set up an isolated Claude profile
     by hand, point Relay at its config directory once and it adopts it.

   If you already have profiles from before (including from before you ever ran `relay setup`),
   it detects them and asks if you want to keep using them — it never re-authenticates something
   that's already working.
3. **Primary and fallback.** If you have two or more accounts, you pick which is primary and the
   priority order for the rest. This is saved to a small file Agent Relay owns
   (`~/.config/agent-relay/preferences.toml`) — just profile *names*, never anything that could
   authenticate as one of your accounts.
4. **Automatic quota detection** (recommended, default yes). This installs a couple of small,
   already-existing Claude Code hooks (`StopFailure` and a statusline addition) into each chosen
   profile, so Relay can tell when an account is genuinely out of quota without spending an extra
   API call to check. If your Claude Code version is newer than what Relay has explicitly
   verified, it says so and asks before installing anyway — it never does this silently.
5. **Herdr** (if installed, recommended, default yes). Wires the same status/doctor/watch/handoff
   behavior into Herdr so a Claude pane inside Herdr can trigger them without a separate terminal.
   If Herdr isn't installed, Relay just says so and moves on — it works fine standalone.
6. **Done.** It prints your primary/fallback and reminds you of the one command you actually need
   day to day: `relay claude`.

You can re-run `relay setup` any time — to add another account, change the primary, or toggle an
integration. It's safe: it never re-authenticates something that's already logged in, and it never
throws away an existing profile.

## 3. `relay claude`

```sh
cd ~/repos/my-project
relay claude
```

This is the command you actually use every day. It:

- figures out the project from your current directory,
- figures out which account to use from what `relay setup` saved (no `--profile` needed),
- starts Claude under that account (if there's no session for this project yet) or reconnects you
  to the one already running,
- if you're inside a Herdr pane, tells Herdr which account/session this pane belongs to
  automatically (you never type a pane id or copy a session UUID anywhere),
- and hands you a normal, interactive Claude terminal.

The first time you run it for a project with no existing session, it needs an opening message —
either give it on the command line (`relay claude "let's refactor the auth module"`) or it asks you
once. After that, you're talking to Claude normally.

## 4. What happens when quota runs out

If your primary account genuinely, verifiably runs out of quota (not a transient rate-limit blip —
Relay is deliberately conservative about that distinction), Agent Relay:

1. safely stops your current session (the conversation itself is preserved, not discarded),
2. moves it to your next fallback account,
3. resumes it there — same conversation, same context, same session, just under a different
   account,
4. and you keep working. If you're in Herdr, this can happen automatically as Herdr notices your
   session's status change; outside Herdr, it happens the next time `relay claude` or
   `relay watch run` is invoked (Relay is intentionally not a background daemon).

You are never asked to copy a session id, look up a pane id, or run a lower-level handoff command
for this to work.

## 5. Checking on things: `relay status` and `relay profiles`

```sh
relay status
```

```
Project: ~/repos/my-project
Claude session: active
Current profile: work
Fallback: backup
Primary profile auth: authenticated
Automatic handoff: enabled
Herdr: connected
```

```sh
relay profiles
```

```
work         primary    authenticated
backup       fallback   authenticated
```

Neither of these prints transaction ids, session UUIDs, or config paths unless you ask for the
`--json` form (meant for scripts, not for reading).

## 6. Recovery basics

If Relay (or your machine) was interrupted mid-handoff, `relay claude`/`relay watch run`
automatically detect and recover an incomplete transaction before doing anything else — you don't
need to know what "recovery" means for this to work. If it can't safely decide on its own, it
tells you plainly and points at `relay recover <id> --project-dir DIR` (an advanced command — see
below) rather than guessing.

## 7. Reauthenticating

If Claude logs you out of an account (it happens), `relay claude` notices automatically:

```
Profile "work" needs Claude authentication.

Opening Claude login...
```

It opens the same official login flow `relay setup` used, waits, and continues once you're signed
in again. If a *fallback* account is the one that's logged out, Relay just warns you — your
primary keeps working — since you might not have gotten around to setting that one up yet.

You can also do this directly: `relay login work` / `relay logout work`.

## 8. The advanced/manual path

Everything above is a friendly layer over commands that still work exactly as they did before:
`relay profile ...`, `relay launch`, `relay watch run`, `relay handoff run`, `relay recover`,
`relay session conflict ...`, `relay integration claude ...`, `relay integration herdr ...`. If you
want to script Agent Relay, inspect a transaction's internals, or understand exactly what's
happening under the hood, start with [`docs/architecture.md`](architecture.md) and
[`docs/automatic-handoff.md`](automatic-handoff.md) — and the main [README](../README.md)'s
"Advanced / manual setup" section shows the equivalent manual commands for everything `relay
setup`/`relay claude` do automatically.
