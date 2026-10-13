// SPDX-FileCopyrightText: The Pixel contributors
// SPDX-License-Identifier: MIT

//! The environment Pixel core gives a plugin (API 1).
//!
//! | Variable | Meaning |
//! | --- | --- |
//! | `PIXEL_API` | plugin API version the host speaks (`1`) |
//! | `PIXEL_REPO_ROOT` | repository root of the caller's working directory (unused here) |
//! | `PIXEL_GRAPH_DB` | the graph store (unused here) |
//! | `PIXEL_BIN` | path of the core `pixel` binary that started the plugin |
//!
//! Every variable is optional. `flow` reads none of them; `ultraflow` needs
//! the core binary to ask `pixel classify` for its decisions.

use std::path::PathBuf;

/// A non-empty environment variable, read through `get` so the rule is
/// testable without touching the process environment.
fn non_empty(get: &dyn Fn(&str) -> Option<String>, name: &str) -> Option<String> {
    get(name).filter(|value| !value.is_empty())
}

/// The core `pixel` binary: the host's `PIXEL_BIN`, else `pixel` from `PATH`.
pub fn pixel_bin() -> PathBuf {
    pixel_bin_from(&|name| std::env::var(name).ok())
}

fn pixel_bin_from(get: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    non_empty(get, "PIXEL_BIN").map_or_else(|| "pixel".into(), PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_binary_wins_and_an_empty_value_is_unset() {
        let with = |value: &'static str| {
            move |name: &str| (name == "PIXEL_BIN").then(|| value.to_string())
        };
        assert_eq!(pixel_bin_from(&with("/x/pixel")), PathBuf::from("/x/pixel"));
        assert_eq!(pixel_bin_from(&with("")), PathBuf::from("pixel"));
        assert_eq!(pixel_bin_from(&|_| None), PathBuf::from("pixel"));
    }
}
