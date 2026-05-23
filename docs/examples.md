# Command examples

The runnable command examples live under [`examples/`](../examples/). They are
local-only smoke demos for the command surfaces implemented so far and are backed
by the repository's deterministic mock A2A server.

Run all examples from a clean checkout with:

```bash
examples/run-smoke.sh
```

The runner starts a mock A2A server on `127.0.0.1`, creates temporary
`MISSIVE_HOME` state, and executes demos for:

* agent registry and Agent Card discovery
* non-streaming `send`
* `stream` plus task `list/get/wait` and artifact listing
* context lifecycle and group/routing inspection
* short-lived gateway daemon startup/shutdown

For faster repeated runs, build the CLI once and pass its path:

```bash
cargo build -p missive-cli --bin missive
MISSIVE_BIN="$PWD/target/debug/missive" examples/run-smoke.sh
```

Use `MISSIVE_EXAMPLE_KEEP_WORKDIR=1` when you want to inspect the temporary
SQLite database and output files after a run. Keep those runtime files outside
the repository and do not commit them.

The Rust smoke test `crates/missive-cli/tests/example_smoke.rs` executes the same
`examples/run-smoke.sh` entry point with an in-process mock A2A fixture and an
already-built `missive` binary, so the examples are covered by the default
`cargo test --workspace --all-targets --all-features` pass in
`scripts/quality-gate.sh`.
