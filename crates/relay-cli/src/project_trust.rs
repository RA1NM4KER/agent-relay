//! Read-only evidence of project trust. Never accepts a provider's trust prompt on the user's
//! behalf. Unknown/unreadable state is not evidence that an unattended terminal can start.

use std::{io::Read as _, path::Path};

use relay_core::{ClaudeConfigMode, Profile, ProviderKind};

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Trust {
    Accepted,
    Missing,
    Unknown,
}

pub(crate) fn check(profile: &Profile, project: &Path) -> Trust {
    check_with_home(
        profile,
        project,
        std::env::var_os("HOME").as_deref().map(Path::new),
    )
}

fn check_with_home(profile: &Profile, project: &Path, home: Option<&Path>) -> Trust {
    let Ok(project) = std::fs::canonicalize(project) else {
        return Trust::Unknown;
    };
    match profile.provider {
        ProviderKind::Claude => {
            let path = match profile.effective_claude_config_mode() {
                ClaudeConfigMode::Explicit => profile.config_dir.join(".claude.json"),
                ClaudeConfigMode::NativeDefault => {
                    let Some(home) = home else {
                        return Trust::Unknown;
                    };
                    home.join(".claude.json")
                }
            };
            read_trust(&path, |text| claude_trust(text, &project))
        }
        ProviderKind::Codex => read_trust(&profile.config_dir.join("config.toml"), |text| {
            codex_trust(text, &project)
        }),
        ProviderKind::Fake => Trust::Accepted,
    }
}

fn read_trust(path: &Path, parse: impl FnOnce(&str) -> Trust) -> Trust {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Trust::Missing,
        Err(_) => return Trust::Unknown,
    };
    // Settings can include sensitive values. Neither file contents nor parser errors escape.
    const LIMIT: u64 = 8 * 1024 * 1024;
    let mut text = String::new();
    if file.take(LIMIT + 1).read_to_string(&mut text).is_err() || text.len() as u64 > LIMIT {
        return Trust::Unknown;
    }
    parse(&text)
}

fn claude_trust(text: &str, project: &Path) -> Trust {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Trust::Unknown;
    };
    let Some(key) = project.to_str() else {
        return Trust::Unknown;
    };
    if !value.is_object() {
        return Trust::Unknown;
    }
    match value
        .get("projects")
        .and_then(|projects| projects.get(key))
        .and_then(|entry| entry.get("hasTrustDialogAccepted"))
    {
        Some(serde_json::Value::Bool(true)) => Trust::Accepted,
        Some(serde_json::Value::Bool(false)) | None => Trust::Missing,
        _ => Trust::Unknown,
    }
}

fn codex_trust(text: &str, project: &Path) -> Trust {
    let Ok(value) = toml::from_str::<toml::Value>(text) else {
        return Trust::Unknown;
    };
    let Some(key) = project.to_str() else {
        return Trust::Unknown;
    };
    // Deliberately require evidence for this exact project. Do not infer trust for siblings or
    // arbitrary descendants from a trusted directory elsewhere in the profile.
    match value
        .get("projects")
        .and_then(|projects| projects.get(key))
        .and_then(|entry| entry.get("trust_level"))
        .and_then(toml::Value::as_str)
    {
        Some("trusted") => Trust::Accepted,
        Some("untrusted") | None => Trust::Missing,
        _ => Trust::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(dir: &Path) -> Profile {
        serde_json::from_value(serde_json::json!({
            "name": "alice", "provider": "claude", "config_dir": dir, "enabled": true,
            "origin": "adopted", "expected_identity": {"stable_id": "test", "display_label": null},
            "last_availability": {"state": "AVAILABLE", "source": "test", "observed_unix_ms": 0, "reset_unix_ms": null}
        })).unwrap()
    }

    #[test]
    fn profiles_are_isolated_and_native_default_reads_the_native_home_file() {
        let root = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(project.path()).unwrap();
        let config = root.path().join("explicit");
        std::fs::create_dir(&config).unwrap();
        let accepted = serde_json::to_vec(&serde_json::json!({"projects": {
            canonical.to_str().unwrap(): {"hasTrustDialogAccepted": true}
        }}))
        .unwrap();
        std::fs::write(config.join(".claude.json"), &accepted).unwrap();
        let mut profile = profile(&config);
        assert_eq!(
            check_with_home(&profile, project.path(), Some(root.path())),
            Trust::Accepted
        );
        profile.claude_config_mode = Some(ClaudeConfigMode::NativeDefault);
        assert_eq!(
            check_with_home(&profile, project.path(), Some(root.path())),
            Trust::Missing
        );
        std::fs::write(root.path().join(".claude.json"), &accepted).unwrap();
        assert_eq!(
            check_with_home(&profile, project.path(), Some(root.path())),
            Trust::Accepted
        );
        assert_eq!(
            check_with_home(&profile, project.path(), None),
            Trust::Unknown
        );
        profile.claude_config_mode = Some(ClaudeConfigMode::Explicit);
        profile.config_dir = root.path().join("another-profile");
        assert_eq!(
            check_with_home(&profile, project.path(), Some(root.path())),
            Trust::Missing
        );
    }

    #[test]
    #[cfg(unix)]
    fn project_symlinks_use_the_canonical_trust_key() {
        let root = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(project.path()).unwrap();
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(project.path(), &alias).unwrap();
        std::fs::write(
            root.path().join(".claude.json"),
            serde_json::to_vec(&serde_json::json!({"projects": {
                canonical.to_str().unwrap(): {"hasTrustDialogAccepted": true}
            }}))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(check(&profile(root.path()), &alias), Trust::Accepted);
    }

    #[test]
    fn claude_requires_a_boolean_acceptance_for_the_exact_project() {
        let project = Path::new("/work/project");
        for (value, expected) in [
            ("true", Trust::Accepted),
            ("false", Trust::Missing),
            ("\"true\"", Trust::Unknown),
        ] {
            let text = format!(
                r#"{{"projects":{{"/work/project":{{"hasTrustDialogAccepted":{value}}}}}}}"#
            );
            assert_eq!(claude_trust(&text, project), expected);
            assert_eq!(
                claude_trust(&text, Path::new("/work/project/other")),
                Trust::Missing
            );
        }
        assert_eq!(claude_trust("{}", project), Trust::Missing);
        assert_eq!(claude_trust("broken", project), Trust::Unknown);
        assert_eq!(claude_trust("[]", project), Trust::Unknown);
    }

    #[test]
    fn codex_requires_explicit_project_trust() {
        let project = Path::new("/work/project");
        for (value, expected) in [
            ("trusted", Trust::Accepted),
            ("untrusted", Trust::Missing),
            ("other", Trust::Unknown),
        ] {
            let text = format!("[projects.\"/work/project\"]\ntrust_level = \"{value}\"\n");
            assert_eq!(codex_trust(&text, project), expected);
            assert_eq!(
                codex_trust(&text, Path::new("/work/project/other")),
                Trust::Missing
            );
        }
        assert_eq!(codex_trust("", project), Trust::Missing);
        assert_eq!(codex_trust("broken [", project), Trust::Unknown);
    }

    #[test]
    fn missing_corrupt_and_oversize_files_never_pass() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        let check = || read_trust(&path, |text| claude_trust(text, Path::new("/work")));
        assert_eq!(check(), Trust::Missing);
        std::fs::write(&path, b"broken").unwrap();
        assert_eq!(check(), Trust::Unknown);
        std::fs::write(&path, vec![b' '; 8 * 1024 * 1024 + 1]).unwrap();
        assert_eq!(check(), Trust::Unknown);
    }
}
