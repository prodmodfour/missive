# BUILD_NOTES.md

## Current state

Tickets 000 through 027 are complete. The repository uses the target Cargo workspace layout for `missive` with these crates:

* `crates/missive-cli` — package `missive-cli`, binary `missive`, clap-derived CLI tree, global flags, configuration loading from CLI/env/discovery, A2A service-parameter CLI overrides, authentication input resolution for implemented Agent Card/send/stream/task requests, output rendering and redaction helpers, help snapshots and A2A conformance CLI golden tests, implemented `missive agent add/list/show/inspect/refresh/remove/rename`, implemented non-streaming `missive send` with rich text/file-reference/file-bytes/JSON-data message parts, implemented streaming `missive stream` with the same rich input parser, implemented `missive task get/list/wait/cancel`, implemented `missive task artifact list/show/save/export`, implemented `missive context create/list/show/fork/close/export`, implemented `missive events list/tail/replay/export`, and placeholder execution status for later commands
* `crates/missive-core` — core domain primitive scaffolding, including shared error/result types, strongly typed IDs, timestamps, metadata maps and A2A metadata keys, envelopes, configuration schema, protocol service-parameter defaults, config auth-ref schema, config discovery, profile validation, redacted config rendering, and deterministic task-wait exit-code variants
* `crates/missive-a2a` — A2A protocol/client integration scaffolding, official `a2a-lf` protocol type re-exports, Agent Card discovery/parsing helpers for `/.well-known/agent-card.json`, A2A service-parameter request handling, resolved auth-header request handling, A2A interface negotiation helpers, non-streaming SendMessage HTTP+JSON/JSON-RPC client support, streaming SendStreamingMessage HTTP+JSON/JSON-RPC SSE client support, task GetTask/ListTasks/CancelTask HTTP+JSON/JSON-RPC client support, and a protocol-versioned A2A 1.0 conformance fixture suite with serde round-trip tests
* `crates/missive-store` — persistence scaffolding with local state path resolution, profile-specific data/state/cache directories, SQLite database path resolution, process locks for state mutation and gateway operation, embedded SQLite schema migrations, a blocking typed repository facade for non-secret auth refs, agents, contexts, tasks, artifacts, messages, events, groups, group members, and gateway jobs, plus helpers to record A2A protocol versions in task/event metadata and message rows that can carry protocol metadata
* `crates/missive-router` — routing and collectives scaffolding
* `crates/missive-gateway` — gateway daemon scaffolding
* `crates/missive-adapters` — adapter scaffolding
* `crates/missive-observe` — observability scaffolding
* `crates/missive-test-support` — reusable local A2A mock server fixtures for integration tests, including Agent Card discovery, HTTP+JSON and JSON-RPC endpoints, controllable task states, streaming SSE events, push-config fixture routes, auth/version-error paths, malformed responses, and request recording

The root `Cargo.toml` is a virtual workspace manifest with shared workspace package metadata and shared dependency versions for planned foundational Rust crates. The store layer depends on `rusqlite` with bundled SQLite plus `serde`/`serde_json` for typed JSON repository boundaries. The A2A layer depends on the official `a2a-lf` crate from `a2aproject/a2a-rs` for protocol types and on `reqwest` with blocking rustls-backed HTTP/TLS support for Agent Card discovery, non-streaming sends, SSE streaming sends, and task operations. The versioned conformance fixtures live under `tests/fixtures/a2a/1.0/` and are consumed by `missive-a2a` round-trip tests plus `missive-cli` golden-output tests. The test-support crate depends on `missive-a2a` and `serde_json` so fixtures can use official A2A method/error constants while staying local and deterministic. The CLI directly depends on the workspace `url` crate for registry URL validation, has a default `native-keyring` feature using the Rust `keyring` crate for platform keyring-backed auth refs where available, and uses `missive-test-support` as a dev-dependency for reusable local A2A integration coverage.

Autonomous build tooling is documented in `docs/tooling.md`. `scripts/bootstrap-tools.sh` is executable, idempotent, supports `--check`, and can install Rust components, optional cargo tools, and opt-in system dependencies.

`scripts/quality-gate.sh` is the hardened default gate for autonomous cycles. It runs shell checks, secret and generated/private-file guardrails, Rust feature checks, formatting, clippy with warnings denied, workspace tests, doc tests, docs with warnings denied, debug/release builds, and optional installed dependency checks. `MISSIVE_AGGRESSIVE_TESTS=1` enables deeper optional checks without editing the script.

Architecture decision records live under `docs/adr/`, with a template and accepted ADRs for Rust workspace structure, A2A-first protocol strategy, SQLite local state, CLI-first UX, and the official A2A Rust protocol type strategy. `docs/architecture.md` links the ADRs and records the current high-level crate boundaries, shared error handling contract, core primitive contract, CLI command/agent-registry/Agent-Card/send/stream/task/context/events/service-parameter/auth contract, configuration contract, output rendering contract, store path/lock contract, SQLite migration contract, typed repository contract, A2A type boundary, and reusable local A2A fixture crate. `docs/protocol.md` documents the current official Rust type boundary, Agent Card discovery, rich SendMessage/SendStreamingMessage message-part mapping, task GetTask/ListTasks/CancelTask mapping, local context continuity mapping, service-parameter handling, auth-header handling, interface negotiation mapping, error mapping, conformance fixture coverage, and fixture update process. `docs/security.md` documents current auth inputs, keyring support, storage tradeoffs, local file-input path disclosure tradeoffs, redaction, context/event export redaction, and limitations. `docs/testing.md` documents local validation, how to run/extend the reusable mock A2A server fixtures, and how to run/update protocol-versioned A2A conformance fixtures.

`missive-core` exposes `MissiveError`, `Result<T>`, `ErrorCategory`, `MissiveExitCode`, and `ErrorReport`. The error taxonomy covers I/O, configuration, protocol, transport, storage, authentication, validation, and orchestration failures. Each category has a stable diagnostic code, deterministic exit code mapping for CLI use, human `Display` rendering, `miette::Diagnostic` metadata, and a serializable JSON/NDJSON report shape. `MissiveError::with_exit_code` allows command-specific deterministic exit codes, currently used by task wait: failed `80`, cancelled `81`, timeout `82`, and input-required `83`.

`missive-core` also exposes `AgentAlias`, `GroupName`, `RankName`, `TransportName`, `ContextId`, `TaskId`, `MessageId`, `EventId`, `MissiveTimestamp`, `Metadata`, `Envelope<T>`, and A2A metadata keys such as `a2a.protocol_version`. Named identifiers validate lowercase CLI-safe forms; opaque A2A/local IDs preserve server-provided text while rejecting empty, whitespace, control, or unbounded values. `Metadata` uses deterministic JSON object ordering, and `MissiveTimestamp` uses canonical RFC3339 UTC rendering.

The core configuration layer exposes `MissiveConfig`, `ConfigDiscovery`, `LoadedConfig`, and schema structs for profiles, agents, auth refs, storage, output, protocol service parameters, gateway, adapters, and quality of service. Discovery precedence is `--config`, `MISSIVE_CONFIG`, repository-local config when `MISSIVE_REPO_CONFIG=1`, XDG config locations, then built-in defaults. Config parsing rejects unknown fields, validates cross references, rejects embedded credentials in URLs, rejects attempts to redefine reserved `A2A-Version`/`A2A-Extensions` service-parameter names in arbitrary parameter maps, validates env/keyring auth refs without accepting raw token values, provides redacted JSON rendering, and now has its selected profile `qos.max_request_bytes` enforced by send/stream message-part parsing and serialized A2A request-size validation.

The store path layer exposes `StatePathResolver`, `StatePaths`, `StatePlatform`, `StatePathSource`, `ProcessLockKind`, and `ProcessLock`. Runtime state defaults to XDG-compatible roots on Linux/Unix-like platforms, macOS `~/Library` fallbacks when XDG variables are absent, or `MISSIVE_HOME` when explicitly set. Paths include `profiles/<profile>`, relative database paths resolve under the selected profile state directory, and lock files live under `<state-dir>/locks/`.

The store migration layer exposes `Migration`, `AppliedMigration`, `MigrationReport`, `embedded_migrations`, `open_sqlite_database`, `migrate_database`, `migrate_connection`, `applied_migrations`, `schema_version`, `CURRENT_SCHEMA_VERSION`, and `SQLITE_APPLICATION_ID`. Migration SQL lives under `crates/missive-store/migrations`, is applied in version order inside transactions, and records checksums in `schema_migrations`. Schema version 1 creates tables for agents, contexts, tasks, messages, artifacts, events, groups, group_members, auth_refs, push_configs, gateway_jobs, and adapter_bindings.

The store repository layer exposes blocking `Store` and `StoreTransaction` APIs. `Store::open`, `Store::from_connection`, and `Store::open_in_memory` apply migrations before use. Typed methods cover non-secret auth refs, agents, contexts, tasks, artifacts, messages, events, groups/group members, and gateway jobs with public upsert/insert/record structs, state/source/direction/role/artifact-kind enums, core identifiers, validated `GatewayJobId`/`AdapterBindingId`/`ArtifactId`, JSON serialization at the repository boundary, A2A protocol-version metadata helpers for tasks/events plus message metadata storage, and transaction rollback on closure or SQL failures. SQL strings remain private to `missive-store` rather than leaking into CLI code. Raw auth tokens are not stored in SQLite; auth-ref rows contain only env var names or keyring service/account coordinates.

The A2A layer exposes the official `a2a-lf` protocol models through `missive_a2a::protocol`, aliases Agent Card/message/task/stream types to official SDK structs, provides `AgentCardClient`, `SendMessageClient`, `StreamMessageClient`, and `TaskClient`, includes interface negotiation helpers, centralizes A2A service parameters in `ServiceParameters`, and applies resolved auth headers through `AuthHeaders`. It resolves `/.well-known/agent-card.json` from a registered base URL, sends `A2A-Version` on every implemented HTTP request, optionally sends `A2A-Extensions` and validated extra service-parameter headers, marks auth header values sensitive before sending, sends conditional refresh headers when cached ETags/Last-Modified values exist, parses supported interfaces, provider, versions, capabilities, default modes, and skills through the official Agent Card type, maps HTTP/TLS/network failures to transport errors, maps invalid card JSON to protocol errors, and maps A2A `VERSION_NOT_SUPPORTED` responses to protocol errors with exit code 76. Non-streaming send maps `http+json` to `POST <interface>/message:send` with `application/a2a+json`, maps `json-rpc` to JSON-RPC method `SendMessage`, and parses direct `Message` and `Task` response shapes. Streaming send maps `http+json` to `POST <interface>/message:stream`, maps `json-rpc` to JSON-RPC method `SendStreamingMessage`, parses SSE `data` records incrementally, accepts direct or JSON-RPC-wrapped `StreamResponse` payloads, and rejects malformed stream events with protocol diagnostics that include the event sequence. Task operations map `http+json` to `GET <interface>/tasks/{id}`, `GET <interface>/tasks`, and `POST <interface>/tasks/{id}:cancel`, map `json-rpc` to `GetTask`, `ListTasks`, and `CancelTask`, and parse official `Task`/`ListTasksResponse` payloads. Interface negotiation canonicalizes Agent Card bindings such as `HTTP+JSON` and `JSONRPC`, supports local `http+json` and `json-rpc`, recognizes gRPC for future extension diagnostics, respects agent binding preference or `agent inspect --binding`, and falls back to registry/base-URL interfaces when older cards omit `supportedInterfaces`.

The `missive` binary uses clap derive and exposes help pages for `agent`, `send`, `stream`, `task`, `context`, `group`, `gateway`, `webhook`, `push`, `doctor`, `logs`, `events`, `completion`, and `manpage`. Global flags parse at every command level: `--json`, `--ndjson`, `--quiet`, `--no-color`, `--config`, `--profile`, `--timeout`, `--protocol-version`, `--a2a-extension`, `--service-param`, `--bearer-token-env`, `--header`, `--trace`, and `--verbose`. `task artifact` now has help-covered `list`, `show`, `save`, and `export` subcommands, `context` has help-covered `create`, `list`, `show`, `fork`, `close`, and `export` subcommands, and `events` has help-covered `list`, `tail`, `replay`, and `export` subcommands.

The `agent` command has implemented `add`, `remove`, `list`, `show`, `inspect`, `refresh`, and `rename` subcommands. Agent registry commands resolve the selected profile state paths, create directories, acquire the state mutation lock, open/migrate SQLite, sync config auth refs as non-secret rows, sync config-seeded agents as read-only rows, persist local registry entries through `missive-store`, and append redacted `missive.agent.add/remove/rename` event rows for local registry mutations. They support aliases, base URLs, explicit interface URLs, binding preference, config auth refs, tags, notes, metadata, human output, JSON output, NDJSON output, quiet mode, duplicate-alias checks, missing-agent diagnostics, and read-only protections for config-seeded agents.

`missive agent inspect <alias>` fetches and caches an A2A Agent Card when needed, applies configured and CLI-overridden A2A service parameters to the fetch, resolves and sends auth headers from agent config auth refs, `--bearer-token-env`, and repeatable `--header Name:Value`, negotiates the selected interface from `supportedInterfaces` and the agent binding preference, then renders the parsed provider, capabilities, skills, versions, supported interfaces, selected interface, and raw card JSON in machine output. `missive agent inspect <alias> --binding <binding>` requires a specific locally supported binding for advanced users/tests, `missive agent inspect <alias> --refresh` bypasses/revalidates the cache, and `missive agent refresh <alias>` explicitly refreshes the cached card.

`missive send <agent> [message]` sends one non-streaming A2A message to a registered agent. It supports positional text, `--stdin`, repeatable `--part text=...`, repeatable local file references through `--file`, repeatable raw byte attachments through `--file-bytes`, repeatable structured data parts through `--json-part`, repeatable MIME metadata through `--mime`, repeatable non-secret `--metadata KEY=VALUE`, `--context`, `--task`, repeatable `--accepted-output-mode`, and the existing global output/auth/service-parameter flags. Local file inputs are canonicalized to regular files with safe UTF-8 filenames, `--file` serializes as an A2A `url` file-reference part, `--file-bytes` serializes as an A2A `raw` part, JSON data parts default to `mediaType: application/json`, and both local inputs plus the serialized `SendMessageRequest` are bounded by the selected profile's `qos.max_request_bytes`. It uses the cached Agent Card when present, otherwise fetches and caches the public card before negotiation. It persists request and response rows in `messages`; direct `Message` responses are stored as response messages, and `Task` responses are upserted in `tasks` with remote task JSON, state, context/task linkage, protocol-version metadata, a linked response row, and redacted `a2a.send.request`, `a2a.send.response`, and task-update events where applicable. Machine output uses `kind: "send_result"` and includes request part summaries, response, selected-interface, and persistence summaries.

`missive stream <agent> [message]` sends one A2A streaming message to a registered agent. It shares the rich send input parser for text, file references, file bytes, JSON data, MIME metadata, and profile request-size limits, validates the Agent Card's `capabilities.streaming` unless `--force` is passed, negotiates HTTP+JSON or JSON-RPC, opens an SSE response, renders one human/NDJSON update per parsed event as it arrives, and emits a final `stream_result` summary. The outbound stream request and each parsed `task`, `message`, `statusUpdate`, or `artifactUpdate` are appended to the SQLite event journal with event types under `a2a.stream.*`; parsed remote stream updates are also stored as `messages` rows with direction `stream_event` and linked to task/context rows where IDs are present. Status and task events update the local task state and completion timestamp for completed/failed/cancelled states. Task events persist embedded artifacts, and `artifactUpdate` events upsert dedicated artifact rows, merging appended chunks and incrementing the local artifact version.

`missive task` now implements `get`, `list`, `wait`, and `cancel`. Local `task list` filters SQLite task rows by agent, context, state, updated-after timestamp, and source (`remote`, `local`, or `gateway`). `task get` reads a local row by default or refreshes it with A2A `GetTask` when `--remote` is passed. Remote `task list --remote --agent <alias>` calls A2A `ListTasks`, sends supported filters to the remote agent, persists each returned task and its artifacts, and renders the updated local task view with artifact metadata summaries. `task wait` polls remote `GetTask` by default with global `--timeout` and `--interval`, or polls only the SQLite row with `--local`; completed exits `0`, failed exits `80`, cancelled exits `81`, timeout exits `82`, and input-required exits `83`. `task cancel` calls A2A `CancelTask`, persists the returned task and artifacts, and renders `kind: "task_cancel"` output. Remote task get/list/wait/cancel persistence appends `a2a.task.updated` events when an observed remote task row changes.

`missive task artifact` now implements local `list`, `show`, `save`, and `export` operations for artifacts persisted from send, stream, and task responses. `list` and task show output expose artifact ids, kinds, names, MIME types, versions, metadata, and text previews. `show` includes part summaries plus raw A2A artifact JSON in machine output. `save` writes one artifact to a selected path or sanitized filename inside an existing output directory. `export` writes every artifact for one task into a chosen directory. Text parts are written as UTF-8, JSON data parts as pretty JSON, raw parts as bytes, and URL/file-reference artifacts as JSON manifests; remote names and filenames are sanitized to prevent path traversal, and existing files require `--force` to overwrite.

`missive context` now implements `create`, `list`, `show`, `fork`, `close`, and `export`. Context commands use the same profile-scoped store path/lock handling as agent/send/stream/task commands, can generate local A2A context ids or persist explicit ids, support unique human-friendly names, optional owning agent aliases, summaries, non-secret metadata, selectors by id or unique name, lifecycle filters, parent/fork links, local close timestamps, linked task/message/event counts, and redacted JSON/NDJSON exports containing context rows plus linked tasks, messages, and events. Forking records the parent id in both `parent_context_id` and metadata keys such as `missive.context.parent_id`.

`missive events` now implements `list`, `tail`, `replay`, and `export` for the selected profile's append-only event journal. Event records render sequence, timestamp, source, type, optional agent/context/task/group/gateway/adapter links, redacted payload, metadata, and redaction status. Filters include agent, context, task, source, type, since timestamp, after-sequence, and limit where applicable. `events tail` follows newly appended records without holding the state-mutation lock after opening the database and can be bounded with `--limit` and global `--timeout`. `events export --ndjson` emits one `event_record` envelope per line for agent/subprocess use, and `events replay` reconstructs deterministic context/task summaries from matching journal rows.

The CLI auth resolver reads bearer tokens from environment variables, resolves config env/keyring auth refs, accepts one-off `--header Name:Value` values, and returns `missive::auth` errors with exit code 77 when required auth material is unavailable. CLI-supplied header values and resolved tokens are kept in memory for outbound requests only and are not persisted. `AuthHeaders` debug rendering is redacted, reqwest header values are marked sensitive, and normal output rendering redacts authorization, token, API key, password, cookie, and secret-like fields.

The current CLI output contract supports human, JSON, NDJSON, and quiet renderers. Implemented agent commands emit command-specific kinds such as `agent_add`, `agent_list`, `agent_show`, `agent_inspect`, `agent_refresh`, `agent_remove`, and `agent_rename`; send emits `send_result` with stable request part summaries and byte counts; stream emits `stream_event` and `stream_result` in NDJSON and one `stream_result` in JSON with the same request part summaries; task emits `task_get`, `task_list`, `task_wait`, and `task_cancel`; task artifact commands emit `task_artifact_list`, `task_artifact_show`, `task_artifact_save`, and `task_artifact_export`; context emits `context_create`, `context_list`, `context_show`, `context_fork`, `context_close`, and `context_export`; events emits `events_list`, `events_replay`, `events_export`, `events_tail`, and per-record `event_record` NDJSON lines for tail/export. Skeletal commands load/validate config, then emit a stable `missive.output.v1` `command_status` envelope in machine-readable modes. Structured execution errors render as `kind: "error"` envelopes when `--json` or `--ndjson` is active. The renderer recursively redacts secret-like JSON fields and HTTP-style authorization headers before writing output.

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
cargo test -p missive-a2a --test protocol_fixtures -- --nocapture
MISSIVE_UPDATE_GOLDENS=1 cargo test -p missive-cli --test a2a_conformance_fixtures --all-features -- --nocapture
cargo test -p missive-cli --test a2a_conformance_fixtures --all-features -- --nocapture
cargo test -p missive-a2a --test protocol_fixtures --all-features
cargo test -p missive-cli --test a2a_conformance_fixtures --all-features
cargo clippy -p missive-a2a --test protocol_fixtures --all-features -- -D warnings
cargo clippy -p missive-cli --test a2a_conformance_fixtures --all-features -- -D warnings
scripts/quality-gate.sh
```

The targeted checks covered the versioned A2A 1.0 fixture directory, official SDK serde round trips for Agent Cards/messages/tasks/artifacts/stream events/operation requests/push configs/JSON-RPC envelopes, HTTP A2A error-info validation, and normalized CLI golden outputs for fixture-backed `agent inspect --json` and `send --json` flows. `MISSIVE_UPDATE_GOLDENS=1` was used only to create the new normalized golden files before rerunning the same CLI test without the update flag.

Environment/tooling notes: no new cargo subcommands, Rust components, OS packages, or Rust dependencies were installed during this cycle.

## Latest cycle notes

Implemented ticket 027 — Add A2A conformance fixture suite.

Included:

* expanded `tests/fixtures/a2a/1.0/` with deterministic A2A 1.0 JSON examples for Agent Cards, messages, tasks, artifacts, stream responses, operation requests/responses, push notification configs, JSON-RPC envelopes, and HTTP error bodies
* added `tests/fixtures/a2a/1.0/README.md` documenting fixture sources, redaction rules, protocol-version directory naming, and the update process for future A2A versions
* broadened `crates/missive-a2a/tests/protocol_fixtures.rs` so official `a2a-lf` types round-trip all supported fixture categories through serde and validate HTTP error-info payloads
* added `crates/missive-cli/tests/a2a_conformance_fixtures.rs` with fixture-backed CLI golden output coverage for `missive agent inspect --json` and `missive send --json`
* added normalized CLI goldens under `tests/fixtures/a2a/1.0/cli/`, replacing local ports, generated message IDs, and timestamps with stable placeholders
* updated README, `docs/protocol.md`, and `docs/testing.md` to describe the conformance fixture suite and targeted validation commands

## Known blockers

None known.

## Limitations

The A2A conformance fixture suite is a static, local compatibility suite based on A2A 1.0 specification examples and the current official Rust SDK wire shape. It is not an external certification program and does not prove interoperability with every independent A2A implementation; ticket 063 remains responsible for running against an upstream/example A2A agent.

`missive-test-support` remains a deterministic local fixture server rather than a formal external conformance service. It intentionally serves the endpoint shapes needed by current and near-future missive tests.

Older CLI integration tests still contain some per-file minimal TCP mocks from earlier tickets. New protocol/gateway/push/adapters tests should prefer `crates/missive-test-support`, and legacy mocks can be migrated opportunistically when those tests are touched for functional work.

`missive task` implements direct task get/list/wait/cancel only. It does not implement A2A task resubscription/`SubscribeToTask`, gateway-managed task monitoring, background waits, restart resume, or push/webhook-driven task updates; those remain for later gateway/push tickets.

Remote `task list --remote` is scoped to one agent and one response page. It accepts `--page-size` and `--page-token`, renders the returned `nextPageToken`, and persists returned tasks, but it does not auto-page through every remote task yet.

`task wait` uses blocking polling in the current CLI process. It does not subscribe to remote task updates, cancel an in-progress wait on local input, or coordinate with gateway jobs. Non-success wait states intentionally render the final task output on stdout first and then return a structured error on stderr with exit codes `80` through `83` for shell automation.

`task cancel` calls the remote A2A agent and requires either `--agent` or an existing local task row from which an agent alias can be resolved. There is no local-only task cancellation/marking mode yet.

Task commands persist raw remote task JSON in the `tasks.remote_task_json` column, matching earlier send/stream task persistence. Normal human/JSON/NDJSON command output is redacted before printing, but local SQLite state should still be treated as potentially sensitive runtime data and kept outside the repository.

Task artifacts are persisted and exportable only after an implemented command has observed them in a `Task` payload or streaming `artifactUpdate`. `missive task artifact` does not call remote A2A endpoints itself, auto-refresh missing artifacts, fetch URL/file-reference artifact contents, prune old artifact versions, or store large byte payloads outside SQLite yet. URL/file-reference artifacts export as JSON manifests rather than dereferencing potentially untrusted locations.

`missive context` is local control-plane state only. It does not call a remote A2A context-close endpoint, synchronize named contexts with agents, merge/fork remote context history, or implement gateway session reset policies; those remain for later protocol/gateway/session tickets.

Context names are accepted by `context show/fork/close/export` only when they uniquely identify one local row. `send`, `stream`, and `task` still accept explicit A2A context ids rather than context names; use `missive context show <name> --json` to resolve a name to an id for automation.

`context export` includes linked task, message, and event rows and redacts normal stdout/stderr output, but it does not yet include dedicated artifact rows/files, push configs, adapter bindings, gateway jobs, event replay summaries, or retention/pruning metadata. Local SQLite state can still contain raw remote protocol payloads before export-time redaction and must stay outside the repository.

`missive stream` implements initial A2A `SendStreamingMessage` only. It does not implement task resubscription/`SubscribeToTask`, gateway-managed streaming jobs, background resume after process restart, or local user-triggered cancellation of an active stream.

`missive send` and `missive stream` now support text, local file-reference, raw file-byte, JSON data, and MIME-aware message parts, but they still do not implement streaming/chunked file upload. Local file inputs larger than selected-profile `qos.max_request_bytes` fail locally instead of being streamed. `--file` sends a canonical `file://` URL and can reveal the local path to the remote agent and in local SQLite request-message rows; use `--file-bytes` only for content you are willing to embed in the A2A request.

`--mime` assignment is deterministic but grouped by missive's part-building order rather than the user's shell flag order across different repeatable flag kinds. One value applies to all non-text file/JSON parts; multiple values must match either the non-text part count or total part count.

`missive events` reads the local SQLite journal only. It does not subscribe to remote A2A task updates, reconstruct full protocol objects from incomplete event streams, replay side effects, prune old events, compact snapshots, or export to external observability systems. `events tail` uses polling and must be bounded with `--limit`, global `--timeout`, or process control in non-interactive automation.

The `--timeout` global flag now drives `missive task wait`, but it still does not drive send/stream HTTP client timeout construction or general command cancellation. The streaming/client layers currently use the A2A layer's default blocking reqwest timeouts for non-wait network calls.

Authentication is wired into implemented Agent Card fetch/refresh, non-streaming send, streaming send, and remote task HTTP paths and exposed as reusable helpers for future push clients. Push/webhook/gateway behavior, adapters, collectives, and observability remain for later tickets and must reuse the same auth/redaction path when they add network calls.

Keyring-backed auth refs can be resolved when the `native-keyring` feature is enabled and the local platform/session keyring is available, but missive does not yet provide commands to create, update, list, or delete keyring entries. Users must provision those entries with OS tooling or another keyring client. Builds without `native-keyring` parse keyring refs but fail clearly if one is needed.

There is no local-only insecure raw-token storage mode. SQLite auth-ref rows intentionally store only env var names or keyring service/account coordinates. If an insecure mode is ever added, it must be explicit and documented in a later security/storage ticket.

Config-seeded agents are synced into SQLite as read-only rows when agent/send/stream/task/context commands run. If a config entry is later removed, an already-synced row may remain in the local database until a future reconciliation/maintenance command defines stale config-seed pruning. Cached Agent Card fields are preserved only while the config-seeded base URL remains unchanged.

The `missive` binary has a real command tree, global parser, configuration discovery/profile validation, A2A service-parameter flags, auth input flags, output rendering contract, implemented agent registry commands, Agent Card inspection/refresh, selected-interface negotiation, non-streaming send, streaming send, task get/list/wait/cancel, task artifact list/show/save/export, context create/list/show/fork/close/export, and events list/tail/replay/export. Gateway behaviour, adapters, collectives, shell completion generation, and manpage generation remain for later tickets.

The store layer resolves state paths, provides process locks, migrates fresh SQLite databases to schema version 1, and exposes typed repository APIs for auth refs, agents, contexts, tasks, artifacts, messages, events, groups, group members, and gateway jobs. Push-config and adapter-binding repositories, retention enforcement, compaction, future group/gateway/webhook/adapter event producers, and broader durable A2A protocol persistence remain for later tickets.

The store repository is synchronous because it uses `rusqlite`; async gateway/adapter code should call it through a blocking task or store worker when those tickets wire runtime behaviour.

The config schema includes protocol, auth refs, gateway, adapter, and QoS defaults. Selected-profile `qos.max_request_bytes` is enforced by send/stream message-part parsing and serialized request-size validation, but other gateway/adapter/QoS values do not yet start a gateway, enforce send/stream HTTP timeouts, run adapters, or manage background jobs.

The `--json`, `--ndjson`, and `--quiet` flags override config output defaults. `--config`, `--profile`, `--protocol-version`, `--a2a-extension`, `--service-param`, `--bearer-token-env`, and `--header` are active for implemented Agent Card, send, stream, and remote task paths. The `--trace`, `--verbose`, and `--no-color` flags are still parsed but do not yet drive tracing, verbose diagnostics, or color control.

Redaction is active at the config and CLI output boundaries for structured values rendered through the current helpers, and `AuthHeaders` debug output is redacted. Current event journal producers redact payloads through the CLI output redaction helper before insertion, and `context export` plus `events list/tail/replay/export` redact linked task/message/event payloads before printing. Broader trace/log redaction, storage retention/compaction policy, external event export sinks, and adapter/webhook trust boundaries remain for later security and observability tickets.

Optional exhaustive validation tools are not all installed in this environment yet. The default quality gate passes without them and uses installed optional tools automatically. `cargo-nextest` is currently missing, and miri is unavailable for the active stable toolchain.

There is not yet a `cargo-deny` policy file; the quality gate skips deny checks until the supply-chain policy ticket introduces one.

## Next recommended ticket

Ticket 028 — Implement push notification config commands.
