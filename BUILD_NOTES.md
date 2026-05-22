# BUILD_NOTES.md

## Current state

Tickets 000 through 020 are complete. The repository uses the target Cargo workspace layout for `missive` with these crates:

* `crates/missive-cli` — package `missive-cli`, binary `missive`, clap-derived CLI tree, global flags, configuration loading from CLI/env/discovery, A2A service-parameter CLI overrides, authentication input resolution for implemented Agent Card/send/stream requests, output rendering and redaction helpers, help snapshots, implemented `missive agent add/list/show/inspect/refresh/remove/rename`, implemented non-streaming `missive send`, implemented streaming `missive stream`, and placeholder execution status for later commands
* `crates/missive-core` — core domain primitive scaffolding, including shared error/result types, strongly typed IDs, timestamps, metadata maps and A2A metadata keys, envelopes, configuration schema, protocol service-parameter defaults, config auth-ref schema, config discovery, profile validation, and redacted config rendering
* `crates/missive-a2a` — A2A protocol/client integration scaffolding, official `a2a-lf` protocol type re-exports, Agent Card discovery/parsing helpers for `/.well-known/agent-card.json`, A2A service-parameter request handling, resolved auth-header request handling, A2A interface negotiation helpers, non-streaming SendMessage HTTP+JSON/JSON-RPC client support, streaming SendStreamingMessage HTTP+JSON/JSON-RPC SSE client support, and A2A serde fixture/parser tests
* `crates/missive-store` — persistence scaffolding with local state path resolution, profile-specific data/state/cache directories, SQLite database path resolution, process locks for state mutation and gateway operation, embedded SQLite schema migrations, a blocking typed repository facade for non-secret auth refs, agents, contexts, tasks, messages, events, groups, group members, and gateway jobs, plus helpers to record A2A protocol versions in task/event metadata and message rows that can carry protocol metadata
* `crates/missive-router` — routing and collectives scaffolding
* `crates/missive-gateway` — gateway daemon scaffolding
* `crates/missive-adapters` — adapter scaffolding
* `crates/missive-observe` — observability scaffolding

The root `Cargo.toml` is a virtual workspace manifest with shared workspace package metadata and shared dependency versions for planned foundational Rust crates. The store layer depends on `rusqlite` with bundled SQLite plus `serde`/`serde_json` for typed JSON repository boundaries. The A2A layer depends on the official `a2a-lf` crate from `a2aproject/a2a-rs` for protocol types and on `reqwest` with blocking rustls-backed HTTP/TLS support for Agent Card discovery, non-streaming sends, and SSE streaming sends. The CLI directly depends on the workspace `url` crate for registry URL validation and has a default `native-keyring` feature using the Rust `keyring` crate for platform keyring-backed auth refs where available.

Autonomous build tooling is documented in `docs/tooling.md`. `scripts/bootstrap-tools.sh` is executable, idempotent, supports `--check`, and can install Rust components, optional cargo tools, and opt-in system dependencies.

`scripts/quality-gate.sh` is the hardened default gate for autonomous cycles. It runs shell checks, secret and generated/private-file guardrails, Rust feature checks, formatting, clippy with warnings denied, workspace tests, doc tests, docs with warnings denied, debug/release builds, and optional installed dependency checks. `MISSIVE_AGGRESSIVE_TESTS=1` enables deeper optional checks without editing the script.

Architecture decision records live under `docs/adr/`, with a template and accepted ADRs for Rust workspace structure, A2A-first protocol strategy, SQLite local state, CLI-first UX, and the official A2A Rust protocol type strategy. `docs/architecture.md` links the ADRs and records the current high-level crate boundaries, shared error handling contract, core primitive contract, CLI command/agent-registry/Agent-Card/send/stream/service-parameter/auth contract, configuration contract, output rendering contract, store path/lock contract, SQLite migration contract, typed repository contract, and A2A type boundary. `docs/protocol.md` documents the current official Rust type boundary, Agent Card discovery, non-streaming SendMessage mapping, streaming SendStreamingMessage mapping, service-parameter handling, auth-header handling, interface negotiation mapping, error mapping, and fixture update process. `docs/security.md` documents current auth inputs, keyring support, storage tradeoffs, redaction, and limitations.

`missive-core` exposes `MissiveError`, `Result<T>`, `ErrorCategory`, `MissiveExitCode`, and `ErrorReport`. The error taxonomy covers I/O, configuration, protocol, transport, storage, authentication, validation, and orchestration failures. Each category has a stable diagnostic code, deterministic exit code mapping for CLI use, human `Display` rendering, `miette::Diagnostic` metadata, and a serializable JSON/NDJSON report shape.

`missive-core` also exposes `AgentAlias`, `GroupName`, `RankName`, `TransportName`, `ContextId`, `TaskId`, `MessageId`, `EventId`, `MissiveTimestamp`, `Metadata`, `Envelope<T>`, and A2A metadata keys such as `a2a.protocol_version`. Named identifiers validate lowercase CLI-safe forms; opaque A2A/local IDs preserve server-provided text while rejecting empty, whitespace, control, or unbounded values. `Metadata` uses deterministic JSON object ordering, and `MissiveTimestamp` uses canonical RFC3339 UTC rendering.

The core configuration layer exposes `MissiveConfig`, `ConfigDiscovery`, `LoadedConfig`, and schema structs for profiles, agents, auth refs, storage, output, protocol service parameters, gateway, adapters, and quality of service. Discovery precedence is `--config`, `MISSIVE_CONFIG`, repository-local config when `MISSIVE_REPO_CONFIG=1`, XDG config locations, then built-in defaults. Config parsing rejects unknown fields, validates cross references, rejects embedded credentials in URLs, rejects attempts to redefine reserved `A2A-Version`/`A2A-Extensions` service-parameter names in arbitrary parameter maps, validates env/keyring auth refs without accepting raw token values, and provides redacted JSON rendering.

The store path layer exposes `StatePathResolver`, `StatePaths`, `StatePlatform`, `StatePathSource`, `ProcessLockKind`, and `ProcessLock`. Runtime state defaults to XDG-compatible roots on Linux/Unix-like platforms, macOS `~/Library` fallbacks when XDG variables are absent, or `MISSIVE_HOME` when explicitly set. Paths include `profiles/<profile>`, relative database paths resolve under the selected profile state directory, and lock files live under `<state-dir>/locks/`.

The store migration layer exposes `Migration`, `AppliedMigration`, `MigrationReport`, `embedded_migrations`, `open_sqlite_database`, `migrate_database`, `migrate_connection`, `applied_migrations`, `schema_version`, `CURRENT_SCHEMA_VERSION`, and `SQLITE_APPLICATION_ID`. Migration SQL lives under `crates/missive-store/migrations`, is applied in version order inside transactions, and records checksums in `schema_migrations`. Schema version 1 creates tables for agents, contexts, tasks, messages, artifacts, events, groups, group_members, auth_refs, push_configs, gateway_jobs, and adapter_bindings.

The store repository layer exposes blocking `Store` and `StoreTransaction` APIs. `Store::open`, `Store::from_connection`, and `Store::open_in_memory` apply migrations before use. Typed methods cover non-secret auth refs, agents, contexts, tasks, messages, events, groups/group members, and gateway jobs with public upsert/insert/record structs, state/source/direction/role enums, core identifiers, validated `GatewayJobId`/`AdapterBindingId`, JSON serialization at the repository boundary, A2A protocol-version metadata helpers for tasks/events plus message metadata storage, and transaction rollback on closure or SQL failures. SQL strings remain private to `missive-store` rather than leaking into CLI code. Raw auth tokens are not stored in SQLite; auth-ref rows contain only env var names or keyring service/account coordinates.

The A2A layer exposes the official `a2a-lf` protocol models through `missive_a2a::protocol`, aliases Agent Card/message/task/stream types to official SDK structs, provides `AgentCardClient`, `SendMessageClient`, and `StreamMessageClient`, includes interface negotiation helpers, centralizes A2A service parameters in `ServiceParameters`, and applies resolved auth headers through `AuthHeaders`. It resolves `/.well-known/agent-card.json` from a registered base URL, sends `A2A-Version` on every implemented HTTP request, optionally sends `A2A-Extensions` and validated extra service-parameter headers, marks auth header values sensitive before sending, sends conditional refresh headers when cached ETags/Last-Modified values exist, parses supported interfaces, provider, versions, capabilities, default modes, and skills through the official Agent Card type, maps HTTP/TLS/network failures to transport errors, maps invalid card JSON to protocol errors, and maps A2A `VERSION_NOT_SUPPORTED` responses to protocol errors with exit code 76. Non-streaming send maps `http+json` to `POST <interface>/message:send` with `application/a2a+json`, maps `json-rpc` to JSON-RPC method `SendMessage`, and parses direct `Message` and `Task` response shapes. Streaming send maps `http+json` to `POST <interface>/message:stream`, maps `json-rpc` to JSON-RPC method `SendStreamingMessage`, parses SSE `data` records incrementally, accepts direct or JSON-RPC-wrapped `StreamResponse` payloads, and rejects malformed stream events with protocol diagnostics that include the event sequence. Interface negotiation canonicalizes Agent Card bindings such as `HTTP+JSON` and `JSONRPC`, supports local `http+json` and `json-rpc`, recognizes gRPC for future extension diagnostics, respects agent binding preference or `agent inspect --binding`, and falls back to registry/base-URL interfaces when older cards omit `supportedInterfaces`.

The `missive` binary uses clap derive and exposes help pages for `agent`, `send`, `stream`, `task`, `context`, `group`, `gateway`, `webhook`, `push`, `doctor`, `logs`, `events`, `completion`, and `manpage`. Global flags parse at every command level: `--json`, `--ndjson`, `--quiet`, `--no-color`, `--config`, `--profile`, `--timeout`, `--protocol-version`, `--a2a-extension`, `--service-param`, `--bearer-token-env`, `--header`, `--trace`, and `--verbose`.

The `agent` command has implemented `add`, `remove`, `list`, `show`, `inspect`, `refresh`, and `rename` subcommands. Agent registry commands resolve the selected profile state paths, create directories, acquire the state mutation lock, open/migrate SQLite, sync config auth refs as non-secret rows, sync config-seeded agents as read-only rows, and persist local registry entries through `missive-store`. They support aliases, base URLs, explicit interface URLs, binding preference, config auth refs, tags, notes, metadata, human output, JSON output, NDJSON output, quiet mode, duplicate-alias checks, missing-agent diagnostics, and read-only protections for config-seeded agents.

`missive agent inspect <alias>` fetches and caches an A2A Agent Card when needed, applies configured and CLI-overridden A2A service parameters to the fetch, resolves and sends auth headers from agent config auth refs, `--bearer-token-env`, and repeatable `--header Name:Value`, negotiates the selected interface from `supportedInterfaces` and the agent binding preference, then renders the parsed provider, capabilities, skills, versions, supported interfaces, selected interface, and raw card JSON in machine output. `missive agent inspect <alias> --binding <binding>` requires a specific locally supported binding for advanced users/tests, `missive agent inspect <alias> --refresh` bypasses/revalidates the cache, and `missive agent refresh <alias>` explicitly refreshes the cached card.

`missive send <agent> [message]` sends one non-streaming A2A message to a registered agent. It supports positional text, `--stdin`, repeatable UTF-8 text `--file`, repeatable `--part text=...`, repeatable non-secret `--metadata KEY=VALUE`, `--context`, `--task`, repeatable `--accepted-output-mode`, and the existing global output/auth/service-parameter flags. It uses the cached Agent Card when present, otherwise fetches and caches the public card before negotiation. It persists request and response rows in `messages`; direct `Message` responses are stored as response messages, and `Task` responses are upserted in `tasks` with remote task JSON, state, context/task linkage, protocol-version metadata, and a linked response row. Machine output uses `kind: "send_result"` and includes request, response, selected-interface, and persistence summaries.

`missive stream <agent> [message]` sends one A2A streaming message to a registered agent. It shares the current text-only input flags with `send`, validates the Agent Card's `capabilities.streaming` unless `--force` is passed, negotiates HTTP+JSON or JSON-RPC, opens an SSE response, renders one human/NDJSON update per parsed event as it arrives, and emits a final `stream_result` summary. Each parsed `task`, `message`, `statusUpdate`, or `artifactUpdate` is appended to the SQLite event journal with event types under `a2a.stream.*`, stored as a `messages` row with direction `stream_event`, and linked to task/context rows where IDs are present. Status and task events update the local task state and completion timestamp for completed/failed/cancelled states.

The CLI auth resolver reads bearer tokens from environment variables, resolves config env/keyring auth refs, accepts one-off `--header Name:Value` values, and returns `missive::auth` errors with exit code 77 when required auth material is unavailable. CLI-supplied header values and resolved tokens are kept in memory for outbound requests only and are not persisted. `AuthHeaders` debug rendering is redacted, reqwest header values are marked sensitive, and normal output rendering redacts authorization, token, API key, password, cookie, and secret-like fields.

The current CLI output contract supports human, JSON, NDJSON, and quiet renderers. Implemented agent commands emit command-specific kinds such as `agent_add`, `agent_list`, `agent_show`, `agent_inspect`, `agent_refresh`, `agent_remove`, and `agent_rename`; send emits `send_result`; stream emits `stream_event` and `stream_result` in NDJSON and one `stream_result` in JSON. Skeletal commands load/validate config, then emit a stable `missive.output.v1` `command_status` envelope in machine-readable modes. Structured execution errors render as `kind: "error"` envelopes when `--json` or `--ndjson` is active. The renderer recursively redacts secret-like JSON fields and HTTP-style authorization headers before writing output.

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
cargo check -p missive-a2a -p missive-cli --all-targets --all-features
cargo fmt --all
cargo test -p missive-cli --test stream_command --all-features
cargo clippy -p missive-a2a -p missive-cli --all-targets --all-features -- -D warnings
cargo test -p missive-cli --all-targets --all-features
cargo test -p missive-a2a --all-targets --all-features
cargo check -p missive-cli --all-targets --no-default-features
cargo test --workspace --all-targets --all-features
scripts/quality-gate.sh
```

The targeted checks covered the new stream command parser, shared text/stdin/file input preparation, Agent Card streaming capability validation, `--force`, A2A request body and headers, SSE status updates, artifact updates, completion, cancellation, malformed events, direct and JSON-RPC-wrapped stream event parsing, NDJSON event sequencing, SQLite message/event/task linkage, no-default CLI compilation, output contract updates, and workspace regression coverage.

Environment/tooling notes: no new cargo subcommands, Rust components, OS packages, or Rust dependencies were installed during this cycle.

## Latest cycle notes

Implemented ticket 020 — Implement streaming message command.

Included:

* added `missive stream <agent> [message]` with the same current text-only inputs as `send`, plus `--force` for interoperability testing when an Agent Card does not advertise streaming
* added `StreamMessageClient` in `missive-a2a` for HTTP+JSON `message:stream` and JSON-RPC `SendStreamingMessage` over SSE
* parsed SSE records incrementally and accepted both direct A2A `StreamResponse` payloads and JSON-RPC response envelopes whose `result` is a stream response
* rejected malformed SSE data, JSON-RPC stream errors, and unknown stream response variants with protocol diagnostics
* validated `capabilities.streaming = true` before stream attempts unless `--force` is used
* sent `A2A-Version`, requested extensions/service parameters, and resolved auth headers on stream requests and Agent Card fetches
* persisted the outbound stream request message plus one `messages` row and one redacted `events` journal row for each parsed stream event
* updated local context/task rows from task/status/message/artifact stream IDs and mapped completed/failed/cancelled task states to completion timestamps
* rendered human streaming updates as they arrive, NDJSON `stream_event` envelopes with stable sequence numbers, and final `stream_result` summaries for JSON/NDJSON/human modes
* added local mock SSE integration tests covering status updates, artifact updates, completion, cancellation, malformed events, streaming capability validation, `--force`, request body/header shape, NDJSON output, and persisted SQLite state
* updated README plus CLI, configuration, protocol, architecture, storage, and security docs

## Known blockers

None known.

## Limitations

`missive stream` implements initial A2A `SendStreamingMessage` only. It does not implement task resubscription/`SubscribeToTask`, gateway-managed streaming jobs, background resume after process restart, local user-triggered cancellation of an active stream, or `missive task get/list/wait/cancel`; those remain for later task/gateway tickets.

Streaming input is intentionally text-only for this ticket, matching `missive send`: positional text, stdin text, UTF-8 file text, and `--part text=...` are supported. Binary file bytes, MIME-specific file references, JSON structured-data parts, size-limit profiles, and richer part parsing remain for ticket 023.

Streaming artifact updates are persisted as redacted event payloads and `stream_event` message rows, but the dedicated `artifacts` table is not populated yet and there are no artifact list/show/save/export commands. Artifact handling/export remains for ticket 024.

Stream status/task events update the local task row state, but there is still no task polling, waiting, cancellation command, event replay command, or artifact reconstruction command. Event journal read/tail/replay/export behavior remains for ticket 025 and task operations remain for ticket 021.

`missive stream --json` buffers parsed event summaries until the stream closes so it can print one JSON document. Use `--ndjson` when another agent needs incremental machine-readable output. If a remote stream errors mid-flight, earlier human/NDJSON output and earlier persisted rows remain as a truthful partial stream record.

The `--timeout` global flag is still parsed but does not yet drive command-specific timeout configuration. The streaming client currently uses the A2A layer's default blocking reqwest timeout; configurable timeout enforcement remains for a later cross-cutting CLI/runtime ticket.

Authentication is wired into implemented Agent Card fetch/refresh, non-streaming send, and streaming send HTTP paths and exposed as reusable helpers for future task/push clients. Push/webhook/gateway behavior, adapters, collectives, and observability remain for later tickets and must reuse the same auth/redaction path when they add network calls.

Keyring-backed auth refs can be resolved when the `native-keyring` feature is enabled and the local platform/session keyring is available, but missive does not yet provide commands to create, update, list, or delete keyring entries. Users must provision those entries with OS tooling or another keyring client. Builds without `native-keyring` parse keyring refs but fail clearly if one is needed.

There is no local-only insecure raw-token storage mode. SQLite auth-ref rows intentionally store only env var names or keyring service/account coordinates. If an insecure mode is ever added, it must be explicit and documented in a later security/storage ticket.

Config-seeded agents are synced into SQLite as read-only rows when agent/send/stream commands run. If a config entry is later removed, an already-synced row may remain in the local database until a future reconciliation/maintenance command defines stale config-seed pruning. Cached Agent Card fields are preserved only while the config-seeded base URL remains unchanged.

The `missive` binary has a real command tree, global parser, configuration discovery/profile validation, A2A service-parameter flags, auth input flags, output rendering contract, implemented agent registry commands, Agent Card inspection/refresh, selected-interface negotiation, non-streaming send, and streaming send. Task list/get/wait/cancel, contexts, gateway behaviour, adapters, collectives, shell completion generation, and manpage generation remain for later tickets.

The store layer resolves state paths, provides process locks, migrates fresh SQLite databases to schema version 1, and exposes typed repository APIs for auth refs, agents, contexts, tasks, messages, events, groups, group members, and gateway jobs. Artifact, push-config, and adapter-binding repositories, retention enforcement, compaction, event replay commands, and broader durable A2A protocol persistence remain for later tickets.

The store repository is synchronous because it uses `rusqlite`; async gateway/adapter code should call it through a blocking task or store worker when those tickets wire runtime behaviour.

The config schema includes protocol, auth refs, gateway, adapter, and QoS defaults, but gateway/adapter/QoS values do not yet start a gateway, enforce timeouts, run adapters, or manage background jobs.

The `--json`, `--ndjson`, and `--quiet` flags override config output defaults. `--config`, `--profile`, `--protocol-version`, `--a2a-extension`, `--service-param`, `--bearer-token-env`, and `--header` are active for implemented Agent Card, send, and stream paths. The `--timeout`, `--trace`, `--verbose`, and `--no-color` flags are still parsed but do not yet drive timeout enforcement, tracing, verbose diagnostics, or color control.

Redaction is active at the config and CLI output boundaries for structured values rendered through the current helpers, and `AuthHeaders` debug output is redacted. Stream event journal payloads are redacted through the CLI output redaction helper before insertion. Broader trace/log redaction, storage redaction policy for future event replay exports, and adapter/webhook trust boundaries remain for later security and observability tickets.

Optional exhaustive validation tools are not all installed in this environment yet. The default quality gate passes without them and uses installed optional tools automatically. `cargo-nextest` is currently missing, and miri is unavailable for the active stable toolchain.

There is not yet a `cargo-deny` policy file; the quality gate skips deny checks until the supply-chain policy ticket introduces one.

## Next recommended ticket

Ticket 021 — Implement task get/list/wait/cancel.
