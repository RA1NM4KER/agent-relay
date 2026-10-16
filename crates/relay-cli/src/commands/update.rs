//! `relay update`: delegates to whichever package manager owns this installation (currently only
//! Homebrew) rather than Relay ever replacing its own binary — see agent-relay#6. `--check` only
//! reports whether a newer stable release exists, reusing the same cached check `relay
//! status`/`doctor`/`setup` already show a hint from (`crate::update_check`), without installing
//! anything.

use std::{path::PathBuf, process::Command};

use relay_core::{Error, RelayPaths};
use serde_json::json;

use crate::{
    cli::UpdateArgs,
    output::{CommandOutput, header, success},
    progress::Progress,
    update_check::{self, CheckOutcome},
};

const REPO: &str = "RA1NM4KER/agent-relay";
/// Bounded the same way `git_checkpoint_error` bounds git's stderr: one line, no raw command
/// dumps, no credential-bearing environment output could ever appear here either way since `brew`
/// never receives any.
const MAX_DETAIL_LEN: usize = 300;

pub(crate) fn run(
    paths: &RelayPaths,
    args: &UpdateArgs,
    json_mode: bool,
) -> Result<CommandOutput, Error> {
    if args.check {
        return run_check(paths);
    }
    match update_check::homebrew_formula_name() {
        Some(formula) => run_homebrew_upgrade(formula, json_mode),
        None => run_manual_instructions(),
    }
}

fn run_check(paths: &RelayPaths) -> Result<CommandOutput, Error> {
    let heading = header("Relay update check");
    let (human, data) = match update_check::check_now(paths) {
        CheckOutcome::UpToDate { current_version } => (
            format!("{heading}Running v{current_version}. Already the latest stable release."),
            json!({
                "current_version": current_version,
                "latest_version": null,
                "update_available": false,
            }),
        ),
        CheckOutcome::Available {
            current_version,
            latest_version,
        } => (
            format!(
                "{heading}Running v{current_version}. A newer stable release is available: \
                 v{latest_version}\nSee: https://github.com/{REPO}/releases/tag/v{latest_version}"
            ),
            json!({
                "current_version": current_version,
                "latest_version": latest_version,
                "update_available": true,
            }),
        ),
        CheckOutcome::Unknown { current_version } => (
            format!(
                "{heading}Running v{current_version}. Could not reach GitHub to check for a \
                 newer release right now."
            ),
            json!({
                "current_version": current_version,
                "latest_version": null,
                "update_available": null,
            }),
        ),
    };
    success("update", human, data)
}

/// Homebrew's own documented upgrade sequence (see README.md's install instructions): `brew
/// update` first, so `upgrade` sees a current formula definition, then `upgrade <formula>`. Both
/// are real Homebrew subcommands that only ever touch this one formula's install — Relay performs
/// no file replacement of its own.
fn run_homebrew_upgrade(formula: &str, json_mode: bool) -> Result<CommandOutput, Error> {
    let progress = Progress::start(
        &format!("Running: brew update && brew upgrade {formula}…"),
        json_mode,
    );
    run_brew(&["update"])?;
    run_brew(&["upgrade", formula])?;
    progress.finish();
    let version = installed_version(formula)?;
    success(
        "update",
        format!(
            "{}Delegated to Homebrew: brew upgrade {formula}\nNow installed: {version}",
            header("Relay update")
        ),
        json!({"method": "homebrew", "formula": formula, "version": version}),
    )
}

fn run_brew(args: &[&str]) -> Result<(), Error> {
    let output = Command::new("brew")
        .args(args)
        .output()
        .map_err(|source| Error::Io {
            path: PathBuf::from("brew"),
            source,
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(brew_command_error(args, &output.stderr))
}

/// The resulting version Homebrew now has installed for `formula`, read back from `brew` itself
/// (not re-derived from this already-running, now possibly-stale process's own compiled-in
/// version) — so a real upgrade is reported honestly even though the current process cannot
/// re-exec itself mid-command.
fn installed_version(formula: &str) -> Result<String, Error> {
    let output = Command::new("brew")
        .args(["list", "--versions", formula])
        .output()
        .map_err(|source| Error::Io {
            path: PathBuf::from("brew"),
            source,
        })?;
    if !output.status.success() {
        return Err(brew_command_error(
            &["list", "--versions", formula],
            &output.stderr,
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // `brew list --versions <formula>` prints exactly `"<formula> <version>"`; the version is
    // everything after the formula name, trimmed, never guessed from a different format.
    Ok(text
        .trim()
        .strip_prefix(formula)
        .map(str::trim)
        .filter(|version| !version.is_empty())
        .unwrap_or("unknown")
        .to_owned())
}

fn brew_command_error(args: &[&str], stderr: &[u8]) -> Error {
    let text = String::from_utf8_lossy(stderr);
    let first_line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("the command exited with a non-zero status")
        .trim();
    let detail = if first_line.chars().count() > MAX_DETAIL_LEN {
        first_line.chars().take(MAX_DETAIL_LEN).collect::<String>() + "…"
    } else {
        first_line.to_owned()
    };
    Error::UpdateCommandFailed {
        command: format!("brew {}", args.join(" ")),
        detail,
    }
}

/// Any install Relay cannot prove is Homebrew-managed: print where to get the latest release
/// instead of guessing at a self-replace. No constraint violation either way, since Relay never
/// attempts the self-update itself regardless of install method.
fn run_manual_instructions() -> Result<CommandOutput, Error> {
    let current_version = env!("RELAY_VERSION");
    success(
        "update",
        format!(
            "{}Running v{current_version}. This install was not recognized as Homebrew-managed, \
             so Relay will not try to replace its own binary.\nSee the latest release: \
             https://github.com/{REPO}/releases/latest",
            header("Relay update")
        ),
        json!({
            "method": "manual",
            "current_version": current_version,
            "release_url": format!("https://github.com/{REPO}/releases/latest"),
        }),
    )
}
