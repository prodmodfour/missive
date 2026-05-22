//! HTTP inbound adapter framing and adapter-event mapping.
//!
//! The HTTP adapter is the local control-plane boundary for processes that want
//! to post JSON control messages to a running missive gateway instead of using
//! stdin/stdout or a file-drop directory. The gateway owns the HTTP listener;
//! this module owns the schema validation and conversion into generic adapter
//! inbound events.

use std::sync::atomic::{AtomicU64, Ordering};

use missive_core::{AgentAlias, ContextId, EventId, MessageId, Metadata, MissiveError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::stdio::{
    STDIO_FRAME_SCHEMA_VERSION, StdioCommand, StdioFrameSource, StdioInputFrame,
    StdioMessageFields, StdioStreamCommand, StdioTaskCancelCommand, StdioTaskGetCommand,
    StdioTaskListCommand, StdioTaskWaitCommand,
};
use crate::{
    Adapter, AdapterAcknowledgement, AdapterContext, AdapterDefinition, AdapterEvent,
    AdapterExternalIdentity, AdapterIdentity, AdapterInboundMessage, AdapterInboundPayload,
    AdapterLifecycleEvent, AdapterLifecycleState, AdapterOutboundUpdate,
};

/// Built-in adapter kind for local HTTP control messages.
pub const HTTP_ADAPTER_KIND: &str = "http";

/// Stable schema marker for HTTP adapter request frames.
pub const HTTP_ADAPTER_FRAME_SCHEMA_VERSION: &str = "missive.http.v1";

const DEFAULT_SOURCE_ID: &str = "http";
const DEFAULT_RESUME_NAME: &str = "default";
const MAX_HTTP_ID_BYTES: usize = 128;

static EVENT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Source/session hints attached to one HTTP control request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HttpFrameSource {
    /// Stable source id for session continuity. Defaults to `http`.
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

impl Default for HttpFrameSource {
    fn default() -> Self {
        Self {
            source_id: default_source_id(),
            display_name: None,
            resume_name: default_resume_name(),
            profile: None,
        }
    }
}

impl From<HttpFrameSource> for StdioFrameSource {
    fn from(value: HttpFrameSource) -> Self {
        Self {
            source_id: value.source_id,
            display_name: value.display_name,
            resume_name: value.resume_name,
            profile: value.profile,
        }
    }
}

/// Commands accepted by HTTP control request frames.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum HttpControlCommand {
    /// Submit a non-streaming send intent to the gateway adapter bus.
    Send(StdioMessageFields),
    /// Submit a streaming send intent to the gateway adapter bus.
    Stream(StdioStreamCommand),
    /// Submit a task get intent to the gateway adapter bus.
    TaskGet(StdioTaskGetCommand),
    /// Submit a task list intent to the gateway adapter bus.
    TaskList(StdioTaskListCommand),
    /// Submit a task wait intent to the gateway adapter bus.
    TaskWait(StdioTaskWaitCommand),
    /// Submit a task cancel intent to the gateway adapter bus.
    TaskCancel(StdioTaskCancelCommand),
}

impl HttpControlCommand {
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

    /// Converts this command into the stdio-compatible control model.
    #[must_use]
    pub fn to_stdio_command(&self) -> StdioCommand {
        match self {
            Self::Send(command) => StdioCommand::Send(command.clone()),
            Self::Stream(command) => StdioCommand::Stream(command.clone()),
            Self::TaskGet(command) => StdioCommand::TaskGet(command.clone()),
            Self::TaskList(command) => StdioCommand::TaskList(command.clone()),
            Self::TaskWait(command) => StdioCommand::TaskWait(command.clone()),
            Self::TaskCancel(command) => StdioCommand::TaskCancel(command.clone()),
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
}

/// One HTTP control request body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HttpInputFrame {
    /// Stable frame schema marker.
    #[serde(default = "default_http_schema_version")]
    pub schema_version: String,
    /// Request/correlation id supplied by the caller.
    pub id: String,
    /// Source/session hints.
    #[serde(default)]
    pub source: HttpFrameSource,
    /// Command to send to the gateway adapter bus.
    #[serde(flatten)]
    pub command: HttpControlCommand,
    /// Non-secret frame metadata.
    #[serde(default)]
    pub metadata: Metadata,
}

impl HttpInputFrame {
    /// Parses and validates one JSON HTTP control frame.
    pub fn from_json_str(input: &str) -> Result<Self> {
        let frame = serde_json::from_str::<Self>(input).map_err(|error| {
            MissiveError::validation("failed to parse HTTP adapter request as JSON")
                .with_source(error)
                .with_help(
                    "Send one JSON object with schema_version, id, command, and command fields.",
                )
        })?;
        frame.validate()?;
        Ok(frame)
    }

    /// Parses and validates one request frame from a JSON value.
    pub fn from_value(value: Value) -> Result<Self> {
        let frame = serde_json::from_value::<Self>(value).map_err(|error| {
            MissiveError::validation("failed to parse HTTP adapter request")
                .with_source(error)
                .with_help("Use schema_version, id, command, and command-specific fields.")
        })?;
        frame.validate()?;
        Ok(frame)
    }

    /// Validates the schema marker, id, source, and command fields.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != HTTP_ADAPTER_FRAME_SCHEMA_VERSION {
            return Err(MissiveError::validation(format!(
                "unsupported HTTP adapter schema_version {:?}",
                self.schema_version
            ))
            .with_help(format!(
                "Use schema_version {HTTP_ADAPTER_FRAME_SCHEMA_VERSION:?}."
            )));
        }
        validate_http_id(&self.id)?;
        AdapterExternalIdentity::new(self.source.source_id.clone())?;
        if self.source.resume_name.trim().is_empty() {
            return Err(MissiveError::validation(
                "HTTP adapter source.resume_name cannot be empty",
            ));
        }
        self.to_stdio_frame()?.validate()
    }

    /// Converts this request into the stdio-compatible command model used by
    /// existing foreground adapters and command validation.
    pub fn to_stdio_frame(&self) -> Result<StdioInputFrame> {
        Ok(StdioInputFrame {
            schema_version: STDIO_FRAME_SCHEMA_VERSION.to_owned(),
            id: self.id.clone(),
            source: self.source.clone().into(),
            command: self.command.to_stdio_command(),
            metadata: self.metadata.clone(),
        })
    }
}

/// Minimal HTTP adapter implementation for the shared adapter registry.
#[derive(Debug, Clone)]
pub struct HttpAdapter {
    definition: AdapterDefinition,
    started: bool,
    stopped: bool,
    delivered_updates: Vec<AdapterOutboundUpdate>,
    acknowledgements: Vec<AdapterAcknowledgement>,
}

impl HttpAdapter {
    /// Creates an HTTP adapter instance for one definition.
    pub fn new(definition: AdapterDefinition) -> Result<Self> {
        if definition.kind != HTTP_ADAPTER_KIND {
            return Err(MissiveError::config(format!(
                "HTTP adapter cannot be created for adapter kind {:?}",
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

    /// Maps one validated HTTP request frame into a generic adapter inbound message.
    pub fn inbound_message_from_frame(
        &self,
        frame: &HttpInputFrame,
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
            MessageId::new(format!("msg/http/{}", frame.id))?,
            identity,
            session,
            payload,
        )?;
        message.metadata = frame.metadata.clone();
        message
            .metadata
            .insert_str("missive.http.command", frame.command.as_str())?;
        message
            .metadata
            .insert_str("missive.http.frame_id", frame.id.clone())?;
        Ok(message)
    }

    /// Emits one frame as an adapter inbound-message event through a running context.
    pub fn emit_frame(&self, context: &AdapterContext, frame: &HttpInputFrame) -> Result<()> {
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

impl Adapter for HttpAdapter {
    fn definition(&self) -> &AdapterDefinition {
        &self.definition
    }

    fn start(&mut self, context: AdapterContext) -> Result<()> {
        context.emit(AdapterEvent::lifecycle(AdapterLifecycleEvent::new(
            context.definition(),
            AdapterLifecycleState::Running,
            "HTTP adapter ready to accept JSON control requests",
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
            HTTP_ADAPTER_KIND,
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
                "HTTP adapter {:?} cannot deliver update for adapter {:?}",
                self.definition.name, update.adapter_name
            )));
        }
        self.delivered_updates.push(update);
        Ok(())
    }

    fn acknowledge(&mut self, acknowledgement: AdapterAcknowledgement) -> Result<()> {
        if acknowledgement.adapter_name != self.definition.name {
            return Err(MissiveError::validation(format!(
                "HTTP adapter {:?} cannot acknowledge message for adapter {:?}",
                self.definition.name, acknowledgement.adapter_name
            )));
        }
        self.acknowledgements.push(acknowledgement);
        Ok(())
    }
}

/// Registers the built-in HTTP adapter factory in an adapter registry.
pub fn register_http_adapter(registry: &mut crate::AdapterRegistry) -> Result<()> {
    registry.register_fn(HTTP_ADAPTER_KIND, |definition| {
        Ok(Box::new(HttpAdapter::new(definition)?))
    })
}

fn inbound_payload_for_command(command: &HttpControlCommand) -> Result<AdapterInboundPayload> {
    match command {
        HttpControlCommand::Send(message) => {
            command_payload_from_message(command.as_str(), message)
        }
        HttpControlCommand::Stream(stream) => {
            command_payload_from_message(command.as_str(), &stream.message)
        }
        HttpControlCommand::TaskGet(_)
        | HttpControlCommand::TaskList(_)
        | HttpControlCommand::TaskWait(_)
        | HttpControlCommand::TaskCancel(_) => Ok(AdapterInboundPayload::json(json!({
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

fn validate_http_id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.chars().any(char::is_whitespace)
        || value.chars().any(char::is_control)
    {
        return Err(MissiveError::validation(
            "HTTP adapter id must be non-empty and cannot contain whitespace or control characters",
        ));
    }
    if value.len() > MAX_HTTP_ID_BYTES {
        return Err(MissiveError::validation(format!(
            "HTTP adapter id cannot exceed {MAX_HTTP_ID_BYTES} bytes"
        )));
    }
    Ok(())
}

fn default_source_id() -> String {
    DEFAULT_SOURCE_ID.to_owned()
}

fn default_resume_name() -> String {
    DEFAULT_RESUME_NAME.to_owned()
}

fn default_http_schema_version() -> String {
    HTTP_ADAPTER_FRAME_SCHEMA_VERSION.to_owned()
}

/// Returns a best-effort process-unique event id for HTTP adapter lifecycle frames.
pub fn new_http_event_id() -> Result<EventId> {
    let sequence = EVENT_COUNTER.fetch_add(1, Ordering::Relaxed);
    EventId::new(format!("evt/http/{}/{}", std::process::id(), sequence))
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
    fn valid_http_send_maps_to_inbound_message() -> Result<()> {
        let frame = HttpInputFrame::from_json_str(
            r#"{
                "schema_version":"missive.http.v1",
                "id":"http-1",
                "source":{"source_id":"agent-42","display_name":"Agent 42","resume_name":"work"},
                "command":"send",
                "agent":"echo",
                "message":"hello from HTTP"
            }"#,
        )?;
        let definition = AdapterDefinition::new("http", HTTP_ADAPTER_KIND)?;
        let adapter = HttpAdapter::new(definition)?;
        let message = adapter.inbound_message_from_frame(&frame)?;

        assert_eq!(message.adapter_name, "http");
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
            message.metadata.get_str("missive.http.command"),
            Some("send")
        );
        assert_eq!(
            message.payload,
            AdapterInboundPayload::text("hello from HTTP")
        );
        Ok(())
    }

    #[test]
    fn invalid_http_frame_is_validation_error() {
        let error = HttpInputFrame::from_json_str(
            r#"{"schema_version":"wrong","id":"req-1","command":"task_list"}"#,
        )
        .expect_err("wrong schema should fail");

        assert!(error.message().contains("unsupported HTTP adapter"));
    }

    #[test]
    fn registry_can_create_http_adapter_and_emit_frame() -> Result<()> {
        let mut registry = AdapterRegistry::new();
        register_http_adapter(&mut registry)?;
        let definition = AdapterDefinition::new("http", HTTP_ADAPTER_KIND)?;
        let mut adapter = registry.create(&definition)?;
        let sink = Arc::new(RecordingSink::default());
        let context = AdapterContext::new(definition.clone(), sink.clone());
        adapter.start(context.clone())?;

        let frame = HttpInputFrame::from_value(json!({
            "id": "req-list",
            "command": "task_list",
            "source": {"source_id": "runner"}
        }))?;
        let http = HttpAdapter::new(definition)?;
        http.emit_frame(&context, &frame)?;

        let events = sink.events();
        assert!(matches!(events[0], AdapterEvent::Lifecycle(_)));
        assert!(matches!(events[1], AdapterEvent::InboundMessage(_)));
        Ok(())
    }
}
