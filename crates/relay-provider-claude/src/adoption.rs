//! Registers an existing, already-authenticated Claude profile with Relay by reference.
//!
//! This provider never creates a Claude profile, never writes beneath the profile's
//! `CLAUDE_CONFIG_DIR`, and never touches credentials. It re-runs the same read-only
//! inspection as `profile inspect-existing` / `profile adopt --dry-run` immediately before
//! Relay's registry write so the identity check is fresh at transaction time.

use std::{
    ffi::OsString,
    path::Path,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use relay_core::{
    AuthenticationState, Availability, AvailabilityObservation, Error, IdentityMetadata,
    ProfileSetupMode, ProfileSetupRequest, Provider, ProviderKind, ProviderObservation, Result,
};

use crate::{
    ClaudeInspectionReport, ClaudeInspector, CommandRunner, SystemCommandRunner,
    inspect_environment_with,
};

type EnvironmentLookup = Arc<dyn Fn(&str) -> Option<OsString> + Send + Sync>;

#[derive(Clone)]
pub struct ClaudeAdoptionProvider<R: CommandRunner = SystemCommandRunner> {
    inspector: ClaudeInspector<R>,
    environment_lookup: EnvironmentLookup,
}

impl ClaudeAdoptionProvider<SystemCommandRunner> {
    pub fn discover(requested_executable: Option<&Path>) -> Result<Self> {
        Ok(Self {
            inspector: ClaudeInspector::discover(requested_executable)?,
            environment_lookup: Arc::new(|name: &str| std::env::var_os(name)),
        })
    }
}

impl<R: CommandRunner> ClaudeAdoptionProvider<R> {
    #[must_use]
    pub fn with_inspector(inspector: ClaudeInspector<R>) -> Self {
        Self {
            inspector,
            environment_lookup: Arc::new(|name: &str| std::env::var_os(name)),
        }
    }

    #[cfg(test)]
    fn with_inspector_and_environment(
        inspector: ClaudeInspector<R>,
        environment_lookup: impl Fn(&str) -> Option<OsString> + Send + Sync + 'static,
    ) -> Self {
        Self {
            inspector,
            environment_lookup: Arc::new(environment_lookup),
        }
    }
}

impl<R: CommandRunner> Provider for ClaudeAdoptionProvider<R> {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Claude
    }

    /// Only `AdoptExisting` is supported: this provider never creates a Claude profile.
    /// Gating (authentication required, identity required) is left to `ProfileService::add`,
    /// exactly as `FakeProvider` leaves it, so this reports truthfully rather than erroring.
    fn setup_profile(&self, request: &ProfileSetupRequest) -> Result<ProviderObservation> {
        if request.mode != ProfileSetupMode::AdoptExisting {
            return Err(Error::ProviderUnsupported);
        }
        self.inspect_profile(&request.config_dir)
    }

    /// Never errors on an expected state (unauthenticated, no identity yet): a profile can
    /// become unauthenticated after adoption, and `status`/`doctor` must be able to report
    /// that plainly rather than failing the whole inspection.
    fn inspect_profile(&self, config_dir: &Path) -> Result<ProviderObservation> {
        let environment =
            inspect_environment_with(config_dir, |name| (self.environment_lookup)(name));
        let report = self.inspector.inspect(config_dir, environment)?;
        Ok(to_observation(&report))
    }
}

fn to_observation(report: &ClaudeInspectionReport) -> ProviderObservation {
    let identity = report.identity_pin.as_ref().map(|pin| IdentityMetadata {
        stable_id: pin.stable_id(),
        display_label: pin.email.clone().or_else(|| pin.account_id.clone()),
    });
    let authentication = if report.authenticated && identity.is_some() {
        AuthenticationState::Authenticated
    } else {
        AuthenticationState::Required
    };
    // A successful auth check does not by itself imply quota availability.
    let state = match authentication {
        AuthenticationState::Authenticated => Availability::Unknown,
        AuthenticationState::Required => Availability::AuthRequired,
        AuthenticationState::InspectionFailed => Availability::Unknown,
    };
    let observed_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default();
    ProviderObservation {
        authentication,
        identity,
        availability: AvailabilityObservation {
            state,
            source: "claude_auth_status".to_owned(),
            observed_unix_ms,
            reset_unix_ms: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use relay_core::{ProfileName, ProfileSetupMode, ProfileSetupRequest, Provider};
    use tempfile::tempdir;

    use super::ClaudeAdoptionProvider;
    use crate::{ClaudeInspector, CommandRunner, ProcessResult, ProcessSpec};

    #[derive(Clone)]
    struct ScriptedRunner {
        responses: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<&'static str>>>,
    }

    impl ScriptedRunner {
        fn new(responses: Vec<&'static str>) -> Self {
            Self {
                responses: std::sync::Arc::new(std::sync::Mutex::new(responses.into())),
            }
        }
    }

    impl CommandRunner for ScriptedRunner {
        fn run(&self, spec: &ProcessSpec) -> relay_core::Result<ProcessResult> {
            let _ = spec;
            let next = self
                .responses
                .lock()
                .expect("responses lock")
                .pop_front()
                .expect("scripted response");
            Ok(ProcessResult {
                success: true,
                stdout: next.as_bytes().to_vec(),
            })
        }
    }

    fn executable(root: &std::path::Path) -> PathBuf {
        let executable = root.join("claude-fixture");
        fs::write(&executable, "fixture").expect("write executable fixture");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
                .expect("executable permissions");
        }
        executable
    }

    fn adopt_request(config_dir: PathBuf) -> ProfileSetupRequest {
        ProfileSetupRequest {
            name: ProfileName::new("erika").expect("profile name"),
            config_dir,
            mode: ProfileSetupMode::AdoptExisting,
        }
    }

    #[test]
    fn authenticated_profile_produces_a_registration_observation_without_writing() {
        let root = tempdir().expect("temp directory");
        let config_dir = root.path().join("profile");
        fs::create_dir(&config_dir).expect("config dir");
        let runner = ScriptedRunner::new(vec![
            "2.1.276 (Claude Code)",
            r#"{"loggedIn":true,"authMethod":"oauth","apiProvider":"firstParty","accountUuid":"account-erika","email":"erika@example.com"}"#,
        ]);
        let inspector =
            ClaudeInspector::with_runner(executable(root.path()), runner).expect("inspector");
        let provider = ClaudeAdoptionProvider::with_inspector_and_environment(inspector, |_| None);

        let before = fs::read_dir(&config_dir).expect("read config dir").count();
        let observation = provider
            .setup_profile(&adopt_request(config_dir.clone()))
            .expect("setup profile");
        let after = fs::read_dir(&config_dir).expect("read config dir").count();

        assert_eq!(
            before, after,
            "adoption must not write into the Claude directory"
        );
        let identity = observation.identity.expect("identity present");
        assert!(identity.stable_id.contains("account-erika"));
    }

    #[test]
    fn create_mode_is_rejected() {
        let root = tempdir().expect("temp directory");
        let config_dir = root.path().join("profile");
        fs::create_dir(&config_dir).expect("config dir");
        let runner = ScriptedRunner::new(vec![]);
        let inspector =
            ClaudeInspector::with_runner(executable(root.path()), runner).expect("inspector");
        let provider = ClaudeAdoptionProvider::with_inspector(inspector);

        let mut request = adopt_request(config_dir);
        request.mode = ProfileSetupMode::Create;
        let error = provider
            .setup_profile(&request)
            .expect_err("create mode must be rejected");
        assert_eq!(error.code(), "provider_unsupported");
    }

    #[test]
    fn unauthenticated_profile_is_reported_without_erroring() {
        let root = tempdir().expect("temp directory");
        let config_dir = root.path().join("profile");
        fs::create_dir(&config_dir).expect("config dir");
        let runner = ScriptedRunner::new(vec![
            "2.1.276 (Claude Code)",
            r#"{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty"}"#,
        ]);
        let inspector =
            ClaudeInspector::with_runner(executable(root.path()), runner).expect("inspector");
        let provider = ClaudeAdoptionProvider::with_inspector_and_environment(inspector, |_| None);

        let observation = provider
            .setup_profile(&adopt_request(config_dir))
            .expect("inspection itself must succeed; gating is ProfileService's job");
        assert_eq!(
            observation.authentication,
            relay_core::AuthenticationState::Required
        );
        assert!(observation.identity.is_none());
    }
}
