//! Non-interactive post-upgrade reconciliation of integrations that Relay can prove it owns.

use std::path::PathBuf;

use relay_core::{Error, ProfileService, ProviderKind};
use relay_provider_claude::{apply_install, load_manifest, plan_install};
use serde_json::json;

use crate::{
    output::{CommandOutput, header, success},
    util::current_unix_ms,
};

pub(crate) fn run(service: &ProfileService) -> Result<CommandOutput, Error> {
    let executable = std::env::current_exe().map_err(|source| Error::Io {
        path: PathBuf::from("relay"),
        source,
    })?;
    let mut results = Vec::new();
    for profile in service.list()? {
        match profile.provider {
            ProviderKind::Claude => {
                // A manifest is the durable proof that this profile opted into, and Relay owns,
                // this integration. Never infer consent from a hook-shaped settings entry.
                if load_manifest(&profile.config_dir)?.is_none() {
                    results.push(json!({"profile": profile.name, "provider": "claude", "status": "skipped", "reason": "not installed"}));
                    continue;
                }
                let plan = plan_install(&profile.config_dir, &executable)?;
                let changed = plan.has_pending_writes();
                let changes = plan.changes.clone();
                if changed {
                    apply_install(&plan, current_unix_ms())?;
                }
                results.push(json!({"profile": profile.name, "provider": "claude", "status": if changed { "refreshed" } else { "current" }, "changes": changes}));
            }
            ProviderKind::Codex => {
                // The sidecar hash proves both origin and that the user has not edited it.
                if !crate::codex_integration::is_relay_managed_and_unmodified(&profile.config_dir) {
                    results.push(json!({"profile": profile.name, "provider": "codex", "status": "skipped", "reason": "not Relay-owned and unmodified"}));
                    continue;
                }
                let current = crate::codex_integration::installed(&profile.config_dir);
                crate::codex_integration::install(&profile.config_dir)?;
                results.push(json!({"profile": profile.name, "provider": "codex", "status": if current { "current" } else { "refreshed" }}));
            }
            _ => results.push(json!({"profile": profile.name, "provider": profile.provider.to_string(), "status": "skipped", "reason": "no refreshable Relay-owned assets"})),
        }
    }
    let lines = results
        .iter()
        .map(|entry| {
            format!(
                "{} ({}) {}",
                entry["profile"].as_str().unwrap_or("?"),
                entry["provider"].as_str().unwrap_or("?"),
                entry["status"].as_str().unwrap_or("?")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    success(
        "refresh",
        format!(
            "{}{}",
            header("Relay integration refresh"),
            if lines.is_empty() {
                "No registered profiles.".to_owned()
            } else {
                lines
            }
        ),
        json!({"executable": executable, "results": results}),
    )
}
