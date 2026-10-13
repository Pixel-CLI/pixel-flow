// SPDX-FileCopyrightText: The Pixel contributors
// SPDX-License-Identifier: MIT

//! `pixel-flow ultraflow` against the built-in `pixel ultraflow`.
//!
//! Every case stops before `agent-browser` is spawned: the mechanical loop is
//! `pixel-ultraflow`'s own suite (scripted browser and engine), and the
//! decision seam has its unit tests in `src/decider.rs`. What is pinned here
//! is what a user meets first: the flags, the kill switch, and the order the
//! refusals come in. The cases are the ones of core's
//! `crates/pixel/tests/cli/ultraflow_cli.rs`, with goldens recorded from the
//! built-in.
//!
//! The plugin asks the core binary for its decisions (`PIXEL_BIN`). With
//! `PIXEL_CORE_BIN` set that is the real core, so the whole path is the
//! recorded one; without it a stand-in script answers what core would.

use std::path::PathBuf;

use pixel_flow_plugin::testing::{
    CLASSIFY_HANDSHAKE, Setup, assert_parity, core_bin, fake_pixel, scratch_dir, write,
};

const BIN: &str = env!("CARGO_BIN_EXE_pixel-flow");

/// One flow document, saved into the store the run is pointed at.
const FLOW: &str = r#"{
  "name": "sign-in",
  "title": "Sign in",
  "description": "Sign in",
  "tags": ["ultraflow"],
  "url": "https://example.com",
  "steps": [{"action": "snapshot"}],
  "created_unix": 1, "revised_unix": 1, "revision": 1, "proven": false
}"#;

fn parity(case: &str, make: &dyn Fn() -> Setup) {
    assert_parity(env!("CARGO_MANIFEST_DIR"), BIN, "ultraflow", case, make);
}

/// How the stand-in for core answers when there is no real core.
enum Core {
    /// `classify` is switched off, as it is by default.
    Disabled,
    /// The engine opens; nothing else is asked.
    Open,
}

/// A setup with a hermetic `HOME`, a flow store holding `sign-in` and the
/// core binary the plugin will ask.
fn setup(args: &[&str], core: &Core) -> Setup {
    let mut setup = Setup::new(scratch_dir(), args);
    let scratch = setup.scratch();
    let store = scratch.join("flows");
    write(&store, "sign-in.json", FLOW);
    let pixel: PathBuf = core_bin().unwrap_or_else(|| match core {
        Core::Disabled => fake_pixel(
            &scratch,
            &[],
            "pixel: classify is disabled; enable it with `pixel config classify on` or `pixel config setup`\n",
            true,
        ),
        Core::Open => fake_pixel(&scratch, &[CLASSIFY_HANDSHAKE], "", false),
    });
    setup
        .env("HOME", scratch.join("home"))
        .env("PIXEL_FLOW_DIR", &store)
        .env("PIXEL_BIN", pixel)
}

#[test]
fn ultraflow_respects_the_classify_kill_switch() {
    parity("ultraflow-kill-switch", &|| {
        setup(&["replay", "sign-in"], &Core::Disabled)
    });
}

#[test]
fn an_unknown_flow_is_named_before_any_engine_is_opened() {
    parity("ultraflow-unknown-flow", &|| {
        setup(&["replay", "not-there"], &Core::Open)
    });
}

#[test]
fn a_malformed_variable_is_refused_before_the_engine_on_discover() {
    parity("ultraflow-bad-var-discover", &|| {
        setup(
            &[
                "discover",
                "--url",
                "https://example.com",
                "--goal",
                "do the thing",
                "--var",
                "broken",
            ],
            &Core::Disabled,
        )
    });
}

#[test]
fn a_malformed_variable_is_refused_before_the_engine_on_replay() {
    parity("ultraflow-bad-var-replay", &|| {
        setup(&["replay", "sign-in", "--var", "=Zurich"], &Core::Disabled)
    });
}

#[test]
fn discover_needs_a_page_and_a_goal() {
    parity("ultraflow-discover-needs-args", &|| {
        setup(&["discover"], &Core::Disabled)
    });
}

/// With the switch on, the engine is resolved and the flow's own required
/// inputs are checked before any browser work, proven with an engine address
/// nothing answers at.
#[test]
fn an_enabled_run_reports_the_flows_missing_variable() {
    parity("ultraflow-missing-variable", &|| {
        let setup = setup(
            &[
                "replay",
                "gated",
                "--engine",
                "ollaya",
                "--ollaya-url",
                "http://127.0.0.1:9/",
            ],
            &Core::Open,
        );
        let scratch = setup.scrub_paths[0].clone();
        write(
            &scratch,
            "home/.pixel/config.yaml",
            "classify: {enabled: true}\n",
        );
        write(
            &scratch,
            "flows/gated.json",
            &FLOW.replace(r#""name": "sign-in""#, r#""name": "gated""#).replace(
                r#""steps": [{"action": "snapshot"}]"#,
                r#""vars": [{"name": "account", "description": "which account", "required": true}],
               "steps": [{"action": "snapshot"}]"#,
            ),
        );
        setup
    });
}

/// The flags a caller drives the loop with are the command's contract: the
/// same engine inputs `pixel classify` takes, and the loop's own bounds.
#[test]
fn the_discover_help_is_the_built_ins() {
    parity("ultraflow-discover-help", &|| {
        setup(&["discover", "--help"], &Core::Disabled)
    });
}

#[test]
fn the_replay_help_is_the_built_ins() {
    parity("ultraflow-replay-help", &|| {
        setup(&["replay", "--help"], &Core::Disabled)
    });
}
