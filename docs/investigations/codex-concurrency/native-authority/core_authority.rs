//! Research native authority hooks. A persistent profile marker makes missing
//! host integration an error, including direct exec/embedded alternative paths.
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

pub trait NativeAuthority: Send + Sync {
    fn cancellation_grace(&self, _thread: &str) -> Option<Duration> {
        None
    }
    fn shell_route(&self, _thread: &str, _script: &str) -> Result<Option<ShellRoute>, String> {
        Ok(None)
    }
    fn cancel_shell(&self, _thread: &str, _execution: &str) -> Result<(), String> {
        Err("scoped shell cancellation unavailable".into())
    }
    fn shell_stopped(&self, _thread: &str, _execution: &str) -> Result<bool, String> {
        Err("scoped shell stop evidence unavailable".into())
    }
    fn check_home(&self, home: &Path) -> Result<(), String>;
    fn register(
        &self,
        thread: &str,
        parent: Option<&str>,
        operation: Option<&str>,
    ) -> Result<(), String>;
    fn check(&self, thread: &str) -> Result<(), String>;
}

pub struct ShellRoute {
    pub command: Vec<String>,
    pub execution: String,
}

static HOST: OnceLock<Arc<dyn NativeAuthority>> = OnceLock::new();
tokio::task_local! { static OPERATION: String; }

pub fn install(host: Arc<dyn NativeAuthority>) -> Result<(), String> {
    HOST.set(host)
        .map_err(|_| "native authority already installed".to_string())
}

pub async fn scope<T>(operation: String, future: impl Future<Output = T>) -> T {
    OPERATION.scope(operation, future).await
}

pub fn register(home: &Path, thread: &str, parent: Option<&str>) -> Result<(), String> {
    if HOST.get().is_none() && !home.join("relay-authority-v1").exists() {
        return Ok(());
    }
    require_host(home)?;
    let host = HOST
        .get()
        .ok_or("protected profile requires authoritative native host")?;
    let operation = OPERATION.try_with(Clone::clone).ok();
    host.register(thread, parent, operation.as_deref())
}

pub fn check(thread: &str) -> Result<(), String> {
    match HOST.get() {
        Some(host) => host.check(thread),
        None => Ok(()),
    }
}

pub fn shell_route(thread: &str, script: &str) -> Result<Option<ShellRoute>, String> {
    match HOST.get() {
        Some(host) => host.shell_route(thread, script),
        None => Ok(None),
    }
}

pub fn cancel_shell(thread: &str, execution: &str) -> Result<(), String> {
    HOST.get()
        .ok_or("missing native execution authority")?
        .cancel_shell(thread, execution)
}

pub fn cancellation_grace(thread: &str) -> Option<Duration> {
    HOST.get().and_then(|host| host.cancellation_grace(thread))
}

pub fn shell_stopped(thread: &str, execution: &str) -> Result<bool, String> {
    HOST.get()
        .ok_or("missing native execution authority")?
        .shell_stopped(thread, execution)
}

pub fn require_host(home: &Path) -> Result<(), String> {
    match HOST.get() {
        Some(host) => host.check_home(home),
        None if home.join("relay-authority-v1").exists() => {
            Err("protected profile requires authoritative native host".into())
        }
        None => Ok(()),
    }
}

/// A different CODEX_HOME must not import an enrolled native store/history.
/// Canonicalization covers aliases; arbitrary same-uid file replacement still
/// requires the OS executor boundary described in CONTAINMENT.md.
pub fn require_source(path: &Path) -> Result<(), String> {
    let mut existing = path;
    while !existing.exists() {
        existing = existing
            .parent()
            .ok_or("native source has no existing ancestor")?;
    }
    let canonical = existing.canonicalize().map_err(|error| error.to_string())?;
    for ancestor in canonical.ancestors() {
        if ancestor.join("relay-authority-v1").exists() {
            require_host(ancestor)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod source_tests {
    use super::*;

    #[test]
    fn unmediated_protected_sources_and_aliases_refuse() {
        let root = std::env::temp_dir().join(format!("relay-source-test-{}", uuid::Uuid::now_v7()));
        let profile = root.join("profile");
        std::fs::create_dir_all(profile.join("relay-authority-v1")).unwrap();
        let history = profile.join("sessions/history.jsonl");
        std::fs::create_dir_all(history.parent().unwrap()).unwrap();
        std::fs::write(&history, b"research").unwrap();
        assert!(require_source(&history).is_err());
        assert!(require_source(&profile.join("state_5.sqlite")).is_err());
        assert!(require_source(&root).is_ok());
        #[cfg(unix)]
        {
            let alias = root.join("alias");
            std::os::unix::fs::symlink(&profile, &alias).unwrap();
            assert!(require_source(&alias.join("sessions/history.jsonl")).is_err());
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
