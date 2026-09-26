//! Top-level command dispatch: one module per command family (`relay <name>`). This is the only
//! place that matches on [`crate::cli::Command`] — every arm is a short delegation straight into
//! that family's `run`.
//!
//! `relay watch auto`'s retry loop and `relay resume`'s immediate handoff check both re-enter the
//! CLI in-process (not as a subprocess) by calling [`dispatch`] again with a synthesized `watch
//! run` invocation — the same safety machinery (recovery, cooldown, the orchestration lock) a
//! manual `relay watch run` gets.

pub(crate) mod adopt;
pub(crate) mod claude;
pub(crate) mod codex;
pub(crate) mod doctor;
pub(crate) mod handoff;
pub(crate) mod history;
pub(crate) mod integration;
pub(crate) mod launch;
pub(crate) mod lock;
pub(crate) mod profile;
pub(crate) mod recover;
pub(crate) mod resume;
pub(crate) mod session;
pub(crate) mod setup;
pub(crate) mod state;
pub(crate) mod status;
pub(crate) mod switch;
pub(crate) mod switch_request;
pub(crate) mod watch;
pub(crate) mod why;

use relay_core::{Error, ProfileService, RelayPaths};
use relay_testkit::FakeProvider;

use crate::{
    cli::{Cli, Command},
    output::CommandOutput,
    providers,
};

/// The same [`RelayPaths`] resolution `dispatch` uses (discovery, overridden by `--config-root`/
/// `--state-root` when given) — pulled out so callers outside a dispatched command (namely the
/// update-check hint, which needs the resolved state root after `dispatch` has already returned)
/// can reach the exact same paths without re-deriving the override logic.
pub(crate) fn resolve_paths(cli: &Cli) -> Result<RelayPaths, Error> {
    let discovered = RelayPaths::discover()?;
    let config_root = cli
        .config_root
        .clone()
        .unwrap_or_else(|| discovered.config_root().to_path_buf());
    let state_root = cli
        .state_root
        .clone()
        .unwrap_or_else(|| discovered.state_root().to_path_buf());
    RelayPaths::new(config_root, state_root)
}

pub(crate) fn dispatch(cli: &Cli) -> Result<CommandOutput, Error> {
    let paths = resolve_paths(cli)?;
    let service = ProfileService::new(paths.clone());
    let provider = FakeProvider::default();

    match &cli.command {
        Command::Profile(profile) => {
            self::profile::run(&service, &paths, &provider, profile, cli.json)
        }
        Command::Session(session) => self::session::run(&service, &paths, session),
        Command::Lock(lock) => self::lock::run(&paths, lock),
        Command::Handoff(handoff) => self::handoff::run(&service, &paths, handoff),
        Command::Recover {
            transaction_id,
            project_dir,
            acknowledge,
            claude_executable,
        } => self::recover::run(
            &service,
            &paths,
            transaction_id,
            project_dir,
            acknowledge,
            claude_executable,
        ),
        Command::Launch {
            profile,
            project_dir,
            prompt,
            claude_executable,
        } => self::launch::run(
            &service,
            &paths,
            profile,
            project_dir,
            prompt,
            claude_executable,
        ),
        Command::Hook(_) => Err(Error::ProviderUnsupported),
        Command::Integration(integration) => {
            self::integration::run(&service, &paths, integration, cli.json)
        }
        Command::Watch(watch) => self::watch::run(&service, &paths, watch, cli),
        Command::Setup(args) => self::setup::run(&service, &paths, args, cli.json),
        Command::Claude(args) => self::claude::run(&service, &paths, args, cli.json),
        Command::Codex(args) => self::codex::run(&service, &paths, args, cli.json),
        Command::Status { project_dir, live } => {
            self::status::run_status(&service, &paths, project_dir.as_deref(), cli.json, *live)
        }
        Command::Profiles => self::status::run_profiles(&service, &paths),
        Command::Login {
            name,
            provider,
            claude_executable,
            codex_executable,
        } => self::profile::run_login(
            &service,
            &paths,
            name,
            provider.map(Into::into),
            cli.json,
            &providers::ExecutableOverrides {
                claude: claude_executable.clone(),
                codex: codex_executable.clone(),
            },
        ),
        Command::Logout {
            name,
            claude_executable,
            codex_executable,
        } => self::profile::run_logout(
            &service,
            name,
            &providers::ExecutableOverrides {
                claude: claude_executable.clone(),
                codex: codex_executable.clone(),
            },
        ),
        Command::Switch(args) => self::switch::run(&service, &paths, args, cli.json),
        Command::SwitchRequest(args) => self::switch_request::run(&paths, args),
        Command::Resume(args) => self::resume::run(&service, &paths, args, cli.json),
        Command::Adopt(args) => self::adopt::run(&service, &paths, args),
        Command::Doctor(args) => self::doctor::run(&service, &paths, args, cli.json),
        Command::Why(args) => self::why::run(&service, &paths, args, cli.json),
        Command::History(args) => self::history::run(&service, &paths, args),
        Command::State(args) => self::state::run(&service, &paths, &args.command, cli.json),
        // Always intercepted in `main` before `dispatch` is ever called — see
        // `crate::update_check`. Kept here only so the match stays exhaustive.
        Command::InternalUpdateCheckRefresh => {
            unreachable!("the internal update-check refresh process never reaches normal dispatch")
        }
    }
}
