-- missive SQLite schema v1
--
-- This migration creates the durable local control-plane tables used by later
-- repository APIs. Values that carry protocol payloads are stored as JSON text
-- so early A2A compatibility work can evolve without forcing table churn.

CREATE TABLE auth_refs (
    name TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('env', 'keyring', 'external')),
    header_name TEXT NOT NULL DEFAULT 'Authorization',
    scheme TEXT,
    env_var TEXT,
    keyring_service TEXT,
    keyring_account TEXT,
    secret_storage TEXT NOT NULL DEFAULT 'external' CHECK (secret_storage IN ('external', 'keyring', 'env')),
    metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata_json)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    CHECK ((kind <> 'env') OR env_var IS NOT NULL),
    CHECK ((kind <> 'keyring') OR (keyring_service IS NOT NULL AND keyring_account IS NOT NULL))
) STRICT;

CREATE TABLE agents (
    alias TEXT PRIMARY KEY NOT NULL,
    source TEXT NOT NULL DEFAULT 'local' CHECK (source IN ('local', 'config_seed', 'discovered')),
    base_url TEXT NOT NULL,
    interface_urls_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(interface_urls_json)),
    binding_preference_json TEXT NOT NULL DEFAULT '["http+json","json-rpc"]' CHECK (json_valid(binding_preference_json)),
    auth_ref_name TEXT REFERENCES auth_refs(name) ON UPDATE CASCADE ON DELETE SET NULL,
    tags_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(tags_json)),
    notes TEXT,
    metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata_json)),
    agent_card_json TEXT CHECK (agent_card_json IS NULL OR json_valid(agent_card_json)),
    agent_card_etag TEXT,
    agent_card_last_modified TEXT,
    agent_card_fetched_at TEXT,
    read_only INTEGER NOT NULL DEFAULT 0 CHECK (read_only IN (0, 1)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    CHECK (length(alias) BETWEEN 1 AND 63),
    CHECK (length(base_url) > 0)
) STRICT;

CREATE TABLE contexts (
    context_id TEXT PRIMARY KEY NOT NULL,
    agent_alias TEXT REFERENCES agents(alias) ON UPDATE CASCADE ON DELETE SET NULL,
    name TEXT,
    parent_context_id TEXT REFERENCES contexts(context_id) ON UPDATE CASCADE ON DELETE SET NULL,
    state TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'closed', 'archived')),
    summary TEXT,
    metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata_json)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    closed_at TEXT,
    CHECK (length(context_id) BETWEEN 1 AND 256),
    CHECK (parent_context_id IS NULL OR parent_context_id <> context_id)
) STRICT;

CREATE TABLE tasks (
    task_id TEXT PRIMARY KEY NOT NULL,
    agent_alias TEXT NOT NULL REFERENCES agents(alias) ON UPDATE CASCADE ON DELETE RESTRICT,
    context_id TEXT REFERENCES contexts(context_id) ON UPDATE CASCADE ON DELETE SET NULL,
    state TEXT NOT NULL CHECK (state IN ('submitted', 'working', 'input_required', 'completed', 'failed', 'cancelled', 'unknown')),
    source TEXT NOT NULL DEFAULT 'remote' CHECK (source IN ('remote', 'local', 'gateway')),
    protocol_version TEXT,
    remote_task_json TEXT CHECK (remote_task_json IS NULL OR json_valid(remote_task_json)),
    last_message_id TEXT,
    metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata_json)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    completed_at TEXT,
    CHECK (length(task_id) BETWEEN 1 AND 256)
) STRICT;

CREATE TABLE messages (
    message_id TEXT PRIMARY KEY NOT NULL,
    agent_alias TEXT REFERENCES agents(alias) ON UPDATE CASCADE ON DELETE SET NULL,
    context_id TEXT REFERENCES contexts(context_id) ON UPDATE CASCADE ON DELETE SET NULL,
    task_id TEXT REFERENCES tasks(task_id) ON UPDATE CASCADE ON DELETE SET NULL,
    direction TEXT NOT NULL CHECK (direction IN ('request', 'response', 'stream_event', 'push', 'local')),
    role TEXT CHECK (role IS NULL OR role IN ('user', 'agent', 'system', 'tool', 'unknown')),
    ordinal INTEGER NOT NULL DEFAULT 0 CHECK (ordinal >= 0),
    protocol_message_id TEXT,
    content_json TEXT NOT NULL CHECK (json_valid(content_json)),
    metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata_json)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    CHECK (length(message_id) BETWEEN 1 AND 256)
) STRICT;

CREATE TABLE artifacts (
    artifact_id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(task_id) ON UPDATE CASCADE ON DELETE CASCADE,
    context_id TEXT REFERENCES contexts(context_id) ON UPDATE CASCADE ON DELETE SET NULL,
    name TEXT,
    mime_type TEXT,
    kind TEXT NOT NULL DEFAULT 'unknown' CHECK (kind IN ('text', 'json', 'file', 'bytes', 'unknown')),
    version INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
    content_json TEXT CHECK (content_json IS NULL OR json_valid(content_json)),
    bytes_path TEXT,
    metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata_json)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    CHECK (length(artifact_id) BETWEEN 1 AND 256)
) STRICT;

CREATE TABLE "groups" (
    group_name TEXT PRIMARY KEY NOT NULL,
    routing_policy TEXT NOT NULL DEFAULT 'direct',
    notes TEXT,
    metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata_json)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    CHECK (length(group_name) BETWEEN 1 AND 63)
) STRICT;

CREATE TABLE group_members (
    group_name TEXT NOT NULL REFERENCES "groups"(group_name) ON UPDATE CASCADE ON DELETE CASCADE,
    agent_alias TEXT NOT NULL REFERENCES agents(alias) ON UPDATE CASCADE ON DELETE CASCADE,
    rank_name TEXT NOT NULL,
    tags_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(tags_json)),
    weight REAL NOT NULL DEFAULT 1.0 CHECK (weight > 0.0),
    routing_metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(routing_metadata_json)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (group_name, agent_alias),
    UNIQUE (group_name, rank_name),
    CHECK (length(rank_name) BETWEEN 1 AND 63)
) STRICT;

CREATE TABLE adapter_bindings (
    adapter_binding_id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL,
    profile TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    source_identity TEXT,
    settings_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(settings_json)),
    metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata_json)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    CHECK (length(adapter_binding_id) BETWEEN 1 AND 256),
    CHECK (length(name) BETWEEN 1 AND 63),
    CHECK (length(kind) BETWEEN 1 AND 63),
    CHECK (length(profile) BETWEEN 1 AND 63)
) STRICT;

CREATE TABLE gateway_jobs (
    gateway_job_id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('queued', 'running', 'succeeded', 'failed', 'cancelled', 'retrying')),
    agent_alias TEXT REFERENCES agents(alias) ON UPDATE CASCADE ON DELETE SET NULL,
    context_id TEXT REFERENCES contexts(context_id) ON UPDATE CASCADE ON DELETE SET NULL,
    task_id TEXT REFERENCES tasks(task_id) ON UPDATE CASCADE ON DELETE SET NULL,
    group_name TEXT REFERENCES "groups"(group_name) ON UPDATE CASCADE ON DELETE SET NULL,
    adapter_binding_id TEXT REFERENCES adapter_bindings(adapter_binding_id) ON UPDATE CASCADE ON DELETE SET NULL,
    request_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(request_json)),
    result_json TEXT CHECK (result_json IS NULL OR json_valid(result_json)),
    metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata_json)),
    retry_count INTEGER NOT NULL DEFAULT 0 CHECK (retry_count >= 0),
    max_attempts INTEGER NOT NULL DEFAULT 1 CHECK (max_attempts >= 1),
    next_run_at TEXT,
    locked_by TEXT,
    locked_until TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    completed_at TEXT,
    CHECK (length(gateway_job_id) BETWEEN 1 AND 256),
    CHECK (length(kind) BETWEEN 1 AND 63)
) STRICT;

CREATE TABLE push_configs (
    push_config_id TEXT PRIMARY KEY NOT NULL,
    agent_alias TEXT NOT NULL REFERENCES agents(alias) ON UPDATE CASCADE ON DELETE CASCADE,
    task_id TEXT REFERENCES tasks(task_id) ON UPDATE CASCADE ON DELETE CASCADE,
    callback_url TEXT NOT NULL,
    auth_ref_name TEXT REFERENCES auth_refs(name) ON UPDATE CASCADE ON DELETE SET NULL,
    remote_config_json TEXT CHECK (remote_config_json IS NULL OR json_valid(remote_config_json)),
    metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata_json)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    deleted_at TEXT,
    CHECK (length(push_config_id) BETWEEN 1 AND 256),
    CHECK (length(callback_url) > 0)
) STRICT;

CREATE TABLE events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE,
    timestamp TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    source TEXT NOT NULL,
    event_type TEXT NOT NULL,
    agent_alias TEXT REFERENCES agents(alias) ON UPDATE CASCADE ON DELETE SET NULL,
    context_id TEXT REFERENCES contexts(context_id) ON UPDATE CASCADE ON DELETE SET NULL,
    task_id TEXT REFERENCES tasks(task_id) ON UPDATE CASCADE ON DELETE SET NULL,
    group_name TEXT REFERENCES "groups"(group_name) ON UPDATE CASCADE ON DELETE SET NULL,
    gateway_job_id TEXT REFERENCES gateway_jobs(gateway_job_id) ON UPDATE CASCADE ON DELETE SET NULL,
    adapter_binding_id TEXT REFERENCES adapter_bindings(adapter_binding_id) ON UPDATE CASCADE ON DELETE SET NULL,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata_json)),
    redacted INTEGER NOT NULL DEFAULT 1 CHECK (redacted IN (0, 1)),
    CHECK (length(event_id) BETWEEN 1 AND 256),
    CHECK (length(source) BETWEEN 1 AND 128),
    CHECK (length(event_type) BETWEEN 1 AND 128)
) STRICT;

CREATE INDEX idx_agents_auth_ref ON agents(auth_ref_name);
CREATE INDEX idx_agents_updated_at ON agents(updated_at);
CREATE INDEX idx_contexts_agent ON contexts(agent_alias, updated_at);
CREATE INDEX idx_contexts_parent ON contexts(parent_context_id);
CREATE INDEX idx_tasks_agent_state ON tasks(agent_alias, state, updated_at);
CREATE INDEX idx_tasks_context_state ON tasks(context_id, state, updated_at);
CREATE INDEX idx_messages_context_order ON messages(context_id, ordinal, created_at);
CREATE INDEX idx_messages_task_order ON messages(task_id, ordinal, created_at);
CREATE INDEX idx_messages_agent_created ON messages(agent_alias, created_at);
CREATE INDEX idx_artifacts_task ON artifacts(task_id, version);
CREATE INDEX idx_group_members_agent ON group_members(agent_alias);
CREATE INDEX idx_push_configs_task ON push_configs(task_id, deleted_at);
CREATE INDEX idx_push_configs_agent ON push_configs(agent_alias, deleted_at);
CREATE INDEX idx_gateway_jobs_state_next_run ON gateway_jobs(state, next_run_at);
CREATE INDEX idx_gateway_jobs_task ON gateway_jobs(task_id);
CREATE INDEX idx_adapter_bindings_kind_enabled ON adapter_bindings(kind, enabled);
CREATE INDEX idx_events_timestamp ON events(timestamp);
CREATE INDEX idx_events_type ON events(event_type, timestamp);
CREATE INDEX idx_events_context_task ON events(context_id, task_id, sequence);
CREATE INDEX idx_events_agent_sequence ON events(agent_alias, sequence);
CREATE INDEX idx_events_gateway_job ON events(gateway_job_id, sequence);
