//! M6: Codex usage/exhaustion detection.
//!
//! Research finding (recorded in full in M6_FINAL_REPORT.md): the installed Codex CLI (0.155.0)
//! exposes no documented, structured, machine-readable usage/rate-limit signal equivalent to
//! Claude's `rate_limit_event` stream entries or `agents --json` state. `codex exec --json`'s
//! NDJSON stream can carry a `turn.failed`/`error` event whose `message` is arbitrary English
//! prose (observed live: HTTP 401/429 wording from the underlying API client) — exactly the kind
//! of "arbitrary English stderr matching" the M6 spec explicitly forbids building production
//! behavior on.
//!
//! [`CodexUsageSignal`] therefore always reports [`UsageState::Unknown`] with
//! [`UsageEvidence::ProviderHealthCheck`], which `relay-core`'s routing already treats as
//! fail-closed (never triggers an automatic handoff away from Codex). Codex remains fully usable
//! as an automatic handoff TARGET (chosen because some OTHER profile is exhausted) and as a
//! manual `relay switch` source or target — only automatic exhaustion-detection FROM Codex is
//! unsupported, and it fails closed rather than guessing.

use std::path::Path;

use relay_core::{
    Result,
    usage::{UsageEvidence, UsageObservation, UsageSignal, UsageState},
};

#[derive(Clone, Copy, Debug, Default)]
pub struct CodexUsageSignal;

impl UsageSignal for CodexUsageSignal {
    fn detect(
        &self,
        _config_dir: &Path,
        _project_dir: &Path,
        _session_id: &str,
    ) -> Result<UsageObservation> {
        Ok(UsageObservation {
            state: UsageState::Unknown,
            evidence: UsageEvidence::ProviderHealthCheck,
            detected_via: "codex exposes no documented structured usage/rate-limit signal; \
                           automatic exhaustion handoff away from Codex is not supported in M6 \
                           — use `relay switch` manually"
                .to_owned(),
            observed_unix_ms: now_unix_ms(),
            reset_unix_ms: None,
        })
    }
}

fn now_unix_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::CodexUsageSignal;
    use relay_core::usage::{UsageSignal, UsageState};
    use std::path::Path;

    #[test]
    fn codex_usage_is_always_unknown_and_therefore_never_blocking() {
        let observation = CodexUsageSignal
            .detect(Path::new("/config"), Path::new("/project"), "any-session")
            .expect("detect");
        assert_eq!(observation.state, UsageState::Unknown);
        assert!(!observation.state.is_blocking());
    }
}
