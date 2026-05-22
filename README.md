# missive

`missive` is an early-stage Rust command-line tool and local control plane for A2A-native agent communication. The long-term goal is to make agent messaging feel like `curl`, agent communication state feel like `kubectl`, and multi-agent coordination feel like MPI-style collective operations.

## Current status

This repository is at early workspace stage. It contains:

* a Cargo workspace with the target crate layout under `crates/`
* a `missive-cli` package that exposes the binary named `missive`
* a clap-based CLI with stable top-level commands, global flags, configuration discovery, profiles, A2A service-parameter overrides, env/header/keyring auth inputs for implemented requests, human/JSON/NDJSON/quiet output renderers, agent registry commands, A2A Agent Card discovery/cache inspection, official A2A Rust protocol type integration, A2A interface negotiation, non-streaming `missive send`, streaming `missive stream`, task `get/list/wait/cancel`, and context `create/list/show/fork/close/export`
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
```

The current binary exposes help for the top-level command tree, accepts global flags, implements `missive agent add/list/show/inspect/refresh/remove/rename`, non-streaming `missive send`, streaming `missive stream`, `missive task get/list/wait/cancel`, and `missive context create/list/show/fork/close/export`, and renders remaining skeletal command status in human, JSON, NDJSON, or quiet modes:

```bash
cargo run -p missive-cli --bin missive -- --help
cargo run -p missive-cli --bin missive -- agent --help
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- agent add echo http://127.0.0.1:8080 --tag local
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- agent inspect echo --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- agent refresh echo
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- send echo "Say hello" --json
printf 'hello from stdin' | MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- send echo --stdin
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- stream echo "Show progress" --ndjson
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- task list --agent echo --source remote --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- task wait task-123 --agent echo --timeout 2m --interval 2s --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- task cancel task-123 --agent echo
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- context create --name "Planning round" --agent echo --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- context list
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- context export "Planning round" --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- agent list --json
cargo run -p missive-cli --bin missive -- events --ndjson
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- agent list --config examples/config/minimal.toml --json
```

See [`docs/cli.md`](docs/cli.md) for current command behaviour and the output envelope, [`docs/configuration.md`](docs/configuration.md) for config discovery, schema, A2A service-parameter/auth defaults, state paths, examples, validation, and redaction, [`docs/protocol.md`](docs/protocol.md) for the current official A2A type boundary, Agent Card discovery, service-parameter/auth handling, send/stream/task mapping, context continuity, and interface negotiation mapping, [`docs/storage.md`](docs/storage.md) for the SQLite migration/schema contract, and [`docs/security.md`](docs/security.md) for auth storage tradeoffs. Future tickets add richer message parts, artifact export, gateway runtime behaviour, adapters, collectives, broader tests, and packaging.

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
