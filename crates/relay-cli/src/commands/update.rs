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
/// Keep Homebrew's error detail to one bounded line.
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
            format!("{heading}Running v{current_version}. No newer stable release is available."),
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
                "{heading}Running v{current_version}. Could not verify the latest stable \
                 release right now. Try again later."
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
/// are real Homebrew subcommands; only this formula is requested for upgrade. Relay performs no
/// file replacement of its own.
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

/// Ask Homebrew for the linked keg, not every retained installed keg (#27). Ownership is
/// still established before delegation; this read only reports the post-upgrade result.
fn installed_version(formula: &str) -> Result<String, Error> {
    let args = ["info", "--json=v2", formula];
    let output = Command::new("brew")
        .args(args)
        .output()
        .map_err(|source| Error::Io {
            path: PathBuf::from("brew"),
            source,
        })?;
    if !output.status.success() {
        return Err(brew_command_error(&args, &output.stderr));
    }
    linked_version(&output.stdout, formula).ok_or_else(|| {
        brew_command_error(
            &args,
            b"Homebrew did not report an unambiguous linked version",
        )
    })
}

fn linked_version(bytes: &[u8], formula: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let formulae = value.get("formulae")?.as_array()?;
    let [entry] = formulae.as_slice() else {
        return None;
    };
    if entry.get("name")?.as_str()? != formula {
        return None;
    }
    let linked = entry.get("linked_keg")?.as_str()?;
    if linked.is_empty()
        || linked.len() > 120
        || !linked
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-+_".contains(&b))
    {
        return None;
    }
    entry
        .get("installed")?
        .as_array()?
        .iter()
        .any(|keg| keg.get("version").and_then(serde_json::Value::as_str) == Some(linked))
        .then(|| linked.to_owned())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linked_keg_is_authoritative_among_retained_versions() {
        let value = json!({"formulae": [{"name": "agent-relay", "linked_keg": "0.5.1",
            "installed": [{"version":"0.5.0"}, {"version":"0.5.2"}, {"version":"0.5.1"}]}]});
        assert_eq!(
            linked_version(value.to_string().as_bytes(), "agent-relay").as_deref(),
            Some("0.5.1")
        );
    }

    #[test]
    fn unlinked_ambiguous_malformed_or_mismatched_info_is_not_guessed() {
        for value in [
            json!({}),
            json!({"formulae":[]}),
            json!({"formulae":[{"name":"other", "linked_keg":"0.5.1"}]}),
            json!({"formulae":[{"name":"agent-relay", "linked_keg":null, "installed":[{"version":"0.5.1"}]}]}),
            json!({"formulae":[{"name":"agent-relay", "linked_keg":"0.5.1 0.5.0", "installed":[]}]}),
            json!({"formulae":[{"name":"agent-relay", "linked_keg":"0.5.1", "installed":[{"version":"0.5.0"}]}]}),
        ] {
            assert!(linked_version(value.to_string().as_bytes(), "agent-relay").is_none());
        }
        assert!(linked_version(b"not JSON", "agent-relay").is_none());
    }
}
