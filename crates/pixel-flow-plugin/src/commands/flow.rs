// SPDX-FileCopyrightText: The Pixel contributors
// SPDX-License-Identifier: MIT

//! `pixel flow`: save, retrieve, run and replay proven agent-browser flows.
//!
//! The flow store and executor are core's `pixel-flow` library. The command
//! line, `flow_vars` and the printing of results are ported from
//! `crates/pixel/src/main.rs` of Pixel-CLI/pixel (MIT) at `8a0f30e1`, where
//! `pixel flow` was a built-in; the code was private to the `pixel` binary.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::output::print_data;
use crate::run::{HostFlags, run};

/// Save, retrieve, list, revise, run, and replay proven agent-browser
/// paths (auth flows, config flows) so the agent follows a deterministic
/// shortcut instead of re-discovering the UI from scratch every time.
/// For a classify-decided replay with repair, use `pixel ultraflow replay`.
#[derive(Parser)]
#[command(name = "flow", bin_name = "pixel flow")]
struct Cli {
    #[command(flatten)]
    _host: HostFlags,
    #[command(subcommand)]
    cmd: FlowCmd,
}

#[derive(Subcommand)]
enum FlowCmd {
    /// Create a new flow. Refuses to overwrite — use `revise` to update.
    Save {
        /// Flow name (kebab-case recommended, e.g. "github-auth-device-flow").
        name: String,
        #[arg(long)]
        title: String,
        #[arg(long, default_value = "")]
        description: String,
        /// Tag; repeat the flag once per tag (`--tag auth --tag github`).
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[arg(long)]
        url: Option<String>,
        /// Path to a JSON file containing the steps array.
        #[arg(long)]
        from_file: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Retrieve a flow by name (for the agent to follow deterministically).
    Get {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// List all saved flows, optionally filtered by tag.
    List {
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Update an existing flow's metadata and/or steps. Bumps revision.
    Revise {
        name: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        description: Option<String>,
        /// Path to a JSON file containing the new steps array.
        #[arg(long)]
        from_file: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Run a flow by driving agent-browser (the plain, deterministic
    /// executor: refs resolved from fresh snapshots, conditions matched as
    /// text). For a classify-decided replay with repair, use
    /// `pixel ultraflow replay`.
    Run {
        name: String,
        /// Variable substitution: `--var key=value`. Repeat per var.
        #[arg(long = "var")]
        vars: Vec<String>,
        /// Shortcut for `--var google_account=<value>` (or `openai_account`
        /// depending on the flow). Picks which account to use.
        /// Accepts a full email address (e.g. user@example.com).
        #[arg(long)]
        account: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Emit ready-to-run agent-browser commands with variable substitution.
    /// Pixel does NOT run agent-browser — it outputs the deterministic
    /// command sequence for the agent to execute (or to hand to
    /// `pixel flow run`).
    Replay {
        name: String,
        /// Variable substitution: `--var key=value`. Repeat per var.
        #[arg(long = "var")]
        vars: Vec<String>,
        /// Shortcut for `--var google_account=<value>` (or `openai_account`
        /// depending on the flow). Picks which account to use.
        /// Accepts a full email address (e.g. user@example.com).
        #[arg(long)]
        account: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Delete a flow by name.
    Delete {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Pretty-print the full flow document (human-readable).
    Show {
        name: String,
        #[arg(long)]
        json: bool,
    },
}

/// The variables a flow verb carries: `--var key=value` pairs, plus the
/// `--account` shortcut resolved to whichever account variable the flow
/// declares (`openai_account` for Codex, `google_account` for the rest).
fn flow_vars(
    name: &str,
    vars: &[String],
    account: &Option<String>,
) -> Result<std::collections::HashMap<String, String>, String> {
    let mut var_map = std::collections::HashMap::new();
    for v in vars {
        let (k, val) = v
            .split_once('=')
            .ok_or_else(|| format!("--var expects key=value, got '{v}'"))?;
        var_map.insert(k.to_string(), val.to_string());
    }
    if let Some(acct) = account {
        // Check which var the flow expects by loading it.
        let var_name = pixel_flow::load(name)
            .ok()
            .and_then(|f| {
                f.vars.iter().find_map(|v| {
                    (v.name == "openai_account" || v.name == "google_account")
                        .then(|| v.name.clone())
                })
            })
            .unwrap_or_else(|| "google_account".to_string());
        var_map.insert(var_name, acct.clone());
    }
    Ok(var_map)
}

/// Entry point: `args` is the command line behind a `pixel flow` program name.
pub fn flow(args: Vec<OsString>) -> ExitCode {
    run::<Cli>("flow", args, |cli| execute(cli.cmd))
}

fn execute(cmd: FlowCmd) -> Result<(), String> {
    use pixel_flow::FlowAction;
    let json = match &cmd {
        FlowCmd::Save { json, .. }
        | FlowCmd::Get { json, .. }
        | FlowCmd::List { json, .. }
        | FlowCmd::Revise { json, .. }
        | FlowCmd::Replay { json, .. }
        | FlowCmd::Run { json, .. }
        | FlowCmd::Delete { json, .. }
        | FlowCmd::Show { json, .. } => *json,
    };
    let action = match cmd {
        FlowCmd::Save {
            name,
            title,
            description,
            tags,
            url,
            from_file,
            json: _,
        } => FlowAction::Save {
            name,
            title,
            description,
            tags,
            url,
            from_file: Some(from_file),
        },
        FlowCmd::Get { name, json: _ } => FlowAction::Get { name },
        FlowCmd::List { tag, json: _ } => FlowAction::List { tag },
        FlowCmd::Revise {
            name,
            title,
            description,
            from_file,
            json: _,
        } => FlowAction::Revise {
            name,
            title,
            description,
            from_file,
        },
        FlowCmd::Replay {
            name,
            vars,
            account,
            json: _,
        } => {
            let vars = flow_vars(&name, &vars, &account)?;
            FlowAction::Run {
                name,
                vars,
                dry_run: false,
            }
        }
        FlowCmd::Run {
            name,
            vars,
            account,
            json: _,
        } => {
            let vars = flow_vars(&name, &vars, &account)?;
            FlowAction::Execute { name, vars }
        }
        FlowCmd::Delete { name, json: _ } => FlowAction::Delete { name },
        FlowCmd::Show { name, json: _ } => FlowAction::Show { name },
    };
    let data = pixel_flow::flow(&action)?;
    if matches!(action, FlowAction::Execute { .. })
        && data.get("success").and_then(serde_json::Value::as_bool) != Some(true)
    {
        return Err(format!(
            "flow execution failed: {}",
            data.get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("flow did not complete successfully")
        ));
    }
    if json {
        return print_data(&data, true);
    }
    // For replay and show, the output field contains human-readable
    // text — print it directly to stdout. For everything else, use
    // the standard print_data path (JSON or pretty).
    match &action {
        FlowAction::Run { .. } | FlowAction::Show { .. } => {
            if let Some(output) = data.get("output").and_then(|v| v.as_str()) {
                println!("{output}");
                Ok(())
            } else {
                print_data(&data, true)
            }
        }
        FlowAction::Execute { .. } => {
            // Print the execution log to stderr, result summary to stdout.
            if let Some(log) = data.get("log").and_then(|v| v.as_str()) {
                eprintln!("{log}");
            }
            let success = data
                .get("success")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let steps = data
                .get("steps_executed")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let skipped = data
                .get("steps_skipped")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            if success {
                println!("✓ Flow executed: {steps} steps, {skipped} skipped");
            } else if let Some(err) = data.get("error").and_then(|v| v.as_str()) {
                println!("✗ Flow failed after {steps} steps: {err}");
            } else {
                println!("~ Flow completed with warnings: {steps} steps, {skipped} skipped");
            }
            Ok(())
        }
        _ => print_data(&data, true),
    }
}
