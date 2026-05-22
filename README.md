# missive

`missive` is an early-stage Rust command-line tool and local control plane for A2A-native agent communication. The long-term goal is to make agent messaging feel like `curl`, agent communication state feel like `kubectl`, and multi-agent coordination feel like MPI-style collective operations.

## Current status

This repository is at early workspace stage. It contains:

* a Cargo workspace with the target crate layout under `crates/`, plus a dev-support crate for local A2A integration fixtures and protocol-versioned A2A conformance fixtures under `tests/fixtures/a2a/1.0/`
* a `missive-cli` package that exposes the binary named `missive`
* a clap-based CLI with stable top-level commands, global flags, configuration discovery, profiles, A2A service-parameter overrides, env/header/keyring auth inputs for implemented requests, human/JSON/NDJSON/quiet output renderers, agent registry commands, A2A Agent Card discovery/cache inspection, official A2A Rust protocol type integration, A2A interface negotiation, rich text/file/byte/JSON message parts for non-streaming `missive send` and streaming `missive stream`, task `get/list/wait/cancel`, task-scoped artifact `list/show/save/export`, context `create/list/show/fork/close/export`, group `create/list/show/add/remove/rename/delete`, push notification config `create/get/list/delete`, local webhook receiver `run`, gateway daemon `run`, gateway service `install/start/stop/status/uninstall`, and event journal `list/tail/replay/export`
* store-layer state path resolution, process locks, SQLite migrations, and typed repository APIs that keep default runtime state outside the source tree
* repository hygiene files and guardrails
* the autonomous ticket queue used to build the project one commit at a time
* starter documentation directories, including `docs/adr/`

Current crates:

```text
crates/missive-cli        command parsing, output rendering, exit codes
crates/missive-core       domain types, errors, config, IDs, envelopes
crates/missive-a2a        A2A protocol/client integration and compatibility fixtures
crates/missive-store      state paths, process locks, SQLite migrations and repository APIs
crates/missive-router     agent selection, policies, groups, collectives
crates/missive-gateway    daemon, subscriptions, webhooks, jobs, sessions
crates/missive-adapters   stdin/stdout, file, HTTP, future chat adapters
crates/missive-observe    tracing, logs, diagnostics, event export helpers
crates/missive-test-support reusable local A2A integration fixtures for tests
```

The current binary exposes help for the top-level command tree, accepts global flags, implements `missive agent add/list/show/inspect/refresh/remove/rename`, non-streaming `missive send`, streaming `missive stream`, `missive task get/list/wait/cancel`, `missive task artifact list/show/save/export`, `missive context create/list/show/fork/close/export`, `missive group create/list/show/add/remove/rename/delete`, `missive push create/get/list/delete`, `missive webhook run`, `missive gateway run`, Linux systemd/macOS launchd `missive gateway install/start/stop/status/uninstall`, and `missive events list/tail/replay/export`, and renders remaining skeletal command status in human, JSON, NDJSON, or quiet modes:

```bash
cargo run -p missive-cli --bin missive -- --help
cargo run -p missive-cli --bin missive -- agent --help
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- agent add echo http://127.0.0.1:8080 --tag local
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- agent inspect echo --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- agent refresh echo
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- send echo "Say hello" --json
printf 'hello from stdin' | MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- send echo --stdin
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- send echo --json-part '{"kind":"example"}' --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- send echo --file-bytes ./image.png --mime image/png --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- stream echo "Show progress" --ndjson
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- task list --agent echo --source remote --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- task wait task-123 --agent echo --timeout 2m --interval 2s --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- task cancel task-123 --agent echo
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- task artifact list task-123 --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- task artifact export task-123 --output-dir ./artifacts
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- context create --name "Planning round" --agent echo --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- context list
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- context export "Planning round" --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- group create team --routing-policy weighted --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- group add team echo --rank rank-0 --tag local --weight 2
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- group show team --json
MISSIVE_PUSH_CALLBACK_SECRET=change-me MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- push create echo task-123 http://127.0.0.1:7347/a2a/push --config-id local-webhook --auth-scheme Bearer --auth-credentials-env MISSIVE_PUSH_CALLBACK_SECRET --json
MISSIVE_WEBHOOK_TOKEN=change-me MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- webhook run --port 7347 --auth-token-env MISSIVE_WEBHOOK_TOKEN --max-events 1 --ndjson
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- gateway run --port 7347 --timeout 30s --ndjson
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- push list echo task-123 --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- push delete echo task-123 local-webhook
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- agent list --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- events list --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- events export --ndjson
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- events tail --limit 10 --ndjson
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- events replay --context ctx-123 --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- agent list --config examples/config/minimal.toml --json
```

See [`docs/cli.md`](docs/cli.md) for current command behaviour and the output envelope, [`docs/configuration.md`](docs/configuration.md) for config discovery, schema, A2A service-parameter/auth defaults, state paths, examples, validation, and redaction, [`docs/protocol.md`](docs/protocol.md) for the current official A2A type boundary, Agent Card discovery, service-parameter/auth handling, send/stream/task subscription/push/webhook/artifact mapping, context continuity, interface negotiation mapping, and conformance fixture coverage, [`docs/storage.md`](docs/storage.md) for the SQLite migration/schema contract, [`docs/gateway.md`](docs/gateway.md) for gateway daemon operation, subscription resume, service installation, and log inspection, [`docs/security.md`](docs/security.md) for auth storage tradeoffs, and [`docs/testing.md`](docs/testing.md) for local validation, reusable mock A2A fixtures, gateway/webhook integration coverage, and protocol-versioned conformance fixtures. Future tickets add embedded webhook management, adapters, collectives, broader tests, and packaging.

## Build and validation

Prerequisites:

* Rust stable toolchain with `cargo`, `rustfmt`, and `clippy`
* Bash for repository scripts

Check or install local build tooling with:

```bash
scripts/bootstrap-tools.sh --check
scripts/bootstrap-tools.sh
```

The bootstrap script is idempotent and best-effort. Optional tools are skipped by
the quality gate when absent and used automatically when present. See
[`docs/tooling.md`](docs/tooling.md) for the full tool inventory, `sudo`/package
manager policy, and aggressive validation options.

Validate the checkout with:

```bash
scripts/quality-gate.sh
```

Useful direct Rust commands during this bootstrap phase:

```bash
cargo check --workspace --all-targets --all-features
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
RUSTDOCFLAGS=-Dwarnings cargo doc --workspace --all-features --no-deps
cargo build --workspace --all-features --release
```

Runtime state, credentials, logs, database files, generated artifacts, and other machine-local files must stay out of the repository.

## Autonomous build system

The repository is driven by ordered tickets in `BUILD_TICKETS.md`. Each autonomous run must complete only the lowest-numbered `TODO` or `IN_PROGRESS` ticket, run the quality gate, update `BUILD_NOTES.md`, and commit the result.

Key files:

* `AGENTS.md` — autonomous agent rules
* `PROJECT_BRIEF.md` — project goals, constraints, and architecture expectations
* `BUILD_TICKETS.md` — ordered implementation queue
* `BUILD_NOTES.md` — latest validation notes and next ticket
* `docs/AUTONOMOUS_BUILD.md` — build-loop model
* `docs/USAGE.md` — autonomous build usage

## License

Licensed under the Apache License, Version 2.0. See `LICENSE`.
