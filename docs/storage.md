# Storage

`missive` uses a local SQLite database for profile-scoped communication state. The
schema is introduced by embedded migrations in `crates/missive-store/migrations`
and applied by `missive-store` before future repository APIs read or mutate the
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

## Current limitations

Ticket 011 defines and tests the schema/migration layer only. Typed repository
APIs, CLI commands, retention enforcement, compaction, event replay, and durable
A2A protocol persistence are implemented by later tickets.
