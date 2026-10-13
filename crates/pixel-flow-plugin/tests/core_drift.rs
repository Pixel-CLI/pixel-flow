// SPDX-FileCopyrightText: The Pixel contributors
// SPDX-License-Identifier: MIT

//! What this repository restates from Pixel core must still agree with the
//! pinned commit. A pin bump that moves any of it fails here, not in a user's
//! shell.

use pixel_flow_plugin::run::failure_line;
use serde_json::Value;

#[test]
fn the_failure_envelope_is_the_one_core_prints() {
    for message in [
        "bad path /nowhere: No such file or directory",
        "REFUSED: something",
        "cannot read flow 'x': No such file or directory (os error 2)",
        "no symbol named foo",
        "repository is busy: lock held",
    ] {
        let ours: Value = serde_json::from_str(failure_line("op", message).trim_end()).unwrap();
        let theirs =
            serde_json::to_value(pixel_daemon::api::failure_response("op", message)).unwrap();
        assert_eq!(ours, theirs, "{message}");
    }
}
