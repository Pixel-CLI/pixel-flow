// SPDX-FileCopyrightText: The Pixel contributors
// SPDX-License-Identifier: MIT

//! `pixel-flow flow` against the recorded output of the built-in `pixel flow`.
//! Each run gets its own flow store (`PIXEL_FLOW_DIR`), so a case never sees
//! another's flows.
//!
//! `flow` left core in #899, so these goldens are history: they were recorded
//! from a core built at `8a0f30e1`, the last commit that had the built-in, and
//! are never compared with a live core (CONTRIBUTING.md).

use pixel_flow_plugin::testing::{Setup, assert_golden, scratch_dir, write};

const BIN: &str = env!("CARGO_BIN_EXE_pixel-flow");

const STEPS: &str = r#"[
  {"action": "snapshot"},
  {"action": "open", "url": "https://example.com/{{env_name}}"}
]"#;

fn parity(case: &str, make: &dyn Fn() -> Setup) {
    assert_golden(env!("CARGO_MANIFEST_DIR"), BIN, "flow", case, make);
}

/// A setup with its own empty flow store and a steps file; `args` is run
/// after `before` saved `a-flow` when `saved` is set.
fn setup(args: &[&str], saved: bool) -> Setup {
    let mut setup = Setup::new(scratch_dir(), args);
    let store = setup.scratch();
    let steps = store.join("steps.json");
    write(&store, "steps.json", STEPS);
    setup = setup.env("PIXEL_FLOW_DIR", store.join("flows"));
    if saved {
        let steps = steps.display().to_string();
        setup.before.push(
            [
                "save",
                "a-flow",
                "--title",
                "A flow",
                "--description",
                "d",
                "--tag",
                "x",
                "--tag",
                "y",
                "--from-file",
                &steps,
            ]
            .map(String::from)
            .to_vec(),
        );
    }
    setup
}

#[test]
fn listing_an_empty_store() {
    parity("list-empty", &|| setup(&["list"], false));
}

#[test]
fn listing_an_empty_store_as_json() {
    parity("list-empty-json", &|| setup(&["list", "--json"], false));
}

#[test]
fn saving_a_flow() {
    parity("save", &|| {
        let mut s = setup(&[], false);
        let steps = s.scratch().join("steps.json");
        write(steps.parent().unwrap(), "steps.json", STEPS);
        s.args = [
            "save",
            "a-flow",
            "--title",
            "A flow",
            "--tag",
            "x",
            "--from-file",
            &steps.display().to_string(),
        ]
        .map(String::from)
        .to_vec();
        s
    });
}

#[test]
fn saving_from_a_missing_file_prints_the_failure_envelope() {
    parity("save-missing-file-json", &|| {
        setup(
            &[
                "save",
                "b",
                "--title",
                "t",
                "--from-file",
                "/nonexistent/steps.json",
                "--json",
            ],
            false,
        )
    });
}

#[test]
fn listing_saved_flows() {
    parity("list", &|| setup(&["list"], true));
}

#[test]
fn listing_by_tag_as_json() {
    parity("list-tag-json", &|| {
        setup(&["list", "--tag", "x", "--json"], true)
    });
}

#[test]
fn listing_by_an_unused_tag() {
    parity("list-unused-tag", &|| {
        setup(&["list", "--tag", "zzz"], true)
    });
}

#[test]
fn getting_a_flow_as_json() {
    parity("get-json", &|| setup(&["get", "a-flow", "--json"], true));
}

#[test]
fn showing_a_flow() {
    parity("show", &|| setup(&["show", "a-flow"], true));
}

#[test]
fn showing_a_missing_flow_fails() {
    parity("show-missing", &|| setup(&["show", "nope"], false));
}

#[test]
fn replaying_with_variables() {
    parity("replay-vars", &|| {
        setup(&["replay", "a-flow", "--var", "env_name=prod"], true)
    });
}

#[test]
fn replaying_with_variables_as_json() {
    parity("replay-vars-json", &|| {
        setup(
            &["replay", "a-flow", "--var", "env_name=prod", "--json"],
            true,
        )
    });
}

#[test]
fn a_malformed_variable_is_refused_before_anything_runs() {
    parity("replay-bad-var", &|| {
        setup(&["replay", "a-flow", "--var", "bad"], true)
    });
}

#[test]
fn the_account_shortcut_binds_the_account_variable() {
    parity("replay-account", &|| {
        setup(&["replay", "a-flow", "--account", "me@example.com"], true)
    });
}

#[test]
fn revising_a_flow() {
    parity("revise", &|| {
        setup(&["revise", "a-flow", "--title", "New"], true)
    });
}

#[test]
fn deleting_a_flow() {
    parity("delete", &|| setup(&["delete", "a-flow"], true));
}

#[test]
fn deleting_a_missing_flow_as_json() {
    parity("delete-missing-json", &|| {
        setup(&["delete", "nope", "--json"], false)
    });
}
