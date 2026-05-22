# BUILD_NOTES.md

## Current state

Tickets 000 through 016 are complete. The repository uses the target Cargo workspace layout for `missive` with these crates:

* `crates/missive-cli` — package `missive-cli`, binary `missive`, clap-derived CLI tree, global flags, configuration loading from CLI/env/discovery, output rendering contract, redaction helpers, help snapshots, implemented `missive agent add/list/show/inspect/refresh/remove/rename` including `agent inspect --binding` interface override, and placeholder execution status for later commands
* `crates/missive-core` — core domain primitive scaffolding, including shared error/result types, strongly typed IDs, timestamps, metadata maps, envelopes, configuration schema, config discovery, profile validation, and redacted config rendering
* `crates/missive-a2a` — A2A protocol/client integration scaffolding, official `a2a-lf` protocol type re-exports, public Agent Card discovery/parsing helpers for `/.well-known/agent-card.json`, A2A interface negotiation helpers, and A2A serde fixture round-trip tests
* `crates/missive-store` — persistence scaffolding with local state path resolution, profile-specific data/state/cache directories, SQLite database path resolution, process locks for state mutation and gateway operation, embedded SQLite schema migrations, and a blocking typed repository facade for auth refs, agents, contexts, tasks, events, groups, group members, and gateway jobs
* `crates/missive-router` — routing and collectives scaffolding
* `crates/missive-gateway` — gateway daemon scaffolding
* `crates/missive-adapters` — adapter scaffolding
* `crates/missive-observe` — observability scaffolding

The root `Cargo.toml` is a virtual workspace manifest with shared workspace package metadata and shared dependency versions for planned foundational Rust crates. The store layer depends on `rusqlite` with bundled SQLite plus `serde`/`serde_json` for typed JSON repository boundaries. The A2A layer now depends on the official `a2a-lf` crate from `a2aproject/a2a-rs` for protocol types and on `reqwest` with blocking rustls-backed HTTP/TLS support for public Agent Card discovery. The CLI directly depends on the workspace `url` crate for registry URL validation.

Autonomous build tooling is documented in `docs/tooling.md`. `scripts/bootstrap-tools.sh` is executable, idempotent, supports `--check`, and can install Rust components, optional cargo tools, and opt-in system dependencies.

`scripts/quality-gate.sh` is the hardened default gate for autonomous cycles. It runs shell checks, secret and generated/private-file guardrails, Rust feature checks, formatting, clippy with warnings denied, workspace tests, doc tests, docs with warnings denied, debug/release builds, and optional installed dependency checks. `MISSIVE_AGGRESSIVE_TESTS=1` enables deeper optional checks without editing the script.

Architecture decision records live under `docs/adr/`, with a template and accepted ADRs for Rust workspace structure, A2A-first protocol strategy, SQLite local state, CLI-first UX, and the official A2A Rust protocol type strategy. `docs/architecture.md` links the ADRs and records the current high-level crate boundaries, shared error handling contract, core primitive contract, CLI command/agent-registry/Agent-Card contract, configuration contract, output rendering contract, store path/lock contract, SQLite migration contract, typed repository contract, and A2A type boundary. `docs/protocol.md` documents the current official Rust type boundary, public Agent Card discovery, interface negotiation mapping, and fixture update process.

`missive-core` exposes `MissiveError`, `Result<T>`, `ErrorCategory`, `MissiveExitCode`, and `ErrorReport`. The error taxonomy covers I/O, configuration, protocol, transport, storage, authentication, validation, and orchestration failures. Each category has a stable diagnostic code, deterministic exit code mapping for CLI use, human `Display` rendering, `miette::Diagnostic` metadata, and a serializable JSON/NDJSON report shape.

`missive-core` also exposes `AgentAlias`, `GroupName`, `RankName`, `TransportName`, `ContextId`, `TaskId`, `MessageId`, `EventId`, `MissiveTimestamp`, `Metadata`, and `Envelope<T>`. Named identifiers validate lowercase CLI-safe forms; opaque A2A/local IDs preserve server-provided text while rejecting empty, whitespace, control, or unbounded values. `Metadata` uses deterministic JSON object ordering, and `MissiveTimestamp` uses canonical RFC3339 UTC rendering.

The core configuration layer exposes `MissiveConfig`, `ConfigDiscovery`, `LoadedConfig`, and schema structs for profiles, agents, auth refs, storage, output, gateway, adapters, and quality of service. Discovery precedence is `--config`, `MISSIVE_CONFIG`, repository-local config when `MISSIVE_REPO_CONFIG=1`, XDG config locations, then built-in defaults. Config parsing rejects unknown fields, validates cross references, and provides redacted JSON rendering.

The store path layer exposes `StatePathResolver`, `StatePaths`, `StatePlatform`, `StatePathSource`, `ProcessLockKind`, and `ProcessLock`. Runtime state defaults to XDG-compatible roots on Linux/Unix-like platforms, macOS `~/Library` fallbacks when XDG variables are absent, or `MISSIVE_HOME` when explicitly set. Paths include `profiles/<profile>`, relative database paths resolve under the selected profile state directory, and lock files live under `<state-dir>/locks/`.

The store migration layer exposes `Migration`, `AppliedMigration`, `MigrationReport`, `embedded_migrations`, `open_sqlite_database`, `migrate_database`, `migrate_connection`, `applied_migrations`, `schema_version`, `CURRENT_SCHEMA_VERSION`, and `SQLITE_APPLICATION_ID`. Migration SQL lives under `crates/missive-store/migrations`, is applied in version order inside transactions, and records checksums in `schema_migrations`. Schema version 1 creates tables for agents, contexts, tasks, messages, artifacts, events, groups, group_members, auth_refs, push_configs, gateway_jobs, and adapter_bindings.

The store repository layer exposes blocking `Store` and `StoreTransaction` APIs. `Store::open`, `Store::from_connection`, and `Store::open_in_memory` apply migrations before use. Typed methods cover non-secret auth refs, agents, contexts, tasks, events, groups/group members, and gateway jobs with public upsert/record structs, state/source enums, core identifiers, validated `GatewayJobId`/`AdapterBindingId`, JSON serialization at the repository boundary, and transaction rollback on closure or SQL failures. SQL strings remain private to `missive-store` rather than leaking into CLI code.

The A2A layer exposes the official `a2a-lf` protocol models through `missive_a2a::protocol`, aliases the public Agent Card inspection types to official SDK structs, provides `AgentCardClient`, and includes interface negotiation helpers. It resolves `/.well-known/agent-card.json` from a registered base URL, sends conditional refresh headers when cached ETags/Last-Modified values exist, parses supported interfaces, provider, versions, capabilities, default modes, and skills through the official Agent Card type, and maps HTTP/TLS/network failures to transport errors and invalid card JSON to protocol errors. A small compatibility parser normalizes snake_case fixture aliases and older/pre-release cards that omit `supportedInterfaces` before official deserialization while preserving raw card JSON in the cache/output. Interface negotiation canonicalizes Agent Card bindings such as `HTTP+JSON` and `JSONRPC`, supports local `http+json` and `json-rpc`, recognizes gRPC for future extension diagnostics, respects agent binding preference or `agent inspect --binding`, and falls back to registry/base-URL interfaces when older cards omit `supportedInterfaces`.

The `missive` binary uses clap derive and exposes help pages for `agent`, `send`, `stream`, `task`, `context`, `group`, `gateway`, `webhook`, `push`, `doctor`, `logs`, `events`, `completion`, and `manpage`. Global flags parse at every command level: `--json`, `--ndjson`, `--quiet`, `--no-color`, `--config`, `--profile`, `--timeout`, `--trace`, and `--verbose`.

The `agent` command now has implemented `add`, `remove`, `list`, `show`, `inspect`, `refresh`, and `rename` subcommands. Agent registry commands resolve the selected profile state paths, create directories, acquire the state mutation lock, open/migrate SQLite, sync config auth refs as non-secret rows, sync config-seeded agents as read-only rows, and persist local registry entries through `missive-store`. They support aliases, base URLs, explicit interface URLs, binding preference, config auth refs, tags, notes, metadata, human output, JSON output, NDJSON output, quiet mode, duplicate-alias checks, missing-agent diagnostics, and read-only protections for config-seeded agents.

`missive agent inspect <alias>` fetches and caches a public A2A Agent Card when needed, negotiates the selected interface from `supportedInterfaces` and the agent binding preference, then renders the parsed provider, capabilities, skills, versions, supported interfaces, selected interface, and raw card JSON in machine output. `missive agent inspect <alias> --binding <binding>` requires a specific locally supported binding for advanced users/tests, `missive agent inspect <alias> --refresh` bypasses/revalidates the cache, and `missive agent refresh <alias>` explicitly refreshes the cached public card. Cached card JSON, ETags, Last-Modified values, and fetch timestamps are stored on the agent row and preserved for config-seeded agents while their configured base URL remains unchanged.

The current CLI output contract supports human, JSON, NDJSON, and quiet renderers. Skeletal commands load/validate config, then emit a stable `missive.output.v1` `command_status` envelope in machine-readable modes; implemented agent commands emit command-specific kinds such as `agent_add`, `agent_list`, `agent_show`, `agent_inspect`, `agent_refresh`, `agent_remove`, and `agent_rename`. NDJSON emits one compact JSON object per line with `sequence`. Structured execution errors render as `kind: "error"` envelopes when `--json` or `--ndjson` is active. The renderer recursively redacts secret-like JSON fields and HTTP-style authorization headers before writing output.

## Quality gates

Latest run:

```bash
scripts/quality-gate.sh
```

Result: passed.

Checks run by the default gate included:

* shell script syntax checks with `bash -n`
* `shellcheck` because it is installed
* secret guardrail across tracked files and untracked non-ignored files
* generated/private-file guardrail across tracked files and untracked non-ignored files
* `cargo check --workspace --all-targets`
* `cargo check --workspace --all-targets --all-features`
* `cargo check --workspace --all-targets --no-default-features`
* `cargo fmt --all -- --check`
* `cargo clippy --workspace --all-targets --all-features -- -D warnings`
* `cargo test --workspace --all-targets --all-features`
* `cargo test --workspace --doc --all-features`
* `RUSTDOCFLAGS=-Dwarnings cargo doc --workspace --all-features --no-deps`
* `cargo build --workspace --all-features`
* `cargo build --workspace --all-features --release`
* `cargo build -p missive-cli --bin missive --release`
* optional `cargo machete` check because it is installed
* optional `cargo audit` check because it is installed

Additional targeted validation run during this cycle:

```bash
cargo check -p missive-a2a --all-targets
cargo test -p missive-a2a --all-targets
cargo check --workspace --all-targets
cargo test --workspace --all-targets --all-features
cargo clippy -p missive-a2a -p missive-cli --all-targets --all-features -- -D warnings
scripts/quality-gate.sh
```

The targeted checks covered official `a2a-lf` type compilation, Agent Card compatibility parsing, snake_case fixture aliases, missing-`supportedInterfaces` fallback preservation, official Agent Card/Message/Task/send/push fixture serde round-trips, CLI compatibility with official Agent Card skill option fields, and workspace regression coverage.

Environment/tooling notes: no new cargo subcommands or OS packages were installed during this cycle. The crates.io `a2a-lf` dependency was added and `Cargo.lock` was updated by Cargo.

## Latest cycle notes

Implemented ticket 016 — Integrate official or vendored A2A Rust types.

Included:

* evaluated official and alternative A2A Rust crates and selected the official `a2a-lf` crate from `a2aproject/a2a-rs` via crates.io rather than a Git dependency
* added `a2a-lf` as the workspace A2A protocol type dependency and kept upstream access localized to `crates/missive-a2a`
* added `missive_a2a::protocol` re-exports for official A2A Agent Card, Message, Task, Artifact, send/request/response, and push-config types
* aliased the public Agent Card inspection types to official SDK structs instead of maintaining duplicate local protocol models
* kept a small compatibility parser for public Agent Cards that normalizes snake_case fixture aliases and older cards that omit `supportedInterfaces` before official deserialization
* preserved current interface negotiation behavior, cache behavior, and CLI inspect output while adapting CLI skill rendering to official optional skill mode/example fields
* added minimal A2A v1.0 fixtures under `tests/fixtures/a2a/1.0/` and serde round-trip tests through official SDK types
* documented the protocol type strategy, update process, and limitations in ADR 0005 plus README, architecture, and protocol docs

## Known blockers

None known.

## Limitations

A2A service parameter/version headers, authentication material resolution, outbound send/stream calls, task operations, and push/webhook/gateway behavior remain for later tickets.

Public Agent Card discovery currently expects unauthenticated cards. Config auth refs can be linked to agents but are not resolved or sent during card discovery yet.

The official A2A Rust type boundary is now in place through `a2a-lf`, but only a minimal fixture set exists for ticket 016. The broader conformance fixture suite, SDK/client interoperability against example agents, and protocol update automation remain for later tickets. The Agent Card compatibility parser is intentionally scoped to public inspection and negotiation needs; optional security fields are preserved in raw card JSON but not interpreted yet.

Config-seeded agents are synced into SQLite as read-only rows when agent commands run. If a config entry is later removed, an already-synced row may remain in the local database until a future reconciliation/maintenance command defines stale config-seed pruning. Cached Agent Card fields are preserved only while the config-seeded base URL is unchanged.

The `missive` binary has a real command tree, global parser, configuration discovery/profile validation, output rendering contract, implemented agent registry commands, public Agent Card inspection/refresh, and selected-interface negotiation for inspection, but non-agent command behaviour is still intentionally skeletal. Real messaging, gateway behaviour, adapters, collectives, shell completion generation, and manpage generation remain for later tickets.

The store layer resolves state paths, provides process locks, migrates fresh SQLite databases to schema version 1, and exposes typed repository APIs for auth refs, agents, contexts, tasks, events, groups, group members, and gateway jobs. Message, artifact, push-config, and adapter-binding repositories, retention enforcement, compaction, event replay, and durable A2A protocol persistence beyond the agent-card cache remain for later tickets.

The store repository is synchronous because it uses `rusqlite`; async gateway/adapter code should call it through a blocking task or store worker when those tickets wire runtime behaviour.

The config schema includes gateway, adapter, and QoS defaults, but those values do not yet start a gateway, enforce timeouts, run adapters, or manage background jobs.

The `--json`, `--ndjson`, and `--quiet` flags override config output defaults. `--config` and `--profile` are active. The `--timeout`, `--trace`, `--verbose`, and `--no-color` flags are still parsed but do not yet drive timeout enforcement, tracing, verbose diagnostics, or color control.

Redaction is best-effort at the config and CLI output boundaries for structured values rendered through the current helpers. Trace/log redaction, storage redaction beyond schema design, and adapter/webhook trust boundaries remain for later security and observability tickets.

Detailed gateway operations, adapter lifecycle, collectives, testing, and runbook documentation remain for later implementation/documentation tickets.

Optional exhaustive validation tools are not all installed in this environment yet. The default quality gate passes without them and uses installed optional tools automatically. `cargo-nextest` is currently missing, and miri is unavailable for the active stable toolchain.

There is not yet a `cargo-deny` policy file; the quality gate skips deny checks until the supply-chain policy ticket introduces one.

## Next recommended ticket

Ticket 017 — Implement A2A service parameter handling.
