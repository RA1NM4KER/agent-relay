use std::{fs, path::PathBuf};

use relay_core::{
    AddProfileRequest, AuthenticationState, Error, IdentityMetadata, ProfileName, ProfileService,
    ProfileSetupMode, ProviderKind, RelayPaths,
};
use relay_testkit::{FakeProvider, FakeSetupBehavior};
use tempfile::tempdir;

fn paths(root: &std::path::Path, name: &str) -> RelayPaths {
    RelayPaths::new(
        root.join(format!("{name}-config")),
        root.join(format!("{name}-state")),
    )
    .expect("absolute test paths")
}

fn request(name: &str) -> AddProfileRequest {
    AddProfileRequest {
        name: ProfileName::new(name).expect("valid profile name"),
        provider: ProviderKind::Fake,
        config_dir: None,
        mode: ProfileSetupMode::Create,
        expected_identity: None,
        claude_config_mode: None,
    }
}

#[test]
fn creates_lists_and_removes_profile_without_deleting_directory() {
    let root = tempdir().expect("temp directory");
    let service = ProfileService::new(paths(root.path(), "basic"));
    let provider = FakeProvider::default();

    let added = service
        .add(request("megan"), &provider)
        .expect("add profile");
    assert!(added.config_dir.is_dir());
    assert_eq!(service.list().expect("list profiles"), vec![added.clone()]);

    let removed = service
        .remove(&ProfileName::new("megan").expect("name"))
        .expect("remove profile");
    assert_eq!(removed, added);
    assert!(
        removed.config_dir.is_dir(),
        "provider directory must be retained"
    );
    assert!(service.list().expect("list profiles").is_empty());
}

#[test]
fn wrong_identity_is_visible_in_status_and_doctor() {
    let root = tempdir().expect("temp directory");
    let service = ProfileService::new(paths(root.path(), "identity"));
    let provider = FakeProvider::default();
    let profile = service
        .add(request("megan"), &provider)
        .expect("add profile");
    FakeProvider::overwrite_identity(&profile.config_dir, "fake:intruder")
        .expect("change fake identity");

    let status = service.status(&profile.name, &provider).expect("status");
    let doctor = service.doctor(&profile.name, &provider).expect("doctor");

    assert!(!status.identity_matches);
    assert!(!doctor.healthy);
    assert!(
        doctor
            .checks
            .iter()
            .any(|check| check.name == "identity" && !check.passed)
    );
}

#[test]
fn authentication_failure_does_not_register_profile() {
    let root = tempdir().expect("temp directory");
    let service = ProfileService::new(paths(root.path(), "auth"));
    let provider = FakeProvider::with_setup_behavior(FakeSetupBehavior::AuthenticationRequired);

    let error = service
        .add(request("megan"), &provider)
        .expect_err("auth must fail");

    assert_eq!(error.code(), "authentication_required");
    assert!(service.list().expect("list profiles").is_empty());
}

#[test]
fn duplicate_profile_names_are_rejected() {
    let root = tempdir().expect("temp directory");
    let service = ProfileService::new(paths(root.path(), "duplicate"));
    let provider = FakeProvider::default();
    service.add(request("megan"), &provider).expect("first add");

    let error = service
        .add(request("megan"), &provider)
        .expect_err("duplicate must fail");

    assert_eq!(error.code(), "duplicate_profile");
    assert_eq!(service.list().expect("list profiles").len(), 1);
}

#[test]
fn duplicate_identity_pins_are_rejected_across_profile_names() {
    let root = tempdir().expect("temp directory");
    let service = ProfileService::new(paths(root.path(), "duplicate-identity"));
    let provider =
        FakeProvider::with_setup_behavior(FakeSetupBehavior::Identity("fake:same".to_owned()));
    service.add(request("erika"), &provider).expect("first add");

    let error = service
        .add(request("megan"), &provider)
        .expect_err("duplicate identity must fail closed");

    assert_eq!(error.code(), "duplicate_identity");
    let profiles = service.list().expect("list profiles");
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].name.as_str(), "erika");
}

#[test]
fn paths_with_spaces_and_unicode_are_supported() {
    let root = tempdir().expect("temp directory");
    let nested = root.path().join("relay data 日本語 with spaces");
    fs::create_dir(&nested).expect("unicode parent");
    let service = ProfileService::new(paths(&nested, "unicode"));

    let profile = service
        .add(request("megan"), &FakeProvider::default())
        .expect("add under unicode path");

    assert!(profile.config_dir.is_dir());
    assert!(
        profile
            .config_dir
            .to_string_lossy()
            .contains("日本語 with spaces")
    );
}

#[test]
fn unsafe_relative_and_outside_paths_are_rejected() {
    let root = tempdir().expect("temp directory");
    let relay_paths = paths(root.path(), "unsafe");
    let service = ProfileService::new(relay_paths.clone());
    let provider = FakeProvider::default();
    let mut relative = request("relative");
    relative.config_dir = Some(PathBuf::from("relative/profile"));
    let mut outside = request("outside");
    outside.config_dir = Some(root.path().join("outside-profile"));

    let relative_error = service
        .add(relative, &provider)
        .expect_err("relative must fail");
    let outside_error = service
        .add(outside, &provider)
        .expect_err("outside must fail");

    assert_eq!(relative_error.code(), "path_not_absolute");
    assert_eq!(outside_error.code(), "path_outside_managed_root");
}

#[cfg(unix)]
#[test]
fn symlinked_profile_directory_is_rejected() {
    use std::os::unix::fs::symlink;

    let root = tempdir().expect("temp directory");
    let relay_paths = paths(root.path(), "symlink");
    let target = root.path().join("real-profile");
    fs::create_dir(&target).expect("target directory");
    let profile_parent = relay_paths.profiles_root().join("megan");
    fs::create_dir_all(&profile_parent).expect("profile parent");
    let linked = profile_parent.join("fake");
    symlink(&target, &linked).expect("create symlink");
    let service = ProfileService::new(relay_paths);
    let mut add = request("megan");
    add.config_dir = Some(linked);

    let error = service
        .add(add, &FakeProvider::default())
        .expect_err("symlink must fail");

    assert_eq!(error.code(), "symbolic_link");
}

#[cfg(unix)]
#[test]
fn substituted_profile_symlink_is_rejected_before_provider_inspection() {
    use std::os::unix::fs::symlink;

    let root = tempdir().expect("temp directory");
    let service = ProfileService::new(paths(root.path(), "substitution"));
    let provider = FakeProvider::default();
    let profile = service
        .add(request("megan"), &provider)
        .expect("add profile");
    let replacement = root.path().join("replacement");
    fs::create_dir(&replacement).expect("replacement directory");
    fs::remove_dir_all(&profile.config_dir).expect("remove fake profile in temp directory");
    symlink(&replacement, &profile.config_dir).expect("substitute symlink");

    let status_error = service
        .status(&profile.name, &provider)
        .expect_err("unsafe status must fail");
    let doctor = service.doctor(&profile.name, &provider).expect("doctor");

    assert_eq!(status_error.code(), "symbolic_link");
    assert!(!doctor.healthy);
    assert!(doctor.checks.iter().any(|check| {
        check.name == "provider_inspection" && !check.passed && check.message.contains("skipped")
    }));
}

#[cfg(unix)]
#[test]
fn symlinked_config_root_is_rejected() {
    use std::os::unix::fs::symlink;

    let root = tempdir().expect("temp directory");
    let real = root.path().join("real-config");
    let linked = root.path().join("linked-config");
    fs::create_dir(&real).expect("real config root");
    symlink(&real, &linked).expect("config symlink");

    let error = RelayPaths::new(linked, root.path().join("state"))
        .expect_err("symlinked config root must fail");

    assert_eq!(error.code(), "symbolic_link");
}

#[cfg(unix)]
#[test]
fn doctor_detects_unsafe_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempdir().expect("temp directory");
    let service = ProfileService::new(paths(root.path(), "permissions"));
    let provider = FakeProvider::default();
    let profile = service
        .add(request("megan"), &provider)
        .expect("add profile");
    fs::set_permissions(&profile.config_dir, fs::Permissions::from_mode(0o755))
        .expect("weaken permissions");

    let doctor = service.doctor(&profile.name, &provider).expect("doctor");

    assert!(!doctor.healthy);
    let check = doctor
        .checks
        .iter()
        .find(|check| check.name == "directory_security")
        .expect("directory_security check present");
    assert!(!check.passed);
    // M4.5: the actionable `chmod 700 <path>` guidance must reach the doctor report itself, not
    // be swallowed into a generic "failed safety validation" message.
    assert!(check.message.contains("chmod 700"), "{}", check.message);
    assert!(
        check
            .message
            .contains(&profile.config_dir.display().to_string()),
        "{}",
        check.message
    );
}

#[test]
fn planted_secret_is_not_exposed_by_provider_error() {
    let root = tempdir().expect("temp directory");
    let service = ProfileService::new(paths(root.path(), "secret"));
    let provider = FakeProvider::default();
    let profile = service
        .add(request("megan"), &provider)
        .expect("add profile");
    let canary = "oauth-secret-canary";
    fs::write(
        profile.config_dir.join(".relay-fake-profile.toml"),
        format!("invalid = \"{canary}\""),
    )
    .expect("corrupt marker");

    let error = service
        .status(&profile.name, &provider)
        .expect_err("status must fail");
    let rendered = format!("{error:?} {error}");

    assert!(!rendered.contains(canary));
}

#[test]
fn adoption_requires_explicit_identity_and_never_copies_directory() {
    let root = tempdir().expect("temp directory");
    let relay_paths = paths(root.path(), "adopt");
    let seed_service = ProfileService::new(relay_paths.clone());
    let provider = FakeProvider::default();
    let seed = seed_service
        .add(request("seed"), &provider)
        .expect("seed fake profile");
    seed_service.remove(&seed.name).expect("unregister seed");

    let mut missing_pin = request("adopted");
    missing_pin.mode = ProfileSetupMode::AdoptExisting;
    missing_pin.config_dir = Some(seed.config_dir.clone());
    let error = seed_service
        .add(missing_pin, &provider)
        .expect_err("identity pin required");
    assert_eq!(error.code(), "adoption_identity_required");

    let expected_identity = IdentityMetadata {
        stable_id: "fake:seed".to_owned(),
        display_label: None,
    };
    let mut adoption = request("adopted");
    adoption.mode = ProfileSetupMode::AdoptExisting;
    adoption.config_dir = Some(seed.config_dir.clone());
    adoption.expected_identity = Some(expected_identity);
    let adopted = seed_service
        .add(adoption, &provider)
        .expect("adopt profile");

    assert_eq!(adopted.config_dir, seed.config_dir);
    assert_eq!(adopted.origin, relay_core::ProfileOrigin::Adopted);
}

#[test]
fn authentication_can_become_unavailable_after_registration() {
    let root = tempdir().expect("temp directory");
    let service = ProfileService::new(paths(root.path(), "later-auth"));
    let provider = FakeProvider::default();
    let profile = service
        .add(request("megan"), &provider)
        .expect("add profile");
    FakeProvider::overwrite_authentication(&profile.config_dir, AuthenticationState::Required)
        .expect("change auth state");

    let status = service.status(&profile.name, &provider).expect("status");

    assert_eq!(status.authentication, AuthenticationState::Required);
    assert!(!status.identity_matches);
}

#[test]
fn setup_identity_mismatch_is_rejected() {
    let root = tempdir().expect("temp directory");
    let service = ProfileService::new(paths(root.path(), "setup-mismatch"));
    let provider =
        FakeProvider::with_setup_behavior(FakeSetupBehavior::Identity("fake:observed".to_owned()));
    let mut add = request("megan");
    add.expected_identity = Some(IdentityMetadata {
        stable_id: "fake:expected".to_owned(),
        display_label: None,
    });

    let error = service
        .add(add, &provider)
        .expect_err("identity must mismatch");

    assert!(matches!(error, Error::IdentityMismatch));
    assert!(service.list().expect("list profiles").is_empty());
}
