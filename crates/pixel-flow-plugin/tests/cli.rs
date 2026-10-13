// SPDX-FileCopyrightText: The Pixel contributors
// SPDX-License-Identifier: MIT

//! How `pixel-flow` picks a command: by the name it was started under, by a
//! command word in front of the arguments, and what it says when neither
//! names one.

use std::path::Path;

use pixel_flow_plugin::commands::COMMANDS;
use pixel_flow_plugin::testing::{run, scratch_dir};

const BIN: &str = env!("CARGO_BIN_EXE_pixel-flow");

#[test]
fn the_binarys_own_name_is_flow() {
    let dir = scratch_dir();
    let store = dir.path().join("flows");
    let envs = [("PIXEL_FLOW_DIR", store.to_str().unwrap())];
    let bare = run(BIN.as_ref(), &["list"], dir.path(), &envs);
    assert_eq!(bare.code, 0, "{}", bare.stderr);
    let named = run(BIN.as_ref(), &["flow", "list"], dir.path(), &envs);
    assert_eq!(bare, named);
}

#[test]
fn a_command_word_reaches_ultraflow_from_the_plugin_binary() {
    let dir = scratch_dir();
    let seen = run(BIN.as_ref(), &["ultraflow", "discover"], dir.path(), &[]);
    assert_eq!(seen.code, 2, "{}", seen.stderr);
    assert!(seen.stderr.contains("--url <URL>"), "{}", seen.stderr);
    assert!(
        seen.stderr.contains("Usage: pixel ultraflow"),
        "{}",
        seen.stderr
    );
}

#[test]
fn no_flow_subcommand_is_named_like_a_command() {
    // `pixel-flow <word> …` must never be ambiguous between a `flow`
    // subcommand and a command of the plugin (see `dispatch`).
    let seen = run(BIN.as_ref(), &["flow", "--help"], Path::new("."), &[]);
    assert_eq!(seen.code, 0, "{}", seen.stderr);
    let subcommands: Vec<&str> = seen
        .stdout
        .lines()
        .skip_while(|line| !line.starts_with("Commands:"))
        .skip(1)
        .take_while(|line| !line.trim().is_empty())
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    assert!(subcommands.contains(&"replay"), "{subcommands:?}");
    for c in &COMMANDS {
        assert!(!subcommands.contains(&c.name), "{}", c.name);
    }
}

#[test]
fn the_version_is_the_package_version() {
    let seen = run(BIN.as_ref(), &["--version"], Path::new("."), &[]);
    assert_eq!(
        seen.stdout,
        format!("pixel-flow {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn every_command_answers_help_under_the_name_pixel_dash_command() {
    let dir = scratch_dir();
    let links = tempfile::tempdir().unwrap();
    let seen = run(
        BIN.as_ref(),
        &["install-links", "--dir", links.path().to_str().unwrap()],
        dir.path(),
        &[],
    );
    assert_eq!(seen.code, 0, "{}", seen.stderr);
    for c in &COMMANDS {
        let link = links.path().join(format!("pixel-{}", c.name));
        let via_link = run(&link, &["--help"], dir.path(), &[]);
        let via_arg = run(BIN.as_ref(), &[c.name, "--help"], dir.path(), &[]);
        assert_eq!(via_link.code, 0, "{}: {}", c.name, via_link.stderr);
        assert_eq!(via_link, via_arg, "{}", c.name);
        assert!(
            via_link.stdout.contains(&format!("pixel {}", c.name)),
            "{}: {}",
            c.name,
            via_link.stdout
        );
    }
}

#[test]
fn install_links_beside_the_binary_never_touches_it() {
    let bin_dir = tempfile::tempdir().unwrap();
    let copy = bin_dir.path().join("pixel-flow");
    std::fs::copy(BIN, &copy).unwrap();
    let dir = scratch_dir();
    let seen = run(&copy, &["install-links"], dir.path(), &[]);
    assert_eq!(seen.code, 0, "{}", seen.stderr);
    assert!(bin_dir.path().join("pixel-ultraflow").is_symlink());
    assert!(!bin_dir.path().join("pixel-flow").is_symlink());
    let removed = run(&copy, &["install-links", "--remove"], dir.path(), &[]);
    assert_eq!(removed.code, 0, "{}", removed.stderr);
    assert!(copy.is_file(), "--remove must keep the binary");
    assert!(!bin_dir.path().join("pixel-ultraflow").exists());
}
