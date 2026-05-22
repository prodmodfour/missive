# PROJECT_BRIEF.md

TEMPLATE_CUSTOMISED: true

## Project name

missive

Always spell the project name as lowercase `missive` in prose, command examples, repository files, and generated documentation unless a case-sensitive external format requires otherwise.

## Project type

Rust CLI tool, protocol abstraction library, local gateway daemon, and agent communication control plane.

## Project goal

Build `missive`: a Rust command-line tool that manages communication between AI agents. It should feel like `curl` for agent messages, `kubectl` for agent communication state, and MPI-style collective operations for multi-agent workflows.

`missive` is not an agent framework and should not try to become one. It manages communication, discovery, transport selection, sessions, tasks, routing, groups, persistence, observability, and gateway/adapters around agent communication.

## Protocol and inspiration

`missive` must conform to the A2A Protocol as the canonical protocol layer. It should support A2A Agent Card discovery, interface negotiation, message sending, streaming, task management, push notification configuration, context/task continuity, service parameters, and protocol-version handling.

`missive` should evaluate and preferably wrap the official `a2a-rs` Rust SDK rather than unnecessarily hand-rolling protocol types. If the SDK is not yet suitable for a given feature, isolate compatibility code in `crates/missive-a2a` and keep conformance fixtures.

`missive` should take messaging-system inspiration from `nousresearch/hermes-agent`, especially its gateway/session concepts, background work, adapter style, user/channel access model, and busy-input modes. Do not copy Hermes Agent wholesale and do not import unrelated agent-memory, skill-learning, or terminal-agent behaviour unless a ticket explicitly requires it for communication management.

## Audience

* Humans who want to inspect, message, stream, and coordinate agents from a terminal.
* Autonomous agents that want a stable CLI/subprocess interface for talking to other agents.
* Developers building A2A-compatible services who need a local control plane, test harness, and orchestration layer.
* Maintainers who need reliable state, logs, diagnostics, and reproducible tests.

## Success criteria

The project is successful when:

* `missive` builds as a Rust workspace and exposes a binary named `missive`.
* The CLI supports human-readable, JSON, and NDJSON output modes.
* The CLI can discover and cache A2A Agent Cards.
* The CLI can negotiate A2A transport/interface selection.
* The CLI can send messages, stream messages, inspect/list/wait/cancel tasks, and manage contexts.
* The CLI can configure/list/delete A2A push notification configs and receive push webhook events.
* Local SQLite-backed state tracks agents, contexts, tasks, messages, artifacts, events, groups, and gateway jobs.
* MPI-inspired collective operations exist: group, broadcast, barrier, gather, and reduce.
* A gateway daemon can run subscriptions, webhooks, adapters, and background jobs.
* Hermes-inspired sessions, reset policies, background jobs, and busy-input modes are implemented where relevant.
* Test coverage includes unit tests, integration tests with local mock A2A servers, protocol fixture round-trips, CLI smoke tests, and aggressive optional checks.
* Documentation covers usage, architecture, protocol mapping, security, gateway operation, adapters, troubleshooting, and examples.
* CI and local `scripts/quality-gate.sh` are aligned.
* No real secrets, credentials, private data, or machine-specific runtime files are committed.

## Non-goals

The autonomous agent must not spend time on:

* Building a general-purpose LLM agent framework.
* Implementing agent cognition, memory, skill learning, or tool execution unrelated to communication management.
* Replacing A2A with a proprietary protocol.
* Copying Hermes Agent wholesale.
* Building production chat-platform adapters before the local adapter/gateway architecture is proven.
* Adding payment/marketplace features unless a future ticket explicitly asks for them.
* Embedding vendor-specific LLM provider logic into the core communication layer.

## Technology preferences

Preferred stack:

* language: Rust, stable toolchain, edition 2024 if available and practical, otherwise edition 2021
* CLI: clap derive
* async runtime: tokio
* HTTP: reqwest for client, axum for local gateway/webhook/mock servers where appropriate
* persistence: SQLite through sqlx or rusqlite, with migrations
* serialization: serde, serde_json, toml
* diagnostics: thiserror, miette, tracing, tracing-subscriber
* IDs/time: uuid, time or chrono
* config dirs: directories or equivalent XDG-aware crate
* key storage: keyring where available
* tests: cargo test, cargo nextest, proptest, insta/snapbox/assert_cmd/predicates where useful
* aggressive validation: cargo-llvm-cov, cargo-deny, cargo-audit, cargo-machete, cargo-mutants, cargo-fuzz, miri, criterion where useful
* CI: GitHub Actions
* packaging: cargo-dist or equivalent, decided by ticket

Hard constraints:

* The binary must be named `missive`.
* The project name should be written lowercase as `missive`.
* A2A compatibility is mandatory.
* Shell/agent automation support is mandatory: stable JSON, NDJSON, stdin/stdout, deterministic exit codes, and no interactive-only critical path.
* The autonomous build loop must work one ticket at a time.
* Every ticket must run `scripts/quality-gate.sh` before completion.
* Keep runtime state outside the repository by default.

Flexible choices:

* Whether protocol types come directly from `a2a-rs`, generated protobuf, or a compatibility wrapper, provided the choice is documented and tested.
* Whether SQLite is implemented with sqlx or rusqlite.
* Whether gateway service install support is implemented before or after the core daemon, provided tickets remain ordered.
* Exact release tooling, provided reproducible builds and checksums are supported.

## Architecture expectations

Target workspace:

```text
crates/missive-cli        -> command parsing, output rendering, exit codes
crates/missive-core       -> domain types, errors, config, IDs, envelopes
crates/missive-a2a        -> A2A protocol/client integration and compatibility fixtures
crates/missive-store      -> SQLite migrations and repository APIs
crates/missive-router     -> agent selection, policies, groups, collectives
crates/missive-gateway    -> daemon, subscriptions, webhooks, jobs, sessions
crates/missive-adapters   -> stdin/stdout, file, HTTP, future chat adapters
crates/missive-observe    -> tracing, logs, diagnostics, event export helpers
```

Recommended flow:

```text
CLI/adapters -> command model -> router/session/context -> A2A client -> remote agent
                                           |              -> store/events/artifacts
                                           |              -> gateway jobs/subscriptions/webhooks
```

## Quality expectations

Expected quality gates:

* shell script syntax checks
* no-secret guardrail
* no generated/private runtime file guardrail
* cargo fmt check
* cargo clippy with warnings denied
* cargo test workspace
* cargo doc tests where applicable
* cargo build workspace and release binary
* CLI smoke tests
* mock A2A server integration tests
* protocol fixture tests
* config validation tests
* optional aggressive tests: nextest, coverage, miri, cargo-deny, cargo-audit, cargo-machete, cargo-mutants, cargo-fuzz, benchmarks, Docker/devcontainer validation

The agent is allowed to install any reasonable build/test/development dependency needed to complete and validate the project. It may use `sudo`, apt, brew, rustup, cargo install, Docker, local network services, temporary databases, temporary files, and local mock servers. It should record notable installations or environment changes in `BUILD_NOTES.md`.

There are no artificial limits on testing depth. Prefer stronger validation when feasible. Long-running/destructive-looking tests must be scoped to temporary directories, local containers, or isolated test fixtures, and must not target third-party systems without explicit opt-in configuration.

## Documentation expectations

Required docs:

* README.md
* docs/quickstart.md
* docs/cli.md
* docs/configuration.md
* docs/architecture.md
* docs/protocol.md
* docs/storage.md
* docs/gateway.md
* docs/adapters.md
* docs/collectives.md
* docs/security.md
* docs/testing.md
* docs/runbook.md
* docs/troubleshooting.md
* docs/adr/

## Safety and security constraints

The user has authorised broad local build/test autonomy, including use of sudo and package installation. Keep only essential repository and environment hygiene constraints:

* Do not commit real secrets, credentials, tokens, private keys, real `.env` files, private data, internal-only hostnames, or machine-specific runtime state.
* Do not run destructive operations outside temporary test directories/containers unless a ticket explicitly requires and documents them.
* Do not attack, fuzz, load-test, or scan third-party systems. Use local mock servers or explicitly configured test endpoints.
* Redact auth material in CLI output, logs, events, traces, and test fixtures.
* Prefer HTTPS and clear authentication handling for remote A2A agents.
* Treat external adapters and webhooks as untrusted input.

## Agent behaviour notes

* Work the lowest-numbered TODO or IN_PROGRESS ticket only.
* Use one commit per completed ticket.
* Make the smallest coherent implementation that satisfies the selected ticket, while still testing thoroughly.
* You may install tools and dependencies as needed. Do not let missing tools become blockers unless installation genuinely fails.
* Prefer local mock/integration tests over manual claims.
* Update docs whenever behaviour, architecture, setup, security posture, or public CLI usage changes.
* Update BUILD_NOTES.md with what changed, quality gates run, limitations, blockers, and next recommended ticket.
