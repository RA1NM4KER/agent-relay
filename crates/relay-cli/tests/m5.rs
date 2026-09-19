//! M5.5 test: `relay integration herdr install` (and therefore `relay setup`'s Herdr step) must
//! work from a packaged install with no `agent-relay` source checkout nearby and no dependence on
//! the current working directory. Against the real compiled `relay` binary, a fake `herdr`
//! executable (never a real Herdr socket), and `RELAY_HERDR_PLUGIN_BIN` standing in for the
//! sibling-binary discovery a real Homebrew/release install performs.

use std::path::Path;
use std::process::Command;

use serde_json::Value;
use tempfile::tempdir;

fn relay(root: &Path, cwd: &Path, plugin_bin: &Path, arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_relay"))
        .current_dir(cwd)
        .arg("--json")
        .arg("--config-root")
        .arg(root.join("config"))
        .arg("--state-root")
        .arg(root.join("state"))
        .args(arguments)
        .env("RELAY_HERDR_PLUGIN_BIN", plugin_bin)
        .output()
        .expect("run relay")
}

fn json_stdout(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("valid JSON stdout")
}

#[cfg(unix)]
fn make_executable(path: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, script).expect("write script");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).expect("chmod");
}

/// A fake `herdr` CLI answering only what `relay integration herdr install` (non-dry-run) needs:
/// `plugin list --json` (nothing registered yet) then `plugin link <path>` (reports success), in
/// the exact envelope shapes live-confirmed against a real Herdr 0.9.0 server (M3.2).
#[cfg(unix)]
fn fake_herdr_script() -> &'static str {
    r#"#!/bin/sh
set -e
if [ "$1" = "plugin" ] && [ "$2" = "list" ]; then
  echo '{"id":"cli:plugin","result":{"plugins":[],"type":"plugin_list"}}'
  exit 0
fi
if [ "$1" = "plugin" ] && [ "$2" = "link" ]; then
  echo '{"id":"cli:plugin","result":{"plugin":{"plugin_id":"agent-relay","name":"Agent Relay","version":"0.1.0","min_herdr_version":"0.9.0","enabled":true,"manifest_path":"'"$3"'/herdr-plugin.toml"},"type":"plugin_linked"}}'
  exit 0
fi
echo "fake herdr: unhandled args: $*" >&2
exit 1
"#
}

#[test]
#[cfg(unix)]
fn herdr_install_works_outside_any_repo_checkout_with_no_plugin_path_flag() {
    let root = tempdir().expect("root");
    // A deliberately empty, unrelated working directory: no `plugins/herdr` anywhere near it,
    // proving the default install path does not depend on cwd or a source checkout.
    let cwd = tempdir().expect("cwd");
    let bin_dir = tempdir().expect("bin dir");

    let herdr_bin = bin_dir.path().join("herdr");
    make_executable(&herdr_bin, fake_herdr_script());

    // Stands in for the real `relay-herdr-plugin` binary a packaged install places next to
    // `relay` itself; only its path is used (never executed) by the manifest materializer.
    let plugin_bin = bin_dir.path().join("relay-herdr-plugin");
    std::fs::write(&plugin_bin, "#!/bin/sh\n").expect("write stub plugin binary");

    let output = relay(
        root.path(),
        cwd.path(),
        &plugin_bin,
        &[
            "integration",
            "herdr",
            "install",
            "--herdr-executable",
            herdr_bin.to_str().expect("utf8 path"),
        ],
    );
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let body = json_stdout(&output);
    assert_eq!(body["ok"], true);
    assert_eq!(body["data"]["plugin"], "agent-relay");

    let manifest_path = root
        .path()
        .join("config")
        .join("herdr-plugin")
        .join("herdr-plugin.toml");
    let manifest = std::fs::read_to_string(&manifest_path)
        .expect("install materialized the manifest into the config root, not cwd");
    assert!(manifest.contains(plugin_bin.to_str().unwrap()));
    assert!(!manifest.contains("../../target/release"));
    assert!(
        !cwd.path().join("plugins").exists(),
        "install must never write into the current working directory"
    );
}
