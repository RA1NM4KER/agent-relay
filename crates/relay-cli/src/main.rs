//! `relay`: the user-facing CLI binary. This file only parses arguments, special-cases the
//! internal hook entry point (which must never go through the normal success/error envelope —
//! see [`hook::run_hook`]), dispatches every other command through [`commands::dispatch`], and
//! converts the result to a process exit code.
//!
//! Where things live:
//! - `cli.rs` — every `clap` argument type (`Cli`, `Command`, and each subcommand's own args).
//! - `commands/` — one module per command family; this is where `relay <name>` behavior lives.
//! - `auth.rs`, `launch.rs`, `terminal_session.rs`, `hook.rs`, `util.rs` — domain logic shared by
//!   more than one command family (profile authentication/inspection, writer-lease creation,
//!   interactive terminal supervision, hook entry points, and small generic helpers).
//! - `output.rs` — the stable human/JSON result envelope every command returns.
//! - `sessions.rs`, `target.rs`, `live.rs`, `providers.rs`, `control.rs`, `terminal.rs`,
//!   `preferences.rs`, `provider_args.rs`, `progress.rs`, `badge.rs`, `agent_cmd.rs`,
//!   `auto_handoff.rs` — provider-neutral supporting modules, each already scoped to one concern.
//! - `codex_poll.rs` — Codex-only: the adaptive polling cadence a supervised Codex terminal uses
//!   between structured usage reads, and the sanitized file-based feedback path that lets a
//!   detached one-shot evaluation (`auto_handoff`) report what it read back to the supervisor.

mod agent_cmd;
mod auth;
mod auto_handoff;
mod badge;
mod cli;
mod codex_integration;
mod codex_poll;
mod commands;
mod control;
mod hook;
mod launch;
mod live;
mod output;
mod preferences;
mod progress;
mod project_trust;
mod provider_args;
mod providers;
mod readiness;
mod sessions;
mod target;
mod terminal;
mod terminal_session;
mod update_check;
mod util;

use std::{io::IsTerminal as _, process::ExitCode};

use clap::Parser as _;

use crate::cli::{Cli, Command};

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Command::Hook(hook) = &cli.command {
        return hook::run_hook(hook, &cli);
    }
    if matches!(cli.command, Command::InternalUpdateCheckRefresh) {
        // Only ever reached via the detached process `update_check::maybe_show_hint` spawns —
        // never invoked directly, prints nothing, and always exits successfully regardless of
        // whether the refresh itself found anything.
        if let Ok(paths) = commands::resolve_paths(&cli) {
            update_check::run_internal_refresh(&paths);
        }
        return ExitCode::SUCCESS;
    }
    let is_doctor = matches!(cli.command, Command::Doctor(_));
    // Only these normal, human-facing completion points ever show an update hint — commands run
    // constantly in scripts or launched every conversation (`relay claude`, `relay codex`, …)
    // never gain an extra line they did not ask for. `Command::InternalUpdateCheckRefresh` never
    // reaches here at all (see the early return above), and `shows_update_hint_for` itself would
    // still say `false` for it even if that changed — see that function's own doc comment.
    let show_update_hint = update_check::shows_update_hint_for(
        &cli.command,
        cli.json,
        std::io::stderr().is_terminal(),
    );
    let result = commands::dispatch(&cli);
    let succeeded = result.is_ok();
    let exit_code = if is_doctor {
        // `relay doctor` exits non-zero only when it is genuinely not ready (never for harmless
        // warnings), without ever reporting that diagnosis as a command *error* — the JSON
        // envelope stays the rich success shape either way.
        output::print_result_with_exit(result, cli.json, |output| {
            let ready = output.json["data"]["ready"].as_bool().unwrap_or(false);
            if ready {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        })
    } else {
        output::print_result(result, cli.json)
    };
    if show_update_hint
        && succeeded
        && let Ok(paths) = commands::resolve_paths(&cli)
    {
        update_check::maybe_show_hint(&paths, true);
    }
    exit_code
}
