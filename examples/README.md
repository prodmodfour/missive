# missive command examples

This directory contains runnable, local-only demos for the implemented `missive`
command surfaces. They use a deterministic mock A2A server and temporary
`MISSIVE_HOME` state so they can run from a clean checkout without contacting any
third-party agent. See [`../docs/examples.md`](../docs/examples.md) for the
smoke-test coverage table and links into the user guide.

## Run every demo

```bash
examples/run-smoke.sh
```

The smoke runner starts the mock A2A server once, then executes:

* `examples/demo-agent-registry.sh` — `agent add/list/show/inspect/capabilities`
* `examples/demo-send.sh` — non-streaming `send` plus event inspection
* `examples/demo-stream-tasks.sh` — `stream`, remote `task list/get/wait`, and artifact listing
* `examples/demo-contexts-groups.sh` — `context` lifecycle, `group` membership, capability summary, and `route explain`
* `examples/demo-gateway.sh` — short-lived `gateway run` plus gateway event inspection

Each script can also be run directly.

## Useful overrides

By default the scripts build and run the local mock server from the workspace and
invoke the CLI with `cargo run -p missive-cli --bin missive --`. For faster
iteration, build the binary once and point the demos at it:

```bash
cargo build -p missive-cli --bin missive
MISSIVE_BIN="$PWD/target/debug/missive" examples/run-smoke.sh
```

Other useful environment variables:

```bash
MISSIVE_EXAMPLE_A2A_BASE_URL=http://127.0.0.1:12345  # reuse an existing local mock server
MISSIVE_EXAMPLE_WORKDIR=/tmp/missive-examples        # choose the runtime-state parent directory
MISSIVE_EXAMPLE_KEEP_WORKDIR=1                       # preserve temporary state for inspection
MISSIVE_EXAMPLE_MOCK_BIN=/path/to/mock_a2a_server    # use a prebuilt mock server helper
```

When `MISSIVE_EXAMPLE_A2A_BASE_URL` is unset, the scripts start the helper built
from `crates/missive-test-support/examples/mock_a2a_server.rs`. That helper
serves Agent Card discovery, HTTP+JSON send/stream/task endpoints, JSON-RPC
endpoints, push config routes, and deterministic task/stream fixtures on
`127.0.0.1`.

## Safety

The demos intentionally unset `MISSIVE_CONFIG`/`MISSIVE_REPO_CONFIG` and create a
fresh temporary `MISSIVE_HOME` unless `MISSIVE_EXAMPLE_USE_EXISTING_HOME=1` is
set. Runtime databases, mock logs, and command output stay under the temporary
workdir and must not be committed.
