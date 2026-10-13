// SPDX-FileCopyrightText: The Pixel contributors
// SPDX-License-Identifier: MIT

//! `pixel classify` behind the ultraflow decision seam.
//!
//! The built-in `pixel ultraflow` opened the classify engine inside its own
//! process (`classify::open_session`, private to the `pixel` binary crate). A
//! plugin cannot link that code, so it asks the core binary instead:
//! `pixel classify --jsonl` serves one decision per stdin line on one
//! resident engine, the mode core ships for exactly this use ("one connection,
//! so per-decision latency excludes setup"). One child lives for the whole run,
//! so the engine resolution and the connection are paid once, as before.
//!
//! The wire is core's `parse_spec_line` (`text`, `context`, `labels`,
//! `criteria` in; `ok`, `probs`, `snapshot.model`, `snapshot.provider` or
//! `error` out). Validation, input caps, the `classify` kill switch and the
//! engine choice all stay in core: a refused question carries the same error
//! text it had in-process.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use pixel_ultraflow::{Decider, Decision, Distribution};
use serde_json::{Value, json};

/// Base URL of the local Ollaya server (`decide_ollaya::DEFAULT_BASE` in core).
pub const DEFAULT_OLLAYA_BASE: &str = "http://127.0.0.1:11435";
/// The schema ceiling on options one question may offer (`decide_ollaya::MAX_LABELS`).
pub const MAX_LABELS: usize = 255;
/// How many options the local Ollaya model takes: it refuses a 65th with
/// `422` (`decide_ollaya::MAX_OPTIONS`).
pub const MAX_OPTIONS: usize = 64;

/// The question that tells whether a handshake reached the engine: it has no
/// labels, so core refuses it before any model is asked.
const PROBE_LINE: &str = r#"{"text":"","labels":[]}"#;

/// A real, cheap question, asked only when the engine was left to core's
/// resolution and the provider (which sets the option budget) is unknown.
const PROVIDER_LINE: &str = r#"{"text":"ping","context":"Pick either label.","labels":["a","b"]}"#;

/// Which decision engine answers (`classify::EngineChoice` in core).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Default)]
pub enum EngineChoice {
    /// The OpenAI-compatible remote adapter (default).
    #[default]
    Remote,
    /// A local Ollaya server's native typed-choice readout.
    Ollaya,
}

/// Provider presets, selectable with `--remote-preset`
/// (`decide_remote::Preset` in core).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Default)]
pub enum Preset {
    #[default]
    Openrouter,
    Ollama,
    Local,
    Deepseek,
    OpencodeGo,
    Openai,
    Jev,
    /// Cloudflare's Clef-flash decision model served by Ollama
    /// (`/v1/systemone`): a local server needs no key, a remote Ollama host
    /// takes `OLLAMA_API_KEY`. Served by [`decide_clef`].
    ClefOllama,
    /// Cloudflare's Clef-flash decision model on Workers AI behind a
    /// Cloudflare API token plus account id. Served by [`decide_clef`].
    ClefCloudflare,
}

/// The value of a `ValueEnum` as the command line spells it.
fn spelling<T: clap::ValueEnum>(value: &T) -> String {
    value
        .to_possible_value()
        .map(|v| v.get_name().to_string())
        .unwrap_or_default()
}

/// The engine inputs a run was given: the four `pixel classify` takes.
#[derive(Debug, Clone)]
pub struct Settings {
    pub engine: Option<EngineChoice>,
    pub remote_preset: Option<Preset>,
    pub remote_model: Option<String>,
    pub ollaya_url: String,
}

impl Settings {
    /// The arguments that make `pixel classify` serve these settings.
    fn classify_args(&self) -> Vec<String> {
        let mut args: Vec<String> = ["classify", "--jsonl", "--metrics", "off"]
            .map(String::from)
            .to_vec();
        if let Some(engine) = self.engine {
            args.extend(["--engine".into(), spelling(&engine)]);
        }
        if let Some(preset) = self.remote_preset {
            args.extend(["--remote-preset".into(), spelling(&preset)]);
        }
        if let Some(model) = &self.remote_model {
            args.extend(["--remote-model".into(), model.clone()]);
        }
        args.extend(["--ollaya-url".into(), self.ollaya_url.clone()]);
        args
    }

    /// The provider the settings already name, `None` when core decides.
    fn known_provider(&self) -> Option<String> {
        match self.engine {
            Some(EngineChoice::Ollaya) => Some("ollaya".to_string()),
            Some(EngineChoice::Remote) => Some(
                self.remote_preset
                    .map_or_else(|| "remote".to_string(), |preset| spelling(&preset)),
            ),
            None => None,
        }
    }
}

/// How many options one question may offer the engine behind `provider`.
///
/// The schema ceiling is 255, but a decision head has its own budget: the
/// local Ollaya model refuses a 65th option (`422`), so a caller that
/// builds a question has to offer fewer. A remote chat completion has no
/// such head and takes the ceiling.
pub fn option_budget_for(provider: Option<&str>) -> usize {
    match provider {
        Some("ollaya") => MAX_OPTIONS,
        _ => MAX_LABELS,
    }
}

/// The running `pixel classify --jsonl` and the pipes to it.
struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    /// What the child wrote to stderr before the handshake finished.
    startup_stderr: Arc<Mutex<String>>,
    ready: Arc<AtomicBool>,
    /// The reader of the child's stderr; joined once the child is gone so
    /// nothing it said is lost.
    stderr_thread: Option<std::thread::JoinHandle<()>>,
}

impl Session {
    /// One request line out, one response line back.
    fn ask(&mut self, line: &str) -> Result<Value, String> {
        let stdin = self.stdin.as_mut().ok_or("the classify engine is closed")?;
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.write_all(b"\n"))
            .and_then(|()| stdin.flush())
            .map_err(|e| self.gone(&format!("write: {e}")))?;
        let mut reply = String::new();
        match self.stdout.read_line(&mut reply) {
            Ok(0) => Err(self.gone("it closed its output")),
            Ok(_) => serde_json::from_str(reply.trim_end())
                .map_err(|e| format!("classify answered with something that is not JSON: {e}")),
            Err(e) => Err(self.gone(&format!("read: {e}"))),
        }
    }

    /// The error for a child that is no longer there: what it said on stderr
    /// (`pixel: ` prefix removed), else its exit status.
    fn gone(&mut self, why: &str) -> String {
        self.stdin = None;
        let status = self.child.wait().ok();
        if let Some(thread) = self.stderr_thread.take() {
            let _ = thread.join();
        }
        let said = self
            .startup_stderr
            .lock()
            .map(|text| text.trim().to_string())
            .unwrap_or_default();
        let said = said
            .lines()
            .map(|line| line.strip_prefix("pixel: ").unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n");
        if !said.is_empty() {
            return said;
        }
        match status {
            Some(status) => format!("`pixel classify --jsonl` stopped ({status}): {why}"),
            None => format!("`pixel classify --jsonl` stopped: {why}"),
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Closing stdin ends the serve loop; the wait reaps the child.
        self.stdin = None;
        let _ = self.child.wait();
        if let Some(thread) = self.stderr_thread.take() {
            let _ = thread.join();
        }
    }
}

/// `pixel classify` behind the ultraflow decision seam.
pub struct ClassifyDecider {
    session: RefCell<Session>,
    provider: RefCell<Option<String>>,
    provider_probed: RefCell<bool>,
}

impl ClassifyDecider {
    /// Start `pixel classify --jsonl` from the host's core binary and check
    /// it opened its engine. Opens no browser and asks no model.
    pub fn open(settings: &Settings) -> Result<Self, String> {
        Self::open_with(&crate::env::pixel_bin(), settings)
    }

    /// [`open`](Self::open) with the core binary named.
    pub fn open_with(bin: &Path, settings: &Settings) -> Result<Self, String> {
        let mut child = Command::new(bin)
            .args(settings.classify_args())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                format!(
                    "cannot run the core pixel binary `{}` for its decisions: {e}; \
                     put `pixel` on PATH or set PIXEL_BIN",
                    bin.display()
                )
            })?;
        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .map(BufReader::new)
            .ok_or("no pipe to pixel classify")?;
        let startup_stderr = Arc::new(Mutex::new(String::new()));
        let ready = Arc::new(AtomicBool::new(false));
        let stderr_thread = child.stderr.take().map(|stderr| {
            let (buffer, ready) = (Arc::clone(&startup_stderr), Arc::clone(&ready));
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if ready.load(Ordering::Relaxed) {
                        eprintln!("{line}");
                    } else if let Ok(mut text) = buffer.lock() {
                        text.push_str(&line);
                        text.push('\n');
                    }
                }
            })
        });
        let mut session = Session {
            child,
            stdin,
            stdout,
            startup_stderr,
            ready,
            stderr_thread,
        };
        // The serve loop starts only after core resolved and opened its
        // engine, so an answer (any answer) proves the engine is up, and a
        // closed pipe carries the reason: the kill switch, a refused flag.
        session.ask(PROBE_LINE)?;
        session.ready.store(true, Ordering::Relaxed);
        Ok(Self {
            session: RefCell::new(session),
            provider: RefCell::new(settings.known_provider()),
            provider_probed: RefCell::new(false),
        })
    }

    /// The provider behind the engine, asking core's resolution once when the
    /// settings did not name an engine. `None` when it cannot be learned; the
    /// real error then comes with the first decision.
    fn provider(&self) -> Option<String> {
        if self.provider.borrow().is_none() && !*self.provider_probed.borrow() {
            *self.provider_probed.borrow_mut() = true;
            if let Ok(reply) = self.session.borrow_mut().ask(PROVIDER_LINE) {
                *self.provider.borrow_mut() = provider_of(&reply);
            }
        }
        self.provider.borrow().clone()
    }
}

fn provider_of(reply: &Value) -> Option<String> {
    reply["snapshot"]["provider"].as_str().map(str::to_string)
}

impl Decider for ClassifyDecider {
    fn option_budget(&self) -> usize {
        // An engine whose provider cannot be learned gets the local model's
        // smaller budget: an over-budget question is refused, a short one is not.
        self.provider()
            .map_or(MAX_OPTIONS, |provider| option_budget_for(Some(&provider)))
    }

    fn decide(&mut self, request: &Decision) -> Result<Distribution, String> {
        let line = json!({
            "text": request.text,
            "context": request.context,
            "labels": request.labels,
            "criteria": request.criteria,
        })
        .to_string();
        let reply = self.session.borrow_mut().ask(&line)?;
        if reply["ok"].as_bool() != Some(true) {
            return Err(reply["error"]
                .as_str()
                .unwrap_or("classify refused the question")
                .to_string());
        }
        if let Some(provider) = provider_of(&reply) {
            *self.provider.borrow_mut() = Some(provider);
        }
        let probabilities: BTreeMap<String, f64> =
            serde_json::from_value(reply["probs"].clone())
                .map_err(|e| format!("classify answered without probabilities: {e}"))?;
        Ok(Distribution {
            probabilities,
            model: reply["snapshot"]["model"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(engine: Option<EngineChoice>) -> Settings {
        Settings {
            engine,
            remote_preset: None,
            remote_model: None,
            ollaya_url: DEFAULT_OLLAYA_BASE.to_string(),
        }
    }

    use crate::testing::fake_pixel;

    use crate::testing::CLASSIFY_HANDSHAKE as HANDSHAKE;

    fn question() -> Decision {
        Decision {
            text: "page".into(),
            context: "goal".into(),
            labels: vec!["a".into(), "b".into()],
            criteria: BTreeMap::new(),
        }
    }

    #[test]
    fn the_engine_flags_are_core_classifys() {
        let mut s = settings(Some(EngineChoice::Ollaya));
        s.remote_preset = Some(Preset::ClefOllama);
        s.remote_model = Some("m".into());
        assert_eq!(
            s.classify_args(),
            [
                "classify",
                "--jsonl",
                "--metrics",
                "off",
                "--engine",
                "ollaya",
                "--remote-preset",
                "clef-ollama",
                "--remote-model",
                "m",
                "--ollaya-url",
                DEFAULT_OLLAYA_BASE,
            ]
        );
        assert_eq!(
            settings(None).classify_args(),
            [
                "classify",
                "--jsonl",
                "--metrics",
                "off",
                "--ollaya-url",
                DEFAULT_OLLAYA_BASE
            ]
        );
    }

    /// The local model's budget is measured, not the schema ceiling: a page
    /// that overflows it must be told so here, because the engine refuses an
    /// over-budget question rather than truncating it.
    #[test]
    fn the_option_budget_follows_the_engine_a_run_resolved_to() {
        assert_eq!(option_budget_for(Some("ollaya")), MAX_OPTIONS);
        assert_eq!(MAX_OPTIONS, 64);
        for provider in [Some("openrouter"), Some("ollama"), None] {
            assert_eq!(option_budget_for(provider), MAX_LABELS, "{provider:?}");
        }
    }

    #[test]
    fn a_decision_is_the_probabilities_and_the_model_of_the_reply() {
        let dir = tempfile::tempdir().unwrap();
        let reply = r#"{"ok":true,"probs":{"a":0.25,"b":0.75},"snapshot":{"model":"m1","provider":"openrouter"}}"#;
        let bin = fake_pixel(dir.path(), &[HANDSHAKE, reply], "", false);
        let mut decider =
            ClassifyDecider::open_with(&bin, &settings(Some(EngineChoice::Remote))).unwrap();
        let answer = decider.decide(&question()).unwrap();
        assert_eq!(answer.model, "m1");
        assert_eq!(answer.probabilities["b"], 0.75);
        // `--engine remote` names the provider family: no probe was needed.
        assert_eq!(decider.option_budget(), MAX_LABELS);
    }

    #[test]
    fn a_refused_question_carries_cores_error_text() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_pixel(
            dir.path(),
            &[
                HANDSHAKE,
                r#"{"ok":false,"error":"labels must be non-empty and distinct"}"#,
            ],
            "",
            false,
        );
        let mut decider =
            ClassifyDecider::open_with(&bin, &settings(Some(EngineChoice::Ollaya))).unwrap();
        assert_eq!(
            decider.decide(&question()).unwrap_err(),
            "labels must be non-empty and distinct"
        );
        assert_eq!(decider.option_budget(), MAX_OPTIONS);
    }

    #[test]
    fn an_unnamed_engine_is_learned_once_from_core() {
        let dir = tempfile::tempdir().unwrap();
        let probe =
            r#"{"ok":true,"probs":{"a":0.5,"b":0.5},"snapshot":{"model":"m","provider":"ollaya"}}"#;
        let bin = fake_pixel(dir.path(), &[HANDSHAKE, probe], "", false);
        let decider = ClassifyDecider::open_with(&bin, &settings(None)).unwrap();
        assert_eq!(decider.option_budget(), MAX_OPTIONS);
        // Learned: the second call asks nothing (the fake has no third line).
        assert_eq!(decider.option_budget(), MAX_OPTIONS);
    }

    #[test]
    fn an_engine_that_will_not_start_reports_what_it_said() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_pixel(
            dir.path(),
            &[],
            "pixel: classify is disabled; enable it with `pixel config classify on`\n",
            true,
        );
        let error = ClassifyDecider::open_with(&bin, &settings(None))
            .err()
            .unwrap();
        assert_eq!(
            error,
            "classify is disabled; enable it with `pixel config classify on`"
        );
    }

    #[test]
    fn a_missing_core_binary_says_how_to_find_one() {
        let error = ClassifyDecider::open_with(Path::new("/nonexistent/pixel"), &settings(None))
            .err()
            .unwrap();
        assert!(error.contains("/nonexistent/pixel"), "{error}");
        assert!(error.contains("PIXEL_BIN"), "{error}");
    }
}
