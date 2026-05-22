# Storage

`missive` uses a local SQLite database for profile-scoped communication state. The
schema is introduced by embedded migrations in `crates/missive-store/migrations`
and applied by `missive-store` before typed repository APIs read or mutate the
store.

## Migration strategy

* Migrations are ordered SQL files embedded into the `missive-store` crate.
* `schema_migrations` records the applied version, name, checksum, and timestamp.
* The migrator verifies checksums for applied migrations and refuses databases
  that contain unknown future migrations.
* Each pending migration is applied inside a SQLite transaction.
* `PRAGMA user_version` is set to the current missive schema version after a
  successful migration run.
* Store connections enable foreign-key enforcement and a bounded busy timeout.

The current schema version is `1`, created by
`0001_initial_schema.sql`. Runtime database files are resolved outside the source
tree by the state path contract documented in
[`docs/configuration.md`](configuration.md#local-state-paths).

## Tables

| Table | Purpose | Retention notes |
| --- | --- | --- |
| `schema_migrations` | Tracks applied migration versions, names, checksums, and apply timestamps. | Keep for the lifetime of the database; it is required for safe upgrades. |
| `auth_refs` | Stores non-secret authentication references such as environment-variable and keyring locations. | Keep while referenced by agents or push configs. Raw token values are not part of this schema. |
| `agents` | Stores local/config-seeded/discovered A2A agent registry entries, interface URLs, tags, notes, metadata, and cached Agent Card fields. | Keep until the user removes an agent. Cached card fields may be refreshed or pruned when stale. |
| `contexts` | Tracks A2A context continuity, human-friendly names, parent/fork links, state, summaries, and metadata. | Keep while useful for conversation/task continuity; closed or archived contexts can be exported and pruned later. |
| `tasks` | Tracks local and remote task IDs, agent/context linkage, task state, protocol version, remote task JSON, and metadata. | Keep active tasks until terminal. Terminal task retention should be profile-configurable in later tickets. |
| `messages` | Stores request, response, stream-event, push, and local message records linked to agents, contexts, and tasks. | Keep with their context/task by default; exports and retention pruning must redact secrets. |
| `artifacts` | Stores task artifact metadata, content JSON, optional local byte paths, versions, MIME type, and kind. | Keep with the task by default. Large byte payload files should be pruned with artifact metadata when retention policies are added. |
| `groups` | Stores group definitions, routing policy names, notes, and metadata for collective operations. | Keep until explicitly deleted by group commands. |
| `group_members` | Stores group membership, rank names, tags, weights, and routing metadata. | Keep with the owning group; cascade delete when the group is removed. |
| `adapter_bindings` | Stores configured local adapter bindings, kind, profile mapping, source identity, settings, and metadata. | Keep while the adapter is configured. Disable instead of deleting when audit history is useful. |
| `gateway_jobs` | Stores gateway-managed background jobs, state, related agent/context/task/group/adapter IDs, request/result JSON, retry data, and locks. | Keep queued/running jobs until completion. Completed job retention should be bounded by future gateway policy. |
| `push_configs` | Stores local records of A2A push notification configs, callback URLs, auth refs, remote config JSON, and deletion timestamps. | Keep active configs while remote tasks may call back. Deleted configs retain a tombstone until pruning is safe. |
| `events` | Append-oriented event journal for registry changes, protocol calls, streaming updates, task changes, group operations, gateway jobs, and adapter callbacks. | Keep enough history for replay, diagnostics, and exports. Future retention policies should prefer compact summaries over silent loss. |

## Constraints and indexing

The initial migration enables SQLite foreign keys and creates constraints for
core enum-like values, JSON validity, non-empty identifiers, and positive weights
or counters. Common query paths are indexed for later repository APIs, including
agent/task state lookups, context and task message order, gateway job scheduling,
push config lookup, group membership, and event replay by timestamp, type,
agent, context/task, or gateway job.

JSON payload columns intentionally store protocol-shaped data as text. This keeps
the early schema stable while A2A compatibility work evolves, but repository APIs
must still validate and redact data before writing it.

## Repository API contract

`crates/missive-store` exposes a blocking `Store` facade around one migrated
SQLite connection. `Store::open`, `Store::from_connection`, and
`Store::open_in_memory` apply embedded migrations before returning a repository
handle. The implementation uses `rusqlite`, so async callers should run store
work from a dedicated blocking task or store worker instead of calling it on an
async reactor thread.

The public repository methods use typed records and upsert inputs rather than SQL
strings:

* auth refs: `AuthRefUpsert`, `AuthRefRecord`, and CRUD methods for non-secret
  environment/keyring reference rows used by agent registry auth-ref links
* agents: `AgentUpsert`, `AgentRecord`, and CRUD methods for registry rows used
  by `missive agent add/list/show/inspect/refresh/remove/rename`, including
  cached Agent Card JSON, ETag, Last-Modified, and fetch timestamp fields
* contexts: `ContextUpsert`, `ContextRecord`, and CRUD methods for context state;
  `missive context create/list/show/fork/close/export` uses these rows for
  human-friendly context names, parent/fork links, local close state, relation
  counts, and redacted context exports
* tasks: `TaskUpsert`, `TaskRecord`, and CRUD methods for task state;
  `TaskUpsert::record_a2a_protocol_version` records the selected A2A version in
  both the typed protocol-version column and `a2a.protocol_version` metadata;
  `missive task get/list/wait/cancel` reads these rows, filters them by agent,
  context, state, updated timestamp, and source, and persists remote A2A `Task`
  payloads returned by task operations
* artifacts: `ArtifactUpsert`, `ArtifactRecord`, `ArtifactId`, `ArtifactKind`,
  and CRUD/list methods for artifacts linked to tasks and contexts. Send,
  stream, and task operations persist artifacts embedded in returned A2A `Task`
  payloads; streaming `artifactUpdate` events update the same row and increment
  its local version. `missive task artifact list/show/save/export` reads these
  rows and writes safe local exports without trusting remote filenames.
* messages: `MessageInsert`, `MessageRecord`, `MessageDirection`,
  `MessageRole`, and append/get/list/delete methods for request/response rows
  linked to agents, contexts, and tasks; `missive send` uses these rows for
  non-streaming A2A request/response persistence, and `missive stream` appends
  request plus `stream_event` rows for parsed SSE updates
* events: `EventInsert`, `EventRecord`, append/get/list/delete methods, and
  monotonic SQLite sequences; `EventInsert::record_a2a_protocol_version` records
  the selected A2A version in event metadata. Current CLI producers append
  redacted rows for agent registry mutations, group CRUD/membership mutations,
  A2A send/stream requests, A2A send responses, `a2a.stream.*` updates, changed
  remote task payloads observed by send/task commands, push-config CRUD, gateway
  daemon lifecycle start/stop events, gateway subscription lifecycle and
  `a2a.subscription.*` updates, and webhook callback acceptance/rejection events.
  `missive events list/tail/replay/export` reads these rows without hand-written
  SQL in CLI code.
* groups: `GroupUpsert`, `GroupRecord`, `GroupMemberUpsert`, group rename, and
  membership methods with rank uniqueness enforced by SQLite
* gateway jobs: `GatewayJobUpsert`, `GatewayJobRecord`, `GatewayJobId`, and CRUD
  methods for background job state; the gateway currently uses kind
  `task_subscription` to persist/resume A2A task subscription state and bounded
  backoff

Repository records reuse `missive-core` identifiers such as `AgentAlias`,
`ContextId`, `TaskId`, `EventId`, `GroupName`, `RankName`, `TransportName`,
`MissiveTimestamp`, and `Metadata`. Store-specific ids such as `ArtifactId`,
`GatewayJobId`, and `AdapterBindingId` are validated wrappers. Auth-ref repository rows persist only
non-secret references such as environment variable names or keyring account
coordinates; raw tokens are not stored, and CLI `--bearer-token-env`/`--header`
values are never written to SQLite. JSON columns are serialized and parsed at the
repository boundary so callers receive typed `serde_json::Value`, `Metadata`,
maps, and lists instead of raw JSON text.

`Store::transaction` runs a closure against `StoreTransaction`, which exposes the
same typed repository methods. The transaction commits only when the closure
returns `Ok`; closure errors or SQL constraint failures roll back prior writes.
This is the transaction helper future CLI, gateway, adapter, and routing code
should use for multi-row state changes.

## Current limitations

Typed repository APIs exist for the core store tables needed by upcoming tickets,
and the CLI now uses the auth-ref, agent, context, group, task, artifact,
message, event, and push-config repositories for `missive agent` registry
commands, public Agent Card cache updates, non-streaming `missive send`
persistence, streaming `missive stream` request/event/artifact persistence, task
get/list/wait/cancel state updates, `missive task artifact` local export
commands, context create/list/show/fork/close/export state management, group
create/list/show/add/remove/rename/delete state management, push notification
config create/get/list/delete state, gateway daemon lifecycle event persistence,
gateway task subscription/resume job state and event persistence, webhook
callback event persistence, and event journal list/tail/replay/export.
Adapter-binding repositories, retention enforcement, compaction, and durable
event producers for future collective workers, background jobs, and adapter
callbacks are implemented by later tickets.
