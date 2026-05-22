# Architecture

`missive` is currently in its foundation phase. The high-level architecture follows the project brief and is being built ticket by ticket; this document links the initial architecture decisions that constrain later implementation.

## Current workspace boundaries

```text
crates/missive-cli        -> command parsing, output rendering, exit codes
crates/missive-core       -> domain types, errors, config, IDs, envelopes
crates/missive-a2a        -> A2A protocol/client integration and compatibility fixtures
crates/missive-store      -> state paths, process locks, SQLite migrations and repository APIs
crates/missive-router     -> agent selection, policies, groups, collectives
crates/missive-gateway    -> daemon, subscriptions, webhooks, jobs, sessions
crates/missive-adapters   -> stdin/stdout, file, HTTP, future chat adapters
crates/missive-observe    -> tracing, logs, diagnostics, event export helpers
```

Recommended flow from the project brief:

```text
CLI/adapters -> command model -> router/session/context -> A2A client -> remote agent
                                           |              -> store/events/artifacts
                                           |              -> gateway jobs/subscriptions/webhooks
```

## Error handling contract

`crates/missive-core` exposes the shared `MissiveError` and `Result<T>` types for public APIs. Errors are categorized as I/O, configuration, protocol, transport, storage, authentication, validation, or orchestration failures. Each category has a stable diagnostic code such as `missive::validation`, a deterministic CLI exit code reserved for later CLI mapping, and a serializable `ErrorReport` shape for JSON/NDJSON output.

Human-facing rendering uses normal `Display` text plus `miette::Diagnostic` codes and optional help text. Machine-facing rendering should serialize `ErrorReport` instead of parsing human messages.

## CLI command skeleton

`crates/missive-cli` owns the clap-derived `Cli`, `GlobalArgs`, and `Commands` types. The skeleton currently exposes top-level commands for `agent`, `send`, `stream`, `task`, `context`, `group`, `gateway`, `webhook`, `push`, `doctor`, `logs`, `events`, `completion`, and `manpage`.

Global flags are parsed at every command level: `--json`, `--ndjson`, `--quiet`, `--no-color`, `--config`, `--profile`, `--timeout`, `--trace`, and `--verbose`. `--config` and `--profile` now feed the core configuration loader; timeout enforcement and tracing remain for later tickets.

`crates/missive-core` owns configuration discovery and schema validation. Discovery precedence is explicit `--config`, `MISSIVE_CONFIG`, repository-local config when `MISSIVE_REPO_CONFIG=1`, XDG config locations, then built-in defaults. The schema covers profiles, config-seeded agents, auth refs, storage defaults, output defaults, gateway defaults, adapters, and quality-of-service settings. Config parsing and redacted rendering live in core so future CLI, gateway, adapter, and diagnostic code can share the same validation and secret-handling contract.

`crates/missive-cli` also owns the initial output rendering contract:

* human mode writes redacted terminal status text
* `--json` writes one pretty-printed JSON envelope with stable fields `schema_version`, `ok`, `kind`, and `data`
* `--ndjson` writes one compact JSON envelope per line and adds `sequence`
* `--quiet` suppresses non-error output

If no explicit output flag is present, the loaded config's effective `output.format` selects the default renderer. The current envelope schema marker is `missive.output.v1`. Successful skeletal commands emit `kind: "command_status"` with a secret-free config source summary; structured errors emit `kind: "error"` with `missive-core`'s `ErrorReport` under `data`. The renderer recursively redacts secret-like JSON fields and HTTP-style auth headers before writing machine output.

Help output for the top-level CLI and key commands is covered by snapshot tests under `crates/missive-cli/tests/snapshots/`. JSON, NDJSON, quiet-mode, error-shape, and redaction behavior is covered by output contract tests under `crates/missive-cli/tests/output_contract.rs`.

## Core primitive contract

`crates/missive-core` owns the small domain primitives that other crates should share instead of passing raw strings everywhere:

* `AgentAlias`, `GroupName`, `RankName`, and `TransportName` are validated lowercase identifiers with deterministic `Display`, `FromStr`, and serde-as-string behaviour.
* `ContextId`, `TaskId`, `MessageId`, and `EventId` are opaque A2A/local identifiers that reject empty values, whitespace, control characters, and unbounded strings while preserving the server-provided text exactly.
* `MissiveTimestamp` renders and parses RFC3339 timestamps for durable records and machine-readable output.
* `Metadata` is a deterministic JSON object backed by an ordered map, with helper methods for insertion, lookup, merge, and key validation.
* `Envelope<T>` combines an event id, timestamp, metadata, and typed payload for later event journals, gateway jobs, and adapter streams.
* `MissiveConfig`, `ConfigDiscovery`, and `LoadedConfig` define configuration discovery, validated schema objects, selected-profile handling, output defaults, and redacted config rendering.

Identifier and metadata validation failures use `MissiveError::validation`; configuration discovery, parsing, and schema failures use `MissiveError::config` so CLI and JSON renderers can produce consistent diagnostics and exit codes.

## Store path and lock contract

`crates/missive-store` resolves local data, state, cache, SQLite database, and lock paths from the loaded config and selected profile. Runtime state defaults to XDG-compatible directories on Linux and other Unix-like platforms, macOS `~/Library` fallbacks when XDG variables are absent, or `MISSIVE_HOME` when explicitly set. Every default path includes `profiles/<profile>` so different profiles do not share mutable state accidentally.

Path resolution is side-effect free: directories are created only when store or lock code explicitly calls `StatePaths::ensure_directories()` or acquires a process lock. Relative `storage.database_path` values are resolved beneath the selected profile state directory rather than the current working directory, which keeps default runtime state out of the source tree.

The store layer exposes two process lock kinds for later tickets: `state.lock` for state mutations/migrations and `gateway.lock` for one gateway daemon per profile. Locks use OS-level whole-file locking and nonblocking acquisition maps lock contention to a storage error with deterministic diagnostics.

`crates/missive-store` also owns the embedded SQLite migration strategy. Migration SQL files live under `crates/missive-store/migrations`, are recorded in a `schema_migrations` ledger with checksums, and are applied in version order inside transactions. The current schema version is `1` and creates tables for agents, contexts, tasks, messages, artifacts, events, groups, group members, auth refs, push configs, gateway jobs, and adapter bindings. See [`docs/storage.md`](storage.md) for table purpose and retention notes.

## Architecture decision records

The ADR index lives in [`docs/adr/`](adr/README.md). Initial accepted records are:

* [ADR 0001 — Rust workspace structure](adr/0001-rust-workspace-structure.md)
* [ADR 0002 — A2A-first protocol strategy](adr/0002-a2a-first-protocol-strategy.md)
* [ADR 0003 — SQLite local state](adr/0003-sqlite-local-state.md)
* [ADR 0004 — CLI-first UX](adr/0004-cli-first-ux.md)

These records are intentionally scoped to project-defining decisions. Detailed protocol mappings, storage schema, gateway operation, adapters, collectives, security, and runbook documentation will be expanded by their own implementation tickets.
