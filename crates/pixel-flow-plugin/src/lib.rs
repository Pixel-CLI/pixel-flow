// SPDX-FileCopyrightText: The Pixel contributors
// SPDX-License-Identifier: MIT

//! The `flow` plugin of Pixel: `flow` and `ultraflow` behind one `pixel-flow`
//! binary (plugin API 1).
//!
//! A plugin is a separate executable that Pixel core runs for `pixel <name> …`.
//! This one answers to two names, so [`dispatch`] picks the command from the
//! name the binary was started under (`pixel-flow` is `flow`, `pixel-ultraflow`
//! is a link made by `pixel-flow install-links`) or from the first argument
//! when the binary is started as `pixel-flow` itself:
//! `pixel-flow ultraflow discover …`.
//!
//! The flow store, the executor and the browser loop are core's `pixel-flow`
//! and `pixel-ultraflow` library crates. Ported here is what was private to
//! the `pixel` binary: the command lines, the printing, and the decision seam
//! (`ultraflow` asks the core binary's `pixel classify --jsonl`).

pub mod commands;
pub mod decider;
pub mod env;
pub mod links;
pub mod output;
pub mod run;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

/// The command the binary's own name stands for: `pixel-flow …` is `flow …`.
const DEFAULT_COMMAND: &str = "flow";

/// What `pixel-flow install-links --help` and a bad invocation print.
fn usage() -> String {
    let mut text = String::from(
        "pixel-flow: the flow and ultraflow commands of Pixel\n\n\
         Usage: pixel-flow <flow-subcommand> [args]...      (runs `flow`)\n       \
         pixel-flow <command> [args]...             (flow | ultraflow)\n       \
         pixel-<command> [args]...                  (after `pixel-flow install-links`)\n       \
         pixel <command> [args]...                  (the host runs the line above)\n\n\
         Commands:\n",
    );
    for c in &commands::COMMANDS {
        text.push_str(&format!("  {:<16}{}\n", c.name, c.about));
    }
    text.push_str(
        "\nOther:\n  install-links   Link pixel-<command> to this binary so `pixel <command>` finds it on PATH\n\n\
         `pixel-flow <command> --help` shows a command's own options.\n",
    );
    text
}

/// The command an executable name selects: `pixel-flow` is `flow` and
/// `pixel-ultraflow` is `ultraflow`.
pub fn command_of_program(program: &Path) -> Option<&'static commands::Command> {
    let stem = program.file_name()?.to_str()?;
    commands::find(stem.strip_prefix("pixel-")?)
}

/// Run the plugin for `args` (the whole argv, program name first).
pub fn dispatch(args: &[OsString]) -> ExitCode {
    let Some(program) = args.first() else {
        return ExitCode::FAILURE;
    };
    let rest = &args[1..];
    let first = rest.first().and_then(|arg| arg.to_str());
    if let Some(command) = command_of_program(Path::new(program)) {
        // The plugin's own name stands for `flow`, yet `pixel-flow ultraflow …`
        // must reach the other command: no `flow` subcommand is named like a
        // command (`tests/cli.rs` keeps it so).
        if command.name == DEFAULT_COMMAND {
            match first {
                Some("install-links") => return links::run(&rest[1..]),
                Some("-V" | "--version") if rest.len() == 1 => return print_version(),
                Some(name) => {
                    if let Some(named) = commands::find(name) {
                        return named.run(&rest[1..]);
                    }
                }
                None => {}
            }
        }
        return command.run(rest);
    }
    match first {
        None => {
            eprint!("{}", usage());
            ExitCode::from(2)
        }
        Some("-h" | "--help" | "help") => {
            print!("{}", usage());
            ExitCode::SUCCESS
        }
        Some("-V" | "--version") => print_version(),
        Some("install-links") => links::run(&rest[1..]),
        Some(name) => match commands::find(name) {
            Some(command) => command.run(&rest[1..]),
            None => {
                eprintln!("pixel-flow: unknown command `{name}`\n");
                eprint!("{}", usage());
                ExitCode::from(2)
            }
        },
    }
}

fn print_version() -> ExitCode {
    println!("pixel-flow {}", env!("CARGO_PKG_VERSION"));
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_name_selects_its_command() {
        assert_eq!(
            command_of_program(Path::new("/x/bin/pixel-flow")).map(|c| c.name),
            Some("flow")
        );
        assert_eq!(
            command_of_program(Path::new("pixel-ultraflow")).map(|c| c.name),
            Some("ultraflow")
        );
        assert!(command_of_program(Path::new("/x/flow")).is_none());
        assert!(command_of_program(Path::new("pixel-nope")).is_none());
    }

    #[test]
    fn the_usage_lists_every_command() {
        let text = usage();
        for c in &commands::COMMANDS {
            assert!(text.contains(c.name), "{}", c.name);
        }
    }
}
