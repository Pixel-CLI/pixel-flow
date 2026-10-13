// SPDX-FileCopyrightText: The Pixel contributors
// SPDX-License-Identifier: MIT

//! `print_data`: what a command prints under `--json`, and the byte cap.
//!
//! Ported from `crates/pixel/src/main.rs` of Pixel-CLI/pixel (MIT): `Rendered`,
//! `render_data`, `truncate_structurally` and their helpers are private to
//! the `pixel` binary crate, and the tests are core's `render_data_tests.rs`.
//! Unlike core, nothing here feeds the invocation-metrics counters: a plugin
//! is a separate process and the action log belongs to the core binary.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

/// Global stdout byte cap for [`print_data`]: the last-chance safety net before
/// output reaches an agent's context window. `PIXEL_OUTPUT_CAP_BYTES`
/// overrides it (see [`stdout_byte_cap`]).
const STDOUT_BYTE_CAP: usize = 256 * 1024;

/// Bytes [`write_stdout`] has written: lets [`crate::run`] decide whether a
/// `--json` failure may still print its envelope (it may not once a partial
/// answer is out).
static STDOUT_BYTES: AtomicU64 = AtomicU64::new(0);

/// Effective stdout cap: `PIXEL_OUTPUT_CAP_BYTES=<bytes>` overrides the
/// default and `0` lifts the cap entirely. Unset, empty or unparsable values
/// keep the default so a typo can never silently disable the safety net.
fn stdout_byte_cap() -> usize {
    parse_output_cap(std::env::var("PIXEL_OUTPUT_CAP_BYTES").ok().as_deref())
}

fn parse_output_cap(raw: Option<&str>) -> usize {
    match raw.map(|v| v.trim().parse::<usize>()) {
        Some(Ok(0)) => usize::MAX,
        Some(Ok(n)) => n,
        _ => STDOUT_BYTE_CAP,
    }
}

/// Bytes written through [`write_stdout`] so far.
pub fn stdout_bytes() -> u64 {
    STDOUT_BYTES.load(Ordering::Relaxed)
}

/// Write `text` whole; a reader that closed its end is not a failure.
pub fn write_stdout(text: &str) -> Result<(), String> {
    STDOUT_BYTES.fetch_add(text.len() as u64, Ordering::Relaxed);
    match std::io::stdout().lock().write_all(text.as_bytes()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(format!("write stdout: {error}")),
    }
}

/// Print `data` as core's `print_data` does: compact JSON under `--json`,
/// pretty otherwise, both under the byte cap.
pub fn print_data(data: &Value, raw_json: bool) -> Result<(), String> {
    write_stdout(&render_data(data, raw_json, stdout_byte_cap()).text)
}

/// Output of [`render_data`]: the bytes for stdout plus whether the cap fired.
pub struct Rendered {
    pub text: String,
    pub truncated: bool,
}

/// One array shortened by [`truncate_structurally`]: dotted path from the
/// document root (`snapshot.dirty`, `matches[3].lines`), elements kept, and
/// the original length.
#[derive(Debug)]
struct TruncatedArray {
    path: String,
    kept: usize,
    total: usize,
}

/// Bytes reserved for the metadata [`truncate_structurally`] splices into the
/// top-level object (`truncated`, `cap_bytes`, `truncated_arrays`).
const TRUNCATION_META_RESERVE: usize = 256;

/// Most structural-truncation rounds before giving up: each round shortens
/// the (then) largest array, so a handful of rounds covers every realistic
/// response shape without letting a pathological document spin.
const TRUNCATION_MAX_ROUNDS: usize = 8;

fn serialized_len(v: &Value) -> usize {
    serde_json::to_vec(v).map_or(0, |b| b.len())
}

/// Locate the non-empty array under `v` whose tail is worth cutting most:
/// ranked by REMOVABLE bytes, `bytes - bytes / len` (everything past a
/// mean-sized first element), not raw size. Raw size would always pick an
/// enclosing array over the long list nested inside it (the parent is never
/// smaller than its child) and drop whole sibling records instead of
/// trimming the list. Returns `(path, len, bytes)`.
fn largest_array(v: &Value, path: &str) -> Option<(String, usize, usize)> {
    let mut best: Option<(String, usize, usize)> = None;
    let removable = |c: &(String, usize, usize)| c.2 - c.2 / c.1;
    let mut consider = |cand: Option<(String, usize, usize)>| {
        if let Some(c) = cand
            && best.as_ref().is_none_or(|b| removable(&c) > removable(b))
        {
            best = Some(c);
        }
    };
    match v {
        Value::Array(items) => {
            if !items.is_empty() {
                consider(Some((path.to_string(), items.len(), serialized_len(v))));
            }
            for (i, item) in items.iter().enumerate() {
                consider(largest_array(item, &format!("{path}[{i}]")));
            }
        }
        Value::Object(map) => {
            for (k, item) in map {
                let child = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                consider(largest_array(item, &child));
            }
        }
        _ => {}
    }
    best
}

/// Walk `v` to the array at the dotted `path` produced by [`largest_array`].
fn array_at_mut<'a>(v: &'a mut Value, path: &str) -> Option<&'a mut Vec<Value>> {
    let mut cur = v;
    if !path.is_empty() {
        for seg in path.split('.') {
            let (key, indexes) = match seg.find('[') {
                Some(i) => (&seg[..i], &seg[i..]),
                None => (seg, ""),
            };
            if !key.is_empty() {
                cur = cur.get_mut(key)?;
            }
            for idx in indexes
                .trim_start_matches('[')
                .trim_end_matches(']')
                .split("][")
                .filter(|s| !s.is_empty())
            {
                cur = cur.get_mut(idx.parse::<usize>().ok()?)?;
            }
        }
    }
    cur.as_array_mut()
}

/// Shorten the largest arrays in `data` until its compact serialization fits
/// `cap`, keeping every other field and the document structure intact, then
/// stamp the top-level object with `truncated: true`, `cap_bytes` and a
/// `truncated_arrays` list naming each cut (path, kept, total).
///
/// Returns `None` when `data` is not an object or no amount of array
/// trimming gets it under the cap (for example when the bulk is one huge
/// string), in which case the caller falls back to the textual wrapper.
fn truncate_structurally(data: &Value, cap: usize) -> Option<Value> {
    if !data.is_object() {
        return None;
    }
    let mut doc = data.clone();
    let mut cuts: Vec<TruncatedArray> = Vec::new();
    let budget = cap.saturating_sub(TRUNCATION_META_RESERVE);
    for _ in 0..TRUNCATION_MAX_ROUNDS {
        let size = serialized_len(&doc);
        if size <= budget {
            break;
        }
        let (path, len, bytes) = largest_array(&doc, "")?;
        // Elements that fit: the budget left after everything outside this
        // array, divided by the mean element size — always strictly fewer
        // than `len` so every round makes progress.
        let outside = size.saturating_sub(bytes);
        let per_elem = (bytes / len).max(1);
        let keep = budget
            .saturating_sub(outside)
            .checked_div(per_elem)
            .unwrap_or(0)
            .min(len - 1);
        let arr = array_at_mut(&mut doc, &path)?;
        arr.truncate(keep);
        match cuts.iter_mut().find(|c| c.path == path) {
            Some(c) => c.kept = keep,
            None => cuts.push(TruncatedArray {
                path,
                kept: keep,
                total: len,
            }),
        }
    }
    if serialized_len(&doc) > budget {
        return None;
    }
    let meta: Vec<Value> = cuts
        .iter()
        .map(|c| json!({"path": c.path, "kept": c.kept, "total": c.total}))
        .collect();
    let obj = doc.as_object_mut()?;
    obj.insert("truncated".into(), json!(true));
    obj.insert("cap_bytes".into(), json!(cap));
    obj.insert("truncated_arrays".into(), Value::Array(meta));
    (serialized_len(&doc) <= cap).then_some(doc)
}

/// Serialize `data` for stdout under a byte cap.
///
/// Human mode (`raw_json == false`) pretty-prints and, when over the cap,
/// cuts the text on a char boundary and appends a visible truncation note.
///
/// JSON mode (`raw_json == true`) must never emit anything that is not one
/// JSON document: a caller doing `serde_json::from_slice(stdout)` cannot
/// recover from a cut-off object followed by prose. When the compact
/// serialization exceeds the cap, the document is first truncated
/// STRUCTURALLY ([`truncate_structurally`]): the largest arrays are
/// shortened, every other field survives, and the object gains
/// `truncated: true`, `cap_bytes` and `truncated_arrays`, so `jq '.index'`
/// keeps working on a capped response. Only when no array trimming can fit
/// the cap does the output fall back to the small wrapper object
/// `{truncated: true, cap_bytes, note, partial}` where `partial` is the
/// leading bytes of the original serialization as a string. The wrapper
/// itself can exceed the cap by the size of the note and JSON escaping;
/// that is bounded and preferable to invalid output.
pub fn render_data(data: &Value, raw_json: bool, cap: usize) -> Rendered {
    let mut output = if raw_json {
        serde_json::to_string(data).unwrap_or_default()
    } else {
        serde_json::to_string_pretty(data).unwrap_or_default()
    };
    let truncated = output.len() > cap;
    if truncated
        && raw_json
        && let Some(doc) = truncate_structurally(data, cap)
    {
        output = serde_json::to_string(&doc).unwrap_or_default();
    } else if truncated {
        let mut end = cap;
        while end > 0 && !output.is_char_boundary(end) {
            end -= 1;
        }
        let note = format!(
            "OUTPUT TRUNCATED AT {cap} BYTES (global safety cap). The full \
             response was larger — re-run with a narrower scope, --limit, or \
             --offset to page through results. Remove .pixel/calls.json if \
             the circuit breaker fires."
        );
        output.truncate(end);
        if raw_json {
            output = serde_json::to_string(&json!({
                "truncated": true,
                "cap_bytes": cap,
                "note": note,
                "partial": output,
            }))
            .unwrap_or_default();
        } else {
            output.push_str("\n\n⚠ ");
            output.push_str(&note);
        }
    }
    output.push('\n');
    Rendered {
        text: output,
        truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big() -> Value {
        json!({"matches": (0..200).map(|i| json!({"path": format!("src/file_{i}.rs"), "line": i, "text": "é".repeat(20)})).collect::<Vec<_>>()})
    }

    #[test]
    fn under_cap_is_untouched() {
        let d = json!({"a": 1});
        let r = render_data(&d, true, 1024);
        assert_eq!(r.text, "{\"a\":1}\n");
        assert!(!r.truncated);
        assert_eq!(
            serde_json::from_str::<Value>(&render_data(&d, false, 1024).text).unwrap(),
            d
        );
    }

    /// The reason this matters: agents call `pixel … --json` and parse
    /// stdout, then `jq '.index'` / `.graph` on the result. A response that
    /// is over the cap only because ONE list is long (a `snapshot.dirty`
    /// full of untracked `vendor/bundle` paths) must keep its shape: the
    /// scalar fields stay addressable, only the list is shortened, and the
    /// document says which list was cut and how much survived.
    #[test]
    fn json_mode_truncation_shortens_the_largest_array_and_keeps_structure() {
        let d = json!({
            "index": {"base_files": 238, "commit_oid": "abc"},
            "snapshot": {"head": "abc", "branch": "main",
                          "dirty": (0..500).map(|i| format!("vendor/bundle/gems/g{i}/lib/x.rb")).collect::<Vec<_>>()},
        });
        let full = serde_json::to_string(&d).unwrap().len();
        let cap = full / 3;
        let r = render_data(&d, true, cap);
        assert!(r.truncated);
        assert!(r.text.len() <= cap + 1, "{} > cap {cap}", r.text.len());
        let v: Value = serde_json::from_str(&r.text).expect("stdout must remain one JSON document");
        assert_eq!(v["truncated"], true);
        assert_eq!(v["cap_bytes"], cap);
        assert_eq!(
            v["index"]["base_files"], 238,
            "untouched fields must survive"
        );
        assert_eq!(v["snapshot"]["branch"], "main");
        let dirty = v["snapshot"]["dirty"].as_array().unwrap();
        assert!(
            !dirty.is_empty() && dirty.len() < 500,
            "kept {}",
            dirty.len()
        );
        assert_eq!(
            dirty[0], "vendor/bundle/gems/g0/lib/x.rb",
            "prefix, not a sample"
        );
        let cuts = v["truncated_arrays"].as_array().unwrap();
        assert_eq!(cuts.len(), 1);
        assert_eq!(cuts[0]["path"], "snapshot.dirty");
        assert_eq!(cuts[0]["total"], 500);
        assert_eq!(cuts[0]["kept"], dirty.len());
        assert!(
            v.get("partial").is_none(),
            "no textual wrapper when structure fits"
        );
        assert_eq!(r.text.matches('\n').count(), 1, "single NDJSON-safe line");
    }

    /// Nested arrays: the cut lands on the array that actually carries the
    /// bytes, not blindly on the top-level one, and the path names it.
    #[test]
    fn structural_truncation_targets_nested_array_by_size() {
        let d = json!({"groups": [
            {"name": "small", "items": ["a", "b"]},
            {"name": "huge", "items": (0..2000).map(|i| format!("item-{i:05}")).collect::<Vec<_>>()},
        ]});
        let r = render_data(&d, true, 2000);
        let v: Value = serde_json::from_str(&r.text).unwrap();
        assert_eq!(
            v["groups"].as_array().unwrap().len(),
            2,
            "outer array intact"
        );
        assert_eq!(v["groups"][0]["items"].as_array().unwrap().len(), 2);
        assert!(v["groups"][1]["items"].as_array().unwrap().len() < 2000);
        assert_eq!(v["truncated_arrays"][0]["path"], "groups[1].items");
        assert!(r.text.len() <= 2001);
    }

    /// When the bulk is not an array (one huge string) structural trimming
    /// cannot help; the textual wrapper must still be one valid document
    /// that says it was cut.
    #[test]
    fn json_mode_falls_back_to_wrapper_when_no_array_can_be_cut() {
        let d = json!({"blob": "x".repeat(5000)});
        let r = render_data(&d, true, 500);
        assert!(r.truncated);
        let v: Value = serde_json::from_str(&r.text).expect("stdout must remain one JSON document");
        assert_eq!(v["truncated"], true);
        assert_eq!(v["cap_bytes"], 500);
        let partial = v["partial"].as_str().unwrap();
        assert!(partial.len() <= 500);
        assert!(partial.starts_with("{\"blob\":\""));
        assert!(v["note"].as_str().unwrap().contains("TRUNCATED"));
        assert_eq!(r.text.matches('\n').count(), 1, "single NDJSON-safe line");
    }

    #[test]
    fn json_mode_array_of_objects_is_cut_structurally() {
        let r = render_data(&big(), true, 500);
        let v: Value = serde_json::from_str(&r.text).unwrap();
        assert_eq!(v["truncated"], true);
        assert!(v["matches"].is_array());
        assert_eq!(v["truncated_arrays"][0]["path"], "matches");
        assert_eq!(v["truncated_arrays"][0]["total"], 200);
    }

    #[test]
    fn human_mode_truncation_keeps_visible_note() {
        let r = render_data(&big(), false, 500);
        assert!(r.truncated);
        assert!(r.text.contains("⚠ OUTPUT TRUNCATED AT 500 BYTES"));
        assert!(serde_json::from_str::<Value>(&r.text).is_err());
    }

    /// Multi-byte text near the cap: the cut must land on a char boundary
    /// so the partial string is valid UTF-8 and serializable.
    #[test]
    fn truncation_respects_char_boundaries() {
        let d = json!({"t": "é".repeat(1000)});
        for cap in 100..140 {
            let out = render_data(&d, true, cap).text;
            let v: Value = serde_json::from_str(&out).unwrap();
            assert!(v["partial"].as_str().unwrap().len() <= cap);
        }
    }

    /// `array_at_mut` must round-trip every path shape `largest_array`
    /// emits, otherwise a cut silently targets nothing.
    #[test]
    fn array_path_round_trip() {
        let mut d = json!({"a": {"b": [[1, 2, 3], {"c": [4, 5]}]}, "d": [6]});
        let (path, len, _) = largest_array(&d, "").unwrap();
        assert_eq!(
            path, "a.b",
            "21 bytes / 2 elems: 11 removable, beats a.b[0]'s 5"
        );
        assert_eq!(len, 2);
        assert_eq!(array_at_mut(&mut d, "a.b[0]").unwrap().len(), 3);
        assert_eq!(array_at_mut(&mut d, "a.b[1].c").unwrap().len(), 2);
        assert_eq!(array_at_mut(&mut d, "d").unwrap().len(), 1);
        assert!(array_at_mut(&mut d, "a.b[5]").is_none());
    }

    /// `PIXEL_OUTPUT_CAP_BYTES=0` is the documented escape hatch for a
    /// consumer that wants the whole document; anything unparsable must
    /// keep the safety net rather than silently disabling it.
    #[test]
    fn output_cap_env_parsing() {
        assert_eq!(parse_output_cap(None), STDOUT_BYTE_CAP);
        assert_eq!(parse_output_cap(Some("0")), usize::MAX);
        assert_eq!(parse_output_cap(Some(" 4096 ")), 4096);
        assert_eq!(parse_output_cap(Some("lots")), STDOUT_BYTE_CAP);
        assert_eq!(parse_output_cap(Some("")), STDOUT_BYTE_CAP);
    }
}
