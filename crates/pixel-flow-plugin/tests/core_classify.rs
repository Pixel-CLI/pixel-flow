// SPDX-FileCopyrightText: The Pixel contributors
// SPDX-License-Identifier: MIT

//! The decision seam against a real core: `pixel classify --jsonl` of the
//! binary `PIXEL_CORE_BIN` names, answering from a local stand-in for a
//! chat-completion server. Skipped (and said so) without `PIXEL_CORE_BIN`.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;

use clap::ValueEnum;
use pixel_flow_plugin::decider::{
    ClassifyDecider, DEFAULT_OLLAYA_BASE, EngineChoice, Preset, Settings,
};
use pixel_flow_plugin::testing::core_bin;
use pixel_ultraflow::{Decider, Decision};

/// Serve `replies` chat completions on a free port, one per connection, and
/// return the base URL.
fn chat_server(replies: Vec<&'static str>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for reply in replies {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut seen = Vec::new();
            let mut chunk = [0u8; 4096];
            let body_start = loop {
                let n = stream.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break None;
                }
                seen.extend_from_slice(&chunk[..n]);
                if let Some(at) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
                    break Some(at + 4);
                }
            };
            if let Some(start) = body_start {
                let head = String::from_utf8_lossy(&seen[..start]).to_lowercase();
                let length = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                while seen.len() < start + length {
                    let n = stream.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    seen.extend_from_slice(&chunk[..n]);
                }
            }
            let body = format!(
                r#"{{"choices":[{{"message":{{"content":{}}}}}]}}"#,
                serde_json::to_string(reply).unwrap()
            );
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    base
}

#[test]
fn a_decision_round_trips_through_the_real_core() {
    let Some(core) = core_bin() else {
        eprintln!("skipped: PIXEL_CORE_BIN is not set");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join(".pixel")).unwrap();
    std::fs::write(
        home.join(".pixel/config.yaml"),
        "classify: {enabled: true}\n",
    )
    .unwrap();
    let base = chat_server(vec![r#"{"a": 0.2, "b": 0.8}"#]);
    // Core reads its settings from the environment of the process it runs in.
    let wrapper = dir.path().join("pixel");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexport HOME='{}' PIXEL_REMOTE_BASE='{base}'\nexec '{}' \"$@\"\n",
            home.display(),
            core.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut decider = ClassifyDecider::open_with(
        &wrapper,
        &Settings {
            engine: None,
            remote_preset: Some(Preset::Local),
            remote_model: Some("stand-in".to_string()),
            ollaya_url: "http://127.0.0.1:9/".to_string(),
        },
    )
    .expect("core opens its engine");
    let answer = decider
        .decide(&Decision {
            text: "the page".into(),
            context: "pick".into(),
            labels: vec!["a".into(), "b".into()],
            criteria: BTreeMap::new(),
        })
        .expect("a decision");
    assert_eq!(answer.model, "stand-in");
    assert!((answer.probabilities["b"] - 0.8).abs() < 1e-9, "{answer:?}");
    // The provider came back with the answer: a chat preset takes the ceiling.
    assert_eq!(decider.option_budget(), 255);

    // A refused question carries core's own error text.
    let refused = decider
        .decide(&Decision {
            text: "x".into(),
            context: String::new(),
            labels: vec!["a".into(), "a".into()],
            criteria: BTreeMap::new(),
        })
        .unwrap_err();
    assert_eq!(refused, "labels must be non-empty and distinct");
}

/// The names of `values` as the command line spells them.
fn names<T: ValueEnum>(values: &[T]) -> Vec<String> {
    values
        .iter()
        .map(|v| v.to_possible_value().unwrap().get_name().to_string())
        .collect()
}

/// The engine inputs this repository restates (`decider.rs`) must be the ones
/// `pixel classify` offers: a preset or engine core added or renamed, or a
/// moved default, fails here. The option budgets (`MAX_OPTIONS`,
/// `MAX_LABELS`) have no flag to read; they follow `decide_ollaya.rs`.
#[test]
fn the_restated_engine_inputs_are_cores() {
    let Some(core) = core_bin() else {
        eprintln!("skipped: PIXEL_CORE_BIN is not set");
        return;
    };
    let out = std::process::Command::new(core)
        .args(["classify", "--help"])
        .output()
        .unwrap();
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains(&format!("[default: {DEFAULT_OLLAYA_BASE}]")),
        "the local Ollaya default moved"
    );
    let value_lines = |heading: &str| -> Vec<String> {
        help.lines()
            .skip_while(|line| !line.contains(heading))
            .skip_while(|line| !line.trim().starts_with("Possible values:"))
            .skip(1)
            .map_while(|line| line.trim().strip_prefix("- "))
            .map(|line| {
                line.split(':')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_string()
            })
            .collect()
    };
    assert_eq!(
        value_lines("--remote-preset"),
        names(Preset::value_variants())
    );
    assert_eq!(
        value_lines("--engine <ENGINE>"),
        names(EngineChoice::value_variants())
    );
}
