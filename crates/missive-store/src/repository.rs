//! Typed SQLite repository APIs for missive profile state.
//!
//! The repository layer is intentionally synchronous because it is built on
//! `rusqlite`. Callers running inside an async runtime should execute these
//! blocking methods from a dedicated blocking task or store worker. Public
//! methods expose typed records and update inputs; SQL text remains private to
//! this crate so CLI, gateway, and adapter code use repository operations rather
//! than hand-written queries.

use std::collections::BTreeMap;
use std::fmt::{self, Display};
use std::path::Path;
use std::str::FromStr;

use missive_core::{
    AgentAlias, ContextId, EventId, GroupName, METADATA_A2A_PROTOCOL_VERSION, MessageId, Metadata,
    MissiveError, MissiveTimestamp, RankName, Result, TaskId, TransportName,
};
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{migrate_connection, open_sqlite_database};

const STORE_IDENTIFIER_MAX_BYTES: usize = 256;
const STORE_IDENTIFIER_HELP: &str =
    "Use a non-empty identifier without whitespace or control characters.";

macro_rules! impl_string_enum {
    ($name:ident, $label:literal, { $($variant:ident => $value:literal),+ $(,)? }) => {
        impl $name {
            /// Returns the SQLite representation for this value.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $value,)+
                }
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = MissiveError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($value => Ok(Self::$variant),)+
                    _ => {
                        let expected = [$($value),+].join(", ");
                        Err(MissiveError::validation(format!(
                            "invalid {} {value:?}; expected one of {expected}",
                            $label,
                        )))
                    }
                }
            }
        }
    };
}

macro_rules! store_identifier {
    ($name:ident, $kind:literal) => {
        #[doc = concat!("Strongly typed ", $kind, " wrapper used by the store layer.")]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            #[doc = concat!("Creates a validated ", $kind, ".")]
            pub fn new(value: impl Into<String>) -> Result<Self> {
                let value = value.into();
                validate_store_identifier($kind, &value)?;
                Ok(Self(value))
            }

            #[doc = concat!("Returns this ", $kind, " as a string slice.")]
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            #[doc = concat!("Consumes this ", $kind, " and returns the owned string.")]
            #[must_use]
            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = MissiveError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = MissiveError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = MissiveError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

store_identifier!(GatewayJobId, "gateway job id");
store_identifier!(GatewaySessionId, "gateway session id");
store_identifier!(AdapterBindingId, "adapter binding id");
store_identifier!(ArtifactId, "artifact id");
store_identifier!(PushConfigId, "push config id");

/// Durable authentication reference kind stored without raw secret material.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthRefKind {
    /// Secret value is resolved from an environment variable.
    #[default]
    Env,
    /// Secret value is resolved from the platform keyring.
    Keyring,
    /// Secret value is managed outside the current repository API.
    External,
}
impl_string_enum!(AuthRefKind, "auth ref kind", {
    Env => "env",
    Keyring => "keyring",
    External => "external",
});

/// Where the secret backing an auth reference is stored.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthSecretStorage {
    /// Secret value is managed externally and is not stored in SQLite.
    #[default]
    External,
    /// Secret value is resolved from an environment variable.
    Env,
    /// Secret value is resolved from the platform keyring.
    Keyring,
}
impl_string_enum!(AuthSecretStorage, "auth secret storage", {
    External => "external",
    Env => "env",
    Keyring => "keyring",
});

/// Source of an agent registry entry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSource {
    /// User-created local registry entry.
    #[default]
    Local,
    /// Read-only entry seeded from configuration.
    ConfigSeed,
    /// Entry discovered from a remote source.
    Discovered,
}
impl_string_enum!(AgentSource, "agent source", {
    Local => "local",
    ConfigSeed => "config_seed",
    Discovered => "discovered",
});

/// Durable context lifecycle state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextState {
    /// Context can accept new messages/tasks.
    #[default]
    Open,
    /// Context was intentionally closed.
    Closed,
    /// Context is retained for history but hidden from normal active lists.
    Archived,
}
impl_string_enum!(ContextState, "context state", {
    Open => "open",
    Closed => "closed",
    Archived => "archived",
});

/// Local view of an A2A task state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Request was submitted locally or remotely.
    Submitted,
    /// Remote task is working.
    Working,
    /// Remote task needs more input.
    InputRequired,
    /// Remote task completed successfully.
    Completed,
    /// Remote task failed.
    Failed,
    /// Remote task was cancelled.
    Cancelled,
    /// State could not be mapped to a known value.
    Unknown,
}
impl_string_enum!(TaskState, "task state", {
    Submitted => "submitted",
    Working => "working",
    InputRequired => "input_required",
    Completed => "completed",
    Failed => "failed",
    Cancelled => "cancelled",
    Unknown => "unknown",
});

/// Origin of a task record.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskSource {
    /// Task originated from a remote A2A service.
    #[default]
    Remote,
    /// Task was created locally before a remote response existed.
    Local,
    /// Task is managed by the local gateway.
    Gateway,
}
impl_string_enum!(TaskSource, "task source", {
    Remote => "remote",
    Local => "local",
    Gateway => "gateway",
});

/// Durable artifact content kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// Artifact contains primarily text parts.
    Text,
    /// Artifact contains structured JSON data parts.
    Json,
    /// Artifact references an external file URL.
    File,
    /// Artifact contains inline raw byte parts.
    Bytes,
    /// Artifact content kind could not be determined.
    Unknown,
}
impl_string_enum!(ArtifactKind, "artifact kind", {
    Text => "text",
    Json => "json",
    File => "file",
    Bytes => "bytes",
    Unknown => "unknown",
});

/// Direction/origin of a durable message row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageDirection {
    /// Outbound request sent by missive.
    Request,
    /// Immediate non-streaming response from a remote agent.
    Response,
    /// Streaming update message or event from a remote agent.
    StreamEvent,
    /// Push notification payload from a remote agent.
    Push,
    /// Locally generated note or synthetic message.
    Local,
}
impl_string_enum!(MessageDirection, "message direction", {
    Request => "request",
    Response => "response",
    StreamEvent => "stream_event",
    Push => "push",
    Local => "local",
});

/// Role attached to a durable message row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    /// User/client-authored message.
    User,
    /// Agent-authored message.
    Agent,
    /// System-authored message.
    System,
    /// Tool-authored message.
    Tool,
    /// Role is absent or could not be mapped.
    Unknown,
}
impl_string_enum!(MessageRole, "message role", {
    User => "user",
    Agent => "agent",
    System => "system",
    Tool => "tool",
    Unknown => "unknown",
});

/// Gateway background job state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayJobState {
    /// Job is waiting to run.
    Queued,
    /// Job is currently running.
    Running,
    /// Job completed successfully.
    Succeeded,
    /// Job failed permanently.
    Failed,
    /// Job was cancelled.
    Cancelled,
    /// Job will be retried after backoff.
    Retrying,
}
impl_string_enum!(GatewayJobState, "gateway job state", {
    Queued => "queued",
    Running => "running",
    Succeeded => "succeeded",
    Failed => "failed",
    Cancelled => "cancelled",
    Retrying => "retrying",
});

/// Reset policy mode for persistent gateway communication sessions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewaySessionResetMode {
    /// Never reset automatically; explicit commands/workers may still relink the session.
    #[default]
    None,
    /// Reset when a UTC daily boundary configured by `daily_reset_hour` has passed.
    Daily,
    /// Reset when the session has been idle longer than `idle_timeout_seconds`.
    Idle,
    /// Reset when either the daily boundary or idle timeout is reached.
    Both,
}
impl_string_enum!(GatewaySessionResetMode, "gateway session reset mode", {
    None => "none",
    Daily => "daily",
    Idle => "idle",
    Both => "both",
});

/// Input used to create or update a non-secret authentication reference row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthRefUpsert {
    /// Auth ref name used by agent rows.
    pub name: String,
    /// Reference kind.
    pub kind: AuthRefKind,
    /// HTTP header populated by future auth handling.
    pub header_name: String,
    /// Optional auth scheme prefix such as `Bearer`.
    pub scheme: Option<String>,
    /// Environment variable name for `kind = "env"`.
    pub env_var: Option<String>,
    /// Keyring service name for `kind = "keyring"`.
    pub keyring_service: Option<String>,
    /// Keyring account name for `kind = "keyring"`.
    pub keyring_account: Option<String>,
    /// Secret storage location. Raw secret values are not stored in this row.
    pub secret_storage: AuthSecretStorage,
    /// Non-secret metadata.
    pub metadata: Metadata,
}

impl AuthRefUpsert {
    /// Creates an environment-backed auth ref upsert.
    #[must_use]
    pub fn env(name: impl Into<String>, env_var: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: AuthRefKind::Env,
            header_name: "Authorization".to_owned(),
            scheme: Some("Bearer".to_owned()),
            env_var: Some(env_var.into()),
            keyring_service: None,
            keyring_account: None,
            secret_storage: AuthSecretStorage::Env,
            metadata: Metadata::new(),
        }
    }
}

/// Stored non-secret authentication reference row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthRefRecord {
    /// Auth ref name used by agent rows.
    pub name: String,
    /// Reference kind.
    pub kind: AuthRefKind,
    /// HTTP header populated by future auth handling.
    pub header_name: String,
    /// Optional auth scheme prefix such as `Bearer`.
    pub scheme: Option<String>,
    /// Environment variable name for `kind = "env"`.
    pub env_var: Option<String>,
    /// Keyring service name for `kind = "keyring"`.
    pub keyring_service: Option<String>,
    /// Keyring account name for `kind = "keyring"`.
    pub keyring_account: Option<String>,
    /// Secret storage location.
    pub secret_storage: AuthSecretStorage,
    /// Non-secret metadata.
    pub metadata: Metadata,
    /// Creation time recorded by SQLite.
    pub created_at: MissiveTimestamp,
    /// Last update time recorded by SQLite.
    pub updated_at: MissiveTimestamp,
}

/// Input used to create or update an agent registry row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentUpsert {
    /// Agent alias used by CLI, routing, tasks, and groups.
    pub alias: AgentAlias,
    /// Row source.
    pub source: AgentSource,
    /// Base URL used for Agent Card discovery.
    pub base_url: String,
    /// Explicit interface URLs keyed by transport binding.
    pub interface_urls: BTreeMap<TransportName, String>,
    /// Ordered transport preference.
    pub binding_preference: Vec<TransportName>,
    /// Optional auth reference name.
    pub auth_ref_name: Option<String>,
    /// Selection tags.
    pub tags: Vec<String>,
    /// Human notes.
    pub notes: Option<String>,
    /// Non-secret metadata.
    pub metadata: Metadata,
    /// Cached parsed/raw Agent Card JSON.
    pub agent_card_json: Option<Value>,
    /// Cached Agent Card ETag.
    pub agent_card_etag: Option<String>,
    /// Cached Agent Card Last-Modified value.
    pub agent_card_last_modified: Option<String>,
    /// Time the Agent Card was fetched.
    pub agent_card_fetched_at: Option<MissiveTimestamp>,
    /// Whether the entry is read-only to registry commands.
    pub read_only: bool,
}

impl AgentUpsert {
    /// Creates a local agent upsert with default A2A binding preference.
    #[must_use]
    pub fn new(alias: AgentAlias, base_url: impl Into<String>) -> Self {
        Self {
            alias,
            source: AgentSource::Local,
            base_url: base_url.into(),
            interface_urls: BTreeMap::new(),
            binding_preference: default_binding_preference(),
            auth_ref_name: None,
            tags: Vec::new(),
            notes: None,
            metadata: Metadata::new(),
            agent_card_json: None,
            agent_card_etag: None,
            agent_card_last_modified: None,
            agent_card_fetched_at: None,
            read_only: false,
        }
    }
}

/// Stored agent registry row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRecord {
    /// Agent alias used by CLI, routing, tasks, and groups.
    pub alias: AgentAlias,
    /// Row source.
    pub source: AgentSource,
    /// Base URL used for Agent Card discovery.
    pub base_url: String,
    /// Explicit interface URLs keyed by transport binding.
    pub interface_urls: BTreeMap<TransportName, String>,
    /// Ordered transport preference.
    pub binding_preference: Vec<TransportName>,
    /// Optional auth reference name.
    pub auth_ref_name: Option<String>,
    /// Selection tags.
    pub tags: Vec<String>,
    /// Human notes.
    pub notes: Option<String>,
    /// Non-secret metadata.
    pub metadata: Metadata,
    /// Cached parsed/raw Agent Card JSON.
    pub agent_card_json: Option<Value>,
    /// Cached Agent Card ETag.
    pub agent_card_etag: Option<String>,
    /// Cached Agent Card Last-Modified value.
    pub agent_card_last_modified: Option<String>,
    /// Time the Agent Card was fetched.
    pub agent_card_fetched_at: Option<MissiveTimestamp>,
    /// Whether the entry is read-only to registry commands.
    pub read_only: bool,
    /// Creation time recorded by SQLite.
    pub created_at: MissiveTimestamp,
    /// Last update time recorded by SQLite.
    pub updated_at: MissiveTimestamp,
}

/// Input used to create or update a context row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextUpsert {
    /// A2A context id.
    pub context_id: ContextId,
    /// Optional owning/default agent.
    pub agent_alias: Option<AgentAlias>,
    /// Human-friendly name.
    pub name: Option<String>,
    /// Optional fork parent.
    pub parent_context_id: Option<ContextId>,
    /// Context lifecycle state.
    pub state: ContextState,
    /// Optional summary.
    pub summary: Option<String>,
    /// Non-secret metadata.
    pub metadata: Metadata,
    /// Closure timestamp.
    pub closed_at: Option<MissiveTimestamp>,
}

impl ContextUpsert {
    /// Creates an open context upsert.
    #[must_use]
    pub fn new(context_id: ContextId) -> Self {
        Self {
            context_id,
            agent_alias: None,
            name: None,
            parent_context_id: None,
            state: ContextState::Open,
            summary: None,
            metadata: Metadata::new(),
            closed_at: None,
        }
    }
}

/// Stored context row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextRecord {
    /// A2A context id.
    pub context_id: ContextId,
    /// Optional owning/default agent.
    pub agent_alias: Option<AgentAlias>,
    /// Human-friendly name.
    pub name: Option<String>,
    /// Optional fork parent.
    pub parent_context_id: Option<ContextId>,
    /// Context lifecycle state.
    pub state: ContextState,
    /// Optional summary.
    pub summary: Option<String>,
    /// Non-secret metadata.
    pub metadata: Metadata,
    /// Creation time recorded by SQLite.
    pub created_at: MissiveTimestamp,
    /// Last update time recorded by SQLite.
    pub updated_at: MissiveTimestamp,
    /// Closure timestamp.
    pub closed_at: Option<MissiveTimestamp>,
}

/// Input used to create or update a task row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskUpsert {
    /// A2A/local task id.
    pub task_id: TaskId,
    /// Owning agent alias.
    pub agent_alias: AgentAlias,
    /// Optional context id.
    pub context_id: Option<ContextId>,
    /// Task state.
    pub state: TaskState,
    /// Task source.
    pub source: TaskSource,
    /// A2A protocol version associated with this task.
    pub protocol_version: Option<String>,
    /// Raw remote task JSON.
    pub remote_task_json: Option<Value>,
    /// Last linked message id.
    pub last_message_id: Option<MessageId>,
    /// Non-secret metadata.
    pub metadata: Metadata,
    /// Completion timestamp.
    pub completed_at: Option<MissiveTimestamp>,
}

impl TaskUpsert {
    /// Creates a remote task upsert.
    #[must_use]
    pub fn new(task_id: TaskId, agent_alias: AgentAlias, state: TaskState) -> Self {
        Self {
            task_id,
            agent_alias,
            context_id: None,
            state,
            source: TaskSource::Remote,
            protocol_version: None,
            remote_task_json: None,
            last_message_id: None,
            metadata: Metadata::new(),
            completed_at: None,
        }
    }

    /// Records the A2A protocol version used for the task in both the typed
    /// column and metadata map.
    pub fn record_a2a_protocol_version(
        &mut self,
        protocol_version: impl Into<String>,
    ) -> Result<()> {
        let protocol_version = protocol_version.into();
        validate_store_identifier("A2A protocol version", &protocol_version)?;
        self.protocol_version = Some(protocol_version.clone());
        self.metadata
            .insert_str(METADATA_A2A_PROTOCOL_VERSION, protocol_version)?;
        Ok(())
    }
}

/// Stored task row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskRecord {
    /// A2A/local task id.
    pub task_id: TaskId,
    /// Owning agent alias.
    pub agent_alias: AgentAlias,
    /// Optional context id.
    pub context_id: Option<ContextId>,
    /// Task state.
    pub state: TaskState,
    /// Task source.
    pub source: TaskSource,
    /// A2A protocol version associated with this task.
    pub protocol_version: Option<String>,
    /// Raw remote task JSON.
    pub remote_task_json: Option<Value>,
    /// Last linked message id.
    pub last_message_id: Option<MessageId>,
    /// Non-secret metadata.
    pub metadata: Metadata,
    /// Creation time recorded by SQLite.
    pub created_at: MissiveTimestamp,
    /// Last update time recorded by SQLite.
    pub updated_at: MissiveTimestamp,
    /// Completion timestamp.
    pub completed_at: Option<MissiveTimestamp>,
}

/// Input used to create or update an artifact row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactUpsert {
    /// A2A artifact id.
    pub artifact_id: ArtifactId,
    /// Owning task id.
    pub task_id: TaskId,
    /// Optional owning context id.
    pub context_id: Option<ContextId>,
    /// Artifact display name from the remote agent.
    pub name: Option<String>,
    /// Primary MIME/media type.
    pub mime_type: Option<String>,
    /// Durable artifact kind.
    pub kind: ArtifactKind,
    /// Monotonic local version for updates to the same artifact id.
    pub version: u64,
    /// Protocol-shaped artifact content JSON.
    pub content_json: Option<Value>,
    /// Optional local path for large byte payloads managed outside SQLite.
    pub bytes_path: Option<String>,
    /// Non-secret metadata.
    pub metadata: Metadata,
}

impl ArtifactUpsert {
    /// Creates an artifact upsert with default `unknown` kind and version 1.
    #[must_use]
    pub fn new(artifact_id: ArtifactId, task_id: TaskId) -> Self {
        Self {
            artifact_id,
            task_id,
            context_id: None,
            name: None,
            mime_type: None,
            kind: ArtifactKind::Unknown,
            version: 1,
            content_json: None,
            bytes_path: None,
            metadata: Metadata::new(),
        }
    }
}

/// Stored artifact row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactRecord {
    /// A2A artifact id.
    pub artifact_id: ArtifactId,
    /// Owning task id.
    pub task_id: TaskId,
    /// Optional owning context id.
    pub context_id: Option<ContextId>,
    /// Artifact display name from the remote agent.
    pub name: Option<String>,
    /// Primary MIME/media type.
    pub mime_type: Option<String>,
    /// Durable artifact kind.
    pub kind: ArtifactKind,
    /// Monotonic local version for updates to the same artifact id.
    pub version: u64,
    /// Protocol-shaped artifact content JSON.
    pub content_json: Option<Value>,
    /// Optional local path for large byte payloads managed outside SQLite.
    pub bytes_path: Option<String>,
    /// Non-secret metadata.
    pub metadata: Metadata,
    /// Creation time recorded by SQLite.
    pub created_at: MissiveTimestamp,
    /// Last update time recorded by SQLite.
    pub updated_at: MissiveTimestamp,
}

/// Input used to append a durable message row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageInsert {
    /// Local message id for this row.
    pub message_id: MessageId,
    /// Optional linked agent.
    pub agent_alias: Option<AgentAlias>,
    /// Optional linked context.
    pub context_id: Option<ContextId>,
    /// Optional linked task.
    pub task_id: Option<TaskId>,
    /// Message direction.
    pub direction: MessageDirection,
    /// Optional sender role.
    pub role: Option<MessageRole>,
    /// Stable order within a request/response or stream sequence.
    pub ordinal: u64,
    /// Protocol-level message id, when distinct from the local row id.
    pub protocol_message_id: Option<String>,
    /// Redacted or protocol-shaped message content JSON.
    pub content_json: Value,
    /// Non-secret metadata.
    pub metadata: Metadata,
}

impl MessageInsert {
    /// Creates a message insert with no context/task linkage yet.
    #[must_use]
    pub fn new(message_id: MessageId, direction: MessageDirection, content_json: Value) -> Self {
        Self {
            message_id,
            agent_alias: None,
            context_id: None,
            task_id: None,
            direction,
            role: None,
            ordinal: 0,
            protocol_message_id: None,
            content_json,
            metadata: Metadata::new(),
        }
    }
}

/// Stored message row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageRecord {
    /// Local message id for this row.
    pub message_id: MessageId,
    /// Optional linked agent.
    pub agent_alias: Option<AgentAlias>,
    /// Optional linked context.
    pub context_id: Option<ContextId>,
    /// Optional linked task.
    pub task_id: Option<TaskId>,
    /// Message direction.
    pub direction: MessageDirection,
    /// Optional sender role.
    pub role: Option<MessageRole>,
    /// Stable order within a request/response or stream sequence.
    pub ordinal: u64,
    /// Protocol-level message id, when distinct from the local row id.
    pub protocol_message_id: Option<String>,
    /// Redacted or protocol-shaped message content JSON.
    pub content_json: Value,
    /// Non-secret metadata.
    pub metadata: Metadata,
    /// Creation time recorded by SQLite.
    pub created_at: MissiveTimestamp,
}

/// Input used to append an event journal row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventInsert {
    /// Stable event id.
    pub event_id: EventId,
    /// Optional explicit timestamp for imports/tests.
    pub timestamp: Option<MissiveTimestamp>,
    /// Event source, for example `cli`, `gateway`, or `adapter:<name>`.
    pub source: String,
    /// Event type name.
    pub event_type: String,
    /// Optional linked agent.
    pub agent_alias: Option<AgentAlias>,
    /// Optional linked context.
    pub context_id: Option<ContextId>,
    /// Optional linked task.
    pub task_id: Option<TaskId>,
    /// Optional linked group.
    pub group_name: Option<GroupName>,
    /// Optional linked gateway job.
    pub gateway_job_id: Option<GatewayJobId>,
    /// Optional linked adapter binding.
    pub adapter_binding_id: Option<AdapterBindingId>,
    /// Redacted event payload.
    pub payload_json: Value,
    /// Non-secret metadata.
    pub metadata: Metadata,
    /// Whether the payload has been redacted.
    pub redacted: bool,
}

impl EventInsert {
    /// Creates a redacted event insert.
    #[must_use]
    pub fn new(
        event_id: EventId,
        source: impl Into<String>,
        event_type: impl Into<String>,
        payload_json: Value,
    ) -> Self {
        Self {
            event_id,
            timestamp: None,
            source: source.into(),
            event_type: event_type.into(),
            agent_alias: None,
            context_id: None,
            task_id: None,
            group_name: None,
            gateway_job_id: None,
            adapter_binding_id: None,
            payload_json,
            metadata: Metadata::new(),
            redacted: true,
        }
    }

    /// Records the A2A protocol version used for the event in metadata.
    pub fn record_a2a_protocol_version(
        &mut self,
        protocol_version: impl Into<String>,
    ) -> Result<()> {
        let protocol_version = protocol_version.into();
        validate_store_identifier("A2A protocol version", &protocol_version)?;
        self.metadata
            .insert_str(METADATA_A2A_PROTOCOL_VERSION, protocol_version)?;
        Ok(())
    }
}

/// Stored event journal row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventRecord {
    /// Monotonic SQLite event sequence.
    pub sequence: i64,
    /// Stable event id.
    pub event_id: EventId,
    /// Event timestamp.
    pub timestamp: MissiveTimestamp,
    /// Event source.
    pub source: String,
    /// Event type name.
    pub event_type: String,
    /// Optional linked agent.
    pub agent_alias: Option<AgentAlias>,
    /// Optional linked context.
    pub context_id: Option<ContextId>,
    /// Optional linked task.
    pub task_id: Option<TaskId>,
    /// Optional linked group.
    pub group_name: Option<GroupName>,
    /// Optional linked gateway job.
    pub gateway_job_id: Option<GatewayJobId>,
    /// Optional linked adapter binding.
    pub adapter_binding_id: Option<AdapterBindingId>,
    /// Redacted event payload.
    pub payload_json: Value,
    /// Non-secret metadata.
    pub metadata: Metadata,
    /// Whether the payload has been redacted.
    pub redacted: bool,
}

/// Input used to create or update a group row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupUpsert {
    /// Group name.
    pub group_name: GroupName,
    /// Routing policy name.
    pub routing_policy: String,
    /// Human notes.
    pub notes: Option<String>,
    /// Non-secret metadata.
    pub metadata: Metadata,
}

impl GroupUpsert {
    /// Creates a group upsert with the default direct routing policy.
    #[must_use]
    pub fn new(group_name: GroupName) -> Self {
        Self {
            group_name,
            routing_policy: "direct".to_owned(),
            notes: None,
            metadata: Metadata::new(),
        }
    }
}

/// Stored group row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupRecord {
    /// Group name.
    pub group_name: GroupName,
    /// Routing policy name.
    pub routing_policy: String,
    /// Human notes.
    pub notes: Option<String>,
    /// Non-secret metadata.
    pub metadata: Metadata,
    /// Creation time recorded by SQLite.
    pub created_at: MissiveTimestamp,
    /// Last update time recorded by SQLite.
    pub updated_at: MissiveTimestamp,
}

/// Input used to create or update group membership.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupMemberUpsert {
    /// Owning group.
    pub group_name: GroupName,
    /// Member agent alias.
    pub agent_alias: AgentAlias,
    /// Rank name unique within the group.
    pub rank_name: RankName,
    /// Member-specific tags.
    pub tags: Vec<String>,
    /// Routing weight.
    pub weight: f64,
    /// Routing metadata.
    pub routing_metadata: Metadata,
}

impl GroupMemberUpsert {
    /// Creates a group member with default weight `1.0`.
    #[must_use]
    pub fn new(group_name: GroupName, agent_alias: AgentAlias, rank_name: RankName) -> Self {
        Self {
            group_name,
            agent_alias,
            rank_name,
            tags: Vec::new(),
            weight: 1.0,
            routing_metadata: Metadata::new(),
        }
    }
}

/// Stored group membership row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupMemberRecord {
    /// Owning group.
    pub group_name: GroupName,
    /// Member agent alias.
    pub agent_alias: AgentAlias,
    /// Rank name unique within the group.
    pub rank_name: RankName,
    /// Member-specific tags.
    pub tags: Vec<String>,
    /// Routing weight.
    pub weight: f64,
    /// Routing metadata.
    pub routing_metadata: Metadata,
    /// Creation time recorded by SQLite.
    pub created_at: MissiveTimestamp,
}

/// Input used to create or update a local A2A task push notification config row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushConfigUpsert {
    /// Local/remote push config id.
    pub push_config_id: PushConfigId,
    /// Owning agent alias.
    pub agent_alias: AgentAlias,
    /// Optional linked A2A task id.
    pub task_id: Option<TaskId>,
    /// Callback URL configured on the remote task.
    pub callback_url: String,
    /// Optional non-secret auth reference name associated with the callback.
    pub auth_ref_name: Option<String>,
    /// Redacted remote A2A TaskPushNotificationConfig JSON.
    pub remote_config_json: Option<Value>,
    /// Non-secret metadata.
    pub metadata: Metadata,
    /// Soft-deletion timestamp.
    pub deleted_at: Option<MissiveTimestamp>,
}

impl PushConfigUpsert {
    /// Creates an active push config upsert.
    #[must_use]
    pub fn new(
        push_config_id: PushConfigId,
        agent_alias: AgentAlias,
        callback_url: impl Into<String>,
    ) -> Self {
        Self {
            push_config_id,
            agent_alias,
            task_id: None,
            callback_url: callback_url.into(),
            auth_ref_name: None,
            remote_config_json: None,
            metadata: Metadata::new(),
            deleted_at: None,
        }
    }
}

/// Stored local A2A task push notification config row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushConfigRecord {
    /// Local/remote push config id.
    pub push_config_id: PushConfigId,
    /// Owning agent alias.
    pub agent_alias: AgentAlias,
    /// Optional linked A2A task id.
    pub task_id: Option<TaskId>,
    /// Callback URL configured on the remote task.
    pub callback_url: String,
    /// Optional non-secret auth reference name associated with the callback.
    pub auth_ref_name: Option<String>,
    /// Redacted remote A2A TaskPushNotificationConfig JSON.
    pub remote_config_json: Option<Value>,
    /// Non-secret metadata.
    pub metadata: Metadata,
    /// Creation time recorded by SQLite.
    pub created_at: MissiveTimestamp,
    /// Last update time recorded by SQLite.
    pub updated_at: MissiveTimestamp,
    /// Soft-deletion timestamp.
    pub deleted_at: Option<MissiveTimestamp>,
}

/// Input used to create or update a gateway job row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GatewayJobUpsert {
    /// Gateway job id.
    pub gateway_job_id: GatewayJobId,
    /// Job kind, for example `send`, `stream`, or `wait`.
    pub kind: String,
    /// Job state.
    pub state: GatewayJobState,
    /// Optional linked agent.
    pub agent_alias: Option<AgentAlias>,
    /// Optional linked context.
    pub context_id: Option<ContextId>,
    /// Optional linked task.
    pub task_id: Option<TaskId>,
    /// Optional linked group.
    pub group_name: Option<GroupName>,
    /// Optional linked adapter binding.
    pub adapter_binding_id: Option<AdapterBindingId>,
    /// Request payload.
    pub request_json: Value,
    /// Optional result payload.
    pub result_json: Option<Value>,
    /// Non-secret metadata.
    pub metadata: Metadata,
    /// Number of attempts already made.
    pub retry_count: u32,
    /// Maximum attempts before permanent failure.
    pub max_attempts: u32,
    /// Next run time.
    pub next_run_at: Option<MissiveTimestamp>,
    /// Worker lock owner.
    pub locked_by: Option<String>,
    /// Worker lock expiration.
    pub locked_until: Option<MissiveTimestamp>,
    /// Completion timestamp.
    pub completed_at: Option<MissiveTimestamp>,
}

impl GatewayJobUpsert {
    /// Creates a queued gateway job upsert.
    #[must_use]
    pub fn new(gateway_job_id: GatewayJobId, kind: impl Into<String>, request_json: Value) -> Self {
        Self {
            gateway_job_id,
            kind: kind.into(),
            state: GatewayJobState::Queued,
            agent_alias: None,
            context_id: None,
            task_id: None,
            group_name: None,
            adapter_binding_id: None,
            request_json,
            result_json: None,
            metadata: Metadata::new(),
            retry_count: 0,
            max_attempts: 1,
            next_run_at: None,
            locked_by: None,
            locked_until: None,
            completed_at: None,
        }
    }
}

/// Stored gateway job row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GatewayJobRecord {
    /// Gateway job id.
    pub gateway_job_id: GatewayJobId,
    /// Job kind.
    pub kind: String,
    /// Job state.
    pub state: GatewayJobState,
    /// Optional linked agent.
    pub agent_alias: Option<AgentAlias>,
    /// Optional linked context.
    pub context_id: Option<ContextId>,
    /// Optional linked task.
    pub task_id: Option<TaskId>,
    /// Optional linked group.
    pub group_name: Option<GroupName>,
    /// Optional linked adapter binding.
    pub adapter_binding_id: Option<AdapterBindingId>,
    /// Request payload.
    pub request_json: Value,
    /// Optional result payload.
    pub result_json: Option<Value>,
    /// Non-secret metadata.
    pub metadata: Metadata,
    /// Number of attempts already made.
    pub retry_count: u32,
    /// Maximum attempts before permanent failure.
    pub max_attempts: u32,
    /// Next run time.
    pub next_run_at: Option<MissiveTimestamp>,
    /// Worker lock owner.
    pub locked_by: Option<String>,
    /// Worker lock expiration.
    pub locked_until: Option<MissiveTimestamp>,
    /// Creation time recorded by SQLite.
    pub created_at: MissiveTimestamp,
    /// Last update time recorded by SQLite.
    pub updated_at: MissiveTimestamp,
    /// Completion timestamp.
    pub completed_at: Option<MissiveTimestamp>,
}

/// Input used to create or update a persistent gateway communication session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GatewaySessionUpsert {
    /// Gateway session id.
    pub gateway_session_id: GatewaySessionId,
    /// Source kind such as `cli`, `adapter`, `webhook`, or a local integration name.
    pub source_kind: String,
    /// Stable source identity, for example an adapter user/channel composite.
    pub source_id: String,
    /// Target/default agent for this communication session.
    pub agent_alias: AgentAlias,
    /// Human or adapter-provided name used for later resume.
    pub resume_name: String,
    /// Current A2A context linked to this session.
    pub context_id: ContextId,
    /// Reset policy mode.
    pub reset_mode: GatewaySessionResetMode,
    /// UTC hour at which daily reset boundaries occur.
    pub daily_reset_hour: u8,
    /// Idle timeout in seconds for `idle` and `both` reset modes.
    pub idle_timeout_seconds: Option<u64>,
    /// Last inbound/outbound communication time for this source/agent session.
    pub last_active_at: MissiveTimestamp,
    /// Most recent reset timestamp, if the session has rotated contexts.
    pub last_reset_at: Option<MissiveTimestamp>,
    /// Number of times this session has been reset/rotated.
    pub reset_count: u64,
    /// Non-secret metadata.
    pub metadata: Metadata,
}

impl GatewaySessionUpsert {
    /// Creates a default non-auto-resetting gateway session upsert.
    #[must_use]
    pub fn new(
        gateway_session_id: GatewaySessionId,
        source_kind: impl Into<String>,
        source_id: impl Into<String>,
        agent_alias: AgentAlias,
        resume_name: impl Into<String>,
        context_id: ContextId,
    ) -> Self {
        Self {
            gateway_session_id,
            source_kind: source_kind.into(),
            source_id: source_id.into(),
            agent_alias,
            resume_name: resume_name.into(),
            context_id,
            reset_mode: GatewaySessionResetMode::None,
            daily_reset_hour: 0,
            idle_timeout_seconds: None,
            last_active_at: MissiveTimestamp::now_utc(),
            last_reset_at: None,
            reset_count: 0,
            metadata: Metadata::new(),
        }
    }
}

/// Stored persistent gateway communication session row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GatewaySessionRecord {
    /// Gateway session id.
    pub gateway_session_id: GatewaySessionId,
    /// Source kind such as `cli`, `adapter`, `webhook`, or a local integration name.
    pub source_kind: String,
    /// Stable source identity, for example an adapter user/channel composite.
    pub source_id: String,
    /// Target/default agent for this communication session.
    pub agent_alias: AgentAlias,
    /// Human or adapter-provided name used for later resume.
    pub resume_name: String,
    /// Current A2A context linked to this session.
    pub context_id: ContextId,
    /// Reset policy mode.
    pub reset_mode: GatewaySessionResetMode,
    /// UTC hour at which daily reset boundaries occur.
    pub daily_reset_hour: u8,
    /// Idle timeout in seconds for `idle` and `both` reset modes.
    pub idle_timeout_seconds: Option<u64>,
    /// Last inbound/outbound communication time for this source/agent session.
    pub last_active_at: MissiveTimestamp,
    /// Most recent reset timestamp, if the session has rotated contexts.
    pub last_reset_at: Option<MissiveTimestamp>,
    /// Number of times this session has been reset/rotated.
    pub reset_count: u64,
    /// Non-secret metadata.
    pub metadata: Metadata,
    /// Creation time recorded by SQLite.
    pub created_at: MissiveTimestamp,
    /// Last update time recorded by SQLite.
    pub updated_at: MissiveTimestamp,
}

/// Blocking SQLite repository facade for one profile database.
#[derive(Debug)]
pub struct Store {
    connection: Connection,
}

impl Store {
    /// Opens a SQLite database file and applies embedded migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let mut connection = open_sqlite_database(path.as_ref())?;
        migrate_connection(&mut connection)?;
        Ok(Self { connection })
    }

    /// Creates a store from an existing connection after applying embedded migrations.
    pub fn from_connection(mut connection: Connection) -> Result<Self> {
        migrate_connection(&mut connection)?;
        Ok(Self { connection })
    }

    /// Opens a migrated in-memory database. Useful for tests and local fixtures.
    pub fn open_in_memory() -> Result<Self> {
        let connection = Connection::open_in_memory()
            .map_err(|error| storage_error("opening in-memory SQLite database", error))?;
        Self::from_connection(connection)
    }

    /// Runs a closure inside a SQLite transaction.
    ///
    /// If the closure returns an error, the transaction is rolled back and the
    /// original error is returned. The transaction commits only when the closure
    /// returns `Ok`.
    pub fn transaction<T>(
        &mut self,
        operation: impl FnOnce(&mut StoreTransaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let transaction = self
            .connection
            .transaction()
            .map_err(|error| storage_error("starting SQLite repository transaction", error))?;
        let mut store_transaction = StoreTransaction { transaction };

        match operation(&mut store_transaction) {
            Ok(value) => {
                store_transaction.transaction.commit().map_err(|error| {
                    storage_error("committing SQLite repository transaction", error)
                })?;
                Ok(value)
            }
            Err(error) => {
                store_transaction
                    .transaction
                    .rollback()
                    .map_err(|rollback_error| {
                        storage_error("rolling back SQLite repository transaction", rollback_error)
                    })?;
                Err(error)
            }
        }
    }

    /// Creates or updates an auth ref and returns the stored row.
    pub fn upsert_auth_ref(&self, input: &AuthRefUpsert) -> Result<AuthRefRecord> {
        upsert_auth_ref(&self.connection, input)
    }

    /// Reads one auth ref by name.
    pub fn get_auth_ref(&self, name: &str) -> Result<Option<AuthRefRecord>> {
        get_auth_ref(&self.connection, name)
    }

    /// Lists auth refs in deterministic name order.
    pub fn list_auth_refs(&self) -> Result<Vec<AuthRefRecord>> {
        list_auth_refs(&self.connection)
    }

    /// Deletes an auth ref by name. Returns `true` when a row was removed.
    pub fn delete_auth_ref(&self, name: &str) -> Result<bool> {
        delete_auth_ref(&self.connection, name)
    }

    /// Creates or updates an agent and returns the stored row.
    pub fn upsert_agent(&self, input: &AgentUpsert) -> Result<AgentRecord> {
        upsert_agent(&self.connection, input)
    }

    /// Reads one agent by alias.
    pub fn get_agent(&self, alias: &AgentAlias) -> Result<Option<AgentRecord>> {
        get_agent(&self.connection, alias)
    }

    /// Lists agents in deterministic alias order.
    pub fn list_agents(&self) -> Result<Vec<AgentRecord>> {
        list_agents(&self.connection)
    }

    /// Deletes an agent by alias. Returns `true` when a row was removed.
    pub fn delete_agent(&self, alias: &AgentAlias) -> Result<bool> {
        delete_agent(&self.connection, alias)
    }

    /// Creates or updates a context and returns the stored row.
    pub fn upsert_context(&self, input: &ContextUpsert) -> Result<ContextRecord> {
        upsert_context(&self.connection, input)
    }

    /// Reads one context by id.
    pub fn get_context(&self, context_id: &ContextId) -> Result<Option<ContextRecord>> {
        get_context(&self.connection, context_id)
    }

    /// Lists contexts in deterministic id order.
    pub fn list_contexts(&self) -> Result<Vec<ContextRecord>> {
        list_contexts(&self.connection)
    }

    /// Deletes a context by id. Returns `true` when a row was removed.
    pub fn delete_context(&self, context_id: &ContextId) -> Result<bool> {
        delete_context(&self.connection, context_id)
    }

    /// Creates or updates a task and returns the stored row.
    pub fn upsert_task(&self, input: &TaskUpsert) -> Result<TaskRecord> {
        upsert_task(&self.connection, input)
    }

    /// Reads one task by id.
    pub fn get_task(&self, task_id: &TaskId) -> Result<Option<TaskRecord>> {
        get_task(&self.connection, task_id)
    }

    /// Lists tasks in deterministic id order.
    pub fn list_tasks(&self) -> Result<Vec<TaskRecord>> {
        list_tasks(&self.connection)
    }

    /// Deletes a task by id. Returns `true` when a row was removed.
    pub fn delete_task(&self, task_id: &TaskId) -> Result<bool> {
        delete_task(&self.connection, task_id)
    }

    /// Creates or updates an artifact and returns the stored row.
    pub fn upsert_artifact(&self, input: &ArtifactUpsert) -> Result<ArtifactRecord> {
        upsert_artifact(&self.connection, input)
    }

    /// Reads one artifact by id.
    pub fn get_artifact(&self, artifact_id: &ArtifactId) -> Result<Option<ArtifactRecord>> {
        get_artifact(&self.connection, artifact_id)
    }

    /// Lists artifacts in deterministic task/version/id order.
    pub fn list_artifacts(&self) -> Result<Vec<ArtifactRecord>> {
        list_artifacts(&self.connection)
    }

    /// Lists artifacts for one task in deterministic version/id order.
    pub fn list_artifacts_for_task(&self, task_id: &TaskId) -> Result<Vec<ArtifactRecord>> {
        list_artifacts_for_task(&self.connection, task_id)
    }

    /// Deletes an artifact by id. Returns `true` when a row was removed.
    pub fn delete_artifact(&self, artifact_id: &ArtifactId) -> Result<bool> {
        delete_artifact(&self.connection, artifact_id)
    }

    /// Appends a message and returns the stored row.
    pub fn insert_message(&self, input: &MessageInsert) -> Result<MessageRecord> {
        insert_message(&self.connection, input)
    }

    /// Reads one message by id.
    pub fn get_message(&self, message_id: &MessageId) -> Result<Option<MessageRecord>> {
        get_message(&self.connection, message_id)
    }

    /// Lists messages in deterministic context/task/ordinal order.
    pub fn list_messages(&self) -> Result<Vec<MessageRecord>> {
        list_messages(&self.connection)
    }

    /// Deletes a message by id. Returns `true` when a row was removed.
    pub fn delete_message(&self, message_id: &MessageId) -> Result<bool> {
        delete_message(&self.connection, message_id)
    }

    /// Appends an event and returns the stored event including sequence.
    pub fn append_event(&self, input: &EventInsert) -> Result<EventRecord> {
        append_event(&self.connection, input)
    }

    /// Reads one event by event id.
    pub fn get_event(&self, event_id: &EventId) -> Result<Option<EventRecord>> {
        get_event(&self.connection, event_id)
    }

    /// Lists events in sequence order.
    pub fn list_events(&self) -> Result<Vec<EventRecord>> {
        list_events(&self.connection)
    }

    /// Deletes an event by id. Returns `true` when a row was removed.
    pub fn delete_event(&self, event_id: &EventId) -> Result<bool> {
        delete_event(&self.connection, event_id)
    }

    /// Creates or updates a group and returns the stored row.
    pub fn upsert_group(&self, input: &GroupUpsert) -> Result<GroupRecord> {
        upsert_group(&self.connection, input)
    }

    /// Reads one group by name.
    pub fn get_group(&self, group_name: &GroupName) -> Result<Option<GroupRecord>> {
        get_group(&self.connection, group_name)
    }

    /// Lists groups in deterministic name order.
    pub fn list_groups(&self) -> Result<Vec<GroupRecord>> {
        list_groups(&self.connection)
    }

    /// Renames a group primary key and cascades the update to related rows.
    /// Returns `true` when a row was updated.
    pub fn rename_group(&self, old_name: &GroupName, new_name: &GroupName) -> Result<bool> {
        rename_group(&self.connection, old_name, new_name)
    }

    /// Deletes a group by name. Returns `true` when a row was removed.
    pub fn delete_group(&self, group_name: &GroupName) -> Result<bool> {
        delete_group(&self.connection, group_name)
    }

    /// Creates or updates a group member and returns the stored row.
    pub fn upsert_group_member(&self, input: &GroupMemberUpsert) -> Result<GroupMemberRecord> {
        upsert_group_member(&self.connection, input)
    }

    /// Lists group members in rank order for one group.
    pub fn list_group_members(&self, group_name: &GroupName) -> Result<Vec<GroupMemberRecord>> {
        list_group_members(&self.connection, group_name)
    }

    /// Removes one group member. Returns `true` when a row was removed.
    pub fn remove_group_member(
        &self,
        group_name: &GroupName,
        agent_alias: &AgentAlias,
    ) -> Result<bool> {
        remove_group_member(&self.connection, group_name, agent_alias)
    }

    /// Creates or updates a push config and returns the stored row.
    pub fn upsert_push_config(&self, input: &PushConfigUpsert) -> Result<PushConfigRecord> {
        upsert_push_config(&self.connection, input)
    }

    /// Reads one push config by id.
    pub fn get_push_config(
        &self,
        push_config_id: &PushConfigId,
    ) -> Result<Option<PushConfigRecord>> {
        get_push_config(&self.connection, push_config_id)
    }

    /// Lists push configs in deterministic agent/task/id order.
    pub fn list_push_configs(&self) -> Result<Vec<PushConfigRecord>> {
        list_push_configs(&self.connection)
    }

    /// Lists push configs for one task in deterministic id order.
    pub fn list_push_configs_for_task(
        &self,
        agent_alias: &AgentAlias,
        task_id: &TaskId,
    ) -> Result<Vec<PushConfigRecord>> {
        list_push_configs_for_task(&self.connection, agent_alias, task_id)
    }

    /// Soft-deletes a push config by id. Returns `true` when an active row was marked deleted.
    pub fn delete_push_config(&self, push_config_id: &PushConfigId) -> Result<bool> {
        delete_push_config(&self.connection, push_config_id)
    }

    /// Creates or updates a gateway job and returns the stored row.
    pub fn upsert_gateway_job(&self, input: &GatewayJobUpsert) -> Result<GatewayJobRecord> {
        upsert_gateway_job(&self.connection, input)
    }

    /// Reads one gateway job by id.
    pub fn get_gateway_job(
        &self,
        gateway_job_id: &GatewayJobId,
    ) -> Result<Option<GatewayJobRecord>> {
        get_gateway_job(&self.connection, gateway_job_id)
    }

    /// Lists gateway jobs in deterministic id order.
    pub fn list_gateway_jobs(&self) -> Result<Vec<GatewayJobRecord>> {
        list_gateway_jobs(&self.connection)
    }

    /// Deletes a gateway job by id. Returns `true` when a row was removed.
    pub fn delete_gateway_job(&self, gateway_job_id: &GatewayJobId) -> Result<bool> {
        delete_gateway_job(&self.connection, gateway_job_id)
    }

    /// Creates or updates a gateway session and returns the stored row.
    pub fn upsert_gateway_session(
        &self,
        input: &GatewaySessionUpsert,
    ) -> Result<GatewaySessionRecord> {
        upsert_gateway_session(&self.connection, input)
    }

    /// Reads one gateway session by id.
    pub fn get_gateway_session(
        &self,
        gateway_session_id: &GatewaySessionId,
    ) -> Result<Option<GatewaySessionRecord>> {
        get_gateway_session(&self.connection, gateway_session_id)
    }

    /// Reads one gateway session by source/agent/resume name.
    pub fn get_gateway_session_by_resume(
        &self,
        source_kind: &str,
        source_id: &str,
        agent_alias: &AgentAlias,
        resume_name: &str,
    ) -> Result<Option<GatewaySessionRecord>> {
        get_gateway_session_by_resume(
            &self.connection,
            source_kind,
            source_id,
            agent_alias,
            resume_name,
        )
    }

    /// Lists gateway sessions in deterministic source/agent/name order.
    pub fn list_gateway_sessions(&self) -> Result<Vec<GatewaySessionRecord>> {
        list_gateway_sessions(&self.connection)
    }

    /// Lists gateway sessions for one target agent in deterministic source/name order.
    pub fn list_gateway_sessions_for_agent(
        &self,
        agent_alias: &AgentAlias,
    ) -> Result<Vec<GatewaySessionRecord>> {
        list_gateway_sessions_for_agent(&self.connection, agent_alias)
    }

    /// Deletes a gateway session by id. Returns `true` when a row was removed.
    pub fn delete_gateway_session(&self, gateway_session_id: &GatewaySessionId) -> Result<bool> {
        delete_gateway_session(&self.connection, gateway_session_id)
    }
}

/// Repository view scoped to an active SQLite transaction.
pub struct StoreTransaction<'connection> {
    transaction: Transaction<'connection>,
}

impl StoreTransaction<'_> {
    /// Creates or updates an auth ref and returns the stored row.
    pub fn upsert_auth_ref(&self, input: &AuthRefUpsert) -> Result<AuthRefRecord> {
        upsert_auth_ref(&self.transaction, input)
    }

    /// Reads one auth ref by name.
    pub fn get_auth_ref(&self, name: &str) -> Result<Option<AuthRefRecord>> {
        get_auth_ref(&self.transaction, name)
    }

    /// Lists auth refs in deterministic name order.
    pub fn list_auth_refs(&self) -> Result<Vec<AuthRefRecord>> {
        list_auth_refs(&self.transaction)
    }

    /// Deletes an auth ref by name. Returns `true` when a row was removed.
    pub fn delete_auth_ref(&self, name: &str) -> Result<bool> {
        delete_auth_ref(&self.transaction, name)
    }

    /// Creates or updates an agent and returns the stored row.
    pub fn upsert_agent(&self, input: &AgentUpsert) -> Result<AgentRecord> {
        upsert_agent(&self.transaction, input)
    }

    /// Reads one agent by alias.
    pub fn get_agent(&self, alias: &AgentAlias) -> Result<Option<AgentRecord>> {
        get_agent(&self.transaction, alias)
    }

    /// Lists agents in deterministic alias order.
    pub fn list_agents(&self) -> Result<Vec<AgentRecord>> {
        list_agents(&self.transaction)
    }

    /// Deletes an agent by alias. Returns `true` when a row was removed.
    pub fn delete_agent(&self, alias: &AgentAlias) -> Result<bool> {
        delete_agent(&self.transaction, alias)
    }

    /// Creates or updates a context and returns the stored row.
    pub fn upsert_context(&self, input: &ContextUpsert) -> Result<ContextRecord> {
        upsert_context(&self.transaction, input)
    }

    /// Reads one context by id.
    pub fn get_context(&self, context_id: &ContextId) -> Result<Option<ContextRecord>> {
        get_context(&self.transaction, context_id)
    }

    /// Lists contexts in deterministic id order.
    pub fn list_contexts(&self) -> Result<Vec<ContextRecord>> {
        list_contexts(&self.transaction)
    }

    /// Deletes a context by id. Returns `true` when a row was removed.
    pub fn delete_context(&self, context_id: &ContextId) -> Result<bool> {
        delete_context(&self.transaction, context_id)
    }

    /// Creates or updates a task and returns the stored row.
    pub fn upsert_task(&self, input: &TaskUpsert) -> Result<TaskRecord> {
        upsert_task(&self.transaction, input)
    }

    /// Reads one task by id.
    pub fn get_task(&self, task_id: &TaskId) -> Result<Option<TaskRecord>> {
        get_task(&self.transaction, task_id)
    }

    /// Lists tasks in deterministic id order.
    pub fn list_tasks(&self) -> Result<Vec<TaskRecord>> {
        list_tasks(&self.transaction)
    }

    /// Deletes a task by id. Returns `true` when a row was removed.
    pub fn delete_task(&self, task_id: &TaskId) -> Result<bool> {
        delete_task(&self.transaction, task_id)
    }

    /// Creates or updates an artifact and returns the stored row.
    pub fn upsert_artifact(&self, input: &ArtifactUpsert) -> Result<ArtifactRecord> {
        upsert_artifact(&self.transaction, input)
    }

    /// Reads one artifact by id.
    pub fn get_artifact(&self, artifact_id: &ArtifactId) -> Result<Option<ArtifactRecord>> {
        get_artifact(&self.transaction, artifact_id)
    }

    /// Lists artifacts in deterministic task/version/id order.
    pub fn list_artifacts(&self) -> Result<Vec<ArtifactRecord>> {
        list_artifacts(&self.transaction)
    }

    /// Lists artifacts for one task in deterministic version/id order.
    pub fn list_artifacts_for_task(&self, task_id: &TaskId) -> Result<Vec<ArtifactRecord>> {
        list_artifacts_for_task(&self.transaction, task_id)
    }

    /// Deletes an artifact by id. Returns `true` when a row was removed.
    pub fn delete_artifact(&self, artifact_id: &ArtifactId) -> Result<bool> {
        delete_artifact(&self.transaction, artifact_id)
    }

    /// Appends a message and returns the stored row.
    pub fn insert_message(&self, input: &MessageInsert) -> Result<MessageRecord> {
        insert_message(&self.transaction, input)
    }

    /// Reads one message by id.
    pub fn get_message(&self, message_id: &MessageId) -> Result<Option<MessageRecord>> {
        get_message(&self.transaction, message_id)
    }

    /// Lists messages in deterministic context/task/ordinal order.
    pub fn list_messages(&self) -> Result<Vec<MessageRecord>> {
        list_messages(&self.transaction)
    }

    /// Deletes a message by id. Returns `true` when a row was removed.
    pub fn delete_message(&self, message_id: &MessageId) -> Result<bool> {
        delete_message(&self.transaction, message_id)
    }

    /// Appends an event and returns the stored event including sequence.
    pub fn append_event(&self, input: &EventInsert) -> Result<EventRecord> {
        append_event(&self.transaction, input)
    }

    /// Reads one event by event id.
    pub fn get_event(&self, event_id: &EventId) -> Result<Option<EventRecord>> {
        get_event(&self.transaction, event_id)
    }

    /// Lists events in sequence order.
    pub fn list_events(&self) -> Result<Vec<EventRecord>> {
        list_events(&self.transaction)
    }

    /// Deletes an event by id. Returns `true` when a row was removed.
    pub fn delete_event(&self, event_id: &EventId) -> Result<bool> {
        delete_event(&self.transaction, event_id)
    }

    /// Creates or updates a group and returns the stored row.
    pub fn upsert_group(&self, input: &GroupUpsert) -> Result<GroupRecord> {
        upsert_group(&self.transaction, input)
    }

    /// Reads one group by name.
    pub fn get_group(&self, group_name: &GroupName) -> Result<Option<GroupRecord>> {
        get_group(&self.transaction, group_name)
    }

    /// Lists groups in deterministic name order.
    pub fn list_groups(&self) -> Result<Vec<GroupRecord>> {
        list_groups(&self.transaction)
    }

    /// Renames a group primary key and cascades the update to related rows.
    /// Returns `true` when a row was updated.
    pub fn rename_group(&self, old_name: &GroupName, new_name: &GroupName) -> Result<bool> {
        rename_group(&self.transaction, old_name, new_name)
    }

    /// Deletes a group by name. Returns `true` when a row was removed.
    pub fn delete_group(&self, group_name: &GroupName) -> Result<bool> {
        delete_group(&self.transaction, group_name)
    }

    /// Creates or updates a group member and returns the stored row.
    pub fn upsert_group_member(&self, input: &GroupMemberUpsert) -> Result<GroupMemberRecord> {
        upsert_group_member(&self.transaction, input)
    }

    /// Lists group members in rank order for one group.
    pub fn list_group_members(&self, group_name: &GroupName) -> Result<Vec<GroupMemberRecord>> {
        list_group_members(&self.transaction, group_name)
    }

    /// Removes one group member. Returns `true` when a row was removed.
    pub fn remove_group_member(
        &self,
        group_name: &GroupName,
        agent_alias: &AgentAlias,
    ) -> Result<bool> {
        remove_group_member(&self.transaction, group_name, agent_alias)
    }

    /// Creates or updates a push config and returns the stored row.
    pub fn upsert_push_config(&self, input: &PushConfigUpsert) -> Result<PushConfigRecord> {
        upsert_push_config(&self.transaction, input)
    }

    /// Reads one push config by id.
    pub fn get_push_config(
        &self,
        push_config_id: &PushConfigId,
    ) -> Result<Option<PushConfigRecord>> {
        get_push_config(&self.transaction, push_config_id)
    }

    /// Lists push configs in deterministic agent/task/id order.
    pub fn list_push_configs(&self) -> Result<Vec<PushConfigRecord>> {
        list_push_configs(&self.transaction)
    }

    /// Lists push configs for one task in deterministic id order.
    pub fn list_push_configs_for_task(
        &self,
        agent_alias: &AgentAlias,
        task_id: &TaskId,
    ) -> Result<Vec<PushConfigRecord>> {
        list_push_configs_for_task(&self.transaction, agent_alias, task_id)
    }

    /// Soft-deletes a push config by id. Returns `true` when an active row was marked deleted.
    pub fn delete_push_config(&self, push_config_id: &PushConfigId) -> Result<bool> {
        delete_push_config(&self.transaction, push_config_id)
    }

    /// Creates or updates a gateway job and returns the stored row.
    pub fn upsert_gateway_job(&self, input: &GatewayJobUpsert) -> Result<GatewayJobRecord> {
        upsert_gateway_job(&self.transaction, input)
    }

    /// Reads one gateway job by id.
    pub fn get_gateway_job(
        &self,
        gateway_job_id: &GatewayJobId,
    ) -> Result<Option<GatewayJobRecord>> {
        get_gateway_job(&self.transaction, gateway_job_id)
    }

    /// Lists gateway jobs in deterministic id order.
    pub fn list_gateway_jobs(&self) -> Result<Vec<GatewayJobRecord>> {
        list_gateway_jobs(&self.transaction)
    }

    /// Deletes a gateway job by id. Returns `true` when a row was removed.
    pub fn delete_gateway_job(&self, gateway_job_id: &GatewayJobId) -> Result<bool> {
        delete_gateway_job(&self.transaction, gateway_job_id)
    }

    /// Creates or updates a gateway session and returns the stored row.
    pub fn upsert_gateway_session(
        &self,
        input: &GatewaySessionUpsert,
    ) -> Result<GatewaySessionRecord> {
        upsert_gateway_session(&self.transaction, input)
    }

    /// Reads one gateway session by id.
    pub fn get_gateway_session(
        &self,
        gateway_session_id: &GatewaySessionId,
    ) -> Result<Option<GatewaySessionRecord>> {
        get_gateway_session(&self.transaction, gateway_session_id)
    }

    /// Reads one gateway session by source/agent/resume name.
    pub fn get_gateway_session_by_resume(
        &self,
        source_kind: &str,
        source_id: &str,
        agent_alias: &AgentAlias,
        resume_name: &str,
    ) -> Result<Option<GatewaySessionRecord>> {
        get_gateway_session_by_resume(
            &self.transaction,
            source_kind,
            source_id,
            agent_alias,
            resume_name,
        )
    }

    /// Lists gateway sessions in deterministic source/agent/name order.
    pub fn list_gateway_sessions(&self) -> Result<Vec<GatewaySessionRecord>> {
        list_gateway_sessions(&self.transaction)
    }

    /// Lists gateway sessions for one target agent in deterministic source/name order.
    pub fn list_gateway_sessions_for_agent(
        &self,
        agent_alias: &AgentAlias,
    ) -> Result<Vec<GatewaySessionRecord>> {
        list_gateway_sessions_for_agent(&self.transaction, agent_alias)
    }

    /// Deletes a gateway session by id. Returns `true` when a row was removed.
    pub fn delete_gateway_session(&self, gateway_session_id: &GatewaySessionId) -> Result<bool> {
        delete_gateway_session(&self.transaction, gateway_session_id)
    }
}

fn upsert_auth_ref(connection: &Connection, input: &AuthRefUpsert) -> Result<AuthRefRecord> {
    validate_store_identifier("auth ref name", &input.name)?;
    validate_len("auth ref header_name", &input.header_name, 128)?;
    if let Some(scheme) = &input.scheme {
        validate_len("auth ref scheme", scheme, 64)?;
    }
    match input.kind {
        AuthRefKind::Env => {
            validate_required_option("auth ref env_var", input.env_var.as_deref())?;
        }
        AuthRefKind::Keyring => {
            validate_required_option("auth ref keyring_service", input.keyring_service.as_deref())?;
            validate_required_option("auth ref keyring_account", input.keyring_account.as_deref())?;
        }
        AuthRefKind::External => {}
    }
    let metadata_json = to_json_text("auth ref metadata", &input.metadata)?;

    connection
        .execute(
            "INSERT INTO auth_refs (
                name, kind, header_name, scheme, env_var, keyring_service,
                keyring_account, secret_storage, metadata_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(name) DO UPDATE SET
                kind = excluded.kind,
                header_name = excluded.header_name,
                scheme = excluded.scheme,
                env_var = excluded.env_var,
                keyring_service = excluded.keyring_service,
                keyring_account = excluded.keyring_account,
                secret_storage = excluded.secret_storage,
                metadata_json = excluded.metadata_json,
                updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
            params![
                &input.name,
                input.kind.as_str(),
                &input.header_name,
                input.scheme.as_deref(),
                input.env_var.as_deref(),
                input.keyring_service.as_deref(),
                input.keyring_account.as_deref(),
                input.secret_storage.as_str(),
                metadata_json,
            ],
        )
        .map_err(|error| storage_error("upserting auth ref", error))?;

    get_auth_ref(connection, &input.name)?
        .ok_or_else(|| missing_after_write("auth ref", &input.name))
}

fn get_auth_ref(connection: &Connection, name: &str) -> Result<Option<AuthRefRecord>> {
    connection
        .query_row(
            "SELECT name, kind, header_name, scheme, env_var, keyring_service,
                keyring_account, secret_storage, metadata_json, created_at, updated_at
             FROM auth_refs WHERE name = ?1",
            params![name],
            read_auth_ref_row,
        )
        .optional()
        .map_err(|error| storage_error("reading auth ref", error))
}

fn list_auth_refs(connection: &Connection) -> Result<Vec<AuthRefRecord>> {
    let mut statement = connection
        .prepare(
            "SELECT name, kind, header_name, scheme, env_var, keyring_service,
                keyring_account, secret_storage, metadata_json, created_at, updated_at
             FROM auth_refs ORDER BY name",
        )
        .map_err(|error| storage_error("preparing auth ref list", error))?;
    collect_rows(
        statement.query_map([], read_auth_ref_row),
        "listing auth refs",
    )
}

fn delete_auth_ref(connection: &Connection, name: &str) -> Result<bool> {
    delete_by_key(
        connection,
        "DELETE FROM auth_refs WHERE name = ?1",
        name,
        "deleting auth ref",
    )
}

fn upsert_agent(connection: &Connection, input: &AgentUpsert) -> Result<AgentRecord> {
    validate_non_empty("agent base_url", &input.base_url)?;
    let interface_urls_json = to_json_text("agent interface_urls", &input.interface_urls)?;
    let binding_preference_json =
        to_json_text("agent binding_preference", &input.binding_preference)?;
    let tags_json = to_json_text("agent tags", &input.tags)?;
    let metadata_json = to_json_text("agent metadata", &input.metadata)?;
    let agent_card_json = optional_json_text("agent card", input.agent_card_json.as_ref())?;
    let fetched_at = input
        .agent_card_fetched_at
        .map(MissiveTimestamp::to_rfc3339);

    connection
        .execute(
            "INSERT INTO agents (
                alias, source, base_url, interface_urls_json, binding_preference_json,
                auth_ref_name, tags_json, notes, metadata_json, agent_card_json,
                agent_card_etag, agent_card_last_modified, agent_card_fetched_at, read_only
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
            ON CONFLICT(alias) DO UPDATE SET
                source = excluded.source,
                base_url = excluded.base_url,
                interface_urls_json = excluded.interface_urls_json,
                binding_preference_json = excluded.binding_preference_json,
                auth_ref_name = excluded.auth_ref_name,
                tags_json = excluded.tags_json,
                notes = excluded.notes,
                metadata_json = excluded.metadata_json,
                agent_card_json = excluded.agent_card_json,
                agent_card_etag = excluded.agent_card_etag,
                agent_card_last_modified = excluded.agent_card_last_modified,
                agent_card_fetched_at = excluded.agent_card_fetched_at,
                read_only = excluded.read_only,
                updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
            params![
                input.alias.as_str(),
                input.source.as_str(),
                &input.base_url,
                interface_urls_json,
                binding_preference_json,
                input.auth_ref_name.as_deref(),
                tags_json,
                input.notes.as_deref(),
                metadata_json,
                agent_card_json.as_deref(),
                input.agent_card_etag.as_deref(),
                input.agent_card_last_modified.as_deref(),
                fetched_at.as_deref(),
                input.read_only,
            ],
        )
        .map_err(|error| storage_error("upserting agent", error))?;

    get_agent(connection, &input.alias)?
        .ok_or_else(|| missing_after_write("agent", input.alias.as_str()))
}

fn get_agent(connection: &Connection, alias: &AgentAlias) -> Result<Option<AgentRecord>> {
    connection
        .query_row(
            "SELECT alias, source, base_url, interface_urls_json, binding_preference_json,
                auth_ref_name, tags_json, notes, metadata_json, agent_card_json,
                agent_card_etag, agent_card_last_modified, agent_card_fetched_at,
                read_only, created_at, updated_at
             FROM agents WHERE alias = ?1",
            params![alias.as_str()],
            read_agent_row,
        )
        .optional()
        .map_err(|error| storage_error("reading agent", error))
}

fn list_agents(connection: &Connection) -> Result<Vec<AgentRecord>> {
    let mut statement = connection
        .prepare(
            "SELECT alias, source, base_url, interface_urls_json, binding_preference_json,
                auth_ref_name, tags_json, notes, metadata_json, agent_card_json,
                agent_card_etag, agent_card_last_modified, agent_card_fetched_at,
                read_only, created_at, updated_at
             FROM agents ORDER BY alias",
        )
        .map_err(|error| storage_error("preparing agent list", error))?;
    collect_rows(statement.query_map([], read_agent_row), "listing agents")
}

fn delete_agent(connection: &Connection, alias: &AgentAlias) -> Result<bool> {
    delete_by_key(
        connection,
        "DELETE FROM agents WHERE alias = ?1",
        alias.as_str(),
        "deleting agent",
    )
}

fn upsert_context(connection: &Connection, input: &ContextUpsert) -> Result<ContextRecord> {
    let metadata_json = to_json_text("context metadata", &input.metadata)?;
    let closed_at = input.closed_at.map(MissiveTimestamp::to_rfc3339);

    connection
        .execute(
            "INSERT INTO contexts (
                context_id, agent_alias, name, parent_context_id, state, summary, metadata_json, closed_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(context_id) DO UPDATE SET
                agent_alias = excluded.agent_alias,
                name = excluded.name,
                parent_context_id = excluded.parent_context_id,
                state = excluded.state,
                summary = excluded.summary,
                metadata_json = excluded.metadata_json,
                closed_at = excluded.closed_at,
                updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
            params![
                input.context_id.as_str(),
                input.agent_alias.as_ref().map(AgentAlias::as_str),
                input.name.as_deref(),
                input.parent_context_id.as_ref().map(ContextId::as_str),
                input.state.as_str(),
                input.summary.as_deref(),
                metadata_json,
                closed_at.as_deref(),
            ],
        )
        .map_err(|error| storage_error("upserting context", error))?;

    get_context(connection, &input.context_id)?
        .ok_or_else(|| missing_after_write("context", input.context_id.as_str()))
}

fn get_context(connection: &Connection, context_id: &ContextId) -> Result<Option<ContextRecord>> {
    connection
        .query_row(
            "SELECT context_id, agent_alias, name, parent_context_id, state, summary,
                metadata_json, created_at, updated_at, closed_at
             FROM contexts WHERE context_id = ?1",
            params![context_id.as_str()],
            read_context_row,
        )
        .optional()
        .map_err(|error| storage_error("reading context", error))
}

fn list_contexts(connection: &Connection) -> Result<Vec<ContextRecord>> {
    let mut statement = connection
        .prepare(
            "SELECT context_id, agent_alias, name, parent_context_id, state, summary,
                metadata_json, created_at, updated_at, closed_at
             FROM contexts ORDER BY context_id",
        )
        .map_err(|error| storage_error("preparing context list", error))?;
    collect_rows(
        statement.query_map([], read_context_row),
        "listing contexts",
    )
}

fn delete_context(connection: &Connection, context_id: &ContextId) -> Result<bool> {
    delete_by_key(
        connection,
        "DELETE FROM contexts WHERE context_id = ?1",
        context_id.as_str(),
        "deleting context",
    )
}

fn upsert_task(connection: &Connection, input: &TaskUpsert) -> Result<TaskRecord> {
    let remote_task_json = optional_json_text("remote task", input.remote_task_json.as_ref())?;
    let metadata_json = to_json_text("task metadata", &input.metadata)?;
    let completed_at = input.completed_at.map(MissiveTimestamp::to_rfc3339);

    connection
        .execute(
            "INSERT INTO tasks (
                task_id, agent_alias, context_id, state, source, protocol_version,
                remote_task_json, last_message_id, metadata_json, completed_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(task_id) DO UPDATE SET
                agent_alias = excluded.agent_alias,
                context_id = excluded.context_id,
                state = excluded.state,
                source = excluded.source,
                protocol_version = excluded.protocol_version,
                remote_task_json = excluded.remote_task_json,
                last_message_id = excluded.last_message_id,
                metadata_json = excluded.metadata_json,
                completed_at = excluded.completed_at,
                updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
            params![
                input.task_id.as_str(),
                input.agent_alias.as_str(),
                input.context_id.as_ref().map(ContextId::as_str),
                input.state.as_str(),
                input.source.as_str(),
                input.protocol_version.as_deref(),
                remote_task_json.as_deref(),
                input.last_message_id.as_ref().map(MessageId::as_str),
                metadata_json,
                completed_at.as_deref(),
            ],
        )
        .map_err(|error| storage_error("upserting task", error))?;

    get_task(connection, &input.task_id)?
        .ok_or_else(|| missing_after_write("task", input.task_id.as_str()))
}

fn get_task(connection: &Connection, task_id: &TaskId) -> Result<Option<TaskRecord>> {
    connection
        .query_row(
            "SELECT task_id, agent_alias, context_id, state, source, protocol_version,
                remote_task_json, last_message_id, metadata_json, created_at, updated_at, completed_at
             FROM tasks WHERE task_id = ?1",
            params![task_id.as_str()],
            read_task_row,
        )
        .optional()
        .map_err(|error| storage_error("reading task", error))
}

fn list_tasks(connection: &Connection) -> Result<Vec<TaskRecord>> {
    let mut statement = connection
        .prepare(
            "SELECT task_id, agent_alias, context_id, state, source, protocol_version,
                remote_task_json, last_message_id, metadata_json, created_at, updated_at, completed_at
             FROM tasks ORDER BY task_id",
        )
        .map_err(|error| storage_error("preparing task list", error))?;
    collect_rows(statement.query_map([], read_task_row), "listing tasks")
}

fn delete_task(connection: &Connection, task_id: &TaskId) -> Result<bool> {
    delete_by_key(
        connection,
        "DELETE FROM tasks WHERE task_id = ?1",
        task_id.as_str(),
        "deleting task",
    )
}

fn upsert_artifact(connection: &Connection, input: &ArtifactUpsert) -> Result<ArtifactRecord> {
    if let Some(name) = &input.name {
        validate_len("artifact name", name, 512)?;
    }
    if let Some(mime_type) = &input.mime_type {
        validate_len("artifact mime_type", mime_type, 255)?;
    }
    if let Some(bytes_path) = &input.bytes_path {
        validate_len("artifact bytes_path", bytes_path, 4096)?;
    }
    let version = i64::try_from(input.version).map_err(|error| {
        MissiveError::validation("artifact version is too large to store in SQLite")
            .with_source(error)
    })?;
    let content_json = optional_json_text("artifact content", input.content_json.as_ref())?;
    let metadata_json = to_json_text("artifact metadata", &input.metadata)?;

    connection
        .execute(
            "INSERT INTO artifacts (
                artifact_id, task_id, context_id, name, mime_type, kind, version,
                content_json, bytes_path, metadata_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(artifact_id) DO UPDATE SET
                task_id = excluded.task_id,
                context_id = excluded.context_id,
                name = excluded.name,
                mime_type = excluded.mime_type,
                kind = excluded.kind,
                version = excluded.version,
                content_json = excluded.content_json,
                bytes_path = excluded.bytes_path,
                metadata_json = excluded.metadata_json,
                updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
            params![
                input.artifact_id.as_str(),
                input.task_id.as_str(),
                input.context_id.as_ref().map(ContextId::as_str),
                input.name.as_deref(),
                input.mime_type.as_deref(),
                input.kind.as_str(),
                version,
                content_json.as_deref(),
                input.bytes_path.as_deref(),
                metadata_json,
            ],
        )
        .map_err(|error| storage_error("upserting artifact", error))?;

    get_artifact(connection, &input.artifact_id)?
        .ok_or_else(|| missing_after_write("artifact", input.artifact_id.as_str()))
}

fn get_artifact(
    connection: &Connection,
    artifact_id: &ArtifactId,
) -> Result<Option<ArtifactRecord>> {
    connection
        .query_row(
            "SELECT artifact_id, task_id, context_id, name, mime_type, kind, version,
                content_json, bytes_path, metadata_json, created_at, updated_at
             FROM artifacts WHERE artifact_id = ?1",
            params![artifact_id.as_str()],
            read_artifact_row,
        )
        .optional()
        .map_err(|error| storage_error("reading artifact", error))
}

fn list_artifacts(connection: &Connection) -> Result<Vec<ArtifactRecord>> {
    let mut statement = connection
        .prepare(
            "SELECT artifact_id, task_id, context_id, name, mime_type, kind, version,
                content_json, bytes_path, metadata_json, created_at, updated_at
             FROM artifacts ORDER BY task_id, version, artifact_id",
        )
        .map_err(|error| storage_error("preparing artifact list", error))?;
    collect_rows(
        statement.query_map([], read_artifact_row),
        "listing artifacts",
    )
}

fn list_artifacts_for_task(
    connection: &Connection,
    task_id: &TaskId,
) -> Result<Vec<ArtifactRecord>> {
    let mut statement = connection
        .prepare(
            "SELECT artifact_id, task_id, context_id, name, mime_type, kind, version,
                content_json, bytes_path, metadata_json, created_at, updated_at
             FROM artifacts WHERE task_id = ?1 ORDER BY version, artifact_id",
        )
        .map_err(|error| storage_error("preparing task artifact list", error))?;
    collect_rows(
        statement.query_map(params![task_id.as_str()], read_artifact_row),
        "listing task artifacts",
    )
}

fn delete_artifact(connection: &Connection, artifact_id: &ArtifactId) -> Result<bool> {
    delete_by_key(
        connection,
        "DELETE FROM artifacts WHERE artifact_id = ?1",
        artifact_id.as_str(),
        "deleting artifact",
    )
}

fn insert_message(connection: &Connection, input: &MessageInsert) -> Result<MessageRecord> {
    if let Some(protocol_message_id) = &input.protocol_message_id {
        validate_len(
            "message protocol_message_id",
            protocol_message_id,
            STORE_IDENTIFIER_MAX_BYTES,
        )?;
    }
    let ordinal = i64::try_from(input.ordinal).map_err(|error| {
        MissiveError::validation("message ordinal is too large to store in SQLite")
            .with_source(error)
    })?;
    let content_json = to_json_text("message content", &input.content_json)?;
    let metadata_json = to_json_text("message metadata", &input.metadata)?;

    connection
        .execute(
            "INSERT INTO messages (
                message_id, agent_alias, context_id, task_id, direction, role, ordinal,
                protocol_message_id, content_json, metadata_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                input.message_id.as_str(),
                input.agent_alias.as_ref().map(AgentAlias::as_str),
                input.context_id.as_ref().map(ContextId::as_str),
                input.task_id.as_ref().map(TaskId::as_str),
                input.direction.as_str(),
                input.role.map(MessageRole::as_str),
                ordinal,
                input.protocol_message_id.as_deref(),
                content_json,
                metadata_json,
            ],
        )
        .map_err(|error| storage_error("inserting message", error))?;

    get_message(connection, &input.message_id)?
        .ok_or_else(|| missing_after_write("message", input.message_id.as_str()))
}

fn get_message(connection: &Connection, message_id: &MessageId) -> Result<Option<MessageRecord>> {
    connection
        .query_row(
            "SELECT message_id, agent_alias, context_id, task_id, direction, role, ordinal,
                protocol_message_id, content_json, metadata_json, created_at
             FROM messages WHERE message_id = ?1",
            params![message_id.as_str()],
            read_message_row,
        )
        .optional()
        .map_err(|error| storage_error("reading message", error))
}

fn list_messages(connection: &Connection) -> Result<Vec<MessageRecord>> {
    let mut statement = connection
        .prepare(
            "SELECT message_id, agent_alias, context_id, task_id, direction, role, ordinal,
                protocol_message_id, content_json, metadata_json, created_at
             FROM messages
             ORDER BY COALESCE(context_id, ''), COALESCE(task_id, ''), ordinal, created_at, message_id",
        )
        .map_err(|error| storage_error("preparing message list", error))?;
    collect_rows(
        statement.query_map([], read_message_row),
        "listing messages",
    )
}

fn delete_message(connection: &Connection, message_id: &MessageId) -> Result<bool> {
    delete_by_key(
        connection,
        "DELETE FROM messages WHERE message_id = ?1",
        message_id.as_str(),
        "deleting message",
    )
}

fn append_event(connection: &Connection, input: &EventInsert) -> Result<EventRecord> {
    validate_len("event source", &input.source, 128)?;
    validate_len("event type", &input.event_type, 128)?;
    let timestamp = input.timestamp.map(MissiveTimestamp::to_rfc3339);
    let payload_json = to_json_text("event payload", &input.payload_json)?;
    let metadata_json = to_json_text("event metadata", &input.metadata)?;

    connection
        .execute(
            "INSERT INTO events (
                event_id, timestamp, source, event_type, agent_alias, context_id, task_id,
                group_name, gateway_job_id, adapter_binding_id, payload_json, metadata_json, redacted
            ) VALUES (?1, COALESCE(?2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')), ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                input.event_id.as_str(),
                timestamp.as_deref(),
                &input.source,
                &input.event_type,
                input.agent_alias.as_ref().map(AgentAlias::as_str),
                input.context_id.as_ref().map(ContextId::as_str),
                input.task_id.as_ref().map(TaskId::as_str),
                input.group_name.as_ref().map(GroupName::as_str),
                input.gateway_job_id.as_ref().map(GatewayJobId::as_str),
                input.adapter_binding_id.as_ref().map(AdapterBindingId::as_str),
                payload_json,
                metadata_json,
                input.redacted,
            ],
        )
        .map_err(|error| storage_error("appending event", error))?;

    get_event(connection, &input.event_id)?
        .ok_or_else(|| missing_after_write("event", input.event_id.as_str()))
}

fn get_event(connection: &Connection, event_id: &EventId) -> Result<Option<EventRecord>> {
    connection
        .query_row(
            "SELECT sequence, event_id, timestamp, source, event_type, agent_alias, context_id,
                task_id, group_name, gateway_job_id, adapter_binding_id, payload_json,
                metadata_json, redacted
             FROM events WHERE event_id = ?1",
            params![event_id.as_str()],
            read_event_row,
        )
        .optional()
        .map_err(|error| storage_error("reading event", error))
}

fn list_events(connection: &Connection) -> Result<Vec<EventRecord>> {
    let mut statement = connection
        .prepare(
            "SELECT sequence, event_id, timestamp, source, event_type, agent_alias, context_id,
                task_id, group_name, gateway_job_id, adapter_binding_id, payload_json,
                metadata_json, redacted
             FROM events ORDER BY sequence",
        )
        .map_err(|error| storage_error("preparing event list", error))?;
    collect_rows(statement.query_map([], read_event_row), "listing events")
}

fn delete_event(connection: &Connection, event_id: &EventId) -> Result<bool> {
    delete_by_key(
        connection,
        "DELETE FROM events WHERE event_id = ?1",
        event_id.as_str(),
        "deleting event",
    )
}

fn upsert_group(connection: &Connection, input: &GroupUpsert) -> Result<GroupRecord> {
    validate_non_empty("group routing_policy", &input.routing_policy)?;
    let metadata_json = to_json_text("group metadata", &input.metadata)?;

    connection
        .execute(
            "INSERT INTO \"groups\" (group_name, routing_policy, notes, metadata_json)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(group_name) DO UPDATE SET
                routing_policy = excluded.routing_policy,
                notes = excluded.notes,
                metadata_json = excluded.metadata_json,
                updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
            params![
                input.group_name.as_str(),
                &input.routing_policy,
                input.notes.as_deref(),
                metadata_json,
            ],
        )
        .map_err(|error| storage_error("upserting group", error))?;

    get_group(connection, &input.group_name)?
        .ok_or_else(|| missing_after_write("group", input.group_name.as_str()))
}

fn get_group(connection: &Connection, group_name: &GroupName) -> Result<Option<GroupRecord>> {
    connection
        .query_row(
            "SELECT group_name, routing_policy, notes, metadata_json, created_at, updated_at
             FROM \"groups\" WHERE group_name = ?1",
            params![group_name.as_str()],
            read_group_row,
        )
        .optional()
        .map_err(|error| storage_error("reading group", error))
}

fn list_groups(connection: &Connection) -> Result<Vec<GroupRecord>> {
    let mut statement = connection
        .prepare(
            "SELECT group_name, routing_policy, notes, metadata_json, created_at, updated_at
             FROM \"groups\" ORDER BY group_name",
        )
        .map_err(|error| storage_error("preparing group list", error))?;
    collect_rows(statement.query_map([], read_group_row), "listing groups")
}

fn rename_group(
    connection: &Connection,
    old_name: &GroupName,
    new_name: &GroupName,
) -> Result<bool> {
    connection
        .execute(
            "UPDATE \"groups\"
             SET group_name = ?2,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE group_name = ?1",
            params![old_name.as_str(), new_name.as_str()],
        )
        .map(|affected| affected > 0)
        .map_err(|error| storage_error("renaming group", error))
}

fn delete_group(connection: &Connection, group_name: &GroupName) -> Result<bool> {
    delete_by_key(
        connection,
        "DELETE FROM \"groups\" WHERE group_name = ?1",
        group_name.as_str(),
        "deleting group",
    )
}

fn upsert_group_member(
    connection: &Connection,
    input: &GroupMemberUpsert,
) -> Result<GroupMemberRecord> {
    validate_positive_weight(input.weight)?;
    let tags_json = to_json_text("group member tags", &input.tags)?;
    let routing_metadata_json =
        to_json_text("group member routing metadata", &input.routing_metadata)?;

    connection
        .execute(
            "INSERT INTO group_members (
                group_name, agent_alias, rank_name, tags_json, weight, routing_metadata_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(group_name, agent_alias) DO UPDATE SET
                rank_name = excluded.rank_name,
                tags_json = excluded.tags_json,
                weight = excluded.weight,
                routing_metadata_json = excluded.routing_metadata_json",
            params![
                input.group_name.as_str(),
                input.agent_alias.as_str(),
                input.rank_name.as_str(),
                tags_json,
                input.weight,
                routing_metadata_json,
            ],
        )
        .map_err(|error| storage_error("upserting group member", error))?;

    get_group_member(connection, &input.group_name, &input.agent_alias)?.ok_or_else(|| {
        missing_after_write(
            "group member",
            &format!(
                "{}/{}",
                input.group_name.as_str(),
                input.agent_alias.as_str()
            ),
        )
    })
}

fn get_group_member(
    connection: &Connection,
    group_name: &GroupName,
    agent_alias: &AgentAlias,
) -> Result<Option<GroupMemberRecord>> {
    connection
        .query_row(
            "SELECT group_name, agent_alias, rank_name, tags_json, weight, routing_metadata_json, created_at
             FROM group_members WHERE group_name = ?1 AND agent_alias = ?2",
            params![group_name.as_str(), agent_alias.as_str()],
            read_group_member_row,
        )
        .optional()
        .map_err(|error| storage_error("reading group member", error))
}

fn list_group_members(
    connection: &Connection,
    group_name: &GroupName,
) -> Result<Vec<GroupMemberRecord>> {
    let mut statement = connection
        .prepare(
            "SELECT group_name, agent_alias, rank_name, tags_json, weight, routing_metadata_json, created_at
             FROM group_members WHERE group_name = ?1 ORDER BY rank_name, agent_alias",
        )
        .map_err(|error| storage_error("preparing group member list", error))?;
    collect_rows(
        statement.query_map(params![group_name.as_str()], read_group_member_row),
        "listing group members",
    )
}

fn remove_group_member(
    connection: &Connection,
    group_name: &GroupName,
    agent_alias: &AgentAlias,
) -> Result<bool> {
    connection
        .execute(
            "DELETE FROM group_members WHERE group_name = ?1 AND agent_alias = ?2",
            params![group_name.as_str(), agent_alias.as_str()],
        )
        .map(|affected| affected > 0)
        .map_err(|error| storage_error("removing group member", error))
}

fn upsert_push_config(
    connection: &Connection,
    input: &PushConfigUpsert,
) -> Result<PushConfigRecord> {
    validate_store_identifier("push config id", input.push_config_id.as_str())?;
    validate_len("push config callback_url", &input.callback_url, 4096)?;
    if let Some(auth_ref_name) = &input.auth_ref_name {
        validate_store_identifier("push config auth_ref_name", auth_ref_name)?;
    }
    let remote_config_json = optional_json_text(
        "push config remote config",
        input.remote_config_json.as_ref(),
    )?;
    let metadata_json = to_json_text("push config metadata", &input.metadata)?;
    let deleted_at = input.deleted_at.map(MissiveTimestamp::to_rfc3339);

    connection
        .execute(
            "INSERT INTO push_configs (
                push_config_id, agent_alias, task_id, callback_url, auth_ref_name,
                remote_config_json, metadata_json, deleted_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(push_config_id) DO UPDATE SET
                agent_alias = excluded.agent_alias,
                task_id = excluded.task_id,
                callback_url = excluded.callback_url,
                auth_ref_name = excluded.auth_ref_name,
                remote_config_json = excluded.remote_config_json,
                metadata_json = excluded.metadata_json,
                deleted_at = excluded.deleted_at,
                updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
            params![
                input.push_config_id.as_str(),
                input.agent_alias.as_str(),
                input.task_id.as_ref().map(TaskId::as_str),
                &input.callback_url,
                input.auth_ref_name.as_deref(),
                remote_config_json,
                metadata_json,
                deleted_at,
            ],
        )
        .map_err(|error| storage_error("upserting push config", error))?;

    get_push_config(connection, &input.push_config_id)?
        .ok_or_else(|| missing_after_write("push config", input.push_config_id.as_str()))
}

fn get_push_config(
    connection: &Connection,
    push_config_id: &PushConfigId,
) -> Result<Option<PushConfigRecord>> {
    connection
        .query_row(
            "SELECT push_config_id, agent_alias, task_id, callback_url, auth_ref_name,
                remote_config_json, metadata_json, created_at, updated_at, deleted_at
             FROM push_configs WHERE push_config_id = ?1",
            params![push_config_id.as_str()],
            read_push_config_row,
        )
        .optional()
        .map_err(|error| storage_error("reading push config", error))
}

fn list_push_configs(connection: &Connection) -> Result<Vec<PushConfigRecord>> {
    let mut statement = connection
        .prepare(
            "SELECT push_config_id, agent_alias, task_id, callback_url, auth_ref_name,
                remote_config_json, metadata_json, created_at, updated_at, deleted_at
             FROM push_configs ORDER BY agent_alias, task_id, push_config_id",
        )
        .map_err(|error| storage_error("preparing push config list", error))?;
    collect_rows(
        statement.query_map([], read_push_config_row),
        "listing push configs",
    )
}

fn list_push_configs_for_task(
    connection: &Connection,
    agent_alias: &AgentAlias,
    task_id: &TaskId,
) -> Result<Vec<PushConfigRecord>> {
    let mut statement = connection
        .prepare(
            "SELECT push_config_id, agent_alias, task_id, callback_url, auth_ref_name,
                remote_config_json, metadata_json, created_at, updated_at, deleted_at
             FROM push_configs
             WHERE agent_alias = ?1 AND task_id = ?2
             ORDER BY push_config_id",
        )
        .map_err(|error| storage_error("preparing push config task list", error))?;
    collect_rows(
        statement.query_map(
            params![agent_alias.as_str(), task_id.as_str()],
            read_push_config_row,
        ),
        "listing task push configs",
    )
}

fn delete_push_config(connection: &Connection, push_config_id: &PushConfigId) -> Result<bool> {
    connection
        .execute(
            "UPDATE push_configs
             SET deleted_at = COALESCE(deleted_at, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE push_config_id = ?1 AND deleted_at IS NULL",
            params![push_config_id.as_str()],
        )
        .map(|affected| affected > 0)
        .map_err(|error| storage_error("deleting push config", error))
}

fn upsert_gateway_job(
    connection: &Connection,
    input: &GatewayJobUpsert,
) -> Result<GatewayJobRecord> {
    validate_len("gateway job kind", &input.kind, 63)?;
    validate_gateway_attempts(input.retry_count, input.max_attempts)?;
    let request_json = to_json_text("gateway job request", &input.request_json)?;
    let result_json = optional_json_text("gateway job result", input.result_json.as_ref())?;
    let metadata_json = to_json_text("gateway job metadata", &input.metadata)?;
    let next_run_at = input.next_run_at.map(MissiveTimestamp::to_rfc3339);
    let locked_until = input.locked_until.map(MissiveTimestamp::to_rfc3339);
    let completed_at = input.completed_at.map(MissiveTimestamp::to_rfc3339);

    connection
        .execute(
            "INSERT INTO gateway_jobs (
                gateway_job_id, kind, state, agent_alias, context_id, task_id, group_name,
                adapter_binding_id, request_json, result_json, metadata_json, retry_count,
                max_attempts, next_run_at, locked_by, locked_until, completed_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
            ON CONFLICT(gateway_job_id) DO UPDATE SET
                kind = excluded.kind,
                state = excluded.state,
                agent_alias = excluded.agent_alias,
                context_id = excluded.context_id,
                task_id = excluded.task_id,
                group_name = excluded.group_name,
                adapter_binding_id = excluded.adapter_binding_id,
                request_json = excluded.request_json,
                result_json = excluded.result_json,
                metadata_json = excluded.metadata_json,
                retry_count = excluded.retry_count,
                max_attempts = excluded.max_attempts,
                next_run_at = excluded.next_run_at,
                locked_by = excluded.locked_by,
                locked_until = excluded.locked_until,
                completed_at = excluded.completed_at,
                updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
            params![
                input.gateway_job_id.as_str(),
                &input.kind,
                input.state.as_str(),
                input.agent_alias.as_ref().map(AgentAlias::as_str),
                input.context_id.as_ref().map(ContextId::as_str),
                input.task_id.as_ref().map(TaskId::as_str),
                input.group_name.as_ref().map(GroupName::as_str),
                input
                    .adapter_binding_id
                    .as_ref()
                    .map(AdapterBindingId::as_str),
                request_json,
                result_json.as_deref(),
                metadata_json,
                i64::from(input.retry_count),
                i64::from(input.max_attempts),
                next_run_at.as_deref(),
                input.locked_by.as_deref(),
                locked_until.as_deref(),
                completed_at.as_deref(),
            ],
        )
        .map_err(|error| storage_error("upserting gateway job", error))?;

    get_gateway_job(connection, &input.gateway_job_id)?
        .ok_or_else(|| missing_after_write("gateway job", input.gateway_job_id.as_str()))
}

fn get_gateway_job(
    connection: &Connection,
    gateway_job_id: &GatewayJobId,
) -> Result<Option<GatewayJobRecord>> {
    connection
        .query_row(
            "SELECT gateway_job_id, kind, state, agent_alias, context_id, task_id, group_name,
                adapter_binding_id, request_json, result_json, metadata_json, retry_count,
                max_attempts, next_run_at, locked_by, locked_until, created_at, updated_at, completed_at
             FROM gateway_jobs WHERE gateway_job_id = ?1",
            params![gateway_job_id.as_str()],
            read_gateway_job_row,
        )
        .optional()
        .map_err(|error| storage_error("reading gateway job", error))
}

fn list_gateway_jobs(connection: &Connection) -> Result<Vec<GatewayJobRecord>> {
    let mut statement = connection
        .prepare(
            "SELECT gateway_job_id, kind, state, agent_alias, context_id, task_id, group_name,
                adapter_binding_id, request_json, result_json, metadata_json, retry_count,
                max_attempts, next_run_at, locked_by, locked_until, created_at, updated_at, completed_at
             FROM gateway_jobs ORDER BY gateway_job_id",
        )
        .map_err(|error| storage_error("preparing gateway job list", error))?;
    collect_rows(
        statement.query_map([], read_gateway_job_row),
        "listing gateway jobs",
    )
}

fn delete_gateway_job(connection: &Connection, gateway_job_id: &GatewayJobId) -> Result<bool> {
    delete_by_key(
        connection,
        "DELETE FROM gateway_jobs WHERE gateway_job_id = ?1",
        gateway_job_id.as_str(),
        "deleting gateway job",
    )
}

fn upsert_gateway_session(
    connection: &Connection,
    input: &GatewaySessionUpsert,
) -> Result<GatewaySessionRecord> {
    validate_store_identifier("gateway session source kind", input.source_kind.as_str())?;
    validate_store_identifier("gateway session source id", input.source_id.as_str())?;
    validate_len(
        "gateway session resume_name",
        input.resume_name.as_str(),
        128,
    )?;
    validate_gateway_session_reset_policy(
        input.reset_mode,
        input.daily_reset_hour,
        input.idle_timeout_seconds,
    )?;
    let idle_timeout_seconds = optional_u64_to_i64(
        "gateway session idle_timeout_seconds",
        input.idle_timeout_seconds,
    )?;
    let reset_count = u64_to_i64("gateway session reset_count", input.reset_count)?;
    let last_active_at = input.last_active_at.to_rfc3339();
    let last_reset_at = input.last_reset_at.map(MissiveTimestamp::to_rfc3339);
    let metadata_json = to_json_text("gateway session metadata", &input.metadata)?;

    connection
        .execute(
            "INSERT INTO gateway_sessions (
                gateway_session_id, source_kind, source_id, agent_alias, resume_name,
                context_id, reset_mode, daily_reset_hour, idle_timeout_seconds,
                last_active_at, last_reset_at, reset_count, metadata_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            ON CONFLICT(gateway_session_id) DO UPDATE SET
                source_kind = excluded.source_kind,
                source_id = excluded.source_id,
                agent_alias = excluded.agent_alias,
                resume_name = excluded.resume_name,
                context_id = excluded.context_id,
                reset_mode = excluded.reset_mode,
                daily_reset_hour = excluded.daily_reset_hour,
                idle_timeout_seconds = excluded.idle_timeout_seconds,
                last_active_at = excluded.last_active_at,
                last_reset_at = excluded.last_reset_at,
                reset_count = excluded.reset_count,
                metadata_json = excluded.metadata_json,
                updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
            params![
                input.gateway_session_id.as_str(),
                &input.source_kind,
                &input.source_id,
                input.agent_alias.as_str(),
                &input.resume_name,
                input.context_id.as_str(),
                input.reset_mode.as_str(),
                i64::from(input.daily_reset_hour),
                idle_timeout_seconds,
                last_active_at,
                last_reset_at.as_deref(),
                reset_count,
                metadata_json,
            ],
        )
        .map_err(|error| storage_error("upserting gateway session", error))?;

    get_gateway_session(connection, &input.gateway_session_id)?
        .ok_or_else(|| missing_after_write("gateway session", input.gateway_session_id.as_str()))
}

fn get_gateway_session(
    connection: &Connection,
    gateway_session_id: &GatewaySessionId,
) -> Result<Option<GatewaySessionRecord>> {
    connection
        .query_row(
            "SELECT gateway_session_id, source_kind, source_id, agent_alias, resume_name,
                context_id, reset_mode, daily_reset_hour, idle_timeout_seconds,
                last_active_at, last_reset_at, reset_count, metadata_json, created_at, updated_at
             FROM gateway_sessions WHERE gateway_session_id = ?1",
            params![gateway_session_id.as_str()],
            read_gateway_session_row,
        )
        .optional()
        .map_err(|error| storage_error("reading gateway session", error))
}

fn get_gateway_session_by_resume(
    connection: &Connection,
    source_kind: &str,
    source_id: &str,
    agent_alias: &AgentAlias,
    resume_name: &str,
) -> Result<Option<GatewaySessionRecord>> {
    validate_store_identifier("gateway session source kind", source_kind)?;
    validate_store_identifier("gateway session source id", source_id)?;
    validate_len("gateway session resume_name", resume_name, 128)?;
    connection
        .query_row(
            "SELECT gateway_session_id, source_kind, source_id, agent_alias, resume_name,
                context_id, reset_mode, daily_reset_hour, idle_timeout_seconds,
                last_active_at, last_reset_at, reset_count, metadata_json, created_at, updated_at
             FROM gateway_sessions
             WHERE source_kind = ?1 AND source_id = ?2 AND agent_alias = ?3 AND resume_name = ?4",
            params![source_kind, source_id, agent_alias.as_str(), resume_name],
            read_gateway_session_row,
        )
        .optional()
        .map_err(|error| storage_error("reading gateway session by resume name", error))
}

fn list_gateway_sessions(connection: &Connection) -> Result<Vec<GatewaySessionRecord>> {
    let mut statement = connection
        .prepare(
            "SELECT gateway_session_id, source_kind, source_id, agent_alias, resume_name,
                context_id, reset_mode, daily_reset_hour, idle_timeout_seconds,
                last_active_at, last_reset_at, reset_count, metadata_json, created_at, updated_at
             FROM gateway_sessions ORDER BY source_kind, source_id, agent_alias, resume_name",
        )
        .map_err(|error| storage_error("preparing gateway session list", error))?;
    collect_rows(
        statement.query_map([], read_gateway_session_row),
        "listing gateway sessions",
    )
}

fn list_gateway_sessions_for_agent(
    connection: &Connection,
    agent_alias: &AgentAlias,
) -> Result<Vec<GatewaySessionRecord>> {
    let mut statement = connection
        .prepare(
            "SELECT gateway_session_id, source_kind, source_id, agent_alias, resume_name,
                context_id, reset_mode, daily_reset_hour, idle_timeout_seconds,
                last_active_at, last_reset_at, reset_count, metadata_json, created_at, updated_at
             FROM gateway_sessions WHERE agent_alias = ?1 ORDER BY source_kind, source_id, resume_name",
        )
        .map_err(|error| storage_error("preparing gateway session agent list", error))?;
    collect_rows(
        statement.query_map(params![agent_alias.as_str()], read_gateway_session_row),
        "listing gateway sessions for agent",
    )
}

fn delete_gateway_session(
    connection: &Connection,
    gateway_session_id: &GatewaySessionId,
) -> Result<bool> {
    delete_by_key(
        connection,
        "DELETE FROM gateway_sessions WHERE gateway_session_id = ?1",
        gateway_session_id.as_str(),
        "deleting gateway session",
    )
}

fn read_auth_ref_row(row: &Row<'_>) -> rusqlite::Result<AuthRefRecord> {
    Ok(AuthRefRecord {
        name: row.get(0)?,
        kind: parse_sql(1, row.get(1)?)?,
        header_name: row.get(2)?,
        scheme: row.get(3)?,
        env_var: row.get(4)?,
        keyring_service: row.get(5)?,
        keyring_account: row.get(6)?,
        secret_storage: parse_sql(7, row.get(7)?)?,
        metadata: json_from_sql(8, row.get(8)?)?,
        created_at: timestamp_from_sql(9, row.get(9)?)?,
        updated_at: timestamp_from_sql(10, row.get(10)?)?,
    })
}

fn read_agent_row(row: &Row<'_>) -> rusqlite::Result<AgentRecord> {
    Ok(AgentRecord {
        alias: parse_sql(0, row.get(0)?)?,
        source: parse_sql(1, row.get(1)?)?,
        base_url: row.get(2)?,
        interface_urls: json_from_sql(3, row.get(3)?)?,
        binding_preference: json_from_sql(4, row.get(4)?)?,
        auth_ref_name: row.get(5)?,
        tags: json_from_sql(6, row.get(6)?)?,
        notes: row.get(7)?,
        metadata: json_from_sql(8, row.get(8)?)?,
        agent_card_json: optional_json_from_sql(9, row.get(9)?)?,
        agent_card_etag: row.get(10)?,
        agent_card_last_modified: row.get(11)?,
        agent_card_fetched_at: optional_timestamp_from_sql(12, row.get(12)?)?,
        read_only: row.get(13)?,
        created_at: timestamp_from_sql(14, row.get(14)?)?,
        updated_at: timestamp_from_sql(15, row.get(15)?)?,
    })
}

fn read_context_row(row: &Row<'_>) -> rusqlite::Result<ContextRecord> {
    Ok(ContextRecord {
        context_id: parse_sql(0, row.get(0)?)?,
        agent_alias: optional_parse_sql(1, row.get(1)?)?,
        name: row.get(2)?,
        parent_context_id: optional_parse_sql(3, row.get(3)?)?,
        state: parse_sql(4, row.get(4)?)?,
        summary: row.get(5)?,
        metadata: json_from_sql(6, row.get(6)?)?,
        created_at: timestamp_from_sql(7, row.get(7)?)?,
        updated_at: timestamp_from_sql(8, row.get(8)?)?,
        closed_at: optional_timestamp_from_sql(9, row.get(9)?)?,
    })
}

fn read_task_row(row: &Row<'_>) -> rusqlite::Result<TaskRecord> {
    Ok(TaskRecord {
        task_id: parse_sql(0, row.get(0)?)?,
        agent_alias: parse_sql(1, row.get(1)?)?,
        context_id: optional_parse_sql(2, row.get(2)?)?,
        state: parse_sql(3, row.get(3)?)?,
        source: parse_sql(4, row.get(4)?)?,
        protocol_version: row.get(5)?,
        remote_task_json: optional_json_from_sql(6, row.get(6)?)?,
        last_message_id: optional_parse_sql(7, row.get(7)?)?,
        metadata: json_from_sql(8, row.get(8)?)?,
        created_at: timestamp_from_sql(9, row.get(9)?)?,
        updated_at: timestamp_from_sql(10, row.get(10)?)?,
        completed_at: optional_timestamp_from_sql(11, row.get(11)?)?,
    })
}

fn read_artifact_row(row: &Row<'_>) -> rusqlite::Result<ArtifactRecord> {
    Ok(ArtifactRecord {
        artifact_id: parse_sql(0, row.get(0)?)?,
        task_id: parse_sql(1, row.get(1)?)?,
        context_id: optional_parse_sql(2, row.get(2)?)?,
        name: row.get(3)?,
        mime_type: row.get(4)?,
        kind: parse_sql(5, row.get(5)?)?,
        version: u64_from_sql(6, row.get(6)?)?,
        content_json: optional_json_from_sql(7, row.get(7)?)?,
        bytes_path: row.get(8)?,
        metadata: json_from_sql(9, row.get(9)?)?,
        created_at: timestamp_from_sql(10, row.get(10)?)?,
        updated_at: timestamp_from_sql(11, row.get(11)?)?,
    })
}

fn read_message_row(row: &Row<'_>) -> rusqlite::Result<MessageRecord> {
    Ok(MessageRecord {
        message_id: parse_sql(0, row.get(0)?)?,
        agent_alias: optional_parse_sql(1, row.get(1)?)?,
        context_id: optional_parse_sql(2, row.get(2)?)?,
        task_id: optional_parse_sql(3, row.get(3)?)?,
        direction: parse_sql(4, row.get(4)?)?,
        role: optional_parse_sql(5, row.get(5)?)?,
        ordinal: u64_from_sql(6, row.get(6)?)?,
        protocol_message_id: row.get(7)?,
        content_json: json_from_sql(8, row.get(8)?)?,
        metadata: json_from_sql(9, row.get(9)?)?,
        created_at: timestamp_from_sql(10, row.get(10)?)?,
    })
}

fn read_event_row(row: &Row<'_>) -> rusqlite::Result<EventRecord> {
    Ok(EventRecord {
        sequence: row.get(0)?,
        event_id: parse_sql(1, row.get(1)?)?,
        timestamp: timestamp_from_sql(2, row.get(2)?)?,
        source: row.get(3)?,
        event_type: row.get(4)?,
        agent_alias: optional_parse_sql(5, row.get(5)?)?,
        context_id: optional_parse_sql(6, row.get(6)?)?,
        task_id: optional_parse_sql(7, row.get(7)?)?,
        group_name: optional_parse_sql(8, row.get(8)?)?,
        gateway_job_id: optional_parse_sql(9, row.get(9)?)?,
        adapter_binding_id: optional_parse_sql(10, row.get(10)?)?,
        payload_json: json_from_sql(11, row.get(11)?)?,
        metadata: json_from_sql(12, row.get(12)?)?,
        redacted: row.get(13)?,
    })
}

fn read_group_row(row: &Row<'_>) -> rusqlite::Result<GroupRecord> {
    Ok(GroupRecord {
        group_name: parse_sql(0, row.get(0)?)?,
        routing_policy: row.get(1)?,
        notes: row.get(2)?,
        metadata: json_from_sql(3, row.get(3)?)?,
        created_at: timestamp_from_sql(4, row.get(4)?)?,
        updated_at: timestamp_from_sql(5, row.get(5)?)?,
    })
}

fn read_group_member_row(row: &Row<'_>) -> rusqlite::Result<GroupMemberRecord> {
    Ok(GroupMemberRecord {
        group_name: parse_sql(0, row.get(0)?)?,
        agent_alias: parse_sql(1, row.get(1)?)?,
        rank_name: parse_sql(2, row.get(2)?)?,
        tags: json_from_sql(3, row.get(3)?)?,
        weight: row.get(4)?,
        routing_metadata: json_from_sql(5, row.get(5)?)?,
        created_at: timestamp_from_sql(6, row.get(6)?)?,
    })
}

fn read_push_config_row(row: &Row<'_>) -> rusqlite::Result<PushConfigRecord> {
    Ok(PushConfigRecord {
        push_config_id: parse_sql(0, row.get(0)?)?,
        agent_alias: parse_sql(1, row.get(1)?)?,
        task_id: optional_parse_sql(2, row.get(2)?)?,
        callback_url: row.get(3)?,
        auth_ref_name: row.get(4)?,
        remote_config_json: optional_json_from_sql(5, row.get(5)?)?,
        metadata: json_from_sql(6, row.get(6)?)?,
        created_at: timestamp_from_sql(7, row.get(7)?)?,
        updated_at: timestamp_from_sql(8, row.get(8)?)?,
        deleted_at: optional_timestamp_from_sql(9, row.get(9)?)?,
    })
}

fn read_gateway_job_row(row: &Row<'_>) -> rusqlite::Result<GatewayJobRecord> {
    Ok(GatewayJobRecord {
        gateway_job_id: parse_sql(0, row.get(0)?)?,
        kind: row.get(1)?,
        state: parse_sql(2, row.get(2)?)?,
        agent_alias: optional_parse_sql(3, row.get(3)?)?,
        context_id: optional_parse_sql(4, row.get(4)?)?,
        task_id: optional_parse_sql(5, row.get(5)?)?,
        group_name: optional_parse_sql(6, row.get(6)?)?,
        adapter_binding_id: optional_parse_sql(7, row.get(7)?)?,
        request_json: json_from_sql(8, row.get(8)?)?,
        result_json: optional_json_from_sql(9, row.get(9)?)?,
        metadata: json_from_sql(10, row.get(10)?)?,
        retry_count: u32_from_sql(11, row.get(11)?)?,
        max_attempts: u32_from_sql(12, row.get(12)?)?,
        next_run_at: optional_timestamp_from_sql(13, row.get(13)?)?,
        locked_by: row.get(14)?,
        locked_until: optional_timestamp_from_sql(15, row.get(15)?)?,
        created_at: timestamp_from_sql(16, row.get(16)?)?,
        updated_at: timestamp_from_sql(17, row.get(17)?)?,
        completed_at: optional_timestamp_from_sql(18, row.get(18)?)?,
    })
}

fn read_gateway_session_row(row: &Row<'_>) -> rusqlite::Result<GatewaySessionRecord> {
    Ok(GatewaySessionRecord {
        gateway_session_id: parse_sql(0, row.get(0)?)?,
        source_kind: row.get(1)?,
        source_id: row.get(2)?,
        agent_alias: parse_sql(3, row.get(3)?)?,
        resume_name: row.get(4)?,
        context_id: parse_sql(5, row.get(5)?)?,
        reset_mode: parse_sql(6, row.get(6)?)?,
        daily_reset_hour: u8_from_sql(7, row.get(7)?)?,
        idle_timeout_seconds: optional_u64_from_sql(8, row.get(8)?)?,
        last_active_at: timestamp_from_sql(9, row.get(9)?)?,
        last_reset_at: optional_timestamp_from_sql(10, row.get(10)?)?,
        reset_count: u64_from_sql(11, row.get(11)?)?,
        metadata: json_from_sql(12, row.get(12)?)?,
        created_at: timestamp_from_sql(13, row.get(13)?)?,
        updated_at: timestamp_from_sql(14, row.get(14)?)?,
    })
}

fn collect_rows<T>(
    rows: rusqlite::Result<rusqlite::MappedRows<'_, impl FnMut(&Row<'_>) -> rusqlite::Result<T>>>,
    action: &str,
) -> Result<Vec<T>> {
    rows.map_err(|error| storage_error(action, error))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| storage_error(action, error))
}

fn delete_by_key(connection: &Connection, sql: &str, key: &str, action: &str) -> Result<bool> {
    connection
        .execute(sql, params![key])
        .map(|affected| affected > 0)
        .map_err(|error| storage_error(action, error))
}

fn default_binding_preference() -> Vec<TransportName> {
    ["http+json", "json-rpc"]
        .into_iter()
        .map(|binding| TransportName::new(binding).expect("default transport binding is valid"))
        .collect()
}

fn to_json_text<T>(label: &str, value: &T) -> Result<String>
where
    T: Serialize + ?Sized,
{
    serde_json::to_string(value).map_err(|error| {
        MissiveError::storage(format!("serializing {label} as JSON for SQLite"))
            .with_source(error)
            .with_help("Only valid JSON-compatible values can be persisted in missive state.")
    })
}

fn optional_json_text(label: &str, value: Option<&Value>) -> Result<Option<String>> {
    value.map(|value| to_json_text(label, value)).transpose()
}

fn json_from_sql<T>(column: usize, value: String) -> rusqlite::Result<T>
where
    T: DeserializeOwned,
{
    serde_json::from_str(&value).map_err(|error| conversion_error(column, error))
}

fn optional_json_from_sql<T>(column: usize, value: Option<String>) -> rusqlite::Result<Option<T>>
where
    T: DeserializeOwned,
{
    value.map(|value| json_from_sql(column, value)).transpose()
}

fn parse_sql<T>(column: usize, value: String) -> rusqlite::Result<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    value
        .parse()
        .map_err(|error| conversion_error(column, error))
}

fn optional_parse_sql<T>(column: usize, value: Option<String>) -> rusqlite::Result<Option<T>>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    value.map(|value| parse_sql(column, value)).transpose()
}

fn timestamp_from_sql(column: usize, value: String) -> rusqlite::Result<MissiveTimestamp> {
    parse_sql(column, value)
}

fn optional_timestamp_from_sql(
    column: usize,
    value: Option<String>,
) -> rusqlite::Result<Option<MissiveTimestamp>> {
    optional_parse_sql(column, value)
}

fn u8_from_sql(column: usize, value: i64) -> rusqlite::Result<u8> {
    u8::try_from(value).map_err(|error| conversion_error(column, error))
}

fn u32_from_sql(column: usize, value: i64) -> rusqlite::Result<u32> {
    u32::try_from(value).map_err(|error| conversion_error(column, error))
}

fn u64_from_sql(column: usize, value: i64) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|error| conversion_error(column, error))
}

fn optional_u64_from_sql(column: usize, value: Option<i64>) -> rusqlite::Result<Option<u64>> {
    value.map(|value| u64_from_sql(column, value)).transpose()
}

fn u64_to_i64(label: &str, value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|error| {
        MissiveError::validation(format!("{label} is too large to store in SQLite"))
            .with_source(error)
    })
}

fn optional_u64_to_i64(label: &str, value: Option<u64>) -> Result<Option<i64>> {
    value.map(|value| u64_to_i64(label, value)).transpose()
}

fn conversion_error(
    column: usize,
    error: impl std::error::Error + Send + Sync + 'static,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(column, Type::Text, Box::new(error))
}

fn validate_store_identifier(kind: &'static str, value: &str) -> Result<()> {
    if value.is_empty() {
        return invalid_store_identifier(kind, value, "value cannot be empty");
    }
    if value.len() > STORE_IDENTIFIER_MAX_BYTES {
        return invalid_store_identifier(
            kind,
            value,
            format!(
                "value is {} bytes, but the maximum is {STORE_IDENTIFIER_MAX_BYTES}",
                value.len()
            ),
        );
    }
    if value.chars().any(char::is_whitespace) {
        return invalid_store_identifier(kind, value, "value cannot contain whitespace");
    }
    if value.chars().any(char::is_control) {
        return invalid_store_identifier(kind, value, "value cannot contain control characters");
    }
    Ok(())
}

fn invalid_store_identifier(
    kind: &'static str,
    value: &str,
    reason: impl Into<String>,
) -> Result<()> {
    Err(
        MissiveError::validation(format!("invalid {kind} {value:?}: {}", reason.into()))
            .with_help(STORE_IDENTIFIER_HELP),
    )
}

fn validate_non_empty(label: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(MissiveError::validation(format!("{label} cannot be empty")));
    }
    Ok(())
}

fn validate_required_option(label: &str, value: Option<&str>) -> Result<()> {
    let Some(value) = value else {
        return Err(MissiveError::validation(format!("{label} is required")));
    };
    validate_non_empty(label, value)
}

fn validate_len(label: &str, value: &str, max_bytes: usize) -> Result<()> {
    validate_non_empty(label, value)?;
    if value.len() > max_bytes {
        return Err(MissiveError::validation(format!(
            "{label} is {} bytes, but the maximum is {max_bytes}",
            value.len()
        )));
    }
    Ok(())
}

fn validate_positive_weight(weight: f64) -> Result<()> {
    if weight.is_finite() && weight > 0.0 {
        Ok(())
    } else {
        Err(MissiveError::validation(format!(
            "group member weight must be a positive finite number, got {weight}"
        )))
    }
}

fn validate_gateway_attempts(retry_count: u32, max_attempts: u32) -> Result<()> {
    if max_attempts == 0 {
        return Err(MissiveError::validation(
            "gateway job max_attempts must be at least 1",
        ));
    }
    if retry_count > max_attempts {
        return Err(MissiveError::validation(format!(
            "gateway job retry_count {retry_count} cannot exceed max_attempts {max_attempts}"
        )));
    }
    Ok(())
}

fn validate_gateway_session_reset_policy(
    mode: GatewaySessionResetMode,
    daily_reset_hour: u8,
    idle_timeout_seconds: Option<u64>,
) -> Result<()> {
    if daily_reset_hour > 23 {
        return Err(MissiveError::validation(format!(
            "gateway session daily_reset_hour must be between 0 and 23, got {daily_reset_hour}"
        )));
    }
    match mode {
        GatewaySessionResetMode::Idle | GatewaySessionResetMode::Both => {
            let Some(seconds) = idle_timeout_seconds else {
                return Err(MissiveError::validation(
                    "gateway session idle_timeout_seconds is required for idle and both reset modes",
                ));
            };
            if seconds == 0 {
                return Err(MissiveError::validation(
                    "gateway session idle_timeout_seconds must be greater than zero",
                ));
            }
        }
        GatewaySessionResetMode::None | GatewaySessionResetMode::Daily => {
            if idle_timeout_seconds.is_some() {
                return Err(MissiveError::validation(
                    "gateway session idle_timeout_seconds is only valid for idle and both reset modes",
                ));
            }
        }
    }
    Ok(())
}

fn missing_after_write(kind: &str, identifier: &str) -> MissiveError {
    MissiveError::storage(format!(
        "{kind} {identifier:?} was written but could not be read back"
    ))
    .with_help("Inspect the SQLite database and repository write transaction.")
}

fn storage_error(action: &str, error: rusqlite::Error) -> MissiveError {
    MissiveError::storage(format!("{action}: {error}"))
        .with_source(error)
        .with_help("Inspect the SQLite database path, migrations, and profile state permissions.")
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn store_open_migrates_temp_database() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("missive.sqlite3");

        let store = Store::open(&path).expect("store opens");

        assert!(path.exists());
        assert!(store.list_agents().expect("agents list").is_empty());
    }

    #[test]
    fn auth_ref_crud_round_trip_stores_non_secret_references() {
        let store = Store::open_in_memory().expect("store");
        let mut input = AuthRefUpsert::env("example-env", "MISSIVE_EXAMPLE_TOKEN");
        input
            .metadata
            .insert("purpose", json!("repository-test"))
            .expect("metadata");

        let created = store.upsert_auth_ref(&input).expect("auth ref upsert");

        assert_eq!(created.name, "example-env");
        assert_eq!(created.kind, AuthRefKind::Env);
        assert_eq!(created.env_var.as_deref(), Some("MISSIVE_EXAMPLE_TOKEN"));
        assert_eq!(created.secret_storage, AuthSecretStorage::Env);
        assert_eq!(created.metadata.get_str("purpose"), Some("repository-test"));
        assert_eq!(
            store.get_auth_ref("example-env").expect("get auth ref"),
            Some(created.clone())
        );
        assert_eq!(
            store.list_auth_refs().expect("list auth refs"),
            vec![created]
        );
        assert!(
            store
                .delete_auth_ref("example-env")
                .expect("delete auth ref")
        );
        assert!(
            store
                .get_auth_ref("example-env")
                .expect("auth ref deleted")
                .is_none()
        );
    }

    #[test]
    fn agent_crud_round_trip_uses_typed_json_fields() {
        let store = Store::open_in_memory().expect("store");
        let alias = alias("echo");
        let mut input = AgentUpsert::new(alias.clone(), "http://127.0.0.1:8080");
        input.interface_urls.insert(
            transport("json-rpc"),
            "http://127.0.0.1:8080/rpc".to_owned(),
        );
        input.tags = vec!["local".to_owned(), "test".to_owned()];
        input
            .metadata
            .insert("purpose", json!("repository-test"))
            .expect("metadata");

        let created = store.upsert_agent(&input).expect("agent upsert");

        assert_eq!(created.alias, alias);
        assert_eq!(created.source, AgentSource::Local);
        assert_eq!(
            created.interface_urls[&transport("json-rpc")],
            "http://127.0.0.1:8080/rpc"
        );
        assert_eq!(created.metadata.get_str("purpose"), Some("repository-test"));
        assert_eq!(store.list_agents().expect("list agents").len(), 1);

        let mut update = input.clone();
        update.notes = Some("updated".to_owned());
        update.read_only = true;
        let updated = store.upsert_agent(&update).expect("agent update");

        assert_eq!(updated.notes.as_deref(), Some("updated"));
        assert!(updated.read_only);
        assert_eq!(store.get_agent(&alias).expect("get agent"), Some(updated));
        assert!(store.delete_agent(&alias).expect("delete agent"));
        assert!(store.get_agent(&alias).expect("agent deleted").is_none());
    }

    #[test]
    fn context_task_and_event_crud_preserve_relationships() {
        let store = Store::open_in_memory().expect("store");
        let agent = seed_agent(&store, "echo");
        let context_id = context_id("ctx-1");
        let mut context = ContextUpsert::new(context_id.clone());
        context.agent_alias = Some(agent.clone());
        context.name = Some("Demo".to_owned());
        store.upsert_context(&context).expect("context upsert");

        let task_id = task_id("task-1");
        let mut task = TaskUpsert::new(task_id.clone(), agent.clone(), TaskState::Submitted);
        task.context_id = Some(context_id.clone());
        task.record_a2a_protocol_version("1.0")
            .expect("record task protocol version");
        task.remote_task_json = Some(json!({"id": "task-1", "state": "submitted"}));
        let created_task = store.upsert_task(&task).expect("task upsert");

        assert_eq!(created_task.context_id.as_ref(), Some(&context_id));
        assert_eq!(created_task.state, TaskState::Submitted);
        assert_eq!(created_task.protocol_version.as_deref(), Some("1.0"));
        assert_eq!(
            created_task.metadata.get_str(METADATA_A2A_PROTOCOL_VERSION),
            Some("1.0")
        );

        let mut completed_task = task.clone();
        completed_task.state = TaskState::Completed;
        completed_task.completed_at = Some(timestamp(1_735_787_045));
        let updated_task = store.upsert_task(&completed_task).expect("task update");
        assert_eq!(updated_task.state, TaskState::Completed);
        assert_eq!(updated_task.completed_at, Some(timestamp(1_735_787_045)));

        let message_id = message_id("msg-1");
        let mut message = MessageInsert::new(
            message_id.clone(),
            MessageDirection::Request,
            json!({"messageId": "protocol-msg-1", "parts": [{"text": "hello"}]}),
        );
        message.agent_alias = Some(agent.clone());
        message.context_id = Some(context_id.clone());
        message.task_id = Some(task_id.clone());
        message.role = Some(MessageRole::User);
        message.protocol_message_id = Some("protocol-msg-1".to_owned());
        message.ordinal = 7;
        message
            .metadata
            .insert_str("a2a.protocol_version", "1.0")
            .expect("message metadata");
        let stored_message = store.insert_message(&message).expect("message insert");

        assert_eq!(stored_message.agent_alias.as_ref(), Some(&agent));
        assert_eq!(stored_message.context_id.as_ref(), Some(&context_id));
        assert_eq!(stored_message.task_id.as_ref(), Some(&task_id));
        assert_eq!(stored_message.direction, MessageDirection::Request);
        assert_eq!(stored_message.role, Some(MessageRole::User));
        assert_eq!(stored_message.ordinal, 7);
        assert_eq!(stored_message.content_json["messageId"], "protocol-msg-1");
        assert_eq!(
            store.list_messages().expect("messages"),
            vec![stored_message]
        );

        let event_id = event_id("evt-1");
        let mut event = EventInsert::new(
            event_id.clone(),
            "cli",
            "task.completed",
            json!({"task_id": task_id.as_str()}),
        );
        event.agent_alias = Some(agent);
        event.context_id = Some(context_id.clone());
        event.task_id = Some(task_id.clone());
        event
            .record_a2a_protocol_version("1.0")
            .expect("record event protocol version");
        let stored_event = store.append_event(&event).expect("event append");

        assert_eq!(stored_event.sequence, 1);
        assert_eq!(stored_event.task_id.as_ref(), Some(&task_id));
        assert_eq!(
            stored_event.metadata.get_str(METADATA_A2A_PROTOCOL_VERSION),
            Some("1.0")
        );
        assert_eq!(
            store.list_events().expect("list events"),
            vec![stored_event.clone()]
        );
        assert!(store.delete_event(&event_id).expect("delete event"));
        assert!(store.delete_task(&task_id).expect("delete task"));
        assert!(store.delete_context(&context_id).expect("delete context"));
    }

    #[test]
    fn artifact_crud_round_trip_and_cascades_with_task() {
        let store = Store::open_in_memory().expect("store");
        let agent = seed_agent(&store, "echo");
        let context = context_id("ctx-artifact");
        store
            .upsert_context(&ContextUpsert::new(context.clone()))
            .expect("context");
        let task = seed_task(&store, &agent, "task-artifact");

        let artifact_id = ArtifactId::new("artifact-1").expect("artifact id");
        let mut artifact = ArtifactUpsert::new(artifact_id.clone(), task.clone());
        artifact.context_id = Some(context.clone());
        artifact.name = Some("result.json".to_owned());
        artifact.mime_type = Some("application/json".to_owned());
        artifact.kind = ArtifactKind::Json;
        artifact.content_json =
            Some(json!({"artifactId": "artifact-1", "parts": [{"data": {"answer": 42}}]}));
        artifact
            .metadata
            .insert_str("origin", "repository-test")
            .expect("metadata");

        let created = store.upsert_artifact(&artifact).expect("artifact upsert");
        assert_eq!(created.artifact_id, artifact_id);
        assert_eq!(created.task_id, task);
        assert_eq!(created.context_id.as_ref(), Some(&context));
        assert_eq!(created.kind, ArtifactKind::Json);
        assert_eq!(created.version, 1);
        assert_eq!(created.metadata.get_str("origin"), Some("repository-test"));

        let mut updated = artifact.clone();
        updated.version = 2;
        updated.kind = ArtifactKind::Text;
        updated.content_json =
            Some(json!({"artifactId": "artifact-1", "parts": [{"text": "chunk"}]}));
        let updated_record = store.upsert_artifact(&updated).expect("artifact update");
        assert_eq!(updated_record.version, 2);
        assert_eq!(updated_record.kind, ArtifactKind::Text);
        assert_eq!(
            store
                .list_artifacts_for_task(&task)
                .expect("task artifacts"),
            vec![updated_record.clone()]
        );
        assert_eq!(
            store.list_artifacts().expect("artifacts"),
            vec![updated_record]
        );

        assert!(store.delete_task(&task).expect("delete task"));
        assert!(
            store
                .get_artifact(&artifact_id)
                .expect("artifact cascaded")
                .is_none()
        );
    }

    #[test]
    fn push_config_crud_round_trip_soft_deletes() {
        let store = Store::open_in_memory().expect("store");
        let agent = seed_agent(&store, "echo");
        let task = seed_task(&store, &agent, "task-push");
        let push_config_id = PushConfigId::new("push-1").expect("push config id");
        let mut input = PushConfigUpsert::new(
            push_config_id.clone(),
            agent.clone(),
            "http://127.0.0.1:8080/callback",
        );
        input.task_id = Some(task.clone());
        input.remote_config_json = Some(json!({
            "id": "push-1",
            "taskId": "task-push",
            "url": "http://127.0.0.1:8080/callback",
            "authentication": {"scheme": "Bearer", "credentials": "[REDACTED]"}
        }));
        input
            .metadata
            .insert_str("purpose", "repository-test")
            .expect("metadata");

        let created = store.upsert_push_config(&input).expect("push upsert");
        assert_eq!(created.push_config_id, push_config_id);
        assert_eq!(created.agent_alias, agent);
        assert_eq!(created.task_id.as_ref(), Some(&task));
        assert_eq!(created.metadata.get_str("purpose"), Some("repository-test"));
        assert!(created.deleted_at.is_none());
        assert_eq!(
            store
                .list_push_configs_for_task(&created.agent_alias, &task)
                .expect("task push configs"),
            vec![created.clone()]
        );
        assert_eq!(
            store.list_push_configs().expect("push configs"),
            vec![created.clone()]
        );

        let mut updated = input.clone();
        updated.callback_url = "https://example.test/callback".to_owned();
        let updated = store.upsert_push_config(&updated).expect("push update");
        assert_eq!(updated.callback_url, "https://example.test/callback");

        assert!(
            store
                .delete_push_config(&push_config_id)
                .expect("delete push")
        );
        let deleted = store
            .get_push_config(&push_config_id)
            .expect("get deleted push")
            .expect("push row retained");
        assert!(deleted.deleted_at.is_some());
        assert!(
            !store
                .delete_push_config(&push_config_id)
                .expect("delete again")
        );
    }

    #[test]
    fn group_crud_and_membership_enforce_rank_uniqueness() {
        let store = Store::open_in_memory().expect("store");
        let echo = seed_agent(&store, "echo");
        let plan = seed_agent(&store, "plan");
        let group_name = group("team");
        let mut group_input = GroupUpsert::new(group_name.clone());
        group_input.notes = Some("test group".to_owned());

        let stored_group = store.upsert_group(&group_input).expect("group upsert");
        assert_eq!(stored_group.notes.as_deref(), Some("test group"));

        let mut first = GroupMemberUpsert::new(group_name.clone(), echo.clone(), rank("rank-0"));
        first.tags = vec!["writer".to_owned()];
        store.upsert_group_member(&first).expect("first member");
        let duplicate_rank =
            GroupMemberUpsert::new(group_name.clone(), plan.clone(), rank("rank-0"));
        let error = store
            .upsert_group_member(&duplicate_rank)
            .expect_err("duplicate rank should fail");
        assert_eq!(error.category(), missive_core::ErrorCategory::Storage);

        let second = GroupMemberUpsert::new(group_name.clone(), plan.clone(), rank("rank-1"));
        store.upsert_group_member(&second).expect("second member");
        let members = store.list_group_members(&group_name).expect("members");
        assert_eq!(
            members
                .iter()
                .map(|member| &member.agent_alias)
                .collect::<Vec<_>>(),
            vec![&echo, &plan]
        );

        assert!(
            store
                .remove_group_member(&group_name, &echo)
                .expect("remove member")
        );
        assert_eq!(
            store
                .list_group_members(&group_name)
                .expect("members")
                .len(),
            1
        );
        let renamed = group("team-renamed");
        assert!(
            store
                .rename_group(&group_name, &renamed)
                .expect("rename group")
        );
        assert!(store.get_group(&group_name).expect("old group").is_none());
        assert_eq!(
            store
                .list_group_members(&renamed)
                .expect("renamed members")
                .len(),
            1
        );

        assert!(store.delete_group(&renamed).expect("delete group"));
        assert!(
            store
                .list_group_members(&renamed)
                .expect("members")
                .is_empty()
        );
    }

    #[test]
    fn gateway_job_crud_tracks_state_and_payloads() {
        let store = Store::open_in_memory().expect("store");
        let agent = seed_agent(&store, "echo");
        let task = seed_task(&store, &agent, "task-job");
        let job_id = GatewayJobId::new("job-1").expect("job id");
        let mut job = GatewayJobUpsert::new(job_id.clone(), "wait", json!({"task": task.as_str()}));
        job.agent_alias = Some(agent);
        job.task_id = Some(task);
        job.next_run_at = Some(timestamp(1_735_787_045));

        let queued = store.upsert_gateway_job(&job).expect("job upsert");
        assert_eq!(queued.state, GatewayJobState::Queued);
        assert_eq!(queued.request_json["task"], "task-job");

        let mut running = job.clone();
        running.state = GatewayJobState::Running;
        running.retry_count = 1;
        running.max_attempts = 3;
        running.locked_by = Some("worker-1".to_owned());
        let updated = store.upsert_gateway_job(&running).expect("job update");
        assert_eq!(updated.state, GatewayJobState::Running);
        assert_eq!(updated.locked_by.as_deref(), Some("worker-1"));
        assert_eq!(
            store.list_gateway_jobs().expect("jobs"),
            vec![updated.clone()]
        );
        assert!(store.delete_gateway_job(&job_id).expect("delete job"));
        assert!(
            store
                .get_gateway_job(&job_id)
                .expect("job deleted")
                .is_none()
        );
    }

    #[test]
    fn gateway_session_persists_named_resume_across_store_reopen() {
        let temp = tempdir().expect("tempdir");
        let database_path = temp.path().join("missive.sqlite3");
        let agent = alias("echo");
        let context = context_id("ctx-session");
        let session_id = GatewaySessionId::new("session-1").expect("session id");

        {
            let store = Store::open(&database_path).expect("store");
            store
                .upsert_agent(&AgentUpsert::new(agent.clone(), "http://127.0.0.1/session"))
                .expect("agent");
            let mut context_input = ContextUpsert::new(context.clone());
            context_input.agent_alias = Some(agent.clone());
            context_input.name = Some("gateway-session-context".to_owned());
            store.upsert_context(&context_input).expect("context");

            let mut session = GatewaySessionUpsert::new(
                session_id.clone(),
                "adapter",
                "stdin/user-1",
                agent.clone(),
                "daily-standup",
                context.clone(),
            );
            session.reset_mode = GatewaySessionResetMode::Both;
            session.daily_reset_hour = 4;
            session.idle_timeout_seconds = Some(3_600);
            session.last_active_at = timestamp(1_735_787_045);
            session.last_reset_at = Some(timestamp(1_735_700_000));
            session.reset_count = 2;
            session
                .metadata
                .insert_str("purpose", "repository-test")
                .expect("metadata");

            let created = store
                .upsert_gateway_session(&session)
                .expect("session upsert");
            assert_eq!(created.gateway_session_id, session_id);
            assert_eq!(created.source_kind, "adapter");
            assert_eq!(created.source_id, "stdin/user-1");
            assert_eq!(created.agent_alias, agent);
            assert_eq!(created.resume_name, "daily-standup");
            assert_eq!(created.context_id, context);
            assert_eq!(created.reset_mode, GatewaySessionResetMode::Both);
            assert_eq!(created.daily_reset_hour, 4);
            assert_eq!(created.idle_timeout_seconds, Some(3_600));
            assert_eq!(created.reset_count, 2);
            assert_eq!(created.metadata.get_str("purpose"), Some("repository-test"));
            assert_eq!(
                store
                    .get_gateway_session_by_resume(
                        "adapter",
                        "stdin/user-1",
                        &created.agent_alias,
                        "daily-standup",
                    )
                    .expect("named resume"),
                Some(created.clone())
            );
            assert_eq!(
                store
                    .list_gateway_sessions_for_agent(&created.agent_alias)
                    .expect("sessions for agent"),
                vec![created]
            );
        }

        let reopened = Store::open(&database_path).expect("reopened store");
        let persisted = reopened
            .get_gateway_session(&session_id)
            .expect("session get after reopen")
            .expect("session persisted");
        assert_eq!(persisted.resume_name, "daily-standup");
        assert_eq!(persisted.context_id, context);
        assert_eq!(persisted.last_active_at, timestamp(1_735_787_045));
        assert_eq!(persisted.last_reset_at, Some(timestamp(1_735_700_000)));
        assert_eq!(
            reopened.list_gateway_sessions().expect("sessions"),
            vec![persisted.clone()]
        );
        assert!(
            reopened
                .delete_gateway_session(&session_id)
                .expect("delete session")
        );
    }

    #[test]
    fn gateway_session_reset_policy_validation_is_actionable() {
        let store = Store::open_in_memory().expect("store");
        let agent = seed_agent(&store, "echo");
        let context = context_id("ctx-session-validation");
        store
            .upsert_context(&ContextUpsert::new(context.clone()))
            .expect("context");
        let mut session = GatewaySessionUpsert::new(
            GatewaySessionId::new("session-invalid").expect("session id"),
            "adapter",
            "stdin/user-1",
            agent,
            "default",
            context,
        );
        session.reset_mode = GatewaySessionResetMode::Idle;

        let error = store
            .upsert_gateway_session(&session)
            .expect_err("idle reset requires timeout");

        assert_eq!(error.category(), missive_core::ErrorCategory::Validation);
        assert!(error.to_string().contains("idle_timeout_seconds"));
    }

    #[test]
    fn transactions_commit_on_success_and_rollback_on_error() {
        let mut store = Store::open_in_memory().expect("store");
        let committed = alias("committed");
        store
            .transaction(|transaction| {
                transaction.upsert_agent(&AgentUpsert::new(
                    committed.clone(),
                    "http://127.0.0.1:9001",
                ))?;
                Ok(())
            })
            .expect("commit transaction");
        assert!(
            store
                .get_agent(&committed)
                .expect("committed agent")
                .is_some()
        );

        let rolled_back = alias("rollback");
        let error = store
            .transaction(|transaction| {
                transaction.upsert_agent(&AgentUpsert::new(
                    rolled_back.clone(),
                    "http://127.0.0.1:9002",
                ))?;
                Err::<(), MissiveError>(MissiveError::validation("force transaction rollback"))
            })
            .expect_err("transaction should fail");

        assert_eq!(error.category(), missive_core::ErrorCategory::Validation);
        assert!(
            store
                .get_agent(&rolled_back)
                .expect("rolled back agent")
                .is_none()
        );
    }

    #[test]
    fn transaction_rolls_back_after_sql_constraint_failure() {
        let mut store = Store::open_in_memory().expect("store");
        let agent = alias("echo");
        let event_id = event_id("evt-duplicate");
        let result = store.transaction(|transaction| {
            transaction.upsert_agent(&AgentUpsert::new(agent.clone(), "http://127.0.0.1:9003"))?;
            let mut first = EventInsert::new(event_id.clone(), "cli", "test", json!({}));
            first.agent_alias = Some(agent.clone());
            transaction.append_event(&first)?;
            transaction.append_event(&first)?;
            Ok(())
        });

        assert!(result.is_err());
        assert!(
            store
                .get_agent(&agent)
                .expect("agent rolled back")
                .is_none()
        );
        assert!(
            store
                .get_event(&event_id)
                .expect("event rolled back")
                .is_none()
        );
    }

    fn seed_agent(store: &Store, value: &str) -> AgentAlias {
        let alias = alias(value);
        store
            .upsert_agent(&AgentUpsert::new(
                alias.clone(),
                format!("http://127.0.0.1/{value}"),
            ))
            .expect("seed agent");
        alias
    }

    fn seed_task(store: &Store, agent: &AgentAlias, value: &str) -> TaskId {
        let task = task_id(value);
        store
            .upsert_task(&TaskUpsert::new(
                task.clone(),
                agent.clone(),
                TaskState::Submitted,
            ))
            .expect("seed task");
        task
    }

    fn alias(value: &str) -> AgentAlias {
        AgentAlias::new(value).expect("agent alias")
    }

    fn group(value: &str) -> GroupName {
        GroupName::new(value).expect("group name")
    }

    fn rank(value: &str) -> RankName {
        RankName::new(value).expect("rank name")
    }

    fn transport(value: &str) -> TransportName {
        TransportName::new(value).expect("transport")
    }

    fn context_id(value: &str) -> ContextId {
        ContextId::new(value).expect("context id")
    }

    fn task_id(value: &str) -> TaskId {
        TaskId::new(value).expect("task id")
    }

    fn message_id(value: &str) -> MessageId {
        MessageId::new(value).expect("message id")
    }

    fn event_id(value: &str) -> EventId {
        EventId::new(value).expect("event id")
    }

    fn timestamp(seconds: i64) -> MissiveTimestamp {
        MissiveTimestamp::from_unix_timestamp(seconds).expect("timestamp")
    }
}
