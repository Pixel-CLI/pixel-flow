// SPDX-FileCopyrightText: The Pixel contributors
// SPDX-License-Identifier: MIT

//! The `main` of a command: parse, run, report a failure the way core does.
//!
//! Ported from `run` / `failure_envelope_wanted` / `write_failure_envelope` in
//! `crates/pixel/src/main.rs` and `classify_error` in
//! `crates/pixel-daemon/src/api.rs` of Pixel-CLI/pixel (MIT), minus the
//! action log, metrics trailer and update notice, which belong to the core
//! binary.

use std::ffi::OsString;
use std::process::ExitCode;

use clap::{ArgMatches, Parser};
use pixel_proto::{Envelope, ErrorCode, PixelError};
use serde_json::Value;

use crate::output::{stdout_bytes, write_stdout};

/// Flags every command of the `pixel` binary accepts and a plugin command
/// must keep accepting, so `pixel <name> --metrics off …` still parses when a script
/// passes it. They are hidden (the live-metrics line belongs to the core
/// binary, so the value is ignored). Flatten into the plugin's `Cli`:
/// `#[command(flatten)] host: HostFlags`.
#[derive(clap::Args, Debug, Default)]
pub struct HostFlags {
    /// Disable live metrics (accepted for compatibility; plugins print none).
    #[arg(long, global = true, hide = true, value_parser = ["on", "off"])]
    pub metrics: Option<String>,
}

/// Parse the process arguments as `C`, run `handler`, and turn its result
/// into the exit status core's built-in had: 0 on success, 1 with
/// `pixel: <reason>` on stderr on failure, and under `--json` a failure
/// envelope on stdout when nothing else was printed. A usage error exits
/// with clap's own status (2) and message.
///
/// `args` is the whole argv of the command, program name first: the part
/// after `pixel-git <command>`, behind a placeholder program name.
pub fn run<C: Parser>(
    op: &str,
    args: Vec<OsString>,
    handler: impl FnOnce(C) -> Result<(), String>,
) -> ExitCode {
    let matches = C::command()
        .try_get_matches_from(args)
        .unwrap_or_else(|error| with_request_id_tip(error).exit());
    let json = json_requested(&matches);
    let cli = C::from_arg_matches(&matches).unwrap_or_else(|error| error.exit());
    match handler(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if json && stdout_bytes() == 0 {
                let _ = write_stdout(&failure_line(op, &error));
            }
            eprintln!("pixel: {error}");
            ExitCode::FAILURE
        }
    }
}

/// What a missing `--request-id` means, for an agent that copied a command
/// without it. Ported from `with_request_id_tip` in `crates/pixel/src/main.rs`.
const REQUEST_ID_TIP: &str = "mutation ops need --request-id <id>: any stable string naming this operation, passed unchanged on a retry so pixel replays its result instead of running it twice";

/// `error` with [`REQUEST_ID_TIP`] as its `tip:` line when the argument it
/// reports missing is `--request-id`; any other error is returned untouched.
fn with_request_id_tip(mut error: clap::Error) -> clap::Error {
    use clap::error::{ContextKind, ContextValue, ErrorKind};
    let lacks_request_id = error.kind() == ErrorKind::MissingRequiredArgument
        && matches!(
            error.get(ContextKind::InvalidArg),
            Some(ContextValue::Strings(missing))
                if missing.iter().any(|arg| arg.starts_with("--request-id"))
        );
    if lacks_request_id {
        error.insert(
            ContextKind::Suggested,
            ContextValue::StyledStrs(vec![REQUEST_ID_TIP.into()]),
        );
    }
    error
}

/// Whether the parsed invocation asked for JSON on stdout: the deepest
/// subcommand's own `json` flag, `false` for a command without one.
pub fn json_requested(matches: &ArgMatches) -> bool {
    let mut deepest = matches;
    while let Some((_, sub)) = deepest.subcommand() {
        deepest = sub;
    }
    deepest
        .try_get_one::<bool>("json")
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false)
}

/// The `--json` failure line: core's failure envelope (`ok: false` +
/// `error.code`), one JSON document and a newline.
pub fn failure_line(op: &str, message: &str) -> String {
    let envelope: Envelope<Value> =
        Envelope::failure(op, PixelError::new(classify_error(message), message));
    format!("{}\n", serde_json::to_string(&envelope).unwrap_or_default())
}

/// The best-fit [`ErrorCode`] for an error string, as core classifies it:
/// a leading `"<CODE>: …"` token naming a code is that code, the daemon's
/// `no symbol …` and `repository is busy…` shapes map to `NOT_FOUND` and
/// `BUSY_REPOSITORY`, everything else is `INVALID_INPUT`.
/// `tests/core_drift.rs` checks this against `pixel_daemon::api::failure_response`.
pub fn classify_error(msg: &str) -> ErrorCode {
    if let Some(code) = code_named_in_prefix(msg) {
        return code;
    }
    let lower = msg.to_lowercase();
    if lower.starts_with("no symbol named") || lower.starts_with("no symbol with uid") {
        ErrorCode::NotFound
    } else if lower.starts_with("repository is busy") {
        ErrorCode::BusyRepository
    } else {
        ErrorCode::InvalidInput
    }
}

fn code_named_in_prefix(msg: &str) -> Option<ErrorCode> {
    let (head, _) = msg.split_once(':')?;
    serde_json::from_str(&format!("\"{}\"", head.trim())).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prefixed_code_wins_and_the_rest_defaults_to_invalid_input() {
        assert_eq!(
            classify_error("NON_FAST_FORWARD: merge-base is not an ancestor"),
            ErrorCode::NonFastForward
        );
        assert_eq!(classify_error("REFUSED: dirty"), ErrorCode::InvalidInput);
        assert_eq!(classify_error("No symbol named x"), ErrorCode::NotFound);
        assert_eq!(
            classify_error("repository is busy: lock"),
            ErrorCode::BusyRepository
        );
    }

    #[test]
    fn the_failure_line_is_one_envelope_document() {
        let line = failure_line("flow", "bad path x");
        let value: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(value["ok"], false);
        assert_eq!(value["op"], "flow");
        assert_eq!(value["error"]["code"], "INVALID_INPUT");
        assert_eq!(value["error"]["message"], "bad path x");
        assert!(line.ends_with('\n'));
    }
}
