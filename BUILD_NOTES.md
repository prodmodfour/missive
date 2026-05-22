# BUILD_NOTES.md

## Current state

Tickets 000 through 014 are complete. The repository uses the target Cargo workspace layout for `missive` with these crates:

* `crates/missive-cli` — package `missive-cli`, binary `missive`, clap-derived CLI tree, global flags, configuration loading from CLI/env/discovery, output rendering contract, redaction helpers, help snapshots, implemented `missive agent add/list/show/inspect/refresh/remove/rename`, and placeholder execution status for later commands
* `crates/missive-core` — core domain primitive scaffolding, including shared error/result types, strongly typed IDs, timestamps, metadata maps, envelopes, configuration schema, config discovery, profile validation, and redacted config rendering
* `crates/missive-a2a` — A2A protocol/client integration scaffolding plus public Agent Card discovery/parsing helpers for `/.well-known/agent-card.json`
* `crates/missive-store` — persistence scaffolding with local state path resolution, profile-specific data/state/cache directories, SQLite database path resolution, process locks for state mutation and gateway operation, embedded SQLite schema migrations, and a blocking typed repository facade for auth refs, agents, contexts, tasks, events, groups, group members, and gateway jobs
* `crates/missive-router` — routing and collectives scaffolding
* `crates/missive-gateway` — gateway daemon scaffolding
* `crates/missive-adapters` — adapter scaffolding
* `crates/missive-observe` — observability scaffolding

The root `Cargo.toml` is a virtual workspace manifest with shared workspace package metadata and shared dependency versions for planned foundational Rust crates. The store layer depends on `rusqlite` with bundled SQLite plus `serde`/`serde_json` for typed JSON repository boundaries. The A2A layer now depends on `reqwest` with blocking rustls-backed HTTP/TLS support for public Agent Card discovery. The CLI directly depends on the workspace `url` crate for registry URL validation.

Autonomous build tooling is documented in `docs/tooling.md`. `scripts/bootstrap-tools.sh` is executable, idempotent, supports `--check`, and can install Rust components, optional cargo tools, and opt-in system dependencies.

`scripts/quality-gate.sh` is the hardened default gate for autonomous cycles. It runs shell checks, secret and generated/private-file guardrails, Rust feature checks, formatting, clippy with warnings denied, workspace tests, doc tests, docs with warnings denied, debug/release builds, and optional installed dependency checks. `MISSIVE_AGGRESSIVE_TESTS=1` enables deeper optional checks without editing the script.

Architecture decision records live under `docs/adr/`, with a template and initial accepted ADRs for Rust workspace structure, A2A-first protocol strategy, SQLite local state, and CLI-first UX. `docs/architecture.md` links the ADRs and records the current high-level crate boundaries, shared error handling contract, core primitive contract, CLI command/agent-registry/Agent-Card contract, configuration contract, output rendering contract, store path/lock contract, SQLite migration contract, and typed repository contract. `docs/protocol.md` documents the current public Agent Card discovery mapping.

`missive-core` exposes `MissiveError`, `Result<T>`, `ErrorCategory`, `MissiveExitCode`, and `ErrorReport`. The error taxonomy covers I/O, configuration, protocol, transport, storage, authentication, validation, and orchestration failures. Each category has a stable diagnostic code, deterministic exit code mapping for CLI use, human `Display` rendering, `miette::Diagnostic` metadata, and a serializable JSON/NDJSON report shape.

`missive-core` also exposes `AgentAlias`, `GroupName`, `RankName`, `TransportName`, `ContextId`, `TaskId`, `MessageId`, `EventId`, `MissiveTimestamp`, `Metadata`, and `Envelope<T>`. Named identifiers validate lowercase CLI-safe forms; opaque A2A/local IDs preserve server-provided text while rejecting empty, whitespace, control, or unbounded values. `Metadata` uses deterministic JSON object ordering, and `MissiveTimestamp` uses canonical RFC3339 UTC rendering.

The core configuration layer exposes `MissiveConfig`, `ConfigDiscovery`, `LoadedConfig`, and schema structs for profiles, agents, auth refs, storage, output, gateway, adapters, and quality of service. Discovery precedence is `--config`, `MISSIVE_CONFIG`, repository-local config when `MISSIVE_REPO_CONFIG=1`, XDG config locations, then built-in defaults. Config parsing rejects unknown fields, validates cross references, and provides redacted JSON rendering.

The store path layer exposes `StatePathResolver`, `StatePaths`, `StatePlatform`, `StatePathSource`, `ProcessLockKind`, and `ProcessLock`. Runtime state defaults to XDG-compatible roots on Linux/Unix-like platforms, macOS `~/Library` fallbacks when XDG variables are absent, or `MISSIVE_HOME` when explicitly set. Paths include `profiles/<profile>`, relative database paths resolve under the selected profile state directory, and lock files live under `<state-dir>/locks/`.

The store migration layer exposes `Migration`, `AppliedMigration`, `MigrationReport`, `embedded_migrations`, `open_sqlite_database`, `migrate_database`, `migrate_connection`, `applied_migrations`, `schema_version`, `CURRENT_SCHEMA_VERSION`, and `SQLITE_APPLICATION_ID`. Migration SQL lives under `crates/missive-store/migrations`, is applied in version order inside transactions, and records checksums in `schema_migrations`. Schema version 1 creates tables for agents, contexts, tasks, messages, artifacts, events, groups, group_members, auth_refs, push_configs, gateway_jobs, and adapter_bindings.

The store repository layer exposes blocking `Store` and `StoreTransaction` APIs. `Store::open`, `Store::from_connection`, and `Store::open_in_memory` apply migrations before use. Typed methods cover non-secret auth refs, agents, contexts, tasks, events, groups/group members, and gateway jobs with public upsert/record structs, state/source enums, core identifiers, validated `GatewayJobId`/`AdapterBindingId`, JSON serialization at the repository boundary, and transaction rollback on closure or SQL failures. SQL strings remain private to `missive-store` rather than leaking into CLI code.

The A2A layer exposes public Agent Card compatibility structs and `AgentCardClient`. It resolves `/.well-known/agent-card.json` from a registered base URL, sends conditional refresh headers when cached ETags/Last-Modified values exist, parses supported interfaces, provider, versions, capabilities, default modes, and skills, and maps HTTP/TLS/network failures to transport errors and invalid card JSON to protocol errors.

The `missive` binary uses clap derive and exposes help pages for `agent`, `send`, `stream`, `task`, `context`, `group`, `gateway`, `webhook`, `push`, `doctor`, `logs`, `events`, `completion`, and `manpage`. Global flags parse at every command level: `--json`, `--ndjson`, `--quiet`, `--no-color`, `--config`, `--profile`, `--timeout`, `--trace`, and `--verbose`.

The `agent` command now has implemented `add`, `remove`, `list`, `show`, `inspect`, `refresh`, and `rename` subcommands. Agent registry commands resolve the selected profile state paths, create directories, acquire the state mutation lock, open/migrate SQLite, sync config auth refs as non-secret rows, sync config-seeded agents as read-only rows, and persist local registry entries through `missive-store`. They support aliases, base URLs, explicit interface URLs, binding preference, config auth refs, tags, notes, metadata, human output, JSON output, NDJSON output, quiet mode, duplicate-alias checks, missing-agent diagnostics, and read-only protections for config-seeded agents.

`missive agent inspect <alias>` fetches and caches a public A2A Agent Card when needed, then renders the parsed provider, capabilities, skills, versions, and supported interfaces along with raw card JSON in machine output. `missive agent inspect <alias> --refresh` bypasses/revalidates the cache, and `missive agent refresh <alias>` explicitly refreshes the cached public card. Cached card JSON, ETags, Last-Modified values, and fetch timestamps are stored on the agent row and preserved for config-seeded agents while their configured base URL remains unchanged.

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
cargo check --workspace --all-targets
cargo test -p missive-a2a --all-targets
cargo test -p missive-cli --test agent_card_discovery --all-features
cargo test -p missive-cli --test help_snapshots --all-features
cargo clippy -p missive-a2a -p missive-cli --all-targets --all-features -- -D warnings
cargo test -p missive-cli --all-targets --all-features
cargo fmt --all
cargo test --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

The targeted checks covered Agent Card URL resolution, A2A card parsing/validation, local mock HTTP success, public card caching, ETag/Last-Modified persistence, conditional refresh headers, `--refresh` cache bypass, `agent refresh`, human/JSON inspect output, 404 handling, malformed JSON handling, TLS/HTTP transport errors, help snapshot updates, and workspace regression coverage.

Environment/tooling notes: no new cargo subcommands or OS packages were installed during this cycle. `Cargo.lock` now records `reqwest` and its transitive dependencies for the A2A public Agent Card HTTP client.

## Latest cycle notes

Implemented ticket 014 — Implement public Agent Card discovery.

Included:

* added `missive-a2a` Agent Card compatibility structs, parser/validator, summary helpers, discovery URL resolution, and a blocking rustls-backed `AgentCardClient`
* fetched public cards from `/.well-known/agent-card.json` using registered agent base URLs
* captured and persisted raw Agent Card JSON, fetch timestamps, ETags, and Last-Modified values on agent records
* added conditional refresh support using `If-None-Match` and `If-Modified-Since`, including `304 Not Modified` cache reuse
* added `missive agent inspect <alias> [--refresh]` and `missive agent refresh <alias>` in human, JSON, NDJSON, and quiet output modes
* rendered card provider, versions, capabilities, supported interfaces, default media modes, and skills for inspection
* preserved config-seeded Agent Card cache fields across sync when the config base URL is unchanged
* added local mock HTTP integration tests for success, cache use, cache refresh/bypass, refresh command, human output, 404, malformed JSON, and TLS/HTTP transport errors
* updated help snapshots, README, CLI docs, architecture docs, storage docs, and added `docs/protocol.md`

## Known blockers

None known.

## Limitations

A2A interface negotiation, protocol type integration with the official SDK, service parameter/version headers, authentication material resolution, outbound send/stream calls, task operations, and push/webhook/gateway behavior remain for later tickets.

Public Agent Card discovery currently expects unauthenticated cards. Config auth refs can be linked to agents but are not resolved or sent during card discovery yet.

The Agent Card compatibility structs are intentionally scoped to the public inspection fields needed now. The official/vendored A2A Rust type strategy and broader conformance fixture suite remain for later tickets.

Config-seeded agents are synced into SQLite as read-only rows when agent commands run. If a config entry is later removed, an already-synced row may remain in the local database until a future reconciliation/maintenance command defines stale config-seed pruning. Cached Agent Card fields are preserved only while the config-seeded base URL is unchanged.

The `missive` binary has a real command tree, global parser, configuration discovery/profile validation, output rendering contract, implemented agent registry commands, and public Agent Card inspection/refresh, but non-agent command behaviour is still intentionally skeletal. Real messaging, gateway behaviour, adapters, collectives, shell completion generation, and manpage generation remain for later tickets.

The store layer resolves state paths, provides process locks, migrates fresh SQLite databases to schema version 1, and exposes typed repository APIs for auth refs, agents, contexts, tasks, events, groups, group members, and gateway jobs. Message, artifact, push-config, and adapter-binding repositories, retention enforcement, compaction, event replay, and durable A2A protocol persistence beyond the agent-card cache remain for later tickets.

The store repository is synchronous because it uses `rusqlite`; async gateway/adapter code should call it through a blocking task or store worker when those tickets wire runtime behaviour.

The config schema includes gateway, adapter, and QoS defaults, but those values do not yet start a gateway, enforce timeouts, run adapters, or manage background jobs.

The `--json`, `--ndjson`, and `--quiet` flags override config output defaults. `--config` and `--profile` are active. The `--timeout`, `--trace`, `--verbose`, and `--no-color` flags are still parsed but do not yet drive timeout enforcement, tracing, verbose diagnostics, or color control.

Redaction is best-effort at the config and CLI output boundaries for structured values rendered through the current helpers. Trace/log redaction, storage redaction beyond schema design, and adapter/webhook trust boundaries remain for later security and observability tickets.

Detailed gateway operations, adapter lifecycle, collectives, testing, and runbook documentation remain for later implementation/documentation tickets.

Optional exhaustive validation tools are not all installed in this environment yet. The default quality gate passes without them and uses installed optional tools automatically. `cargo-nextest` is currently missing, and miri is unavailable for the active stable toolchain.

There is not yet a `cargo-deny` policy file; the quality gate skips deny checks until the supply-chain policy ticket introduces one.

## Next recommended ticket

Ticket 015 — Implement A2A interface negotiation.
