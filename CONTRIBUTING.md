# Contributing

The `flow` plugin of [Pixel](https://github.com/Pixel-CLI/pixel). Read the
[README](README.md) first.

## Before a pull request

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
scripts/check-pin.sh
```

CI runs the same on Ubuntu and macOS. Every source file opens with
`SPDX-FileCopyrightText: The Pixel contributors` and
`SPDX-License-Identifier: MIT`.

## A command keeps the built-in's behaviour

A command reproduces the one it replaced: same arguments, same output, same
exit status. `crates/pixel-flow-plugin/tests/parity_*.rs` run `pixel-flow` in a
temporary directory and compare the result with a recorded golden in
`tests/golden/`. Goldens are recorded from the **built-in** command, never
from this plugin. Directories become `ROOT`/`EXTRA`; ids and timestamps are
scrubbed. The one known difference, the `--metrics` option in `--help`, is
dropped from both sides.

- `ultraflow-*.txt` were recorded from a core built at `ad788194`, the pinned
  commit (`cargo build -p pixel-cli` in a checkout of it).
- `list-*`, `save`, `show`, ... (`flow`) were recorded from a core built at
  `8a0f30e1`, the last commit with the built-in `pixel flow`. They are never
  compared with a live core: `flow` left it in #899.

Re-record `ultraflow` after a change, or after moving the pin:

```bash
PIXEL_CORE_BIN=/path/to/pixel UPDATE_GOLDEN=1 cargo test --workspace
git diff crates/pixel-flow-plugin/tests/golden   # every change is a behaviour change: explain it
```

Running the tests with `PIXEL_CORE_BIN` set and without `UPDATE_GOLDEN` also
compares the plugin with that live built-in, runs `ultraflow` against the real
`pixel classify --jsonl` (`tests/core_classify.rs`, with a local stand-in for
the model server) and checks the engine inputs `decider.rs` restates. Without
it a stand-in script plays core, so CI needs no core build.

A deliberate difference from the built-in has no golden: test it directly and
list it under "Differences from the built-ins" in the README.

## Moving the Pixel pin

The pinned commit lives only in `[workspace.dependencies]` of `Cargo.toml`.
Change `rev` on every entry together, run `cargo update`, then
`cargo test --workspace`. `tests/core_drift.rs` fails when something this
repository restates (the failure-envelope classifier) no longer matches core.

## Adding a command

1. A `Parser` struct and an entry function in `src/commands/`, with
   `#[command(name = "<name>", bin_name = "pixel <name>")]` and
   `#[command(flatten)] _host: HostFlags`. Run it through `run::run` and print
   with `output::print_data`.
2. One row in `COMMANDS` (`src/commands/mod.rs`) and one name in the `commands`
   list of `pixel-plugin.toml`; `tests/manifest.rs` compares the two. No `flow`
   subcommand may be named like a command (`tests/cli.rs`).
3. `tests/parity_*.rs` cases and their goldens.

## Releasing

Bump `version` in `Cargo.toml` (`[workspace.package]`) and `pixel-plugin.toml`
together, merge to `main`, then tag `vX.Y.Z` on `main`. `release.yml` checks
the tag against both files, creates the release and uploads
`pixel-flow-<target-triple>` with its `.sha256` for four targets.
