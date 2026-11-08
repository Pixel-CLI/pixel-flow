// SPDX-FileCopyrightText: The Pixel contributors
// SPDX-License-Identifier: MIT

//! Native macOS apps behind the [`Browser`] seam, through `agent-desktop`.
//!
//! A flow whose start is `app://<App>` runs here instead of in
//! `agent-browser`. Every agent-browser verb the executor and ultraflow
//! issue is translated: `open app://X` launches X, `snapshot -i` is
//! `agent-desktop snapshot --app X --compact` rendered into agent-browser's
//! snapshot text (so `pixel-ultraflow`'s parser and the executor's ref
//! matcher read it unchanged), and `@eN` refs resolve against the snapshot
//! that printed them. Verbs with no desktop meaning (`eval`, `connect`,
//! tabs) are refused with an error that says so.

use std::process::Command;
use std::time::Duration;

use serde_json::Value;

use crate::execute::Browser;

/// The scheme that marks a flow start as a desktop app.
pub const APP_SCHEME: &str = "app://";

/// One `agent-desktop` call: its arguments in, the envelope's `data` out, or
/// its `error` object (a synthesized `{message}` when there was no envelope).
type Cli = Box<dyn FnMut(&[String]) -> Result<Value, Value>>;

/// A desktop app driven through `agent-desktop`.
pub struct Desktop {
    app: Option<String>,
    /// The id of the latest snapshot: the short `eN` refs it printed
    /// qualify against it (`@<id>:eN`), and only against it.
    snapshot_id: Option<String>,
    /// The first scroll area of the latest snapshot: agent-browser scrolls
    /// the page, agent-desktop scrolls an element.
    scroll_area: Option<String>,
    cli: Cli,
}

impl Desktop {
    /// The real `agent-desktop` on PATH.
    pub fn new() -> Self {
        Self::with_cli(Box::new(run_agent_desktop))
    }

    fn with_cli(cli: Cli) -> Self {
        Self {
            app: None,
            snapshot_id: None,
            scroll_area: None,
            cli,
        }
    }

    /// One call, retried once when agent-desktop says nothing was delivered
    /// (`disposition.retry == "safe"`) and never otherwise: a repeat of an
    /// action that may have landed clicks or types twice.
    fn call(&mut self, args: &[String]) -> Result<Value, String> {
        let first = (self.cli)(args);
        let result = match first {
            Err(error) if error["disposition"]["retry"] == "safe" => (self.cli)(args),
            other => other,
        };
        result.map_err(|error| {
            let code = error["code"].as_str().unwrap_or("ERROR");
            let message = error["message"].as_str().unwrap_or("agent-desktop failed");
            format!(
                "agent-desktop {}: {code}: {message}",
                args.first().map_or("", String::as_str)
            )
        })
    }

    fn app(&self) -> Result<&str, String> {
        self.app
            .as_deref()
            .ok_or_else(|| "no app is open: a desktop flow starts with `open app://<App>`".into())
    }

    /// The qualified agent-desktop ref for a short `@eN` of the latest snapshot.
    fn qualify(&self, short: &str) -> Result<String, String> {
        let short = short.trim_start_matches('@');
        let sid = self
            .snapshot_id
            .as_deref()
            .ok_or_else(|| format!("@{short}: no snapshot taken yet"))?;
        if !short.starts_with('e') {
            return Err(format!("@{short} is page text, not a control"));
        }
        Ok(format!("@{sid}:{short}"))
    }

    fn snapshot(&mut self) -> Result<String, String> {
        let app = self.app()?.to_string();
        // Not `-i`: the interactive filter drops static text, and a
        // calculator's result or a dialog's message is static text.
        let data = self.call(&args(&["snapshot", "--app", &app, "--compact"]))?;
        let (text, scroll_area) = render_snapshot(&data["tree"]);
        self.snapshot_id = data["snapshot_id"].as_str().map(str::to_string);
        self.scroll_area = scroll_area;
        Ok(text)
    }

    /// One ref action: `verb <qualified ref> [-- value]`.
    fn act(&mut self, verb: &str, short: &str, value: Option<&str>) -> Result<String, String> {
        let target = self.qualify(short)?;
        let mut call = args(&[verb, &target]);
        if let Some(value) = value {
            // `--` so a value that starts with `-` is text, not a flag.
            call.extend(args(&["--", value]));
        }
        self.call(&call).map(|data| data.to_string())
    }
}

impl Default for Desktop {
    fn default() -> Self {
        Self::new()
    }
}

impl Browser for Desktop {
    fn run(&mut self, argv: &[&str]) -> Result<String, String> {
        match argv {
            ["open", url] => {
                let app = url
                    .strip_prefix(APP_SCHEME)
                    .filter(|app| !app.trim().is_empty())
                    .ok_or_else(|| format!("a desktop flow opens app://<App>, not {url}"))?;
                self.call(&args(&["launch", app, "--activate"]))?;
                self.app = Some(app.to_string());
                self.snapshot_id = None;
                Ok(String::new())
            }
            // The "page" of a desktop flow is the app, so the executor's
            // navigation poll sees no move and runs to its cap: that is
            // the settle time a native UI gets after a click.
            ["get", "url"] => Ok(self
                .app
                .as_deref()
                .map(|app| format!("{APP_SCHEME}{app}"))
                .unwrap_or_default()),
            ["snapshot", ..] => self.snapshot(),
            ["click", target] => self.act("click", target, None),
            ["fill", target, value] => self.act("set-value", target, Some(value)),
            ["type", target, value] => self.act("type", target, Some(value)),
            ["select", target, value] => self.act("select", target, Some(value)),
            ["press", key] => {
                let app = self.app()?.to_string();
                let combo = desktop_key(key);
                self.call(&args(&["press", &combo, "--app", &app]))
                    .map(|data| data.to_string())
            }
            ["scroll", direction, pixels] => {
                // shortcut: scrolls the first scroll area of the window, so a
                // window with several scrolls the first; upgrade when a step
                // can name its scroll area.
                let area = self
                    .scroll_area
                    .clone()
                    .ok_or("the app shows no scroll area to scroll")?;
                let target = self.qualify(&area)?;
                let units = (pixels.parse::<u32>().unwrap_or(300) / 100).max(1);
                self.call(&args(&[
                    "scroll",
                    &target,
                    "--direction",
                    direction,
                    "--amount",
                    &units.to_string(),
                ]))
                .map(|data| data.to_string())
            }
            [verb, ..] => Err(format!(
                "`{verb}` is a browser command; a desktop app ({}) does not support it",
                self.app
                    .as_deref()
                    .map_or_else(|| "app://".to_string(), |app| format!("{APP_SCHEME}{app}"))
            )),
            [] => Err("empty command".into()),
        }
    }

    fn pause(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(ToString::to_string).collect()
}

/// Spawn `agent-desktop <args>` and read its one JSON envelope.
// Spawns the real agent-desktop: `Desktop::with_cli` is what tests script.
#[cfg_attr(test, mutants::skip)]
fn run_agent_desktop(argv: &[String]) -> Result<Value, Value> {
    let output = Command::new("agent-desktop").args(argv).output().map_err(
        |e| serde_json::json!({"message": format!("failed to spawn agent-desktop: {e}")}),
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    match serde_json::from_str::<Value>(stdout.trim()) {
        Ok(envelope) if envelope["ok"] == true => Ok(envelope["data"].clone()),
        Ok(envelope) => Err(envelope["error"].clone()),
        Err(_) => Err(serde_json::json!({
            "message": format!(
                "exited with {}: {}{}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim(),
                stdout.trim()
            )
        })),
    }
}

/// The names of the running apps that show a UI, for the "which app" question.
#[cfg_attr(test, mutants::skip)]
pub fn running_apps() -> Result<Vec<String>, String> {
    let data = Desktop::new().call(&args(&["list-apps"]))?;
    Ok(foreground_apps(&data))
}

fn foreground_apps(data: &Value) -> Vec<String> {
    let mut names: Vec<String> = data["apps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|app| app["presentation"] == "foreground")
        .filter_map(|app| app["name"].as_str().map(str::to_string))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// An agent-browser key (`Enter`, `ArrowDown`, `Control+a`) as an
/// agent-desktop combo (`return`, `down`, `ctrl+a`). Lowercase throughout:
/// agent-desktop reads a bare uppercase letter as shift+letter.
fn desktop_key(key: &str) -> String {
    key.split('+')
        .map(|part| match part.to_ascii_lowercase().as_str() {
            "enter" => "return".to_string(),
            "esc" => "escape".to_string(),
            "backspace" => "delete".to_string(),
            "control" => "ctrl".to_string(),
            "meta" | "command" => "cmd".to_string(),
            "option" => "alt".to_string(),
            "arrowup" => "up".to_string(),
            "arrowdown" => "down".to_string(),
            "arrowleft" => "left".to_string(),
            "arrowright" => "right".to_string(),
            " " => "space".to_string(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// The agent-browser role word for an agent-desktop role. Roles agent-browser
/// has no word for keep their own and are marked `clickable` when they
/// advertise a click.
fn browser_role(role: &str) -> &str {
    match role {
        "textfield" | "searchfield" | "securetextfield" => "textbox",
        "radiobutton" => "radio",
        // Menu-backed: `select` opens the menu and picks the item.
        "menubutton" | "popupbutton" => "combobox",
        "menubaritem" => "menuitem",
        "statictext" => "text",
        other => other,
    }
}

/// Roles agent-browser's parser already offers an operation for: only other
/// roles need the `clickable` marker.
const KNOWN_ROLES: &[&str] = &[
    "button", "link", "menuitem", "tab", "checkbox", "radio", "switch", "slider", "option",
    "combobox", "textbox", "textarea",
];

/// Render an agent-desktop snapshot tree as agent-browser `snapshot -i`
/// text, and name its first scroll area. A node with a ref is
/// `- role "name" [ref=eN]` (the short ref of the qualified one), with
/// `disabled`, `checked=` and `clickable` in the attribute block and the
/// value echoed after it. A named node without a ref is page text, printed
/// with a `t` ref the executor never resolves, so a decision sees it and
/// can never target it.
pub fn render_snapshot(tree: &Value) -> (String, Option<String>) {
    let mut out = String::new();
    let mut scroll_area = None;
    let mut texts = 0usize;
    render_node(tree, 0, &mut out, &mut scroll_area, &mut texts);
    (out, scroll_area)
}

fn render_node(
    node: &Value,
    depth: usize,
    out: &mut String,
    scroll_area: &mut Option<String>,
    texts: &mut usize,
) {
    let role = node["role"].as_str().unwrap_or("group");
    let name = one_line(node["name"].as_str().unwrap_or_default());
    let value = one_line(node["value"].as_str().unwrap_or_default());
    let states: Vec<&str> = node["states"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let short = node["ref_id"]
        .as_str()
        .and_then(|qualified| qualified.rsplit_once(':'))
        .map(|(_, short)| short.to_string());
    let indent = "  ".repeat(depth);
    match short {
        Some(short) => {
            if role == "scrollarea" && scroll_area.is_none() {
                *scroll_area = Some(short.clone());
            }
            let word = browser_role(role);
            let mut attrs = Vec::new();
            if states.contains(&"disabled") {
                attrs.push("disabled".to_string());
            }
            if matches!(word, "checkbox" | "radio" | "switch") {
                let on = states.iter().any(|s| matches!(*s, "checked" | "on")) || value == "1";
                attrs.push(format!("checked={on}"));
            }
            let clicks = node["available_actions"]
                .as_array()
                .is_some_and(|actions| actions.iter().any(|a| a == "Click"));
            if clicks && !KNOWN_ROLES.contains(&word) {
                attrs.push("clickable".to_string());
            }
            attrs.push(format!("ref={short}"));
            out.push_str(&format!(
                "{indent}- {word} \"{name}\" [{}]",
                attrs.join(", ")
            ));
            if !value.is_empty()
                && value != name
                && !matches!(word, "checkbox" | "radio" | "switch")
            {
                out.push_str(&format!(": {value}"));
            }
            out.push('\n');
        }
        None if !name.is_empty() => {
            *texts += 1;
            out.push_str(&format!(
                "{indent}- {} \"{name}\" [ref=t{texts}]\n",
                browser_role(role)
            ));
        }
        None => {}
    }
    for child in node["children"].as_array().into_iter().flatten() {
        render_node(child, depth + 1, out, scroll_area, texts);
    }
}

/// A name or value fit for one snapshot line: no line breaks, and no `"`,
/// which would end the quoted name early.
fn one_line(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('"', "'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A real `agent-desktop snapshot --app Calculator --compact` envelope.
    const CALCULATOR: &str = include_str!("testdata/calculator-snapshot.json");

    fn calculator() -> Value {
        serde_json::from_str::<Value>(CALCULATOR).unwrap()["data"].clone()
    }

    #[test]
    fn a_real_snapshot_renders_as_agent_browser_text() {
        let (text, scroll) = render_snapshot(&calculator()["tree"]);
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        assert_eq!(lines[0], "- window \"Calculator\" [ref=t1]");
        assert!(lines.contains(&"- scrollarea \"\" [ref=e2]"), "{text}");
        // The result is static text: shown as page text, never a target.
        assert!(lines.contains(&"- text \"42\" [ref=t3]"), "{text}");
        assert!(lines.contains(&"- button \"2\" [ref=e16]"), "{text}");
        assert!(
            lines.contains(&"- button \"Zoom\" [disabled, ref=e26]"),
            "{text}"
        );
        // A nameless splitter carries nothing a decision reads.
        assert!(!text.contains("splitter"), "{text}");
        assert_eq!(scroll.as_deref(), Some("e1"));
    }

    #[test]
    fn roles_states_and_values_map_to_agent_browser_shapes() {
        let tree = serde_json::json!({"role": "window", "children": [
            {"role": "textfield", "name": "To", "value": "a@b.c\nd", "ref_id": "@s1:e1"},
            {"role": "checkbox", "name": "Bold", "states": ["checked"], "ref_id": "@s1:e2"},
            {"role": "switch", "name": "Wi\"Fi", "value": "0", "ref_id": "@s1:e3"},
            {"role": "cell", "name": "Desktop", "available_actions": ["Click"], "ref_id": "@s1:e4"},
            {"role": "menubutton", "name": "Mode", "available_actions": ["Click"], "ref_id": "@s1:e5"},
            {"role": "radiobutton", "name": "One", "states": ["on"], "ref_id": "@s1:e6"},
        ]});
        let (text, scroll) = render_snapshot(&tree);
        assert_eq!(
            text,
            "  - textbox \"To\" [ref=e1]: a@b.c d\n\
             \x20 - checkbox \"Bold\" [checked=true, ref=e2]\n\
             \x20 - switch \"Wi'Fi\" [checked=false, ref=e3]\n\
             \x20 - cell \"Desktop\" [clickable, ref=e4]\n\
             \x20 - combobox \"Mode\" [ref=e5]\n\
             \x20 - radio \"One\" [checked=true, ref=e6]\n"
        );
        assert_eq!(scroll, None);
    }

    /// A Desktop whose agent-desktop answers from a queue and records calls.
    fn scripted(answers: Vec<Result<Value, Value>>) -> (Desktop, Rc<RefCell<Vec<Vec<String>>>>) {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let seen = Rc::clone(&calls);
        let mut queue = std::collections::VecDeque::from(answers);
        let desktop = Desktop::with_cli(Box::new(move |argv: &[String]| {
            seen.borrow_mut().push(argv.to_vec());
            queue.pop_front().unwrap_or(Ok(Value::Null))
        }));
        (desktop, calls)
    }

    #[test]
    fn agent_browser_verbs_become_agent_desktop_commands() {
        let (mut d, calls) = scripted(vec![Ok(Value::Null), Ok(calculator())]);
        assert_eq!(d.run(&["get", "url"]).unwrap(), "");
        d.run(&["open", "app://Calculator"]).unwrap();
        assert_eq!(d.run(&["get", "url"]).unwrap(), "app://Calculator");
        let text = d.run(&["snapshot", "-i"]).unwrap();
        assert!(text.contains("[ref=e16]"));
        d.run(&["click", "@e16"]).unwrap();
        d.run(&["fill", "@e2", "-5"]).unwrap();
        d.run(&["type", "@e2", "x"]).unwrap();
        d.run(&["select", "@e24", "Scientific"]).unwrap();
        d.run(&["press", "Control+Enter"]).unwrap();
        d.run(&["scroll", "down", "500"]).unwrap();
        let sid = "@s1v0knx355lq8y";
        assert_eq!(
            *calls.borrow(),
            vec![
                args(&["launch", "Calculator", "--activate"]),
                args(&["snapshot", "--app", "Calculator", "--compact"]),
                args(&["click", &format!("{sid}:e16")]),
                args(&["set-value", &format!("{sid}:e2"), "--", "-5"]),
                args(&["type", &format!("{sid}:e2"), "--", "x"]),
                args(&["select", &format!("{sid}:e24"), "--", "Scientific"]),
                args(&["press", "ctrl+return", "--app", "Calculator"]),
                args(&[
                    "scroll",
                    &format!("{sid}:e1"),
                    "--direction",
                    "down",
                    "--amount",
                    "5"
                ]),
            ]
        );
    }

    #[test]
    fn browser_only_verbs_and_page_text_are_refused() {
        let (mut d, calls) = scripted(vec![Ok(Value::Null), Ok(calculator())]);
        assert!(
            d.run(&["click", "@e1"])
                .unwrap_err()
                .contains("no snapshot")
        );
        assert!(d.run(&["open", "https://e.com"]).is_err());
        d.run(&["open", "app://Calculator"]).unwrap();
        d.run(&["snapshot", "-i"]).unwrap();
        assert!(d.run(&["click", "@t3"]).unwrap_err().contains("page text"));
        for verb in [
            &["eval", "1"][..],
            &["connect", "9222"],
            &["tab", "list"],
            &["get", "title"],
        ] {
            let error = d.run(verb).unwrap_err();
            assert!(error.contains("does not support"), "{error}");
            assert!(error.contains("app://Calculator"), "{error}");
        }
        assert_eq!(calls.borrow().len(), 2, "nothing was sent for a refusal");
    }

    #[test]
    fn only_an_undelivered_failure_is_retried_and_errors_read_verbatim() {
        let safe = serde_json::json!({"code": "TIMEOUT", "message": "slow", "disposition": {"retry": "safe"}});
        let unsafe_ = serde_json::json!({
            "code": "ACTION_FAILED",
            "message": "Application has no verified focused element for keyboard delivery",
            "disposition": {"retry": "unsafe"}
        });
        let (mut d, calls) = scripted(vec![Err(safe), Ok(Value::Null), Err(unsafe_)]);
        d.run(&["open", "app://Calculator"]).unwrap();
        assert_eq!(calls.borrow().len(), 2, "a safe failure is retried once");
        let error = d.run(&["press", "Enter"]).unwrap_err();
        assert_eq!(
            error,
            "agent-desktop press: ACTION_FAILED: Application has no verified focused element for keyboard delivery"
        );
        assert_eq!(calls.borrow().len(), 3, "an unsafe failure is not retried");
    }

    #[test]
    fn the_app_list_is_the_foreground_apps_once_each() {
        let data = serde_json::json!({"apps": [
            {"name": "Finder", "presentation": "foreground"},
            {"name": "Always", "presentation": "foreground"},
            {"name": "Always", "presentation": "foreground"},
            {"name": "AirPlay", "presentation": "background"},
            {"name": "Helper"},
        ]});
        assert_eq!(foreground_apps(&data), ["Always", "Finder"]);
    }

    #[test]
    fn keys_are_spelled_the_agent_desktop_way() {
        assert_eq!(desktop_key("Enter"), "return");
        assert_eq!(desktop_key("Meta+Shift+K"), "cmd+shift+k");
        assert_eq!(desktop_key("ArrowDown"), "down");
        assert_eq!(desktop_key("Tab"), "tab");
    }
}
