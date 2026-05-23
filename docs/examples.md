# Command examples

The runnable command examples live under [`examples/`](../examples/). They are
local-only smoke demos for implemented command surfaces and use the repository's
deterministic mock A2A server.

Run all examples from a clean checkout with:

```bash
examples/run-smoke.sh
```

For faster repeated runs, build the CLI once and pass its path:

```bash
cargo build -p missive-cli --bin missive
MISSIVE_BIN="$PWD/target/debug/missive" examples/run-smoke.sh
```

Use `MISSIVE_EXAMPLE_KEEP_WORKDIR=1` when you want to inspect the temporary
SQLite database, mock logs, and output files after a run. Keep those runtime
files outside the repository and do not commit them.

## Smoke-tested coverage

`examples/run-smoke.sh` starts one mock A2A server on `127.0.0.1`, creates
temporary `MISSIVE_HOME` state, and executes these scripts:

| Script | Commands covered |
| --- | --- |
| `examples/demo-agent-registry.sh` | `agent add`, `agent list`, `agent show`, `agent inspect --refresh`, `agent capabilities` |
| `examples/demo-send.sh` | `send` with context, metadata, accepted output mode, and `events list` |
| `examples/demo-stream-tasks.sh` | `stream --ndjson`, remote `task list`, remote `task get`, `task wait`, `task artifact list` |
| `examples/demo-contexts-groups.sh` | `context create/show/fork/list/export`, `group create/add/show/capabilities`, `route explain` |
| `examples/demo-gateway.sh` | short-lived `gateway run` and gateway event inspection |

The Rust smoke test `crates/missive-cli/tests/example_smoke.rs` executes the same
entry point with an already-built `missive` binary, so the examples run during
the normal `cargo test --workspace --all-targets --all-features` pass in
`scripts/quality-gate.sh`.

## Useful overrides

```bash
MISSIVE_BIN=/path/to/missive examples/run-smoke.sh
MISSIVE_EXAMPLE_A2A_BASE_URL=http://127.0.0.1:12345 examples/run-smoke.sh
MISSIVE_EXAMPLE_WORKDIR=/tmp/missive-examples examples/run-smoke.sh
MISSIVE_EXAMPLE_KEEP_WORKDIR=1 examples/run-smoke.sh
MISSIVE_EXAMPLE_MOCK_BIN=/path/to/mock_a2a_server examples/run-smoke.sh
```

When `MISSIVE_EXAMPLE_A2A_BASE_URL` is unset, the scripts build and start the
helper at `crates/missive-test-support/examples/mock_a2a_server.rs`. The helper
serves Agent Card discovery, HTTP+JSON send/stream/task endpoints, JSON-RPC
endpoints, push config routes, and deterministic task/stream fixtures on
`127.0.0.1`.

## Manual examples and coverage notes

The user docs also include short command snippets for push/webhooks, adapter
stdio/file-drop, HTTP adapter ingress, background jobs, and collectives. Those
surfaces have Rust unit/integration coverage and are documented honestly, but not
every snippet is part of the top-level smoke runner because some require a
long-running receiver, callback coordination, or caller-provided local files. Run
`missive <command> --help` for the authoritative option set before adapting a
snippet to a real endpoint.

See [`quickstart.md`](quickstart.md) for a guided local session and
[`troubleshooting.md`](troubleshooting.md) for debugging failed demos.
