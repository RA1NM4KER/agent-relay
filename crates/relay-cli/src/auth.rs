//! Shared Claude/Codex profile authentication and inspection primitives, used by more than one
//! command family (`relay profile`, `relay login`/`logout`, `relay claude`'s reauth-before-launch
//! step, and `relay setup`): running the provider's own `auth login`/`logout`, verifying a
//! directory's authentication and identity, adopting it as a registered profile, and resolving
//! which profile a bare `--profile`-less invocation should use.

use std::path::{Path, PathBuf};

use relay_core::{
    AddProfileRequest, AuthenticationState, ClaudeConfigMode, Error, IdentityMetadata, Profile,
    ProfileDirectory, ProfileName, ProfileService, ProfileSetupMode, Provider, ProviderKind,
    RelayPaths,
    automation::ProviderIdentityExhaustionStore,
    usage::{UsageEvidence, UsageObservation, UsageState},
};
use relay_provider_claude::{
    AUTHENTICATION_OVERRIDE_VARIABLES, ClaudeAdoptionProvider, ClaudeIdentityPin,
    ClaudeInspectionReport, ClaudeInspector, inspect_environment,
};
use relay_testkit::FakeProvider;

use crate::{preferences, providers, util::prompt_line};

/// Runs `claude auth login` or `claude auth logout` for one profile's isolated `CLAUDE_CONFIG_DIR`,
/// with this terminal's stdin/stdout/stderr inherited so the user sees and drives Claude's own
/// real login UI (browser open, device code, etc.) directly. Relay only waits for it to exit;
/// nothing about the child's output is read.
pub(crate) fn run_claude_auth_subcommand(
    claude_executable: Option<&Path>,
    config_dir: &Path,
    mode: ClaudeConfigMode,
    subcommand: &str,
) -> Result<(), Error> {
    let inspector = ClaudeInspector::discover(claude_executable)?;
    let mut command = std::process::Command::new(inspector.executable());
    command
        .arg("auth")
        .arg(subcommand)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());
    relay_provider_claude::apply_config_mode(&mut command, mode, config_dir);
    for variable in AUTHENTICATION_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }
    let status = command.status().map_err(|_| Error::ProviderCommandFailed)?;
    if !status.success() {
        return Err(Error::ProviderCommandFailed);
    }
    Ok(())
}

/// After a successful `claude auth login`, runs the exact same strict inspection
/// (`ClaudeInspector::inspect`) `relay profile adopt`/`inspect-existing` already use, and confirms
/// the three things M4.1 Step 2 requires: authenticated, identity available, and (implicitly, since
/// `config_dir` is the exact directory just logged into) the config directory matches.
pub(crate) fn verify_authenticated(
    config_dir: &Path,
    mode: ClaudeConfigMode,
    claude_executable: Option<&Path>,
) -> Result<ClaudeInspectionReport, Error> {
    let environment = inspect_environment(config_dir);
    let inspector = ClaudeInspector::discover(claude_executable)?;
    inspector.inspect(config_dir, mode, environment)
}

/// Registers a freshly authenticated (or re-authenticated) directory as a Relay profile through
/// the unchanged, already-tested adoption path (`ProfileSetupMode::AdoptExisting`) — the same code
/// `relay profile adopt` uses. `ClaudeAdoptionProvider` only ever inspects; it never creates a
/// Claude profile itself (`setup_profile` returns `ProviderUnsupported` for anything but
/// `AdoptExisting`), which is exactly why the directory must already be authenticated before this
/// is called.
pub(crate) fn adopt_authenticated_profile(
    service: &ProfileService,
    name: &ProfileName,
    config_dir: &Path,
    report: &ClaudeInspectionReport,
    claude_executable: Option<&Path>,
) -> Result<Profile, Error> {
    let pin = report
        .identity_pin
        .clone()
        .ok_or(Error::IdentityUnavailable)?;
    let expected_identity = IdentityMetadata {
        stable_id: pin.stable_id(),
        display_label: pin.email.clone().or_else(|| pin.account_id.clone()),
    };
    let claude_provider = ClaudeAdoptionProvider::discover(claude_executable)?;
    service.add(
        AddProfileRequest {
            name: name.clone(),
            provider: ProviderKind::Claude,
            config_dir: Some(config_dir.to_path_buf()),
            mode: ProfileSetupMode::AdoptExisting,
            expected_identity: Some(expected_identity),
            claude_config_mode: Some(ClaudeConfigMode::Explicit),
        },
        &claude_provider,
    )
}

/// M4.1 Step 2A: a brand-new isolated profile. Relay creates the private (mode 0700) directory
/// itself (`ProfileDirectory::create_managed`, the same safety-checked call `relay profile add`
/// uses), launches Claude's own official login flow there, verifies the result with the existing
/// strict inspector, and adopts it — all through machinery that already existed before M4.
pub(crate) fn create_and_authenticate_profile(
    service: &ProfileService,
    paths: &RelayPaths,
    name: &ProfileName,
    claude_executable: Option<&Path>,
) -> Result<Profile, Error> {
    let config_dir = paths.default_profile_dir(name, ProviderKind::Claude);
    ProfileDirectory::new(paths.profiles_root())?.create_managed(&config_dir)?;
    // A freshly created directory under Relay's managed profiles root is always an explicit,
    // isolated profile — never Claude's native-default account.
    run_claude_auth_subcommand(
        claude_executable,
        &config_dir,
        ClaudeConfigMode::Explicit,
        "login",
    )?;
    let report = verify_authenticated(&config_dir, ClaudeConfigMode::Explicit, claude_executable)?;
    if !report.authenticated || report.identity_pin.is_none() {
        return Err(Error::AuthenticationRequired);
    }
    adopt_authenticated_profile(service, name, &config_dir, &report, claude_executable)
}

/// M6: mirrors `create_and_authenticate_profile` for Codex. `CodexBackend::setup_profile`
/// only ever inspects (never creates credentials itself — see its doc comment), so exactly like
/// the Claude path, Relay creates the private directory itself, runs the official `codex login`
/// there (inherited stdio: if browser/device interaction is required, it happens in this exact
/// process, which is the M6 spec's designated stop point for human authorization), then adopts
/// the now-authenticated directory.
pub(crate) fn create_and_authenticate_codex_profile(
    service: &ProfileService,
    paths: &RelayPaths,
    name: &ProfileName,
    codex_executable: Option<&Path>,
) -> Result<Profile, Error> {
    let config_dir = paths.default_profile_dir(name, ProviderKind::Codex);
    ProfileDirectory::new(paths.profiles_root())?.create_managed(&config_dir)?;
    run_codex_auth_subcommand(codex_executable, &config_dir, "login")?;
    let backend = relay_provider_codex::CodexBackend::discover(codex_executable)?;
    let observation = backend.inspect_profile(&config_dir, None)?;
    if observation.authentication != AuthenticationState::Authenticated {
        return Err(Error::AuthenticationRequired);
    }
    let expected_identity = observation
        .identity
        .clone()
        .ok_or(Error::IdentityUnavailable)?;
    service.add(
        AddProfileRequest {
            name: name.clone(),
            provider: ProviderKind::Codex,
            config_dir: Some(config_dir),
            mode: ProfileSetupMode::AdoptExisting,
            expected_identity: Some(expected_identity),
            claude_config_mode: None,
        },
        &backend,
    )
}

pub(crate) fn run_codex_auth_subcommand(
    codex_executable: Option<&Path>,
    config_dir: &Path,
    subcommand: &str,
) -> Result<(), Error> {
    let inspector = relay_provider_codex::CodexInspector::discover(codex_executable)?;
    let mut command = std::process::Command::new(inspector.executable());
    command
        .arg(subcommand)
        .env("CODEX_HOME", config_dir)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());
    for variable in relay_provider_codex::AUTHENTICATION_OVERRIDE_VARIABLES {
        command.env_remove(variable);
    }
    let status = command.status().map_err(|_| Error::ProviderCommandFailed)?;
    if !status.success() {
        return Err(Error::ProviderCommandFailed);
    }
    Ok(())
}

/// Which provider a brand-new `relay login <name>` profile is for when `--provider` was not
/// given: never silently Claude. Exactly one installed CLI decides it; with both, an interactive
/// terminal is asked and anything else fails clearly.
pub(crate) fn resolve_new_profile_provider(
    executables: &providers::ExecutableOverrides,
    json_mode: bool,
) -> Result<ProviderKind, Error> {
    use std::io::IsTerminal as _;
    let claude = ClaudeInspector::discover(executables.claude.as_deref()).is_ok();
    let codex =
        relay_provider_codex::CodexInspector::discover(executables.codex.as_deref()).is_ok();
    let interactive =
        !json_mode && std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    choose_provider(claude, codex, interactive, || {
        prompt_line("Provider for the new profile (claude/codex)", None)
    })
}

pub(crate) fn choose_provider(
    claude_installed: bool,
    codex_installed: bool,
    interactive: bool,
    mut ask: impl FnMut() -> Result<String, Error>,
) -> Result<ProviderKind, Error> {
    match (claude_installed, codex_installed) {
        (true, false) => Ok(ProviderKind::Claude),
        (false, true) => Ok(ProviderKind::Codex),
        (false, false) => Err(Error::ProviderExecutableMissing),
        (true, true) if !interactive => Err(Error::ProviderChoiceRequired),
        (true, true) => loop {
            match ask()?.trim().to_ascii_lowercase().as_str() {
                "claude" => return Ok(ProviderKind::Claude),
                "codex" => return Ok(ProviderKind::Codex),
                _ => println!("Please answer 'claude' or 'codex'."),
            }
        },
    }
}

pub(crate) fn doctor_is_healthy(
    service: &ProfileService,
    profile: &Profile,
    executables: &providers::ExecutableOverrides,
) -> Result<bool, Error> {
    let provider = provider_for_profile(service, &profile.name, executables)?;
    Ok(service.doctor(&profile.name, provider.as_ref())?.healthy)
}

/// Minimum current eligibility proof for an automatic fallback. Human-facing `relay doctor`
/// remains comprehensive; this avoids repeating Codex's expensive `doctor --json` after the
/// immediately preceding supported app-server usage read has already authenticated and bound the
/// exact isolated home.
pub(crate) fn fallback_is_healthy(
    service: &ProfileService,
    profile: &Profile,
    executables: &providers::ExecutableOverrides,
    usage: &UsageObservation,
) -> Result<bool, Error> {
    if profile.provider != ProviderKind::Codex {
        return doctor_is_healthy(service, profile, executables);
    }

    service.validate_profile_directory(profile)?;
    let identity_matches = relay_provider_codex::config_dir_stable_id(&profile.config_dir)
        == profile.expected_identity.stable_id;
    let app_server_authenticated = codex_usage_proves_current_auth(usage);
    Ok(identity_matches && app_server_authenticated)
}

fn codex_usage_proves_current_auth(usage: &UsageObservation) -> bool {
    usage.evidence == UsageEvidence::ProviderRateLimitApi
}

/// Resolves the identity now exposed by the provider, then requires it to equal Relay's registered
/// pin. A provider-account exhaustion record is never applied merely because a profile name still
/// points at an old config directory after the operator has changed accounts.
pub(crate) fn verified_current_stable_identity(
    profile: &Profile,
    executables: &providers::ExecutableOverrides,
) -> Result<Option<String>, Error> {
    let observed = match profile.provider {
        ProviderKind::Claude => {
            let inspector = ClaudeInspector::discover(executables.claude.as_deref())?;
            let report = inspector.inspect(
                &profile.config_dir,
                profile.effective_claude_config_mode(),
                inspect_environment(&profile.config_dir),
            )?;
            report
                .authenticated
                .then(|| report.identity_pin.map(|pin| pin.stable_id()))
                .flatten()
        }
        // Codex's canonical registered identity is intentionally the isolated CODEX_HOME
        // fingerprint: its supported APIs expose no account identifier. Recomputing it is a
        // local validation of that exact isolation boundary, not a provider/auth inspection.
        ProviderKind::Codex => Some(relay_provider_codex::config_dir_stable_id(
            &profile.config_dir,
        )),
        ProviderKind::Fake => Some(profile.expected_identity.stable_id.clone()),
    };
    Ok(observed.filter(|stable_id| stable_id == &profile.expected_identity.stable_id))
}

/// Records a newly proven provider-account exhaustion, then applies an unexpired matching record
/// as `RESET_PENDING`. An authoritative future reset is never erased by the mere *absence* of a
/// fresh signal (`UNKNOWN` still defers to it exactly as before) — but a real M6 incident found a
/// genuinely-detected exhaustion invalidated less than a minute later by an out-of-band, mid-window
/// provider-side usage reset the record's own clock had no way to know about; with nothing to
/// supersede it, Relay kept refusing a healthy account as `RESET_PENDING` for three more days. A
/// *positive* fresh reading — the account actually observed `AVAILABLE`/`NEAR_LIMIT` right now, not
/// just unread — is real, current, contradicting evidence and clears the stale record outright
/// rather than being silently overridden by the older stored fact.
pub(crate) fn apply_provider_exhaustion(
    paths: &RelayPaths,
    profile: &Profile,
    executables: &providers::ExecutableOverrides,
    usage: UsageObservation,
    now_unix_ms: u64,
) -> Result<UsageObservation, Error> {
    let Some(stable_identity) = verified_current_stable_identity(profile, executables)? else {
        return Ok(usage);
    };
    let store = ProviderIdentityExhaustionStore::at_paths(
        paths.provider_identity_exhaustion_file(),
        paths.provider_identity_exhaustion_lock_file(),
    );
    store.record(
        profile.provider,
        stable_identity.clone(),
        &usage,
        now_unix_ms,
    )?;
    if matches!(usage.state, UsageState::Available | UsageState::NearLimit) {
        store.clear_identity(profile.provider, &stable_identity, now_unix_ms)?;
        return Ok(usage);
    }
    let ledger = store.load()?;
    if let Some(inherited) = ledger.inherited_usage(profile.provider, &stable_identity, now_unix_ms)
    {
        return Ok(inherited);
    }
    Ok(usage)
}

/// Read-only pre-launch check for a previously proven provider-account reset window. It does not
/// contact a usage API or create a provider process; identity resolution remains strict.
pub(crate) fn inherited_provider_exhaustion(
    paths: &RelayPaths,
    profile: &Profile,
    executables: &providers::ExecutableOverrides,
    now_unix_ms: u64,
) -> Result<Option<UsageObservation>, Error> {
    let Some(stable_identity) = verified_current_stable_identity(profile, executables)? else {
        return Ok(None);
    };
    let store = ProviderIdentityExhaustionStore::at_paths(
        paths.provider_identity_exhaustion_file(),
        paths.provider_identity_exhaustion_lock_file(),
    );
    Ok(store
        .load()?
        .inherited_usage(profile.provider, &stable_identity, now_unix_ms))
}

/// Friendly per-profile authentication summary for `relay profiles`/`relay status`. Never fails
/// the whole listing on one profile's inspection error — reports it as "unreachable" instead, so
/// one broken profile does not hide every other one.
pub(crate) fn friendly_auth_state(
    profile: &Profile,
    executables: &providers::ExecutableOverrides,
) -> (&'static str, Option<String>) {
    if profile.provider == ProviderKind::Claude {
        // Kept on the strict Claude-specific inspector (identity pin required, not just
        // "authenticated") rather than the generic Provider::inspect_profile dispatch: this is
        // the pre-M6 behavior and changing it is out of M6's scope.
        let Ok(inspector) = ClaudeInspector::discover(executables.claude.as_deref()) else {
            return ("unreachable", None);
        };
        let environment = inspect_environment(&profile.config_dir);
        return match inspector.inspect(
            &profile.config_dir,
            profile.effective_claude_config_mode(),
            environment,
        ) {
            Ok(report) if report.authenticated && report.identity_pin.is_some() => {
                ("authenticated", None)
            }
            Ok(_) => ("needs login", None),
            Err(error) => ("unreachable", Some(error.to_string())),
        };
    }
    let Ok(provider) = providers::provider_backend(profile.provider, executables) else {
        return ("unreachable", None);
    };
    match provider.inspect_profile(&profile.config_dir, None) {
        Ok(observation) if observation.authentication == AuthenticationState::Authenticated => {
            ("authenticated", None)
        }
        Ok(_) => ("needs login", None),
        Err(error) => ("unreachable", Some(error.to_string())),
    }
}

/// M4.2/M4.3/M4.4/M4.5, revised post-M6: `relay claude` — the normal daily entry point for
/// *starting* a new Relay-managed conversation. `relay claude = claude + Relay supervision`: it
/// always launches a fresh session under the configured primary profile via `perform_launch`
/// (M2B.5), auto-writes Herdr pane/workspace metadata when running inside a Herdr pane, then hands
/// the user a live interactive terminal via `claude attach` (M4.4) — never printing a session UUID
/// for the user to copy anywhere.
///
/// Every `relay claude` starts its own Relay session (the unit of continuity and handoff), even
/// when the project already has other active ones — under this profile or any other. It never stops,
/// replaces or reattaches to another session; `relay resume` continues a dormant one.
/// The profile a provider-specific entrypoint starts: `--profile` when given (it must belong to
/// that provider), otherwise the highest-priority configured profile *of that provider* in the one
/// global order (`primary`, then the fallbacks). Deterministic, never prompts.
pub(crate) fn select_profile<'a>(
    registered: &'a [Profile],
    preferences: &preferences::Preferences,
    explicit: Option<&ProfileName>,
    expected: ProviderKind,
) -> Result<&'a Profile, Error> {
    let matches_provider = |provider: ProviderKind| match expected {
        ProviderKind::Codex => provider == ProviderKind::Codex,
        ProviderKind::Claude | ProviderKind::Fake => {
            matches!(provider, ProviderKind::Claude | ProviderKind::Fake)
        }
    };
    let label = match expected {
        ProviderKind::Codex => "Codex",
        ProviderKind::Claude | ProviderKind::Fake => "Claude",
    };
    if let Some(name) = explicit {
        let profile = registered
            .iter()
            .find(|candidate| &candidate.name == name)
            .ok_or_else(|| Error::ProfileNotFound(name.to_string()))?;
        if !matches_provider(profile.provider) {
            return Err(Error::ProfileProviderMismatch {
                profile: name.to_string(),
                expected: label,
                actual: profile.provider.to_string(),
            });
        }
        return Ok(profile);
    }
    if preferences.primary_profile.is_none() && preferences.fallback_profiles.is_empty() {
        // Not set up at all: keep the long-standing "run relay setup" failure.
        return Err(Error::AdoptionIdentityRequired);
    }
    preferences
        .primary_profile
        .iter()
        .chain(preferences.fallback_profiles.iter())
        .find_map(|name| {
            registered
                .iter()
                .find(|candidate| &candidate.name == name && matches_provider(candidate.provider))
        })
        .ok_or(Error::NoProfileForProvider(label))
}

/// Resolves `--config-dir`/`--native-default`/`--allow-external` into the directory to inspect,
/// the `allow_external` to actually apply, and the [`ClaudeConfigMode`] the rest of the pipeline
/// must use. `~/.claude` is never inside Relay's managed profiles root, so native-default always
/// implies external. Clap's `required_unless_present`/`conflicts_with` guarantee exactly one of
/// `config_dir`/`native_default` is meaningfully set before this runs.
pub(crate) fn resolve_claude_adoption_target(
    config_dir: Option<&std::path::Path>,
    native_default: bool,
    allow_external: bool,
) -> Result<(PathBuf, bool, ClaudeConfigMode), Error> {
    if native_default {
        let dir =
            relay_provider_claude::native_default_dir().ok_or(Error::MissingEnvironment("HOME"))?;
        Ok((dir, true, ClaudeConfigMode::NativeDefault))
    } else {
        let dir = config_dir
            .ok_or(Error::MissingEnvironment("--config-dir"))?
            .to_path_buf();
        Ok((dir, allow_external, ClaudeConfigMode::Explicit))
    }
}

pub(crate) fn inspect_existing_claude(
    paths: &RelayPaths,
    requested_config_dir: &std::path::Path,
    allow_external: bool,
    requested_executable: Option<&std::path::Path>,
    mode: ClaudeConfigMode,
) -> Result<ClaudeInspectionReport, Error> {
    let config_dir = paths.validate_adoption_path(requested_config_dir, allow_external)?;
    let environment = inspect_environment(&config_dir);
    let inspector = ClaudeInspector::discover(requested_executable)?;
    inspector.inspect(&config_dir, mode, environment)
}

pub(crate) fn inspection_human(report: &ClaudeInspectionReport) -> String {
    let identity = report
        .identity_pin
        .as_ref()
        .map(identity_summary)
        .unwrap_or_else(|| "unavailable".to_owned());
    let conflicts = report
        .environment_override_status
        .conflicting_names()
        .join(", ");
    format!(
        "Claude profile: {}\nVersion: {}\nAuthenticated: {}\nIdentity: {}\nEnvironment overrides: {}\nSafe to adopt: {}",
        report.config_dir.display(),
        report.claude_version,
        if report.authenticated { "yes" } else { "no" },
        identity,
        if conflicts.is_empty() {
            "none".to_owned()
        } else {
            format!("conflict ({conflicts})")
        },
        if report.safe_to_adopt { "yes" } else { "no" }
    )
}

pub(crate) fn identity_summary(identity: &ClaudeIdentityPin) -> String {
    if let Some(account_id) = &identity.account_id {
        format!("account_id={account_id}")
    } else if let Some(email) = &identity.email {
        match &identity.organization_id {
            Some(organization) => format!("email={email}, organization_id={organization}"),
            None => format!("email={email}"),
        }
    } else {
        "unavailable".to_owned()
    }
}

/// `status`/`doctor` must inspect through the profile's own provider, not always the fake one:
/// a real Claude profile that has been adopted needs a real Claude inspection, not a fake marker.
pub(crate) fn provider_for_profile(
    service: &ProfileService,
    name: &ProfileName,
    executables: &providers::ExecutableOverrides,
) -> Result<Box<dyn Provider>, Error> {
    let kind = service
        .list()?
        .into_iter()
        .find(|profile| &profile.name == name)
        .map(|profile| profile.provider);
    Ok(match kind {
        Some(ProviderKind::Claude) => Box::new(ClaudeAdoptionProvider::discover(
            executables.claude.as_deref(),
        )?),
        Some(ProviderKind::Codex) => providers::provider_backend(ProviderKind::Codex, executables)?,
        _ => Box::new(FakeProvider::default()),
    })
}

#[cfg(test)]
mod tests {
    use super::{apply_provider_exhaustion, choose_provider, codex_usage_proves_current_auth};
    use crate::providers;
    use std::path::PathBuf;

    use relay_core::{
        Availability, AvailabilityObservation, Error, IdentityMetadata, Profile, ProfileName,
        ProfileOrigin, ProviderKind, RelayPaths,
        automation::ProviderIdentityExhaustionStore,
        usage::{UsageEvidence, UsageObservation, UsageState},
    };
    use tempfile::tempdir;

    fn fake_profile(name: &str, stable_id: &str, config_dir: PathBuf) -> Profile {
        Profile {
            name: ProfileName::new(name).expect("name"),
            provider: ProviderKind::Fake,
            config_dir,
            enabled: true,
            origin: ProfileOrigin::Created,
            expected_identity: IdentityMetadata {
                stable_id: stable_id.to_owned(),
                display_label: None,
            },
            last_availability: AvailabilityObservation {
                state: Availability::Unknown,
                source: "test".to_owned(),
                observed_unix_ms: 0,
                reset_unix_ms: None,
            },
            claude_config_mode: None,
        }
    }

    fn observation(state: UsageState, reset_unix_ms: Option<u64>) -> UsageObservation {
        UsageObservation {
            state,
            evidence: UsageEvidence::StopFailureHistoricalCorroborated,
            detected_via: "test".to_owned(),
            observed_unix_ms: 1_000,
            reset_unix_ms,
        }
    }

    /// M6 regression: a genuinely-detected exhaustion, superseded less than a minute later by an
    /// out-of-band provider-side usage reset the durable record's own clock could not know about,
    /// left a real, then-healthy account durably `RESET_PENDING` for three more days with no way to
    /// notice. A fresh, positive `AVAILABLE` reading for the same identity must clear that stale
    /// record outright rather than being silently overridden by it.
    #[test]
    fn a_fresh_available_reading_supersedes_a_stale_durable_exhaustion() {
        let root = tempdir().expect("tempdir");
        let paths =
            RelayPaths::new(root.path().join("config"), root.path().join("state")).expect("paths");
        let profile = fake_profile("claude-main", "fake:megan", root.path().join("profile-dir"));
        let executables = providers::ExecutableOverrides::default();

        let exhausted = apply_provider_exhaustion(
            &paths,
            &profile,
            &executables,
            observation(UsageState::Exhausted, Some(9_999_999)),
            1_000,
        )
        .expect("record exhaustion");
        // `apply_provider_exhaustion` re-reads the ledger it just wrote to, so even this first,
        // establishing call reports back `ResetPending` (the durable, inherited reading) rather
        // than the raw `Exhausted` it was given — this is existing, unchanged behavior; what this
        // test actually checks is what happens on the *next* call, below.
        assert_eq!(exhausted.state, UsageState::ResetPending);

        // A different session/profile sharing the exact same identity would inherit the stale
        // record right up until the fresh reading below clears it.
        let store = ProviderIdentityExhaustionStore::at_paths(
            paths.provider_identity_exhaustion_file(),
            paths.provider_identity_exhaustion_lock_file(),
        );
        assert!(
            store
                .load()
                .expect("load")
                .inherited_usage(ProviderKind::Fake, "fake:megan", 2_000)
                .is_some(),
            "the durable record must exist before the fresh reading clears it"
        );

        // The account has since been reset out-of-band; the very next check reads it fresh and
        // positively as available.
        let recovered = apply_provider_exhaustion(
            &paths,
            &profile,
            &executables,
            observation(UsageState::Available, None),
            2_000,
        )
        .expect("apply fresh reading");
        assert_eq!(
            recovered.state,
            UsageState::Available,
            "a fresh, positive reading must win outright, not be overridden by the stale record"
        );
        assert!(
            store
                .load()
                .expect("load")
                .inherited_usage(ProviderKind::Fake, "fake:megan", 2_001)
                .is_none(),
            "the stale record must be cleared, not merely bypassed once"
        );
    }

    /// The mirror case: a fresh reading of `UNKNOWN` (no real signal at all, e.g. no session
    /// currently running to produce one) is the *absence* of evidence, not evidence of recovery —
    /// it must keep deferring to the durable record exactly as before this fix.
    #[test]
    fn an_unknown_reading_still_defers_to_the_durable_record() {
        let root = tempdir().expect("tempdir");
        let paths =
            RelayPaths::new(root.path().join("config"), root.path().join("state")).expect("paths");
        let profile = fake_profile("claude-main", "fake:megan", root.path().join("profile-dir"));
        let executables = providers::ExecutableOverrides::default();

        apply_provider_exhaustion(
            &paths,
            &profile,
            &executables,
            observation(UsageState::Exhausted, Some(9_999_999)),
            1_000,
        )
        .expect("record exhaustion");

        let unknown = apply_provider_exhaustion(
            &paths,
            &profile,
            &executables,
            observation(UsageState::Unknown, None),
            2_000,
        )
        .expect("apply unknown reading");
        assert_eq!(
            unknown.state,
            UsageState::ResetPending,
            "no real signal must never be treated as proof of recovery"
        );
    }

    #[test]
    fn only_the_supported_codex_rate_limit_read_proves_current_authentication() {
        let observation = |evidence| UsageObservation {
            state: UsageState::Available,
            evidence,
            detected_via: "test".to_owned(),
            observed_unix_ms: 1,
            reset_unix_ms: None,
        };
        assert!(codex_usage_proves_current_auth(&observation(
            UsageEvidence::ProviderRateLimitApi
        )));
        assert!(!codex_usage_proves_current_auth(&observation(
            UsageEvidence::Simulated
        )));
    }

    #[test]
    fn a_new_profile_provider_is_implied_by_the_only_installed_cli() {
        let never = || -> Result<String, Error> { panic!("must not ask") };
        assert_eq!(
            choose_provider(true, false, true, never).unwrap(),
            ProviderKind::Claude
        );
        assert_eq!(
            choose_provider(false, true, false, never).unwrap(),
            ProviderKind::Codex
        );
        assert!(matches!(
            choose_provider(false, false, true, never),
            Err(Error::ProviderExecutableMissing)
        ));
    }

    #[test]
    fn with_both_installed_it_asks_in_a_terminal_and_fails_clearly_otherwise() {
        assert!(matches!(
            choose_provider(true, true, false, || Ok("claude".to_owned())),
            Err(Error::ProviderChoiceRequired)
        ));
        let mut answers = ["maybe", "Codex"].into_iter();
        let chosen = choose_provider(true, true, true, || Ok(answers.next().unwrap().to_owned()));
        assert_eq!(chosen.unwrap(), ProviderKind::Codex);
    }
}
