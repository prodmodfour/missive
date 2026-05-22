-- missive SQLite schema v2
--
-- Adds persistent gateway sessions. Sessions are communication continuity
-- records keyed by inbound source, target agent, and a human/adapter-provided
-- resume name. They link to A2A contexts and carry reset policy metadata; they
-- intentionally do not store agent memory or cognition state.

CREATE TABLE gateway_sessions (
    gateway_session_id TEXT PRIMARY KEY NOT NULL,
    source_kind TEXT NOT NULL,
    source_id TEXT NOT NULL,
    agent_alias TEXT NOT NULL REFERENCES agents(alias) ON UPDATE CASCADE ON DELETE CASCADE,
    resume_name TEXT NOT NULL DEFAULT 'default',
    context_id TEXT NOT NULL REFERENCES contexts(context_id) ON UPDATE CASCADE ON DELETE RESTRICT,
    reset_mode TEXT NOT NULL DEFAULT 'none' CHECK (reset_mode IN ('none', 'daily', 'idle', 'both')),
    daily_reset_hour INTEGER NOT NULL DEFAULT 0 CHECK (daily_reset_hour BETWEEN 0 AND 23),
    idle_timeout_seconds INTEGER CHECK (idle_timeout_seconds IS NULL OR idle_timeout_seconds > 0),
    last_active_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    last_reset_at TEXT,
    reset_count INTEGER NOT NULL DEFAULT 0 CHECK (reset_count >= 0),
    metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata_json)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (source_kind, source_id, agent_alias, resume_name),
    CHECK (length(gateway_session_id) BETWEEN 1 AND 256),
    CHECK (length(source_kind) BETWEEN 1 AND 63),
    CHECK (length(source_id) BETWEEN 1 AND 256),
    CHECK (length(resume_name) BETWEEN 1 AND 128),
    CHECK ((reset_mode IN ('idle', 'both') AND idle_timeout_seconds IS NOT NULL) OR (reset_mode IN ('none', 'daily') AND idle_timeout_seconds IS NULL))
) STRICT;

CREATE INDEX idx_gateway_sessions_source_agent ON gateway_sessions(source_kind, source_id, agent_alias, resume_name);
CREATE INDEX idx_gateway_sessions_agent_updated ON gateway_sessions(agent_alias, updated_at);
CREATE INDEX idx_gateway_sessions_context ON gateway_sessions(context_id);
