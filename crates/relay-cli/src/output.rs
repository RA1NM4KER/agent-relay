//! The stable output envelope every command produces: a human-readable line for a real terminal,
//! and a versioned JSON value for `--json` / scripted consumers.

use std::process::ExitCode;

use relay_core::Error;
use serde::Serialize;
use serde_json::{Value, json};

const OUTPUT_SCHEMA_VERSION: u32 = 1;

#[derive(Serialize)]
struct SuccessEnvelope<T> {
    schema_version: u32,
    ok: bool,
    command: &'static str,
    data: T,
}

pub(crate) struct CommandOutput {
    pub(crate) human: String,
    pub(crate) json: Value,
}

/// The "Title" + blank-line spacer every human-readable Relay report opens with — kept in one
/// place so a report's framing can never drift between commands (each one used to hand-build this
/// prefix independently, which happened to match but had nothing enforcing it).
pub(crate) fn header(title: &str) -> String {
    format!("{title}\n\n")
}

pub(crate) fn success<T: Serialize>(
    command: &'static str,
    human: String,
    data: T,
) -> Result<CommandOutput, Error> {
    let envelope = SuccessEnvelope {
        schema_version: OUTPUT_SCHEMA_VERSION,
        ok: true,
        command,
        data,
    };
    let json = serde_json::to_value(envelope).map_err(|_| Error::SerializationFailed)?;
    Ok(CommandOutput { human, json })
}

/// Prints a dispatched command's result the way its caller asked for it (`--json` or a plain
/// terminal line) and converts it to the process exit code: success is always `0`; any [`Error`]
/// is `1`, after printing the stable JSON error envelope (`--json`) or a one-line message.
pub(crate) fn print_result(result: Result<CommandOutput, Error>, json_mode: bool) -> ExitCode {
    print_result_with_exit(result, json_mode, |_| ExitCode::SUCCESS)
}

/// Like [`print_result`], but a successful result's exit code is decided by `exit_for_ok` instead
/// of always being `0` — for a command like `relay doctor` whose *successful* diagnosis can still
/// mean "not ready", which callers (CI, scripts) need to see as a non-zero exit without that
/// diagnosis being reported as a command *error* (the JSON envelope stays the rich success shape,
/// not the error one).
pub(crate) fn print_result_with_exit(
    result: Result<CommandOutput, Error>,
    json_mode: bool,
    exit_for_ok: impl FnOnce(&CommandOutput) -> ExitCode,
) -> ExitCode {
    match result {
        Ok(output) => {
            if json_mode {
                let serialized = serde_json::to_string_pretty(&output.json)
                    .expect("serializing known Relay output must succeed");
                println!("{serialized}");
            } else {
                println!("{}", output.human);
            }
            exit_for_ok(&output)
        }
        Err(error) => {
            if json_mode {
                let output = json!({
                    "schema_version": OUTPUT_SCHEMA_VERSION,
                    "ok": false,
                    "error": {
                        "code": error.code(),
                        "message": error.to_string(),
                    }
                });
                eprintln!(
                    "{}",
                    serde_json::to_string_pretty(&output)
                        .expect("serializing known Relay error must succeed")
                );
            } else {
                eprintln!("error [{}]: {error}", error.code());
            }
            ExitCode::from(1)
        }
    }
}
