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
//!
//! Some controls are drawn but missing from the accessibility tree (a
//! WebKit settings page's hotkey recorder). Each snapshot therefore also
//! OCRs the window (a `cua-driver` screenshot, macOS Vision through a small
//! bundled Swift helper) and lists every recognised line the tree does not
//! already name as a clickable `text` with an `oN` ref. A click on one is a
//! `cua-driver` foreground pixel click at its centre, and a key chord that
//! follows it is delivered there, the same way.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use std::process::Command;
use std::time::Duration;

use serde_json::Value;

use crate::execute::Browser;

/// The scheme that marks a flow start as a desktop app.
pub const APP_SCHEME: &str = "app://";

/// The `cua-driver` session label every call carries.
const CUA_SESSION: &str = "pixel-flow";

/// The processes a desktop step reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// `agent-desktop <args>`: the accessibility tree and its headless actions.
    AgentDesktop,
    /// `cua-driver call <tool> <json>`: screenshots and foreground pixel input.
    Cua,
    /// The bundled Vision OCR helper on one PNG.
    Ocr,
}

/// One tool call: its arguments in, the JSON answer (agent-desktop: the
/// envelope's `data`) out, or the error object (a synthesized `{message}`
/// when there was none).
type Tools = Box<dyn FnMut(Tool, &[String]) -> Result<Value, Value>>;

/// A desktop app driven through `agent-desktop`.
pub struct Desktop {
    app: Option<String>,
    /// The id of the latest snapshot: the short `eN` refs it printed
    /// qualify against it (`@<id>:eN`), and only against it.
    snapshot_id: Option<String>,
    /// The first scroll area of the latest snapshot: agent-browser scrolls
    /// the page, agent-desktop scrolls an element.
    scroll_area: Option<String>,
    /// The app's process and the window the latest snapshot read: the
    /// target of every `cua-driver` call.
    pid: Option<u64>,
    window_id: Option<u64>,
    /// The centre of each `oN` text target of the latest snapshot, in the
    /// screenshot pixels `cua-driver` clicks in.
    ocr_points: HashMap<String, (f64, f64)>,
    /// Where the last pixel click landed, while the step after it may still
    /// need it: a chord goes where that click put the focus.
    last_point: Option<(f64, f64)>,
    /// Whether this run has (re)opened its `cua-driver` session: an ended
    /// session refuses every call until `start_session` revives it.
    cua_started: bool,
    tools: Tools,
}

impl Desktop {
    /// The real `agent-desktop` on PATH.
    pub fn new() -> Self {
        Self::with_tools(Box::new(run_tool))
    }

    fn with_tools(tools: Tools) -> Self {
        Self {
            app: None,
            snapshot_id: None,
            scroll_area: None,
            pid: None,
            window_id: None,
            ocr_points: HashMap::new(),
            last_point: None,
            cua_started: false,
            tools,
        }
    }

    /// One call, retried once when agent-desktop says nothing was delivered
    /// (`disposition.retry == "safe"`) and never otherwise: a repeat of an
    /// action that may have landed clicks or types twice.
    fn call(&mut self, args: &[String]) -> Result<Value, String> {
        let first = (self.tools)(Tool::AgentDesktop, args);
        let result = match first {
            Err(error) if error["disposition"]["retry"] == "safe" => {
                (self.tools)(Tool::AgentDesktop, args)
            }
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
        let (mut text, scroll_area) = render_snapshot(&data["tree"]);
        self.snapshot_id = data["snapshot_id"].as_str().map(str::to_string);
        self.scroll_area = scroll_area;
        self.window_id = data["window"]["id"]
            .as_str()
            .and_then(|id| id.trim_start_matches("w-").parse().ok());
        self.ocr_points.clear();
        match self.ocr_lines() {
            Ok(lines) => {
                let (extra, points) = ocr_slots(&lines, &ax_names(&data["tree"]));
                text.push_str(&extra);
                self.ocr_points = points;
            }
            // The tree alone is still a page: say what is missing, in a line
            // no parser reads as an element.
            Err(error) => text.push_str(&format!("# ocr unavailable: {error}\n")),
        }
        Ok(text)
    }

    /// The process and window `cua-driver` targets.
    fn window(&self) -> Result<(u64, u64), String> {
        match (self.pid, self.window_id) {
            (Some(pid), Some(window)) => Ok((pid, window)),
            _ => Err("no app window is known yet".into()),
        }
    }

    /// One `cua-driver` call on the app's window, in the shared session.
    fn cua(&mut self, tool: &str, mut arguments: Value) -> Result<Value, String> {
        let (pid, window_id) = self.window()?;
        if !self.cua_started {
            self.cua_started = true;
            let start = serde_json::json!({"session": CUA_SESSION}).to_string();
            // Idempotent; a failure here shows up as the real call's error.
            let _ = (self.tools)(Tool::Cua, &args(&["call", "start_session", &start]));
        }
        arguments["pid"] = pid.into();
        arguments["window_id"] = window_id.into();
        arguments["session"] = CUA_SESSION.into();
        (self.tools)(Tool::Cua, &args(&["call", tool, &arguments.to_string()])).map_err(|error| {
            let code = error["code"].as_str().unwrap_or("ERROR");
            let message = error["message"]
                .as_str()
                .or_else(|| error["hint"].as_str())
                .unwrap_or("cua-driver failed");
            format!("cua-driver {tool}: {code}: {message}")
        })
    }

    /// The text lines of the window as OCR reads them: one screenshot.
    fn ocr_lines(&mut self) -> Result<Value, String> {
        let png = std::env::temp_dir().join(format!("pixel-flow-ocr-{}.png", std::process::id()));
        let shot = serde_json::json!({
            "include_accessibility_tree": false,
            "screenshot_out_file": png.display().to_string(),
        });
        self.cua("get_window_state", shot)?;
        let lines = (self.tools)(Tool::Ocr, &[png.display().to_string()]);
        let _ = std::fs::remove_file(&png);
        lines.map_err(|error| {
            error["message"]
                .as_str()
                .unwrap_or("ocr failed")
                .to_string()
        })
    }

    /// A foreground pixel click on an `oN` text target of the latest snapshot.
    fn click_text(&mut self, short: &str) -> Result<String, String> {
        let short = short.trim_start_matches('@');
        let (x, y) = *self
            .ocr_points
            .get(short)
            .ok_or_else(|| format!("@{short} is not a text target of the latest snapshot"))?;
        let answer = self.cua(
            "click",
            serde_json::json!({"x": x, "y": y, "delivery_mode": "foreground"}),
        )?;
        self.last_point = Some((x, y));
        Ok(answer.to_string())
    }

    /// A key chord: where the last pixel click put the focus when there was
    /// one, else agent-desktop's headless press, falling back to a
    /// foreground `cua-driver` chord when the app has no focused element
    /// agent-desktop can verify (it is not frontmost).
    fn press(&mut self, key: &str) -> Result<String, String> {
        let combo = desktop_key(key);
        let keys = crate::chord::parse(&combo);
        if let (Some(point), Some(keys)) = (self.last_point, keys.as_ref()) {
            return self.press_with_cua(keys, Some(point));
        }
        let app = self.app()?.to_string();
        match self.call(&args(&["press", &combo, "--app", &app])) {
            Ok(data) => Ok(data.to_string()),
            Err(error)
                if error.contains("no verified focused element") && self.window().is_ok() =>
            {
                match keys {
                    Some(keys) => self.press_with_cua(&keys, None),
                    None => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }

    fn press_with_cua(
        &mut self,
        keys: &[String],
        point: Option<(f64, f64)>,
    ) -> Result<String, String> {
        let mut arguments = serde_json::json!({"delivery_mode": "foreground"});
        let tool = match keys {
            [key] => {
                arguments["key"] = key.as_str().into();
                "press_key"
            }
            _ => {
                arguments["keys"] = keys.into();
                "hotkey"
            }
        };
        if let Some((x, y)) = point {
            arguments["x"] = x.into();
            arguments["y"] = y.into();
        }
        self.cua(tool, arguments).map(|answer| answer.to_string())
    }

    /// One ref action: `verb <qualified ref> [-- value]`.
    fn act(&mut self, verb: &str, short: &str, value: Option<&str>) -> Result<String, String> {
        let target = self.qualify(short)?;
        self.last_point = None;
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
                let launched = self.call(&args(&["launch", app, "--activate"]))?;
                self.pid = launched["pid"].as_u64();
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
            ["click", target] if target.trim_start_matches('@').starts_with('o') => {
                self.click_text(target)
            }
            ["click", target] => self.act("click", target, None),
            ["fill", target, value] => self.act("set-value", target, Some(value)),
            ["type", target, value] => self.act("type", target, Some(value)),
            ["select", target, value] => self.act("select", target, Some(value)),
            ["press", key] => self.press(key),
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

/// Spawn one tool and read its JSON answer.
// Spawns the real tools: `Desktop::with_tools` is what tests script.
#[cfg_attr(test, mutants::skip)]
fn run_tool(tool: Tool, argv: &[String]) -> Result<Value, Value> {
    let program = match tool {
        Tool::AgentDesktop => PathBuf::from("agent-desktop"),
        Tool::Cua => cua_driver(),
        Tool::Ocr => ocr_helper().map_err(|message| serde_json::json!({"message": message}))?,
    };
    let output = Command::new(&program).args(argv).output().map_err(
        |e| serde_json::json!({"message": format!("failed to spawn {}: {e}", program.display())}),
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    match (tool, serde_json::from_str::<Value>(stdout.trim())) {
        (Tool::AgentDesktop, Ok(envelope)) if envelope["ok"] == true => {
            Ok(envelope["data"].clone())
        }
        (Tool::AgentDesktop, Ok(envelope)) => Err(envelope["error"].clone()),
        (_, Ok(answer)) if output.status.success() => Ok(answer),
        (_, Ok(error)) => Err(error),
        (_, Err(_)) => Err(serde_json::json!({
            "message": format!(
                "exited with {}: {}{}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim(),
                stdout.trim()
            )
        })),
    }
}

/// `cua-driver` on PATH, else where its installer puts it.
fn cua_driver() -> PathBuf {
    let installed = std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".local/bin/cua-driver"))
        .filter(|path| path.exists());
    installed.unwrap_or_else(|| PathBuf::from("cua-driver"))
}

/// The Vision OCR helper, compiled from its bundled source with `swiftc` on
/// first use and cached under `~/Library/Caches/pixel-flow`.
#[cfg_attr(test, mutants::skip)]
fn ocr_helper() -> Result<PathBuf, String> {
    use std::hash::{Hash, Hasher};
    const SOURCE: &str = include_str!("desktop/ocr.swift");
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    SOURCE.hash(&mut hasher);
    let dir = std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Library/Caches/pixel-flow"))
        .ok_or("HOME is not set")?;
    let binary = dir.join(format!("ocr-{:016x}", hasher.finish()));
    if binary.exists() {
        return Ok(binary);
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let source = dir.join("ocr.swift");
    std::fs::write(&source, SOURCE).map_err(|e| format!("{}: {e}", source.display()))?;
    let partial = binary.with_extension("partial");
    let built = Command::new("swiftc")
        .arg("-O")
        .arg(&source)
        .arg("-o")
        .arg(&partial)
        .output()
        .map_err(|e| format!("the OCR helper needs swiftc (Xcode command line tools): {e}"))?;
    if !built.status.success() {
        return Err(format!(
            "swiftc could not build the OCR helper: {}",
            String::from_utf8_lossy(&built.stderr).trim()
        ));
    }
    std::fs::rename(&partial, &binary).map_err(|e| format!("{}: {e}", binary.display()))?;
    Ok(binary)
}

/// Every name the accessibility tree already shows, one entry per line of a
/// multi-line name, in the form [`same_text`] compares.
fn ax_names(tree: &Value) -> HashSet<String> {
    fn walk(node: &Value, out: &mut HashSet<String>) {
        for field in ["name", "value"] {
            for line in node[field].as_str().unwrap_or_default().lines() {
                out.insert(same_text(line));
            }
        }
        for child in node["children"].as_array().into_iter().flatten() {
            walk(child, out);
        }
    }
    let mut out = HashSet::new();
    walk(tree, &mut out);
    out
}

/// Text as a comparison key: lowercase, no whitespace.
fn same_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

/// One recognised line, cleaned.
struct OcrLine {
    text: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// OCR output: drop the icon glyphs Vision reads as a leading letter or
/// bullet (`S Reload Script Commands`, `- Type /resume`), and lines with
/// fewer than two letters or digits (`+`, `...`).
fn clean_lines(lines: &Value) -> Vec<OcrLine> {
    lines
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|line| {
            let raw = line["text"].as_str()?.trim();
            let raw = raw.trim_start_matches(|c: char| {
                !(c.is_alphanumeric() || matches!(c, '~' | '/' | '(' | '.'))
            });
            let text = match raw.split_once(' ') {
                Some((first, rest)) if first.chars().count() == 1 && !rest.trim().is_empty() => {
                    rest.trim()
                }
                _ => raw,
            };
            let text = one_line(text);
            (text.chars().filter(|c| c.is_alphanumeric()).count() >= 2).then(|| OcrLine {
                text,
                x: line["x"].as_f64().unwrap_or_default(),
                y: line["y"].as_f64().unwrap_or_default(),
                w: line["w"].as_f64().unwrap_or_default(),
                h: line["h"].as_f64().unwrap_or_default(),
            })
        })
        .collect()
}

/// The OCR lines the tree does not name, as clickable `text` slots with
/// `oN` refs, and each one's centre. A line with text to its left on the
/// same row is named after that row's first text (`Add Hotkey (row: Type
/// /resume)`), so identical labels of different rows stay distinct and a
/// recorded step finds its own row again.
fn ocr_slots(lines: &Value, ax: &HashSet<String>) -> (String, HashMap<String, (f64, f64)>) {
    let lines = clean_lines(lines);
    let centre = |line: &OcrLine| line.y + line.h / 2.0;
    let mut out = String::new();
    let mut points = HashMap::new();
    let mut n = 0usize;
    for line in &lines {
        if ax.contains(&same_text(&line.text)) {
            continue;
        }
        let row_start = lines
            .iter()
            .filter(|other| {
                other.x + other.w <= line.x + 1.0
                    && (centre(other) - centre(line)).abs() < 0.35 * other.h.min(line.h)
            })
            .min_by(|a, b| a.x.total_cmp(&b.x));
        let name = match row_start {
            Some(start) => format!("{} (row: {})", line.text, start.text),
            None => line.text.clone(),
        };
        n += 1;
        out.push_str(&format!("- text \"{name}\" [clickable, ref=o{n}]\n"));
        points.insert(format!("o{n}"), (line.x + line.w / 2.0, centre(line)));
    }
    (out, points)
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
        let desktop = Desktop::with_tools(Box::new(move |tool: Tool, argv: &[String]| {
            // agent-desktop calls are recorded as its arguments; the others
            // carry their tool in front.
            let mut call = match tool {
                Tool::AgentDesktop => vec![],
                Tool::Cua => vec!["cua".to_string()],
                Tool::Ocr => vec!["ocr".to_string()],
            };
            call.extend(argv.iter().cloned());
            seen.borrow_mut().push(call);
            queue.pop_front().unwrap_or(Ok(Value::Null))
        }));
        (desktop, calls)
    }

    /// Real Vision output for Raycast's Settings window, Script Commands
    /// page (trimmed to the command table and a few sidebar lines).
    const RAYCAST_OCR: &str = include_str!("testdata/raycast-settings-ocr.json");

    fn raycast_ocr() -> Value {
        serde_json::from_str(RAYCAST_OCR).unwrap()
    }

    #[test]
    fn ocr_lines_the_tree_does_not_name_become_row_named_text_targets() {
        let ax = ax_names(&serde_json::json!({"role": "window", "children": [
            {"role": "link", "name": "Script Commands", "ref_id": "@s:e1"},
            {"role": "statictext", "name": "Commands"},
            {"role": "button", "name": "⌨️ Type /resume\nAlias for Type /resume\nAdd Hotkey for Type /resume", "ref_id": "@s:e2"},
            {"role": "button", "name": "Create Script Command", "ref_id": "@s:e3"},
        ]}));
        let (text, points) = ocr_slots(&raycast_ocr(), &ax);
        let lines: Vec<&str> = text.lines().collect();
        // Named in the tree: not repeated.
        assert!(!text.contains("\"Script Commands\""), "{text}");
        assert!(!text.contains("\"Commands\""), "{text}");
        // Icon glyphs and punctuation-only lines are dropped.
        assert!(!text.contains("\"M\""), "{text}");
        assert!(!text.contains("..."), "{text}");
        assert!(
            text.contains("- text \"Reload Script Commands\" [clickable, ref=o"),
            "{text}"
        );
        // One label per row, told apart by the row's first text.
        let hotkey = "- text \"Add Hotkey (row: Type /resume)\" [clickable, ref=o";
        let line = lines
            .iter()
            .find(|line| line.starts_with(hotkey))
            .expect(&text);
        assert_eq!(
            text.matches("Add Hotkey (row: ").count(),
            4,
            "every row's own Add Hotkey: {text}"
        );
        assert!(
            text.contains("\"Add Hotkey (row: Create Script Command)\""),
            "{text}"
        );
        // A sidebar line beside the table is not that row's start.
        assert!(text.contains("\"Type/model\" [clickable"), "{text}");
        assert!(text.contains("\"Pro (row: Translator)\""), "{text}");
        // The point is the line's centre, in screenshot pixels.
        let reference = &line[line.rfind("ref=").unwrap() + 4..line.len() - 1];
        let (x, y) = points[reference];
        assert!(
            (x - (1081.8 + 126.5 / 2.0)).abs() < 0.01 && (y - (1266.0 + 22.3 / 2.0)).abs() < 0.01
        );
        // The slot parses as an actionable element the executor can match.
        assert_eq!(points.len(), lines.len());
    }

    #[test]
    fn a_text_target_is_a_foreground_pixel_click_and_the_chord_follows_it() {
        let snapshot = serde_json::json!({
            "snapshot_id": "s9", "window": {"id": "w-28371"},
            "tree": {"role": "window", "name": "Settings", "children": [
                {"role": "button", "name": "Row", "ref_id": "@s9:e1"}]},
        });
        let (mut d, calls) = scripted(vec![
            Ok(serde_json::json!({"pid": 81444})),
            Ok(snapshot),
            Ok(Value::Null),
            Ok(serde_json::json!({})),
            Ok(raycast_ocr()),
        ]);
        d.run(&["open", "app://Raycast"]).unwrap();
        let text = d.run(&["snapshot", "-i"]).unwrap();
        let line = text
            .lines()
            .find(|line| line.contains("Add Hotkey (row: Type /resume)"))
            .unwrap();
        let reference = &line[line.rfind("ref=").unwrap() + 4..line.len() - 1];
        d.run(&["click", &format!("@{reference}")]).unwrap();
        d.run(&["press", "Control+Alt+Shift+Meta+r"]).unwrap();
        let calls = calls.borrow();
        assert_eq!(calls[2][..3], ["cua", "call", "start_session"]);
        assert_eq!(calls[3][..3], ["cua", "call", "get_window_state"]);
        assert_eq!(calls[4][0], "ocr");
        let click: Value = serde_json::from_str(&calls[5][3]).unwrap();
        assert_eq!(calls[5][2], "click");
        assert_eq!(click["delivery_mode"], "foreground");
        assert_eq!(
            (click["pid"].as_u64(), click["window_id"].as_u64()),
            (Some(81444), Some(28371))
        );
        assert_eq!(click["session"], CUA_SESSION);
        let chord: Value = serde_json::from_str(&calls[6][3]).unwrap();
        assert_eq!(calls[6][2], "hotkey");
        assert_eq!(
            chord["keys"],
            serde_json::json!(["ctrl", "option", "shift", "cmd", "r"])
        );
        assert_eq!(
            (chord["x"].clone(), chord["y"].clone()),
            (click["x"].clone(), click["y"].clone())
        );
        assert_eq!(chord["delivery_mode"], "foreground");
        assert_eq!(calls.len(), 7);
    }

    #[test]
    fn a_chord_without_a_pixel_target_falls_back_to_cua_only_when_unfocused() {
        let snapshot = serde_json::json!({"snapshot_id": "s9", "window": {"id": "w-7"},
            "tree": {"role": "window", "children": []}});
        let unfocused = serde_json::json!({"code": "ACTION_FAILED",
            "message": "Application has no verified focused element for keyboard delivery"});
        let (mut d, calls) = scripted(vec![
            Ok(serde_json::json!({"pid": 5})),
            Ok(snapshot),
            Ok(Value::Null),
            Err(serde_json::json!({"message": "no screen recording"})),
            Ok(Value::Null),
            Err(unfocused),
            Ok(Value::Null),
        ]);
        d.run(&["open", "app://Raycast"]).unwrap();
        let text = d.run(&["snapshot", "-i"]).unwrap();
        assert!(
            text.contains("# ocr unavailable: cua-driver get_window_state"),
            "{text}"
        );
        d.run(&["press", "Escape"]).unwrap();
        d.run(&["press", "Meta+k"]).unwrap();
        let calls = calls.borrow();
        assert_eq!(calls[4], args(&["press", "escape", "--app", "Raycast"]));
        assert_eq!(calls[5], args(&["press", "cmd+k", "--app", "Raycast"]));
        assert_eq!(calls[6][2], "hotkey");
        let chord: Value = serde_json::from_str(&calls[6][3]).unwrap();
        assert_eq!(chord["keys"], serde_json::json!(["cmd", "k"]));
        assert!(chord.get("x").is_none(), "no point: the window gets it");
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
