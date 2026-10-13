// SPDX-FileCopyrightText: The Pixel contributors
// SPDX-License-Identifier: MIT

//! `pixel-plugin.toml` must describe the binary it ships with.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use pixel_flow_plugin::commands::COMMANDS;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn manifest() -> String {
    std::fs::read_to_string(root().join("pixel-plugin.toml")).expect("pixel-plugin.toml")
}

/// The value of a top-level `key = "value"` line.
fn string_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let rest = line.strip_prefix(key)?.trim_start().strip_prefix('=')?;
        Some(
            rest.trim()
                .split('#')
                .next()?
                .trim()
                .trim_matches('"')
                .to_string(),
        )
    })
}

#[test]
fn the_manifest_follows_plugin_api_one() {
    let text = manifest();
    assert_eq!(string_value(&text, "name").as_deref(), Some("flow"));
    assert_eq!(string_value(&text, "api").as_deref(), Some("1"));
    assert_eq!(
        string_value(&text, "run").as_deref(),
        Some("bin/pixel-flow")
    );
    assert!(text.contains("capabilities = [\"command\"]"));
    assert_eq!(string_value(&text, "network").as_deref(), Some("true"));
    assert!(!string_value(&text, "description").unwrap().is_empty());
}

#[test]
fn the_manifest_version_is_the_package_version() {
    assert_eq!(
        string_value(&manifest(), "version").as_deref(),
        Some(env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn the_launcher_exists_and_is_executable() {
    let launcher = root().join("bin/pixel-flow");
    let mode = std::fs::metadata(&launcher)
        .expect("bin/pixel-flow")
        .permissions()
        .mode();
    assert_ne!(mode & 0o111, 0, "{} is not executable", launcher.display());
}

#[test]
fn the_commands_list_is_the_commands_the_binary_has() {
    let text = manifest();
    let start = text.find("commands = [").expect("commands list") + "commands = [".len();
    let end = start + text[start..].find(']').expect("end of list");
    let listed: Vec<&str> = text[start..end]
        .lines()
        .map(|l| l.trim().trim_end_matches(',').trim_matches('"'))
        .filter(|l| !l.is_empty())
        .collect();
    let have: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
    assert_eq!(listed, have);
}
