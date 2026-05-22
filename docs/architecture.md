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

`crates/missive-cli` owns the clap-derived `Cli`, `GlobalArgs`, and `Commands` types. The CLI currently exposes top-level commands for `agent`, `send`, `stream`, `task`, `context`, `group`, `gateway`, `webhook`, `push`, `doctor`, `logs`, `events`, `completion`, and `manpage`. The `agent` command has implemented registry subcommands for `add`, `remove`, `list`, `show`, `inspect`, `refresh`, and `rename`; `send` performs non-streaming A2A `SendMessage` calls; `stream` performs A2A `SendStreamingMessage` calls over SSE; `task` implements local/remote `get`, `list`, polling `wait`, and remote `cancel`; `context` implements local `create`, `list`, `show`, `fork`, `close`, and redacted `export`; other top-level commands remain skeletal until their ordered tickets land.

Global flags are parsed at every command level: `--json`, `--ndjson`, `--quiet`, `--no-color`, `--config`, `--profile`, `--timeout`, `--protocol-version`, `--a2a-extension`, `--service-param`, `--bearer-token-env`, `--header`, `--trace`, and `--verbose`. `--config`, `--profile`, A2A service-parameter flags, and auth header flags now feed implemented Agent Card, send, stream, and task requests; task wait uses global `--timeout` for its polling budget, while tracing remains for later tickets.

`crates/missive-core` owns configuration discovery and schema validation. Discovery precedence is explicit `--config`, `MISSIVE_CONFIG`, repository-local config when `MISSIVE_REPO_CONFIG=1`, XDG config locations, then built-in defaults. The schema covers profiles, config-seeded agents, auth refs, storage defaults, output defaults, gateway defaults, adapters, and quality-of-service settings. Config parsing and redacted rendering live in core so future CLI, gateway, adapter, and diagnostic code can share the same validation and secret-handling contract.

`crates/missive-cli` also owns the initial output rendering contract:

* human mode writes redacted terminal status text
* `--json` writes one pretty-printed JSON envelope with stable fields `schema_version`, `ok`, `kind`, and `data`
* `--ndjson` writes one compact JSON envelope per line and adds `sequence`
* `--quiet` suppresses non-error output

If no explicit output flag is present, the loaded config's effective `output.format` selects the default renderer. The current envelope schema marker is `missive.output.v1`. Successful skeletal commands emit `kind: "command_status"` with a secret-free config source summary; structured errors emit `kind: "error"` with `missive-core`'s `ErrorReport` under `data`. The renderer recursively redacts secret-like JSON fields and HTTP-style auth headers before writing machine output. The CLI auth resolver keeps raw token values in memory only long enough to build outbound request headers.

Help output for the top-level CLI and key commands is covered by snapshot tests under `crates/missive-cli/tests/snapshots/`. JSON, NDJSON, quiet-mode, error-shape, and redaction behavior is covered by output contract tests under `crates/missive-cli/tests/output_contract.rs`. Agent registry command behavior, duplicate aliases, missing agents, alias validation, renaming, and config-seeded read-only entries are covered by `crates/missive-cli/tests/agent_registry.rs`. Public Agent Card discovery, cache use, cache refresh, interface negotiation, auth header application/redaction, missing auth environment variables, 404 responses, malformed JSON, and TLS/HTTP transport failures are covered by `crates/missive-cli/tests/agent_card_discovery.rs` using local mock HTTP fixtures. Non-streaming send direct-message responses, task responses, stdin/file input, request headers/body shape, and SQLite linkage are covered by `crates/missive-cli/tests/send_command.rs`. Streaming status updates, artifact updates, completion, cancellation, malformed SSE events, capability validation, `--force`, NDJSON output, and SQLite event/message/task persistence are covered by `crates/missive-cli/tests/stream_command.rs`. Task local filtering, remote `ListTasks`, polling state transitions, cancellation, timeout handling, and deterministic wait exit codes are covered by `crates/missive-cli/tests/task_command.rs`. Context name resolution, create/list/show, fork parent metadata, local close state, redacted export, and relation counts are covered by `crates/missive-cli/tests/context_command.rs`.

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

`crates/missive-a2a` currently owns the Agent Card discovery client, non-streaming SendMessage client, streaming SendStreamingMessage SSE client, task GetTask/ListTasks/CancelTask client, the official A2A Rust protocol type boundary, A2A service-parameter handling, resolved auth-header application, a small Agent Card compatibility parser, and the first interface negotiation helper. The crate depends on the official `a2a-lf` package from `a2aproject/a2a-rs`, re-exports protocol models from `missive_a2a::protocol`, and aliases Agent Card/message/task/stream inspection types to the official SDK structs rather than keeping duplicate local models. The discovery client resolves `/.well-known/agent-card.json` from the registered base URL, uses `reqwest` with rustls TLS, applies `A2A-Version` plus optional `A2A-Extensions`, extra service-parameter headers, and resolved auth headers, preserves ETag/Last-Modified validators, parses the raw JSON into the official Agent Card type plus a typed summary, maps malformed card JSON and A2A `VERSION_NOT_SUPPORTED` responses to protocol errors, and maps other HTTP/TLS/network failures to transport errors. The send client maps `http+json` to `POST <interface>/message:send` with `application/a2a+json`, maps `json-rpc` to JSON-RPC method `SendMessage`, applies the same service/auth headers, and parses official `SendMessageResponse` message-or-task shapes. The stream client maps `http+json` to `POST <interface>/message:stream`, maps `json-rpc` to method `SendStreamingMessage`, reads `text/event-stream` responses incrementally, accepts direct or JSON-RPC-wrapped official `StreamResponse` payloads, and surfaces malformed stream events as protocol errors. The task client maps `http+json` to `GET <interface>/tasks/{id}`, `GET <interface>/tasks`, and `POST <interface>/tasks/{id}:cancel`, maps `json-rpc` to methods `GetTask`, `ListTasks`, and `CancelTask`, applies the same service/auth headers, and parses official `Task`/`ListTasksResponse` payloads. The compatibility parser normalizes snake_case fixture aliases and older cards that omit `supportedInterfaces` before official deserialization; raw card JSON is still cached and rendered. Interface negotiation canonicalizes Agent Card bindings such as `HTTP+JSON` and `JSONRPC` to missive's `http+json`/`json-rpc` names, respects the agent row's ordered preference or an explicit CLI `--binding` override, recognizes gRPC as a future extension point, and falls back to registry/base URL interfaces for older cards that omit `supportedInterfaces`.

`crates/missive-store` also owns the embedded SQLite migration strategy. Migration SQL files live under `crates/missive-store/migrations`, are recorded in a `schema_migrations` ledger with checksums, and are applied in version order inside transactions. The current schema version is `1` and creates tables for agents, contexts, tasks, messages, artifacts, events, groups, group members, auth refs, push configs, gateway jobs, and adapter bindings.

The store crate now exposes a blocking typed repository facade, `Store`, for one migrated profile database. Repository methods cover auth refs, agents, contexts, tasks, messages, events, groups/group members, and gateway jobs using `missive-core` identifiers, `MissiveTimestamp`, `Metadata`, typed state/source/direction/role enums, and validated store-specific ids. SQL remains private to `missive-store`; CLI, gateway, adapter, and router code should use the repository APIs rather than constructing queries. `missive agent` resolves the selected profile state path, acquires the state mutation lock, opens/migrates the SQLite store, syncs config auth refs as non-secret rows, syncs config-seeded agents as read-only rows, and then performs registry/card-cache operations through these APIs. Agent Card refresh updates the existing agent row's raw card JSON, ETag, Last-Modified value, and fetch timestamp while preserving registry fields. `missive send` uses the same profile/lock path, negotiates an interface, sends one request, and transactionally records context/task/message linkage. `missive stream` uses the same registry path, validates streaming capability, persists the request, and appends event/message/task updates as each SSE event is parsed. `missive task` uses the same registry path to filter local task rows, persist remote `Task` payloads returned by get/list/wait/cancel, and update task completion timestamps for terminal states. `missive context` uses the same profile store to create named contexts, resolve selectors by id or unique name, record parent links for forks, mark contexts closed locally, count linked tasks/messages/events, and render redacted context exports. `Store::transaction` provides the transactional update helper for multi-row changes, including agent rename, send/stream persistence, and remote task persistence, and rolls back if the closure returns an error or a SQLite constraint fails. Because the implementation uses synchronous `rusqlite`, async components should call it from a blocking task or store worker.

See [`docs/storage.md`](storage.md) for table purpose, retention notes, and repository API details.

## Architecture decision records

The ADR index lives in [`docs/adr/`](adr/README.md). Initial accepted records are:

* [ADR 0001 — Rust workspace structure](adr/0001-rust-workspace-structure.md)
* [ADR 0002 — A2A-first protocol strategy](adr/0002-a2a-first-protocol-strategy.md)
* [ADR 0003 — SQLite local state](adr/0003-sqlite-local-state.md)
* [ADR 0004 — CLI-first UX](adr/0004-cli-first-ux.md)
* [ADR 0005 — Official A2A Rust protocol types](adr/0005-official-a2a-rust-types.md)

These records are intentionally scoped to project-defining decisions. Detailed protocol mappings, storage schema, gateway operation, adapters, collectives, security, and runbook documentation will be expanded by their own implementation tickets.
