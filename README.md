# pixel-flow

The `flow` plugin for [Pixel](https://github.com/Pixel-CLI/pixel): `pixel flow`
and `pixel ultraflow` in one `pixel-flow` binary. Both used to be built into
`pixel`; they live here so the core stays small and only the people who drive a
browser with Pixel install them.

| Command | What it does |
| --- | --- |
| `pixel flow` | save, get, list, revise, run, replay, show and delete proven [agent-browser](https://github.com/vercel-labs/agent-browser) flows (auth flows, configuration flows), so an agent follows a deterministic shortcut instead of re-discovering the UI |
| `pixel ultraflow discover` | drive a page toward a goal with `pixel classify` deciding one operation per cycle, then compose what worked into a flow document (`--save <name>`) |
| `pixel ultraflow replay` | follow a saved flow; its `conditional` steps are decided by `pixel classify`, and a step whose page moved on is re-decided once (`--update` records the new branch) |

Each command reproduces the built-in's arguments, output and exit status.
`pixel flow --help` and `pixel ultraflow --help` show the options.

## Install

With a Pixel that has the plugin host (`pixel install <source>`):

```bash
pixel install github:Pixel-CLI/pixel-flow
pixel plugin enable flow       # the plugin declares network access
```

The release assets are `pixel-flow-<target-triple>` executables, each with a
`.sha256` file: `aarch64-apple-darwin`, `x86_64-apple-darwin`,
`x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` (Linux is
musl: the triples `pixel install` asks for).

From source, without Pixel's plugin management:

```bash
cargo install --git https://github.com/Pixel-CLI/pixel-flow pixel-flow-plugin
pixel-flow install-links       # makes `pixel ultraflow` find it
```

Both commands drive `agent-browser` (on `PATH`, session `comet`), and
`ultraflow` also needs the core `pixel` binary: it asks `pixel classify` for
every decision and honours its kill switch (`pixel config classify on`).

## How `pixel <command>` finds this plugin

Plugin API 1 resolves `pixel flow` to a plugin *named* `flow`, or to an
executable `pixel-flow` on `PATH`, so:

- `pixel flow …` works once the plugin is installed: the host runs
  `bin/pixel-flow`, which is, or finds, the `pixel-flow` binary. The binary
  started as `pixel-flow` is the `flow` command.
- `pixel ultraflow …` needs the `pixel-ultraflow` link:
  `pixel-flow install-links` creates it next to the binary (or in
  `--dir <dir>`), and `--remove` deletes the links it made. It never replaces a
  file it did not make and never touches the binary itself.
- `pixel-flow ultraflow …` always works, links or not; so does
  `pixel-flow flow …`.
- [`pixel-plugin.toml`](pixel-plugin.toml) lists both names under `commands`.
  API 1 ignores that key. A host that reads it could route both names to
  `pixel-flow <name>` with no links at all, and `tests/manifest.rs` keeps the
  list equal to what the binary has.

## Environment

The host passes `PIXEL_API`, `PIXEL_REPO_ROOT`, `PIXEL_GRAPH_DB` and
`PIXEL_BIN`; all are optional and only `PIXEL_BIN` is read: it names the core
binary `ultraflow` asks for decisions, and defaults to `pixel` on `PATH`.
`PIXEL_FLOW_DIR` moves the flow store as it did in the built-in.

## Differences from the built-ins

- **No metrics line, no action-log entry.** A plugin is a separate process and
  the invocation accounting belongs to the `pixel` binary. `--metrics on|off`
  is still accepted (hidden), so scripts that pass it keep parsing; `--help`
  does not list it and a usage line omits `[OPTIONS]` where `--metrics` was
  the only option.
- **`ultraflow` decides through the core binary.** The built-in opened the
  classify engine inside its own process; a plugin cannot link that code. It
  starts one `pixel classify --jsonl` for the run (the serve mode core ships
  for this: one resident engine, one question per line) and asks it every
  question, so engine resolution and the connection are paid once, as before.
  Validation, input caps, the kill switch and the engine choice stay in core,
  and a refused question carries core's own error text. That `pixel classify`
  writes its own action-log line. When `--engine` is not given, the first
  question asks core once which provider answers (a two-label ping) because
  the provider sets how many options a question may offer; `--engine remote`
  and `--engine ollaya` skip that call.
- **No `flow` alias.** The built-in `pixel flow` is gone from core; the plugin
  answers to `flow` and `ultraflow` only.

## Where the code comes from

Library crates of Pixel core (`pixel-flow`, `pixel-ultraflow`, `pixel-proto`,
and `pixel-daemon` for tests) come through a git dependency pinned to one
commit of [Pixel-CLI/pixel](https://github.com/Pixel-CLI/pixel), recorded once
in `[workspace.dependencies]` of [`Cargo.toml`](Cargo.toml)
(`scripts/check-pin.sh` enforces a single value). Logic that lived only in the
`pixel` binary crate is ported here, MIT, with its origin in each module's doc:

| Here | From |
| --- | --- |
| `commands/flow.rs` | the `flow` command line, `flow_vars` and the result printing of `crates/pixel/src/main.rs` at `8a0f30e1` |
| `commands/ultraflow.rs` | `crates/pixel/src/ultraflow_cmd.rs` |
| `decider.rs` | replaces `ClassifyDecider` of `ultraflow_cmd.rs`; restates `EngineChoice`, `Preset`, `DEFAULT_BASE`, `MAX_OPTIONS`, `MAX_LABELS` of `classify.rs`, `decide_remote.rs`, `decide_ollaya.rs` (`tests/core_classify.rs` checks them against a core build) |
| `output.rs`, `run.rs` | `print_data` with its byte cap (`PIXEL_OUTPUT_CAP_BYTES`), the `--json` failure envelope |

## Versioning

One version (`[workspace.package]`), equal to the `version` in
`pixel-plugin.toml`; a release is a `vX.Y.Z` tag on `main`, which builds the
assets above. `api = 1` changes only when Pixel changes the plugin API.

## License

MIT, see [LICENSE](LICENSE).
