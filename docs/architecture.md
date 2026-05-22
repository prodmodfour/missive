# Architecture

`missive` is currently in its foundation phase. The high-level architecture follows the project brief and is being built ticket by ticket; this document links the initial architecture decisions that constrain later implementation.

## Current workspace boundaries

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

Global flags are parsed at every command level: `--json`, `--ndjson`, `--quiet`, `--no-color`, `--config`, `--profile`, `--timeout`, `--trace`, and `--verbose`. The flags establish a stable invocation contract before later tickets implement rendering, configuration discovery, timeout enforcement, and tracing.

Help output for the top-level CLI and key commands is covered by snapshot tests under `crates/missive-cli/tests/snapshots/`.

## Core primitive contract

`crates/missive-core` owns the small domain primitives that other crates should share instead of passing raw strings everywhere:

* `AgentAlias`, `GroupName`, `RankName`, and `TransportName` are validated lowercase identifiers with deterministic `Display`, `FromStr`, and serde-as-string behaviour.
* `ContextId`, `TaskId`, `MessageId`, and `EventId` are opaque A2A/local identifiers that reject empty values, whitespace, control characters, and unbounded strings while preserving the server-provided text exactly.
* `MissiveTimestamp` renders and parses RFC3339 timestamps for durable records and machine-readable output.
* `Metadata` is a deterministic JSON object backed by an ordered map, with helper methods for insertion, lookup, merge, and key validation.
* `Envelope<T>` combines an event id, timestamp, metadata, and typed payload for later event journals, gateway jobs, and adapter streams.

Validation failures use `MissiveError::validation` so CLI and JSON renderers can produce consistent diagnostics.

## Architecture decision records

The ADR index lives in [`docs/adr/`](adr/README.md). Initial accepted records are:

* [ADR 0001 — Rust workspace structure](adr/0001-rust-workspace-structure.md)
* [ADR 0002 — A2A-first protocol strategy](adr/0002-a2a-first-protocol-strategy.md)
* [ADR 0003 — SQLite local state](adr/0003-sqlite-local-state.md)
* [ADR 0004 — CLI-first UX](adr/0004-cli-first-ux.md)

These records are intentionally scoped to project-defining decisions. Detailed protocol mappings, storage schema, gateway operation, adapters, collectives, security, and runbook documentation will be expanded by their own implementation tickets.
