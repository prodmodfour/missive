//! stdin/stdout adapter framing and command mapping.
//!
//! The stdio adapter is the local subprocess boundary for humans and other
//! agents that want to drive missive without scraping terminal text. It accepts
//! JSON or NDJSON frames, maps each frame to a small send/stream/task command
//! model, and emits JSON/NDJSON response frames.

use std::io::{BufRead, Read, Write};

use missive_core::{
    AgentAlias, ContextId, ErrorReport, EventId, MessageId, Metadata, MissiveError, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    Adapter, AdapterAcknowledgement, AdapterContext, AdapterDefinition, AdapterEvent,
    AdapterExternalIdentity, AdapterIdentity, AdapterInboundMessage, AdapterInboundPayload,
    AdapterLifecycleEvent, AdapterLifecycleState, AdapterOutboundUpdate,
};

/// Built-in adapter kind for stdin/stdout frames.
pub const STDIO_ADAPTER_KIND: &str = "stdio";

/// Stable schema marker for stdin/stdout adapter request and response frames.
pub const STDIO_FRAME_SCHEMA_VERSION: &str = "missive.stdio.v1";

/// Output kind used when a command writes one framed command result.
pub const STDIO_OUTPUT_KIND_COMMAND_OUTPUT: &str = "stdio_command_output";

/// Output kind used when frame parsing or command execution fails.
pub const STDIO_OUTPUT_KIND_ERROR: &str = "stdio_error";

/// Output kind used for adapter lifecycle frames.
pub const STDIO_OUTPUT_KIND_LIFECYCLE: &str = "stdio_lifecycle";

const DEFAULT_SOURCE_ID: &str = "stdio";
const DEFAULT_RESUME_NAME: &str = "default";

/// stdin/stdout framing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StdioFraming {
    /// A single JSON object is read or written.
    Json,
    /// One JSON object per line is read or written.
    Ndjson,
}

impl StdioFraming {
    /// Stable lowercase label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Ndjson => "ndjson",
        }
    }
}

/// stdin/stdout adapter runtime mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StdioRunMode {
    /// Read one request frame and write the corresponding response frame(s).
    SingleShot,
    /// Read NDJSON request frames until EOF, writing NDJSON response frames as each frame completes.
    LongRunning,
}

impl StdioRunMode {
    /// Stable lowercase label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SingleShot => "single_shot",
            Self::LongRunning => "long_running",
        }
    }
}

/// Source/session hints attached to one stdio request frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StdioFrameSource {
    /// Stable source id for session continuity. Defaults to `stdio`.
    #[serde(default = "default_source_id")]
    pub source_id: String,
    /// Optional human display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Gateway session resume name. Defaults to `default`.
    #[serde(default = "default_resume_name")]
    pub resume_name: String,
    /// Optional profile hint for a gateway session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

impl Default for StdioFrameSource {
    fn default() -> Self {
        Self {
            source_id: default_source_id(),
            display_name: None,
            resume_name: default_resume_name(),
            profile: None,
        }
    }
}

/// One stdio request frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StdioInputFrame {
    /// Stable frame schema marker.
    #[serde(default = "default_stdio_schema_version")]
    pub schema_version: String,
    /// Request/correlation id supplied by the caller.
    pub id: String,
    /// Source/session hints.
    #[serde(default)]
    pub source: StdioFrameSource,
    /// Command to run or enqueue.
    #[serde(flatten)]
    pub command: StdioCommand,
    /// Non-secret frame metadata.
    #[serde(default)]
    pub metadata: Metadata,
}

impl StdioInputFrame {
    /// Parses and validates one JSON request frame.
    pub fn from_json_str(input: &str) -> Result<Self> {
        let frame = serde_json::from_str::<Self>(input).map_err(|error| {
            MissiveError::validation("failed to parse stdio input frame as JSON")
                .with_source(error)
                .with_help(
                    "Send one JSON object with schema_version, id, command, and command fields.",
                )
        })?;
        frame.validate()?;
        Ok(frame)
    }

    /// Parses and validates one JSON request frame from a [`serde_json::Value`].
    pub fn from_value(value: Value) -> Result<Self> {
        let frame = serde_json::from_value::<Self>(value).map_err(|error| {
            MissiveError::validation("failed to parse stdio input frame")
                .with_source(error)
                .with_help("Send an object with schema_version, id, command, and command fields.")
        })?;
        frame.validate()?;
        Ok(frame)
    }

    /// Validates the schema marker, id, source, and command fields.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != STDIO_FRAME_SCHEMA_VERSION {
            return Err(MissiveError::validation(format!(
                "unsupported stdio frame schema_version {:?}",
                self.schema_version
            ))
            .with_help(format!(
                "Use schema_version {STDIO_FRAME_SCHEMA_VERSION:?}."
            )));
        }
        validate_frame_id(&self.id)?;
        AdapterExternalIdentity::new(self.source.source_id.clone())?;
        if self.source.resume_name.trim().is_empty() {
            return Err(MissiveError::validation(
                "stdio frame source.resume_name cannot be empty",
            ));
        }
        self.command.validate()
    }
}

/// Message fields shared by send and stream stdio commands.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StdioMessageFields {
    /// Registered missive agent alias.
    pub agent: String,
    /// Optional primary text message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Extra text parts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text_parts: Vec<String>,
    /// Structured JSON parts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub json_parts: Vec<Value>,
    /// Local file references to send as A2A file-url parts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// Local files to embed as A2A raw-byte parts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_bytes: Vec<String>,
    /// MIME/media type assignments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mime: Vec<String>,
    /// Non-secret A2A request metadata.
    #[serde(default)]
    pub metadata: Metadata,
    /// Optional A2A context id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Optional A2A task id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// Accepted response modes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepted_output_modes: Vec<String>,
}

impl StdioMessageFields {
    fn validate(&self, command: &str) -> Result<()> {
        AgentAlias::new(self.agent.clone())?;
        if self.message.is_none()
            && self.text_parts.is_empty()
            && self.json_parts.is_empty()
            && self.files.is_empty()
            && self.file_bytes.is_empty()
        {
            return Err(MissiveError::validation(format!(
                "stdio {command} command requires message, text_parts, json_parts, files, or file_bytes"
            )));
        }
        if let Some(context) = &self.context {
            ContextId::new(context.clone())?;
        }
        if let Some(task) = &self.task {
            missive_core::TaskId::new(task.clone())?;
        }
        validate_short_values("text_parts", &self.text_parts)?;
        validate_short_values("files", &self.files)?;
        validate_short_values("file_bytes", &self.file_bytes)?;
        validate_short_values("mime", &self.mime)?;
        validate_short_values("accepted_output_modes", &self.accepted_output_modes)?;
        Ok(())
    }
}

/// A stdio stream command.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StdioStreamCommand {
    /// Message fields to stream.
    #[serde(flatten)]
    pub message: StdioMessageFields,
    /// Attempt streaming even if the Agent Card does not advertise streaming.
    #[serde(default)]
    pub force: bool,
}

/// Arguments for a task get frame.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StdioTaskGetCommand {
    /// A2A task id.
    pub task_id: String,
    /// Optional agent alias for remote refresh or local filtering.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Fetch from the remote A2A agent before rendering.
    #[serde(default)]
    pub remote: bool,
    /// Optional local source filter: remote, local, or gateway.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Optional history length for remote refresh.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_length: Option<i32>,
}

/// Arguments for a task list frame.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StdioTaskListCommand {
    /// Optional agent alias. Required for remote list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Optional context filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Optional task state filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Optional RFC3339 updated-after filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_after: Option<String>,
    /// Optional local source filter: remote, local, or gateway.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Query the selected remote agent with A2A ListTasks.
    #[serde(default)]
    pub remote: bool,
    /// Optional remote page size.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_size: Option<i32>,
    /// Optional remote page token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,
    /// Optional remote history length.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_length: Option<i32>,
    /// Ask remote agent to include artifacts.
    #[serde(default)]
    pub include_artifacts: bool,
}

/// Arguments for a task wait frame.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StdioTaskWaitCommand {
    /// A2A task id.
    pub task_id: String,
    /// Optional agent alias.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Poll only local SQLite state.
    #[serde(default)]
    pub local: bool,
    /// Optional polling interval such as 500ms or 2s.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval: Option<String>,
    /// Optional history length for remote polling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_length: Option<i32>,
}

/// Arguments for a task cancel frame.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StdioTaskCancelCommand {
    /// A2A task id.
    pub task_id: String,
    /// Optional agent alias.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

/// Commands accepted by stdin/stdout frames.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum StdioCommand {
    /// Run a non-streaming send command.
    Send(StdioMessageFields),
    /// Run a streaming send command.
    Stream(StdioStreamCommand),
    /// Run task get.
    TaskGet(StdioTaskGetCommand),
    /// Run task list.
    TaskList(StdioTaskListCommand),
    /// Run task wait.
    TaskWait(StdioTaskWaitCommand),
    /// Run task cancel.
    TaskCancel(StdioTaskCancelCommand),
}

impl StdioCommand {
    /// Stable command label.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Send(_) => "send",
            Self::Stream(_) => "stream",
            Self::TaskGet(_) => "task_get",
            Self::TaskList(_) => "task_list",
            Self::TaskWait(_) => "task_wait",
            Self::TaskCancel(_) => "task_cancel",
        }
    }

    /// Agent targeted by this command when present.
    #[must_use]
    pub fn target_agent(&self) -> Option<&str> {
        match self {
            Self::Send(message) => Some(&message.agent),
            Self::Stream(command) => Some(&command.message.agent),
            Self::TaskGet(command) => command.agent.as_deref(),
            Self::TaskList(command) => command.agent.as_deref(),
            Self::TaskWait(command) => command.agent.as_deref(),
            Self::TaskCancel(command) => command.agent.as_deref(),
        }
    }

    /// Context id referenced by this command when present.
    #[must_use]
    pub fn context_id(&self) -> Option<&str> {
        match self {
            Self::Send(message) => message.context.as_deref(),
            Self::Stream(command) => command.message.context.as_deref(),
            Self::TaskList(command) => command.context.as_deref(),
            Self::TaskGet(_) | Self::TaskWait(_) | Self::TaskCancel(_) => None,
        }
    }

    fn validate(&self) -> Result<()> {
        match self {
            Self::Send(message) => message.validate(self.as_str()),
            Self::Stream(command) => command.message.validate(self.as_str()),
            Self::TaskGet(command) => validate_task_get(command),
            Self::TaskList(command) => validate_task_list(command),
            Self::TaskWait(command) => validate_task_wait(command),
            Self::TaskCancel(command) => validate_task_cancel(command),
        }
    }
}

/// One stdio response frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StdioOutputFrame {
    /// Stable frame schema marker.
    pub schema_version: String,
    /// Request/correlation id when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Whether the frame represents a successful command event.
    pub ok: bool,
    /// Stable output kind.
    pub kind: String,
    /// Sequence number within this request.
    pub sequence: u64,
    /// Output payload when successful.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    /// Structured error when `ok` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorReport>,
}

impl StdioOutputFrame {
    /// Creates one successful output frame.
    pub fn success(
        id: Option<String>,
        kind: impl Into<String>,
        sequence: u64,
        data: impl Serialize,
    ) -> Result<Self> {
        let data = serde_json::to_value(data).map_err(|error| {
            MissiveError::orchestration("failed to encode stdio output frame data")
                .with_source(error)
        })?;
        Ok(Self {
            schema_version: STDIO_FRAME_SCHEMA_VERSION.to_owned(),
            id,
            ok: true,
            kind: kind.into(),
            sequence,
            data: Some(data),
            error: None,
        })
    }

    /// Creates one error output frame.
    #[must_use]
    pub fn error(id: Option<String>, sequence: u64, error: &MissiveError) -> Self {
        Self {
            schema_version: STDIO_FRAME_SCHEMA_VERSION.to_owned(),
            id,
            ok: false,
            kind: STDIO_OUTPUT_KIND_ERROR.to_owned(),
            sequence,
            data: None,
            error: Some(error.to_report()),
        }
    }
}

/// Reads one JSON stdio input frame from a reader.
pub fn read_single_frame<R>(reader: &mut R) -> Result<StdioInputFrame>
where
    R: Read,
{
    let mut input = String::new();
    reader
        .read_to_string(&mut input)
        .map_err(|error| MissiveError::io("reading stdio input frame", error))?;
    if input.trim().is_empty() {
        return Err(MissiveError::validation(
            "stdin/stdout adapter expected one JSON frame on stdin",
        )
        .with_help(
            "Pass --mode long-running for NDJSON request streams, or pipe one JSON object.",
        ));
    }
    StdioInputFrame::from_json_str(&input)
}

/// Parses each non-empty NDJSON line and calls `handler` with either a frame or a per-line error.
pub fn read_ndjson_frames<R, F>(reader: &mut R, mut handler: F) -> Result<usize>
where
    R: BufRead,
    F: FnMut(usize, std::result::Result<StdioInputFrame, MissiveError>) -> Result<()>,
{
    let mut count = 0;
    for (index, line) in reader.lines().enumerate() {
        let line = line.map_err(|error| MissiveError::io("reading stdio NDJSON frame", error))?;
        if line.trim().is_empty() {
            continue;
        }
        count += 1;
        handler(index + 1, StdioInputFrame::from_json_str(&line))?;
    }
    Ok(count)
}

/// Writes one stdio output frame using the requested framing.
pub fn write_output_frame<W>(
    writer: &mut W,
    framing: StdioFraming,
    frame: &StdioOutputFrame,
) -> Result<()>
where
    W: Write,
{
    match framing {
        StdioFraming::Json => serde_json::to_writer_pretty(&mut *writer, frame)
            .map_err(json_write_error)
            .and_then(|()| {
                writeln!(writer)
                    .map_err(|error| MissiveError::io("writing stdio JSON frame", error))
            }),
        StdioFraming::Ndjson => serde_json::to_writer(&mut *writer, frame)
            .map_err(json_write_error)
            .and_then(|()| {
                writeln!(writer)
                    .map_err(|error| MissiveError::io("writing stdio NDJSON frame", error))
            }),
    }
}

/// Minimal stdin/stdout adapter implementation for the shared adapter registry.
#[derive(Debug, Clone)]
pub struct StdioAdapter {
    definition: AdapterDefinition,
    started: bool,
    stopped: bool,
    delivered_updates: Vec<AdapterOutboundUpdate>,
    acknowledgements: Vec<AdapterAcknowledgement>,
}

impl StdioAdapter {
    /// Creates a stdio adapter instance for one definition.
    pub fn new(definition: AdapterDefinition) -> Result<Self> {
        if definition.kind != STDIO_ADAPTER_KIND {
            return Err(MissiveError::config(format!(
                "stdio adapter cannot be created for adapter kind {:?}",
                definition.kind
            )));
        }
        Ok(Self {
            definition,
            started: false,
            stopped: false,
            delivered_updates: Vec::new(),
            acknowledgements: Vec::new(),
        })
    }

    /// Maps one validated request frame into a generic adapter inbound message.
    pub fn inbound_message_from_frame(
        &self,
        frame: &StdioInputFrame,
    ) -> Result<AdapterInboundMessage> {
        let external =
            AdapterExternalIdentity::new(frame.source.source_id.clone()).map(|identity| {
                if let Some(display_name) = &frame.source.display_name {
                    identity.with_display_name(display_name.clone())
                } else {
                    identity
                }
            })?;
        let identity = self.map_identity(external)?;
        let mut session = crate::AdapterSession::new(frame.source.resume_name.clone())?;
        session.profile = frame
            .source
            .profile
            .clone()
            .or_else(|| self.definition.session_profile.clone());
        session.target_agent = frame
            .command
            .target_agent()
            .map(|agent| AgentAlias::new(agent.to_owned()))
            .transpose()?;
        session.context_id = frame
            .command
            .context_id()
            .map(|context| ContextId::new(context.to_owned()))
            .transpose()?;
        let payload = inbound_payload_for_command(&frame.command)?;
        let mut message = AdapterInboundMessage::new(
            self.definition.name.clone(),
            MessageId::new(format!("msg/stdio/{}", frame.id))?,
            identity,
            session,
            payload,
        )?;
        message.metadata = frame.metadata.clone();
        message
            .metadata
            .insert_str("missive.stdio.command", frame.command.as_str())?;
        message
            .metadata
            .insert_str("missive.stdio.frame_id", frame.id.clone())?;
        Ok(message)
    }

    /// Emits one frame as an adapter inbound-message event through a running context.
    pub fn emit_frame(&self, context: &AdapterContext, frame: &StdioInputFrame) -> Result<()> {
        context.emit(AdapterEvent::inbound_message(
            self.inbound_message_from_frame(frame)?,
        ))
    }

    /// Delivered updates recorded by this in-process adapter instance.
    #[must_use]
    pub fn delivered_updates(&self) -> &[AdapterOutboundUpdate] {
        &self.delivered_updates
    }

    /// Acknowledgements recorded by this in-process adapter instance.
    #[must_use]
    pub fn acknowledgements(&self) -> &[AdapterAcknowledgement] {
        &self.acknowledgements
    }
}

impl Adapter for StdioAdapter {
    fn definition(&self) -> &AdapterDefinition {
        &self.definition
    }

    fn start(&mut self, context: AdapterContext) -> Result<()> {
        context.emit(AdapterEvent::lifecycle(AdapterLifecycleEvent::new(
            context.definition(),
            AdapterLifecycleState::Running,
            "stdio adapter ready to process JSON/NDJSON frames",
        )?))?;
        self.started = true;
        self.stopped = false;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.stopped = true;
        self.started = false;
        Ok(())
    }

    fn map_identity(&self, external: AdapterExternalIdentity) -> Result<AdapterIdentity> {
        AdapterIdentity::new(
            self.definition.name.clone(),
            STDIO_ADAPTER_KIND,
            external.provider_user_id,
        )
        .map(|identity| {
            if let Some(display_name) = external.display_name {
                identity.with_display_name(display_name)
            } else {
                identity
            }
        })
    }

    fn deliver_update(&mut self, update: AdapterOutboundUpdate) -> Result<()> {
        if update.adapter_name != self.definition.name {
            return Err(MissiveError::validation(format!(
                "stdio adapter {:?} cannot deliver update for adapter {:?}",
                self.definition.name, update.adapter_name
            )));
        }
        self.delivered_updates.push(update);
        Ok(())
    }

    fn acknowledge(&mut self, acknowledgement: AdapterAcknowledgement) -> Result<()> {
        if acknowledgement.adapter_name != self.definition.name {
            return Err(MissiveError::validation(format!(
                "stdio adapter {:?} cannot acknowledge message for adapter {:?}",
                self.definition.name, acknowledgement.adapter_name
            )));
        }
        self.acknowledgements.push(acknowledgement);
        Ok(())
    }
}

/// Registers the built-in stdio adapter factory in an adapter registry.
pub fn register_stdio_adapter(registry: &mut crate::AdapterRegistry) -> Result<()> {
    registry.register_fn(STDIO_ADAPTER_KIND, |definition| {
        Ok(Box::new(StdioAdapter::new(definition)?))
    })
}

fn validate_task_get(command: &StdioTaskGetCommand) -> Result<()> {
    missive_core::TaskId::new(command.task_id.clone())?;
    validate_optional_agent(command.agent.as_deref())?;
    validate_optional_short("source", command.source.as_deref())?;
    validate_non_negative_i32("history_length", command.history_length)?;
    Ok(())
}

fn validate_task_list(command: &StdioTaskListCommand) -> Result<()> {
    validate_optional_agent(command.agent.as_deref())?;
    if let Some(context) = &command.context {
        ContextId::new(context.clone())?;
    }
    validate_optional_short("state", command.state.as_deref())?;
    validate_optional_short("updated_after", command.updated_after.as_deref())?;
    validate_optional_short("source", command.source.as_deref())?;
    validate_optional_short("page_token", command.page_token.as_deref())?;
    validate_positive_i32("page_size", command.page_size)?;
    validate_non_negative_i32("history_length", command.history_length)?;
    Ok(())
}

fn validate_task_wait(command: &StdioTaskWaitCommand) -> Result<()> {
    missive_core::TaskId::new(command.task_id.clone())?;
    validate_optional_agent(command.agent.as_deref())?;
    validate_optional_short("interval", command.interval.as_deref())?;
    validate_non_negative_i32("history_length", command.history_length)?;
    Ok(())
}

fn validate_task_cancel(command: &StdioTaskCancelCommand) -> Result<()> {
    missive_core::TaskId::new(command.task_id.clone())?;
    validate_optional_agent(command.agent.as_deref())
}

fn validate_optional_agent(value: Option<&str>) -> Result<()> {
    if let Some(value) = value {
        AgentAlias::new(value.to_owned())?;
    }
    Ok(())
}

fn inbound_payload_for_command(command: &StdioCommand) -> Result<AdapterInboundPayload> {
    match command {
        StdioCommand::Send(message) => command_payload_from_message(command.as_str(), message),
        StdioCommand::Stream(stream) => {
            command_payload_from_message(command.as_str(), &stream.message)
        }
        StdioCommand::TaskGet(_)
        | StdioCommand::TaskList(_)
        | StdioCommand::TaskWait(_)
        | StdioCommand::TaskCancel(_) => Ok(AdapterInboundPayload::json(json!({
            "command": command.as_str(),
            "args": command,
        }))),
    }
}

fn command_payload_from_message(
    command_name: &str,
    message: &StdioMessageFields,
) -> Result<AdapterInboundPayload> {
    if let Some(text) = &message.message
        && message.text_parts.is_empty()
        && message.json_parts.is_empty()
        && message.files.is_empty()
        && message.file_bytes.is_empty()
    {
        return Ok(AdapterInboundPayload::text(text.clone()));
    }
    Ok(AdapterInboundPayload::json(json!({
        "command": command_name,
        "agent": message.agent,
        "message": message.message,
        "text_parts": message.text_parts,
        "json_parts": message.json_parts,
        "files": message.files,
        "file_bytes": message.file_bytes,
        "mime": message.mime,
        "context": message.context,
        "task": message.task,
        "accepted_output_modes": message.accepted_output_modes,
        "metadata": message.metadata,
    })))
}

fn validate_frame_id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.chars().any(char::is_whitespace)
        || value.chars().any(char::is_control)
    {
        return Err(MissiveError::validation(
            "stdio frame id must be non-empty and cannot contain whitespace or control characters",
        ));
    }
    if value.len() > 128 {
        return Err(MissiveError::validation(
            "stdio frame id cannot exceed 128 bytes",
        ));
    }
    Ok(())
}

fn validate_short_values(field: &str, values: &[String]) -> Result<()> {
    for value in values {
        validate_optional_short(field, Some(value))?;
    }
    Ok(())
}

fn validate_optional_short(field: &str, value: Option<&str>) -> Result<()> {
    if let Some(value) = value {
        if value.is_empty() {
            return Err(MissiveError::validation(format!(
                "stdio frame field {field} cannot contain empty values"
            )));
        }
        if value.len() > 4096 || value.chars().any(char::is_control) {
            return Err(MissiveError::validation(format!(
                "stdio frame field {field} contains an invalid or too-long value"
            )));
        }
    }
    Ok(())
}

fn validate_positive_i32(field: &str, value: Option<i32>) -> Result<()> {
    if value.is_some_and(|value| value <= 0) {
        return Err(MissiveError::validation(format!(
            "stdio frame field {field} must be greater than zero"
        )));
    }
    Ok(())
}

fn validate_non_negative_i32(field: &str, value: Option<i32>) -> Result<()> {
    if value.is_some_and(|value| value < 0) {
        return Err(MissiveError::validation(format!(
            "stdio frame field {field} must be greater than or equal to zero"
        )));
    }
    Ok(())
}

fn json_write_error(error: serde_json::Error) -> MissiveError {
    MissiveError::io("writing stdio output frame", error.into())
}

fn default_stdio_schema_version() -> String {
    STDIO_FRAME_SCHEMA_VERSION.to_owned()
}

fn default_source_id() -> String {
    DEFAULT_SOURCE_ID.to_owned()
}

fn default_resume_name() -> String {
    DEFAULT_RESUME_NAME.to_owned()
}

/// Returns a best-effort process-unique event id for stdio lifecycle frames.
pub fn new_stdio_event_id() -> Result<EventId> {
    EventId::new(format!(
        "evt/stdio/{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(
                |error| MissiveError::orchestration("system clock is before UNIX epoch")
                    .with_source(error)
            )?
            .as_nanos()
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::*;
    use crate::{AdapterEventSink, AdapterRegistry};

    #[derive(Debug, Default)]
    struct RecordingSink {
        events: Mutex<Vec<AdapterEvent>>,
    }

    impl AdapterEventSink for RecordingSink {
        fn emit(&self, event: AdapterEvent) -> Result<()> {
            self.events.lock().expect("event mutex").push(event);
            Ok(())
        }
    }

    impl RecordingSink {
        fn events(&self) -> Vec<AdapterEvent> {
            self.events.lock().expect("event mutex").clone()
        }
    }

    #[test]
    fn valid_send_frame_maps_to_inbound_message() -> Result<()> {
        let frame = StdioInputFrame::from_json_str(
            r#"{
                "schema_version":"missive.stdio.v1",
                "id":"req-1",
                "source":{"source_id":"agent-42","display_name":"Agent 42","resume_name":"work"},
                "command":"send",
                "agent":"echo",
                "message":"hello from stdin"
            }"#,
        )?;
        let definition = AdapterDefinition::new("stdio", STDIO_ADAPTER_KIND)?;
        let adapter = StdioAdapter::new(definition)?;
        let message = adapter.inbound_message_from_frame(&frame)?;

        assert_eq!(message.adapter_name, "stdio");
        assert_eq!(message.identity.source_id, "agent-42");
        assert_eq!(
            message
                .session
                .target_agent
                .as_ref()
                .map(AgentAlias::as_str),
            Some("echo")
        );
        assert_eq!(
            message.metadata.get_str("missive.stdio.command"),
            Some("send")
        );
        assert_eq!(
            message.payload,
            AdapterInboundPayload::text("hello from stdin")
        );
        Ok(())
    }

    #[test]
    fn invalid_frame_is_reported_as_validation_error_without_panicking() {
        let error = StdioInputFrame::from_json_str(
            r#"{"schema_version":"wrong","id":"req-1","command":"task_list"}"#,
        )
        .expect_err("wrong schema should fail");

        assert!(error.message().contains("unsupported stdio frame"));
        let frame = StdioOutputFrame::error(Some("req-1".to_owned()), 0, &error);
        assert!(!frame.ok);
        assert_eq!(frame.kind, STDIO_OUTPUT_KIND_ERROR);
        assert_eq!(
            frame.error.expect("error report").code,
            "missive::validation"
        );
    }

    #[test]
    fn ndjson_output_frames_are_one_json_object_per_line() -> Result<()> {
        let frames = [
            StdioOutputFrame::success(
                Some("req-stream".to_owned()),
                STDIO_OUTPUT_KIND_COMMAND_OUTPUT,
                0,
                json!({"kind":"stream_event","data":{"state":"working"}}),
            )?,
            StdioOutputFrame::success(
                Some("req-stream".to_owned()),
                STDIO_OUTPUT_KIND_COMMAND_OUTPUT,
                1,
                json!({"kind":"stream_result","data":{"event_count":1}}),
            )?,
        ];
        let mut output = Vec::new();
        for frame in &frames {
            write_output_frame(&mut output, StdioFraming::Ndjson, frame)?;
        }
        let output = String::from_utf8(output).expect("UTF-8");
        let lines = output.lines().collect::<Vec<_>>();

        assert_eq!(lines.len(), 2);
        assert_eq!(
            serde_json::from_str::<Value>(lines[0]).expect("line 1")["id"],
            "req-stream"
        );
        assert_eq!(
            serde_json::from_str::<Value>(lines[1]).expect("line 2")["sequence"],
            1
        );
        Ok(())
    }

    #[test]
    fn registry_can_create_stdio_adapter_and_emit_frame() -> Result<()> {
        let mut registry = AdapterRegistry::new();
        register_stdio_adapter(&mut registry)?;
        let definition = AdapterDefinition::new("stdio", STDIO_ADAPTER_KIND)?;
        let mut adapter = registry.create(&definition)?;
        let sink = Arc::new(RecordingSink::default());
        let context = AdapterContext::new(definition.clone(), sink.clone());
        adapter.start(context.clone())?;

        let frame = StdioInputFrame::from_value(json!({
            "id": "req-list",
            "command": "task_list",
            "source": {"source_id": "runner"}
        }))?;
        let stdio = StdioAdapter::new(definition)?;
        stdio.emit_frame(&context, &frame)?;

        let events = sink.events();
        assert!(matches!(events[0], AdapterEvent::Lifecycle(_)));
        assert!(matches!(events[1], AdapterEvent::InboundMessage(_)));
        Ok(())
    }
}
