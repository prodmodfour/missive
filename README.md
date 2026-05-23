# missive

`missive` is an early-stage Rust command-line tool and local control plane for A2A-native agent communication. The long-term goal is to make agent messaging feel like `curl`, agent communication state feel like `kubectl`, and multi-agent coordination feel like MPI-style collective operations.

## Current status

This repository is still early-stage, but it now contains:

* a Cargo workspace with the target crate layout under `crates/`, plus a dev-support crate for local A2A integration fixtures and protocol-versioned A2A conformance fixtures under `tests/fixtures/a2a/1.0/`
* a `missive-cli` package that exposes the binary named `missive`
* a clap-based CLI with stable top-level commands, global flags, configuration discovery, profiles, A2A service-parameter overrides, env/header/keyring auth inputs for implemented requests, human/JSON/NDJSON/quiet output renderers, shell completion and manpage generation, the foreground `missive adapter stdio` JSON/NDJSON subprocess frame loop, the foreground `missive adapter file-drop` inbox/outbox loop, an opt-in authenticated HTTP inbound adapter mounted by `missive gateway run`, feature-gated external chat adapter stubs in the adapter crate, agent registry commands, A2A Agent Card discovery/cache inspection and capability summaries, official A2A Rust protocol type integration, A2A interface negotiation, rich text/file/byte/JSON message parts for non-streaming `missive send` and streaming `missive stream`, task `get/list/wait/cancel`, task-scoped artifact `list/show/save/export`, context `create/list/show/fork/close/export`, group `create/list/show/add/remove/rename/delete/capabilities`, dry-run route explanation with built-in and Agent Card-aware capability routing policies, broadcast collective `bcast`, barrier collective `barrier`, gather collective `gather`, reduce collective `reduce`, push notification config `create/get/list/delete`, local webhook receiver `run`, gateway daemon `run`, gateway service `install/start/stop/status/uninstall`, gateway-managed background jobs `job start/list/show/cancel`, and event journal `list/tail/replay/export`
* store-layer state path resolution, process locks, SQLite migrations, and typed repository APIs that keep default runtime state outside the source tree
* repository hygiene files and guardrails
* the autonomous ticket queue used to build the project one commit at a time
* user, developer, and maintenance documentation under `docs/`, including `docs/adr/`

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

The current binary exposes help for the top-level command tree, accepts global flags, implements `missive completion <shell>`, `missive manpage`, `missive adapter stdio`, `missive adapter file-drop`, the `missive gateway run --http-adapter` inbound HTTP endpoint, `missive agent add/list/show/inspect/refresh/capabilities/remove/rename`, non-streaming `missive send`, streaming `missive stream`, `missive task get/list/wait/cancel`, `missive task artifact list/show/save/export`, `missive context create/list/show/fork/close/export`, `missive group create/list/show/capabilities/add/remove/rename/delete`, Agent Card-aware `missive route explain`, `missive bcast`, `missive barrier`, `missive gather`, `missive reduce`, `missive push create/get/list/delete`, `missive webhook run`, `missive gateway run`, Linux systemd/macOS launchd `missive gateway install/start/stop/status/uninstall`, `missive job start/list/show/cancel`, `missive doctor`, `missive logs`, and `missive events list/tail/replay/export`:

```bash
cargo run -p missive-cli --bin missive -- --help
cargo run -p missive-cli --bin missive -- completion bash > /tmp/missive.bash
cargo run -p missive-cli --bin missive -- manpage > /tmp/missive.1
cargo run -p missive-cli --bin missive -- adapter stdio --help
cargo run -p missive-cli --bin missive -- adapter file-drop --help
printf '%s\n' '{"schema_version":"missive.stdio.v1","id":"req-1","command":"task_list"}' | MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- adapter stdio --mode single-shot --framing json
mkdir -p /tmp/missive-drop/{inbox,outbox}; printf '%s\n' '{"schema_version":"missive.file_drop.v1","id":"req-1","command":"task_list"}' >/tmp/missive-drop/inbox/req-1.tmp; mv /tmp/missive-drop/inbox/req-1.tmp /tmp/missive-drop/inbox/req-1.json; MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- adapter file-drop --inbox /tmp/missive-drop/inbox --outbox /tmp/missive-drop/outbox --mode once --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- agent --help
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- agent add echo http://127.0.0.1:8080 --tag local
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- agent inspect echo --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- agent refresh echo
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- agent capabilities echo --json
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
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- group capabilities team --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- route explain --group team --policy capability-match --capability echo --input-mode text/plain --streaming --refresh-capabilities --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- bcast team "Say hello to the group" --failure-policy continue --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- barrier team --context ctx-planning-round --timeout 2m --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- gather team --context ctx-planning-round --output-dir ./gathered-artifacts --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- reduce team --context ctx-planning-round --strategy summarise --json
MISSIVE_PUSH_CALLBACK_SECRET=change-me MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- push create echo task-123 http://127.0.0.1:7347/a2a/push --config-id local-webhook --auth-scheme Bearer --auth-credentials-env MISSIVE_PUSH_CALLBACK_SECRET --json
MISSIVE_WEBHOOK_TOKEN=change-me MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- webhook run --port 7347 --auth-token-env MISSIVE_WEBHOOK_TOKEN --max-events 1 --ndjson
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- job start send echo "Run in the gateway" --json
MISSIVE_HTTP_ADAPTER_TOKEN=change-me MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- gateway run --port 7347 --http-adapter --http-adapter-auth-token-env MISSIVE_HTTP_ADAPTER_TOKEN --timeout 30s --ndjson
curl -sS -H 'Authorization: Bearer change-me' -H 'Content-Type: application/json' -d '{"schema_version":"missive.http.v1","id":"req-1","command":"task_list"}' http://127.0.0.1:7347/adapter/http/v1/messages
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- job list --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- doctor --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- logs --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- push list echo task-123 --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- push delete echo task-123 local-webhook
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- agent list --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- events list --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- events export --ndjson
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- events tail --limit 10 --ndjson
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- events replay --context ctx-123 --json
MISSIVE_HOME=/tmp/missive-demo cargo run -p missive-cli --bin missive -- agent list --config examples/config/minimal.toml --json
```

## Documentation

User-facing docs:

* [`docs/quickstart.md`](docs/quickstart.md) — local mock-agent workflow from a clean checkout.
* [`docs/cli.md`](docs/cli.md) — command reference, global flags, output contract, completions, and manpage generation.
* [`docs/configuration.md`](docs/configuration.md) — config discovery, schema, profiles, auth refs, state paths, validation, and redaction.
* [`docs/agent-registry.md`](docs/agent-registry.md) — agent aliases, Agent Card discovery/cache, capability summaries, config-seeded agents, and registry mutations.
* [`docs/messaging.md`](docs/messaging.md) — non-streaming `send`, message parts, metadata, auth, service parameters, persistence, and limits.
* [`docs/streaming.md`](docs/streaming.md) — foreground `stream`, NDJSON event output, capability checks, and streaming persistence.
* [`docs/tasks.md`](docs/tasks.md) — task list/get/wait/cancel, deterministic wait exit codes, and task artifact export.
* [`docs/contexts.md`](docs/contexts.md) — context creation, naming, forking, closing, export, and continuity limits.
* [`docs/groups.md`](docs/groups.md) — group membership, capability summaries, dry-run routing, and collective workflow entry points.
* [`docs/collectives.md`](docs/collectives.md) — broadcast, barrier, gather, and reduce behavior.
* [`docs/gateway.md`](docs/gateway.md) — gateway daemon run mode, health/status endpoints, HTTP adapter ingress, subscriptions, jobs, sessions, and service helpers.
* [`docs/adapters.md`](docs/adapters.md) — stdio, file-drop, HTTP adapter surfaces, registry contract, and external stub boundaries.
* [`docs/push-webhooks.md`](docs/push-webhooks.md) — A2A push config CRUD and the local webhook receiver.
* [`docs/examples.md`](docs/examples.md) and [`examples/README.md`](examples/README.md) — runnable smoke-tested command demos.
* [`docs/troubleshooting.md`](docs/troubleshooting.md) — doctor/logs/events workflow and common setup/runtime issues.

Reference and maintainer docs:

* [`docs/architecture.md`](docs/architecture.md) — crate boundaries, data flow, output/config/store/protocol contracts, and ADR links.
* [`docs/runbook.md`](docs/runbook.md) — operational diagnosis, recovery, backups, gateway/service checks, and escalation steps.
* [`docs/completions.md`](docs/completions.md) — shell completion and manpage installation paths.
* [`docs/protocol.md`](docs/protocol.md) — current A2A type boundary and protocol mapping.
* [`docs/storage.md`](docs/storage.md) — SQLite migration/schema contract and backup/recovery notes.
* [`docs/observability.md`](docs/observability.md) — tracing/log initialization, `RUST_LOG`, JSON log mode, span fields, and redaction boundaries.
* [`docs/adapter-roadmap.md`](docs/adapter-roadmap.md) — Discord/Slack/Telegram/Matrix/Email placeholder boundaries and future secret/permission requirements.
* [`docs/security.md`](docs/security.md) — auth storage tradeoffs, redaction, adapter/webhook trust boundaries, and current limitations.
* [`docs/supply-chain.md`](docs/supply-chain.md) — cargo-deny policy, dependency-update rules, duplicate-dependency exceptions, and SBOM generation.
* [`docs/testing.md`](docs/testing.md) — local validation, smoke tests, mock fixtures, property/fuzz/mutation checks, and conformance fixtures.
* [`docs/performance.md`](docs/performance.md) — local benchmarks and soft performance budgets.
* [`docs/ci.md`](docs/ci.md) — GitHub Actions quality gate, cross-platform matrix, and release dry-run workflow.
* [`docs/container.md`](docs/container.md) — Docker and devcontainer workflows.
* [`docs/release.md`](docs/release.md) — release archives, checksums, and install/update guidance.

## Installation and updates

Build a local development binary with Cargo:

```bash
cargo install --path crates/missive-cli --locked
```

Create a reviewed release archive and checksums for the current host target:

```bash
scripts/release-package.sh --dry-run
```

Generate a local CycloneDX JSON SBOM alongside release artifacts when needed:

```bash
scripts/generate-sbom.sh --output dist/missive-sbom.cdx.json
```

Install or update from a generated or downloaded archive after verifying its checksum:

```bash
scripts/install-release.sh \
  --artifact dist/missive-v0.1.0-x86_64-unknown-linux-gnu.tar.gz \
  --checksum dist/missive-v0.1.0-x86_64-unknown-linux-gnu.tar.gz.sha256 \
  --bin-dir ~/.local/bin
missive --version
missive doctor
```

See [`docs/release.md`](docs/release.md) for the common target matrix, CI release dry run, checksum verification, and current packaging limitations.

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

GitHub Actions runs the same default quality gate on Linux and a workspace check/test/build matrix on Linux, macOS, and Windows for `push` and `pull_request`; see [`docs/ci.md`](docs/ci.md) for workflow details, platform-specific test gating, and optional coverage smoke checks.

Container validation is also available locally:

```bash
docker build --pull=false --tag missive-dev:local .
scripts/docker-integration.sh
```

The wrapper runs `scripts/quality-gate.sh` inside the Docker image with runtime state and build output outside tracked repository files. See [`docs/container.md`](docs/container.md) for Docker build arguments, manual `docker run` examples, and the devcontainer workflow.

Useful direct Rust commands during this bootstrap phase:

```bash
cargo check --workspace --all-targets --all-features
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
RUSTDOCFLAGS=-Dwarnings cargo doc --workspace --all-features --no-deps
cargo build --workspace --all-features --release
cargo deny --locked check
scripts/generate-sbom.sh --output dist/missive-sbom.cdx.json
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
