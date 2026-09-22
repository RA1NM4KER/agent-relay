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

mod agent_cmd;
mod auth;
mod auto_handoff;
mod badge;
mod cli;
mod commands;
mod control;
mod hook;
mod launch;
mod live;
mod output;
mod preferences;
mod progress;
mod provider_args;
mod providers;
mod readiness;
mod sessions;
mod target;
mod terminal;
mod terminal_session;
mod util;

use std::process::ExitCode;

use clap::Parser as _;

use crate::cli::{Cli, Command};

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Command::Hook(hook) = &cli.command {
        return hook::run_hook(hook, &cli);
    }
    let is_doctor = matches!(cli.command, Command::Doctor(_));
    let result = commands::dispatch(&cli);
    if is_doctor {
        // `relay doctor` exits non-zero only when it is genuinely not ready (never for harmless
        // warnings), without ever reporting that diagnosis as a command *error* — the JSON
        // envelope stays the rich success shape either way.
        return output::print_result_with_exit(result, cli.json, |output| {
            let ready = output.json["data"]["ready"].as_bool().unwrap_or(false);
            if ready {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        });
    }
    output::print_result(result, cli.json)
}
