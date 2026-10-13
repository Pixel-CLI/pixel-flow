// SPDX-FileCopyrightText: The Pixel contributors
// SPDX-License-Identifier: MIT

//! The two commands of the `flow` plugin and how a name finds its code.

use std::ffi::OsString;
use std::process::ExitCode;

mod flow;
mod ultraflow;

/// One command: the name `pixel <name>` is typed with, a line for `--help`,
/// and the entry point, which receives the arguments after the name.
pub struct Command {
    pub name: &'static str,
    pub about: &'static str,
    run: fn(Vec<OsString>) -> ExitCode,
}

impl Command {
    /// Run the command on `args` (the arguments after its name).
    pub fn run(&self, args: &[OsString]) -> ExitCode {
        let mut argv = Vec::with_capacity(args.len() + 1);
        argv.push(OsString::from(format!("pixel {}", self.name)));
        argv.extend_from_slice(args);
        (self.run)(argv)
    }
}

/// Every command, in the order `--help` lists them. This list is also the
/// `commands` array of `pixel-plugin.toml` (`tests/manifest.rs` compares the
/// two).
pub const COMMANDS: [Command; 2] = [
    Command {
        name: "flow",
        about: "Save, retrieve, run and replay proven agent-browser flows",
        run: flow::flow,
    },
    Command {
        name: "ultraflow",
        about: "Drive a browser toward a goal with `pixel classify`, save what worked as a flow, replay it",
        run: ultraflow::ultraflow,
    },
];

/// The command named `name`, if this plugin provides it.
pub fn find(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_found() {
        for (i, c) in COMMANDS.iter().enumerate() {
            assert!(find(c.name).is_some_and(|f| f.name == c.name));
            assert!(
                COMMANDS[i + 1..].iter().all(|o| o.name != c.name),
                "{}",
                c.name
            );
        }
        assert!(find("git").is_none());
        assert!(find("").is_none());
    }
}
