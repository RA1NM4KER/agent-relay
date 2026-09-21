//! Build identity for `relay --version`.
//!
//! * Tagged releases (`RELAY_RELEASE_BUILD=1`, set by the release workflow) report the clean
//!   Cargo version, e.g. `0.3.0`.
//! * Every other build reports which exact commit it is: `<next patch>-dev.<commit count>+<short
//!   sha>` (for example `0.3.1-dev.412+67a815c`), with `.dirty` appended when built from a working
//!   tree with uncommitted changes. CI supplies `RELAY_BUILD_SHA` / `RELAY_BUILD_COUNT`; a local
//!   build reads them from git. Cargo.toml is never edited to encode a commit.
//!
//! The commit count is monotonic, so package managers order successive main builds correctly.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|text| !text.is_empty())
}

fn next_patch(version: &str) -> String {
    let mut parts: Vec<String> = version.split('.').map(str::to_owned).collect();
    if let Some(patch) = parts.get_mut(2).and_then(|part| {
        part.split(['-', '+'])
            .next()
            .and_then(|digits| digits.parse::<u64>().ok())
    }) {
        parts[2] = (patch + 1).to_string();
    }
    parts.join(".")
}

fn main() {
    for variable in [
        "RELAY_RELEASE_BUILD",
        "RELAY_BUILD_SHA",
        "RELAY_BUILD_COUNT",
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");

    let package = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let release = std::env::var("RELAY_RELEASE_BUILD").is_ok_and(|value| value == "1");
    let version = if release {
        package
    } else {
        let sha = std::env::var("RELAY_BUILD_SHA")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| git(&["rev-parse", "--short=7", "HEAD"]));
        match sha {
            None => package,
            Some(sha) => {
                let count = std::env::var("RELAY_BUILD_COUNT")
                    .ok()
                    .filter(|value| !value.is_empty())
                    .or_else(|| git(&["rev-list", "--count", "HEAD"]))
                    .unwrap_or_else(|| "0".to_owned());
                let dirty = std::env::var("RELAY_BUILD_SHA").is_err()
                    && git(&["status", "--porcelain"]).is_some();
                format!(
                    "{}-dev.{count}+{sha}{}",
                    next_patch(&package),
                    if dirty { ".dirty" } else { "" }
                )
            }
        }
    };
    println!("cargo:rustc-env=RELAY_VERSION={version}");
}
