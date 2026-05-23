#![doc = "Adapter contracts for missive ingress and egress."]

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use missive_core::config::AdapterConfig;
use missive_core::{AgentAlias, ContextId, EventId, MessageId, Metadata, MissiveError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod external_chat;
pub mod file_drop;
pub mod http;
pub mod stdio;

pub use external_chat::{
    DISCORD_ADAPTER_KIND, EMAIL_ADAPTER_KIND, ExternalChatPlatform, ExternalChatPlatformInfo,
    ExternalChatStubAdapter, MATRIX_ADAPTER_KIND, SLACK_ADAPTER_KIND, TELEGRAM_ADAPTER_KIND,
    enabled_external_chat_stub_platforms, register_external_chat_adapter_stubs,
};
pub use file_drop::{
    FILE_DROP_ADAPTER_KIND, FILE_DROP_FRAME_SCHEMA_VERSION, FILE_DROP_OUTPUT_KIND_ERROR,
    FILE_DROP_OUTPUT_KIND_RESULT, FileDropAdapter, FileDropClaim, FileDropCommand,
    FileDropInputFile, FileDropJobCancelCommand, FileDropJobListCommand, FileDropJobOptions,
    FileDropJobShowCommand, FileDropJobStartReduceCommand, FileDropJobStartSendCommand,
    FileDropJobStartStreamCommand, FileDropJobStartWaitCommand, FileDropOutputFile, FileDropPaths,
    FileDropSource, is_ready_file_name, new_file_drop_event_id, register_file_drop_adapter,
};
pub use http::{
    HTTP_ADAPTER_FRAME_SCHEMA_VERSION, HTTP_ADAPTER_KIND, HttpAdapter, HttpControlCommand,
    HttpFrameSource, HttpInputFrame, new_http_event_id, register_http_adapter,
};
pub use stdio::{
    STDIO_ADAPTER_KIND, STDIO_FRAME_SCHEMA_VERSION, STDIO_OUTPUT_KIND_COMMAND_OUTPUT,
    STDIO_OUTPUT_KIND_ERROR, STDIO_OUTPUT_KIND_LIFECYCLE, StdioAdapter, StdioCommand,
    StdioFrameSource, StdioFraming, StdioInputFrame, StdioMessageFields, StdioOutputFrame,
    StdioRunMode, StdioStreamCommand, StdioTaskCancelCommand, StdioTaskGetCommand,
    StdioTaskListCommand, StdioTaskWaitCommand, read_ndjson_frames, read_single_frame,
    register_stdio_adapter, write_output_frame,
};

/// Event type used when an adapter sends user/source input into the gateway.
pub const ADAPTER_EVENT_TYPE_INBOUND_MESSAGE: &str = "missive.adapter.inbound_message";

/// Event type used when an adapter reports acknowledgement state.
pub const ADAPTER_EVENT_TYPE_ACKNOWLEDGEMENT: &str = "missive.adapter.acknowledgement";

/// Event type used when an adapter reports lifecycle state.
pub const ADAPTER_EVENT_TYPE_LIFECYCLE: &str = "missive.adapter.lifecycle";

const ADAPTER_IDENTIFIER_MAX_BYTES: usize = 63;
const EXTERNAL_IDENTIFIER_MAX_BYTES: usize = 256;
const IDENTIFIER_HELP: &str =
    "Use lowercase ASCII letters or digits, with '-', '_' or '.' only in the middle.";
const EXTERNAL_IDENTIFIER_HELP: &str =
    "Use a non-empty adapter identity without whitespace or control characters.";

/// Configured adapter definition derived from `missive.config.v1`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterDefinition {
    /// Configured adapter name, for example `stdio` or `local-dropbox`.
    pub name: String,
    /// Adapter kind resolved through [`AdapterRegistry`], for example `stdio`.
    pub kind: String,
    /// Whether this adapter should be started by gateway adapter workers.
    pub enabled: bool,
    /// Optional profile selected for sessions/messages entering this adapter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_profile: Option<String>,
    /// Adapter-specific non-secret settings from configuration.
    pub settings: Metadata,
}

impl AdapterDefinition {
    /// Creates a validated enabled adapter definition.
    pub fn new(name: impl Into<String>, kind: impl Into<String>) -> Result<Self> {
        let name = name.into();
        let kind = kind.into();
        validate_adapter_identifier("adapter name", &name)?;
        validate_adapter_identifier("adapter kind", &kind)?;
        Ok(Self {
            name,
            kind,
            enabled: true,
            session_profile: None,
            settings: Metadata::new(),
        })
    }

    /// Builds a definition from one validated core config entry.
    pub fn from_config(name: impl Into<String>, config: &AdapterConfig) -> Result<Self> {
        let name = name.into();
        validate_adapter_identifier("adapter name", &name)?;
        validate_adapter_identifier("adapter kind", &config.kind)?;
        if let Some(profile) = &config.session_profile {
            validate_adapter_identifier("adapter session profile", profile)?;
        }
        Ok(Self {
            name,
            kind: config.kind.clone(),
            enabled: config.enabled,
            session_profile: config.session_profile.clone(),
            settings: config.settings.clone(),
        })
    }

    /// Returns the event-source label used by gateway/event journal producers.
    #[must_use]
    pub fn event_source(&self) -> String {
        format!("adapter:{}", self.name)
    }
}

/// External identity as reported by an adapter-specific platform.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterExternalIdentity {
    /// Platform user/source id before missive-specific normalization.
    pub provider_user_id: String,
    /// Optional platform channel/room/source id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_channel_id: Option<String>,
    /// Optional display name for human diagnostics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Non-secret platform metadata.
    pub metadata: Metadata,
}

impl AdapterExternalIdentity {
    /// Creates a provider identity with a required user/source id.
    pub fn new(provider_user_id: impl Into<String>) -> Result<Self> {
        let provider_user_id = provider_user_id.into();
        validate_external_identifier("adapter provider user id", &provider_user_id)?;
        Ok(Self {
            provider_user_id,
            provider_channel_id: None,
            display_name: None,
            metadata: Metadata::new(),
        })
    }

    /// Adds an optional provider channel id.
    pub fn with_channel_id(mut self, provider_channel_id: impl Into<String>) -> Result<Self> {
        let provider_channel_id = provider_channel_id.into();
        validate_external_identifier("adapter provider channel id", &provider_channel_id)?;
        self.provider_channel_id = Some(provider_channel_id);
        Ok(self)
    }

    /// Adds an optional human display name.
    #[must_use]
    pub fn with_display_name(mut self, display_name: impl Into<String>) -> Self {
        self.display_name = Some(display_name.into());
        self
    }
}

/// Normalized source identity used by sessions, busy-input policy, and events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterIdentity {
    /// Configured adapter name that produced the identity.
    pub adapter_name: String,
    /// Adapter/source kind, usually matching the adapter kind.
    pub source_kind: String,
    /// Stable source id after adapter-specific normalization.
    pub source_id: String,
    /// Optional human display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Non-secret identity metadata.
    pub metadata: Metadata,
}

impl AdapterIdentity {
    /// Creates a normalized adapter identity.
    pub fn new(
        adapter_name: impl Into<String>,
        source_kind: impl Into<String>,
        source_id: impl Into<String>,
    ) -> Result<Self> {
        let adapter_name = adapter_name.into();
        let source_kind = source_kind.into();
        let source_id = source_id.into();
        validate_adapter_identifier("adapter identity adapter name", &adapter_name)?;
        validate_adapter_identifier("adapter identity source kind", &source_kind)?;
        validate_external_identifier("adapter identity source id", &source_id)?;
        Ok(Self {
            adapter_name,
            source_kind,
            source_id,
            display_name: None,
            metadata: Metadata::new(),
        })
    }

    /// Adds an optional human display name.
    #[must_use]
    pub fn with_display_name(mut self, display_name: impl Into<String>) -> Self {
        self.display_name = Some(display_name.into());
        self
    }
}

/// Session routing hint attached to inbound messages and outbound updates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterSession {
    /// Resume name used by gateway session persistence.
    pub resume_name: String,
    /// Optional profile override for this session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Optional target agent alias.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_agent: Option<AgentAlias>,
    /// Optional linked A2A context id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<ContextId>,
    /// Non-secret session metadata.
    pub metadata: Metadata,
}

impl AdapterSession {
    /// Creates a session routing hint with a validated resume name.
    pub fn new(resume_name: impl Into<String>) -> Result<Self> {
        let resume_name = resume_name.into();
        validate_adapter_identifier("adapter session resume name", &resume_name)?;
        Ok(Self {
            resume_name,
            profile: None,
            target_agent: None,
            context_id: None,
            metadata: Metadata::new(),
        })
    }
}

/// Payload carried by an inbound adapter message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AdapterInboundPayload {
    /// Plain text or text-like input.
    Text {
        /// Text content to convert into A2A message parts later.
        text: String,
        /// Optional MIME type such as `text/plain`.
        #[serde(skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
    },
    /// Structured JSON input.
    Json {
        /// JSON value supplied by the adapter source.
        value: Value,
    },
}

impl AdapterInboundPayload {
    /// Creates a text payload.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            mime_type: Some("text/plain".to_owned()),
        }
    }

    /// Creates a structured JSON payload.
    #[must_use]
    pub fn json(value: impl Into<Value>) -> Self {
        Self::Json {
            value: value.into(),
        }
    }
}

/// Message emitted by an adapter into the gateway.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterInboundMessage {
    /// Configured adapter name.
    pub adapter_name: String,
    /// Stable message id supplied by the adapter or generated by a worker.
    pub message_id: MessageId,
    /// Normalized source identity.
    pub identity: AdapterIdentity,
    /// Session routing information.
    pub session: AdapterSession,
    /// Message payload.
    pub payload: AdapterInboundPayload,
    /// Non-secret message metadata.
    pub metadata: Metadata,
}

impl AdapterInboundMessage {
    /// Creates an inbound adapter message.
    pub fn new(
        adapter_name: impl Into<String>,
        message_id: MessageId,
        identity: AdapterIdentity,
        session: AdapterSession,
        payload: AdapterInboundPayload,
    ) -> Result<Self> {
        let adapter_name = adapter_name.into();
        validate_adapter_identifier("adapter inbound message adapter name", &adapter_name)?;
        if identity.adapter_name != adapter_name {
            return Err(MissiveError::validation(format!(
                "inbound adapter message from {adapter_name:?} cannot use identity from adapter {:?}",
                identity.adapter_name
            ))
            .with_help("Map the external identity with the same adapter that emits the message."));
        }
        Ok(Self {
            adapter_name,
            message_id,
            identity,
            session,
            payload,
            metadata: Metadata::new(),
        })
    }
}

/// Acknowledgement states adapters can report for inbound/outbound delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterAcknowledgementStatus {
    /// The adapter accepted the source message for gateway processing.
    Accepted,
    /// The adapter rejected the source message before gateway processing.
    Rejected,
    /// An outbound update was delivered to the source platform.
    Delivered,
    /// Delivery or processing failed.
    Failed,
}

impl AdapterAcknowledgementStatus {
    /// Stable lowercase label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Delivered => "delivered",
            Self::Failed => "failed",
        }
    }
}

/// Adapter acknowledgement event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterAcknowledgement {
    /// Configured adapter name.
    pub adapter_name: String,
    /// Message being acknowledged.
    pub message_id: MessageId,
    /// Acknowledgement state.
    pub status: AdapterAcknowledgementStatus,
    /// Optional human-safe detail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Non-secret acknowledgement metadata.
    pub metadata: Metadata,
}

impl AdapterAcknowledgement {
    /// Creates an acknowledgement event.
    pub fn new(
        adapter_name: impl Into<String>,
        message_id: MessageId,
        status: AdapterAcknowledgementStatus,
    ) -> Result<Self> {
        let adapter_name = adapter_name.into();
        validate_adapter_identifier("adapter acknowledgement adapter name", &adapter_name)?;
        Ok(Self {
            adapter_name,
            message_id,
            status,
            detail: None,
            metadata: Metadata::new(),
        })
    }
}

/// Outbound update kinds the gateway can ask an adapter to render later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterOutboundUpdateKind {
    /// Human/status progress update.
    Status,
    /// Message response update.
    Message,
    /// Artifact availability/update notification.
    Artifact,
    /// Terminal completion update.
    Completed,
    /// Error update.
    Error,
}

impl AdapterOutboundUpdateKind {
    /// Stable lowercase label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Message => "message",
            Self::Artifact => "artifact",
            Self::Completed => "completed",
            Self::Error => "error",
        }
    }
}

/// Update delivered from gateway/session/job code to an adapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterOutboundUpdate {
    /// Configured adapter name.
    pub adapter_name: String,
    /// Stable update id.
    pub update_id: EventId,
    /// Optional source message correlation id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<MessageId>,
    /// Target source identity.
    pub recipient: AdapterIdentity,
    /// Session routing information.
    pub session: AdapterSession,
    /// Update kind.
    pub kind: AdapterOutboundUpdateKind,
    /// Adapter-renderable redacted payload.
    pub payload: Value,
    /// Non-secret update metadata.
    pub metadata: Metadata,
}

impl AdapterOutboundUpdate {
    /// Creates an outbound update for an adapter recipient.
    pub fn new(
        adapter_name: impl Into<String>,
        update_id: EventId,
        recipient: AdapterIdentity,
        session: AdapterSession,
        kind: AdapterOutboundUpdateKind,
        payload: impl Into<Value>,
    ) -> Result<Self> {
        let adapter_name = adapter_name.into();
        validate_adapter_identifier("adapter outbound update adapter name", &adapter_name)?;
        if recipient.adapter_name != adapter_name {
            return Err(MissiveError::validation(format!(
                "outbound adapter update for {adapter_name:?} cannot use recipient from adapter {:?}",
                recipient.adapter_name
            ))
            .with_help("Deliver updates only to identities mapped by the same adapter."));
        }
        Ok(Self {
            adapter_name,
            update_id,
            correlation_id: None,
            recipient,
            session,
            kind,
            payload: payload.into(),
            metadata: Metadata::new(),
        })
    }
}

/// Adapter lifecycle states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterLifecycleState {
    /// Adapter is starting.
    Starting,
    /// Adapter is running.
    Running,
    /// Adapter stopped cleanly.
    Stopped,
    /// Adapter failed.
    Failed,
}

impl AdapterLifecycleState {
    /// Stable lowercase label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }
}

/// Lifecycle event emitted by an adapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterLifecycleEvent {
    /// Configured adapter name.
    pub adapter_name: String,
    /// Adapter kind.
    pub kind: String,
    /// Lifecycle state.
    pub state: AdapterLifecycleState,
    /// Human-safe lifecycle detail.
    pub detail: String,
    /// Non-secret lifecycle metadata.
    pub metadata: Metadata,
}

impl AdapterLifecycleEvent {
    /// Creates a lifecycle event.
    pub fn new(
        definition: &AdapterDefinition,
        state: AdapterLifecycleState,
        detail: impl Into<String>,
    ) -> Result<Self> {
        Ok(Self {
            adapter_name: definition.name.clone(),
            kind: definition.kind.clone(),
            state,
            detail: detail.into(),
            metadata: Metadata::new(),
        })
    }
}

/// Runtime event emitted by adapters into the gateway event bus.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "adapter_event", rename_all = "snake_case")]
pub enum AdapterEvent {
    /// Source/user input received by an adapter.
    InboundMessage(Box<AdapterInboundMessage>),
    /// Adapter acknowledgement state.
    Acknowledgement(AdapterAcknowledgement),
    /// Adapter lifecycle state.
    Lifecycle(AdapterLifecycleEvent),
}

impl AdapterEvent {
    /// Creates an inbound-message adapter event.
    #[must_use]
    pub fn inbound_message(message: AdapterInboundMessage) -> Self {
        Self::InboundMessage(Box::new(message))
    }

    /// Creates an acknowledgement adapter event.
    #[must_use]
    pub const fn acknowledgement(acknowledgement: AdapterAcknowledgement) -> Self {
        Self::Acknowledgement(acknowledgement)
    }

    /// Creates a lifecycle adapter event.
    #[must_use]
    pub const fn lifecycle(lifecycle: AdapterLifecycleEvent) -> Self {
        Self::Lifecycle(lifecycle)
    }

    /// Configured adapter name for this event.
    #[must_use]
    pub fn adapter_name(&self) -> &str {
        match self {
            Self::InboundMessage(message) => &message.adapter_name,
            Self::Acknowledgement(acknowledgement) => &acknowledgement.adapter_name,
            Self::Lifecycle(lifecycle) => &lifecycle.adapter_name,
        }
    }

    /// Stable event type name for journals and gateway runtime output.
    #[must_use]
    pub const fn event_type(&self) -> &'static str {
        match self {
            Self::InboundMessage(_) => ADAPTER_EVENT_TYPE_INBOUND_MESSAGE,
            Self::Acknowledgement(_) => ADAPTER_EVENT_TYPE_ACKNOWLEDGEMENT,
            Self::Lifecycle(_) => ADAPTER_EVENT_TYPE_LIFECYCLE,
        }
    }

    /// Human-safe summary line.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::InboundMessage(message) => format!(
                "Adapter {} received inbound message {} from {}",
                message.adapter_name, message.message_id, message.identity.source_id
            ),
            Self::Acknowledgement(acknowledgement) => format!(
                "Adapter {} acknowledgement for {} is {}",
                acknowledgement.adapter_name,
                acknowledgement.message_id,
                acknowledgement.status.as_str()
            ),
            Self::Lifecycle(lifecycle) => format!(
                "Adapter {} ({}) is {}: {}",
                lifecycle.adapter_name,
                lifecycle.kind,
                lifecycle.state.as_str(),
                lifecycle.detail
            ),
        }
    }
}

/// Sink used by adapters to emit runtime events without depending on the gateway crate.
pub trait AdapterEventSink: Send + Sync {
    /// Emits one adapter runtime event.
    fn emit(&self, event: AdapterEvent) -> Result<()>;
}

impl<T> AdapterEventSink for Arc<T>
where
    T: AdapterEventSink + ?Sized,
{
    fn emit(&self, event: AdapterEvent) -> Result<()> {
        self.as_ref().emit(event)
    }
}

/// Context passed to an adapter when it starts.
#[derive(Clone)]
pub struct AdapterContext {
    definition: AdapterDefinition,
    event_sink: Arc<dyn AdapterEventSink>,
}

impl AdapterContext {
    /// Creates an adapter start context.
    #[must_use]
    pub fn new(definition: AdapterDefinition, event_sink: Arc<dyn AdapterEventSink>) -> Self {
        Self {
            definition,
            event_sink,
        }
    }

    /// Returns the configured adapter definition.
    #[must_use]
    pub const fn definition(&self) -> &AdapterDefinition {
        &self.definition
    }

    /// Emits one event through the configured sink.
    pub fn emit(&self, event: AdapterEvent) -> Result<()> {
        let event_type = event.event_type();
        let event_adapter_name = event.adapter_name().to_owned();
        let span = tracing::debug_span!(
            target: "missive_adapters",
            "adapter.event",
            adapter_name = %self.definition.name,
            adapter_kind = %self.definition.kind,
            event_type,
            event_adapter_name = %event_adapter_name,
        );
        let _span_guard = span.enter();
        tracing::debug!(
            target: "missive_adapters",
            adapter_name = %self.definition.name,
            adapter_kind = %self.definition.kind,
            event_type,
            "adapter event emit started"
        );
        let result = if event_adapter_name != self.definition.name {
            Err(MissiveError::validation(format!(
                "adapter context for {:?} cannot emit event from {:?}",
                self.definition.name, event_adapter_name
            ))
            .with_help("Use the adapter definition name when building adapter events."))
        } else {
            self.event_sink.emit(event)
        };
        match &result {
            Ok(()) => tracing::debug!(
                target: "missive_adapters",
                result = "ok",
                "adapter event emitted"
            ),
            Err(error) => tracing::debug!(
                target: "missive_adapters",
                result = "error",
                error = %error,
                exit_code = error.exit_code().as_i32(),
                "adapter event emit failed"
            ),
        }
        result
    }
}

impl fmt::Debug for AdapterContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdapterContext")
            .field("definition", &self.definition)
            .field("event_sink", &"<adapter event sink>")
            .finish()
    }
}

/// Common adapter interface for inbound messages, outbound updates, identities,
/// sessions, and acknowledgements.
pub trait Adapter: Send {
    /// Returns the configured adapter definition.
    fn definition(&self) -> &AdapterDefinition;

    /// Starts the adapter and gives it a gateway event sink.
    fn start(&mut self, context: AdapterContext) -> Result<()>;

    /// Stops the adapter gracefully.
    fn stop(&mut self) -> Result<()>;

    /// Maps an external platform identity into missive's normalized identity model.
    fn map_identity(&self, external: AdapterExternalIdentity) -> Result<AdapterIdentity>;

    /// Delivers a gateway/session/task update to the adapter's external source.
    fn deliver_update(&mut self, update: AdapterOutboundUpdate) -> Result<()>;

    /// Records or sends an acknowledgement for inbound/outbound delivery state.
    fn acknowledge(&mut self, acknowledgement: AdapterAcknowledgement) -> Result<()>;
}

/// Factory used by [`AdapterRegistry`] to create adapter instances by kind.
pub trait AdapterFactory: Send + Sync {
    /// Adapter kind this factory handles.
    fn kind(&self) -> &str;

    /// Creates one adapter instance for a configured definition.
    fn create(&self, definition: AdapterDefinition) -> Result<Box<dyn Adapter>>;
}

/// Function-backed adapter factory for tests and lightweight built-in adapters.
pub struct FnAdapterFactory<F> {
    kind: String,
    factory: F,
}

impl<F> FnAdapterFactory<F> {
    /// Creates a function-backed adapter factory.
    pub fn new(kind: impl Into<String>, factory: F) -> Result<Self> {
        let kind = kind.into();
        validate_adapter_identifier("adapter factory kind", &kind)?;
        Ok(Self { kind, factory })
    }
}

impl<F> fmt::Debug for FnAdapterFactory<F> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FnAdapterFactory")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl<F> AdapterFactory for FnAdapterFactory<F>
where
    F: Fn(AdapterDefinition) -> Result<Box<dyn Adapter>> + Send + Sync,
{
    fn kind(&self) -> &str {
        &self.kind
    }

    fn create(&self, definition: AdapterDefinition) -> Result<Box<dyn Adapter>> {
        (self.factory)(definition)
    }
}

/// Registry of adapter factories keyed by adapter kind.
#[derive(Default)]
pub struct AdapterRegistry {
    factories: BTreeMap<String, Box<dyn AdapterFactory>>,
}

impl AdapterRegistry {
    /// Creates an empty adapter registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a factory. Duplicate kinds are rejected.
    pub fn register_factory(&mut self, factory: impl AdapterFactory + 'static) -> Result<()> {
        let kind = factory.kind().to_owned();
        let span = tracing::debug_span!(
            target: "missive_adapters",
            "adapter.registry",
            operation = "register_factory",
            adapter_kind = %kind,
        );
        let _span_guard = span.enter();
        tracing::debug!(target: "missive_adapters", adapter_kind = %kind, "adapter factory registration started");
        let result = (|| {
            validate_adapter_identifier("adapter factory kind", &kind)?;
            if self.factories.contains_key(&kind) {
                return Err(MissiveError::config(format!(
                    "adapter factory for kind {kind:?} is already registered"
                ))
                .with_help("Register each adapter kind once per gateway process."));
            }
            self.factories.insert(kind.clone(), Box::new(factory));
            Ok(())
        })();
        match &result {
            Ok(()) => {
                tracing::debug!(target: "missive_adapters", result = "ok", "adapter factory registered")
            }
            Err(error) => {
                tracing::debug!(target: "missive_adapters", result = "error", error = %error, exit_code = error.exit_code().as_i32(), "adapter factory registration failed")
            }
        }
        result
    }

    /// Registers a function-backed factory.
    pub fn register_fn<F>(&mut self, kind: impl Into<String>, factory: F) -> Result<()>
    where
        F: Fn(AdapterDefinition) -> Result<Box<dyn Adapter>> + Send + Sync + 'static,
    {
        self.register_factory(FnAdapterFactory::new(kind, factory)?)
    }

    /// Returns whether a factory exists for `kind`.
    #[must_use]
    pub fn contains_kind(&self, kind: &str) -> bool {
        self.factories.contains_key(kind)
    }

    /// Returns registered kinds in deterministic order.
    #[must_use]
    pub fn kinds(&self) -> Vec<&str> {
        self.factories.keys().map(String::as_str).collect()
    }

    /// Creates an adapter for one enabled definition.
    pub fn create(&self, definition: &AdapterDefinition) -> Result<Box<dyn Adapter>> {
        let span = tracing::debug_span!(
            target: "missive_adapters",
            "adapter.operation",
            operation = "create",
            adapter_name = %definition.name,
            adapter_kind = %definition.kind,
            enabled = definition.enabled,
        );
        let _span_guard = span.enter();
        tracing::debug!(
            target: "missive_adapters",
            adapter_name = %definition.name,
            adapter_kind = %definition.kind,
            "adapter creation started"
        );
        let result = (|| {
            if !definition.enabled {
                return Err(MissiveError::config(format!(
                    "adapter {:?} is disabled and should not be started",
                    definition.name
                ))
                .with_help("Filter definitions with enabled_adapter_definitions_from_config before starting adapters."));
            }
            let factory = self.factories.get(&definition.kind).ok_or_else(|| {
                MissiveError::config(format!(
                    "no adapter factory registered for kind {:?}",
                    definition.kind
                ))
                .with_help("Install or enable the crate feature that provides this adapter kind.")
            })?;
            factory.create(definition.clone())
        })();
        match &result {
            Ok(_) => tracing::debug!(target: "missive_adapters", result = "ok", "adapter created"),
            Err(error) => {
                tracing::debug!(target: "missive_adapters", result = "error", error = %error, exit_code = error.exit_code().as_i32(), "adapter creation failed")
            }
        }
        result
    }

    /// Builds adapter definitions from a validated core config, including disabled entries.
    pub fn definitions_from_config(
        &self,
        config: &missive_core::MissiveConfig,
    ) -> Result<Vec<AdapterDefinition>> {
        adapter_definitions_from_config(config)
    }

    /// Builds enabled adapter definitions from a validated core config.
    pub fn enabled_definitions_from_config(
        &self,
        config: &missive_core::MissiveConfig,
    ) -> Result<Vec<AdapterDefinition>> {
        enabled_adapter_definitions_from_config(config)
    }
}

impl fmt::Debug for AdapterRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdapterRegistry")
            .field("kinds", &self.kinds())
            .finish()
    }
}

/// Builds adapter definitions from a validated core config, including disabled entries.
pub fn adapter_definitions_from_config(
    config: &missive_core::MissiveConfig,
) -> Result<Vec<AdapterDefinition>> {
    config
        .adapters
        .iter()
        .map(|(name, adapter)| AdapterDefinition::from_config(name.clone(), adapter))
        .collect()
}

/// Builds enabled adapter definitions from a validated core config.
pub fn enabled_adapter_definitions_from_config(
    config: &missive_core::MissiveConfig,
) -> Result<Vec<AdapterDefinition>> {
    adapter_definitions_from_config(config).map(|definitions| {
        definitions
            .into_iter()
            .filter(|definition| definition.enabled)
            .collect()
    })
}

fn validate_adapter_identifier(kind: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return invalid_adapter_identifier(kind, "value cannot be empty");
    }
    if value.len() > ADAPTER_IDENTIFIER_MAX_BYTES {
        return invalid_adapter_identifier(
            kind,
            format!(
                "value is {} bytes, but the maximum is {ADAPTER_IDENTIFIER_MAX_BYTES}",
                value.len()
            ),
        );
    }
    let bytes = value.as_bytes();
    if !is_ascii_lower_alphanumeric(bytes[0]) {
        return invalid_adapter_identifier(
            kind,
            "value must start with a lowercase ASCII letter or digit",
        );
    }
    if !is_ascii_lower_alphanumeric(bytes[bytes.len() - 1]) {
        return invalid_adapter_identifier(
            kind,
            "value must end with a lowercase ASCII letter or digit",
        );
    }
    for byte in bytes {
        if is_ascii_lower_alphanumeric(*byte) || matches!(*byte, b'-' | b'_' | b'.') {
            continue;
        }
        return invalid_adapter_identifier(
            kind,
            "value must contain only lowercase ASCII letters, digits, '-', '_' or '.'",
        );
    }
    Ok(())
}

fn invalid_adapter_identifier(kind: &str, reason: impl Into<String>) -> Result<()> {
    Err(
        MissiveError::validation(format!("invalid {kind}: {}", reason.into()))
            .with_help(IDENTIFIER_HELP),
    )
}

fn validate_external_identifier(kind: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return invalid_external_identifier(kind, "value cannot be empty");
    }
    if value.len() > EXTERNAL_IDENTIFIER_MAX_BYTES {
        return invalid_external_identifier(
            kind,
            format!(
                "value is {} bytes, but the maximum is {EXTERNAL_IDENTIFIER_MAX_BYTES}",
                value.len()
            ),
        );
    }
    if value.chars().any(char::is_whitespace) {
        return invalid_external_identifier(kind, "value cannot contain whitespace");
    }
    if value.chars().any(char::is_control) {
        return invalid_external_identifier(kind, "value cannot contain control characters");
    }
    Ok(())
}

fn invalid_external_identifier(kind: &str, reason: impl Into<String>) -> Result<()> {
    Err(
        MissiveError::validation(format!("invalid {kind}: {}", reason.into()))
            .with_help(EXTERNAL_IDENTIFIER_HELP),
    )
}

const fn is_ascii_lower_alphanumeric(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

/// Cargo package name for this crate.
pub const CRATE_NAME: &str = "missive-adapters";

/// Short description of this crate's target responsibility.
pub const CRATE_PURPOSE: &str =
    "stdin/stdout, file, HTTP adapters, and feature-gated chat adapter stubs";

/// Returns metadata for this crate.
#[must_use]
pub const fn crate_info() -> missive_core::CrateInfo {
    missive_core::CrateInfo::new(CRATE_NAME, CRATE_PURPOSE)
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use missive_core::{ErrorCategory, MissiveConfig};
    use missive_observe::{LogFormat, ObserveConfig, dispatch_with_writer};
    use serde_json::json;
    use tracing_subscriber::fmt::writer::MakeWriter;

    use super::*;

    #[derive(Clone, Default)]
    struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

    impl SharedBuffer {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().expect("buffer lock").clone()).expect("UTF-8 logs")
        }
    }

    struct SharedBufferWriter(Arc<Mutex<Vec<u8>>>);

    impl io::Write for SharedBufferWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("buffer lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> MakeWriter<'writer> for SharedBuffer {
        type Writer = SharedBufferWriter;

        fn make_writer(&'writer self) -> Self::Writer {
            SharedBufferWriter(Arc::clone(&self.0))
        }
    }

    #[derive(Debug, Default)]
    struct RecordingSink {
        events: Mutex<Vec<AdapterEvent>>,
    }

    impl RecordingSink {
        fn events(&self) -> Vec<AdapterEvent> {
            self.events.lock().expect("event mutex").clone()
        }
    }

    impl AdapterEventSink for RecordingSink {
        fn emit(&self, event: AdapterEvent) -> Result<()> {
            self.events.lock().expect("event mutex").push(event);
            Ok(())
        }
    }

    #[derive(Debug)]
    struct FakeAdapter {
        definition: AdapterDefinition,
        started: bool,
        stopped: bool,
        updates: Vec<AdapterOutboundUpdate>,
        acknowledgements: Vec<AdapterAcknowledgement>,
    }

    impl FakeAdapter {
        fn new(definition: AdapterDefinition) -> Self {
            Self {
                definition,
                started: false,
                stopped: false,
                updates: Vec::new(),
                acknowledgements: Vec::new(),
            }
        }
    }

    impl Adapter for FakeAdapter {
        fn definition(&self) -> &AdapterDefinition {
            &self.definition
        }

        fn start(&mut self, context: AdapterContext) -> Result<()> {
            context.emit(AdapterEvent::lifecycle(AdapterLifecycleEvent::new(
                context.definition(),
                AdapterLifecycleState::Running,
                "fake adapter started",
            )?))?;
            let external = AdapterExternalIdentity::new("user-1")?.with_display_name("Test User");
            let identity = self.map_identity(external)?;
            let mut session = AdapterSession::new("default")?;
            session.target_agent = Some(AgentAlias::new("echo")?);
            let message = AdapterInboundMessage::new(
                self.definition.name.clone(),
                MessageId::new("msg-fake-1")?,
                identity,
                session,
                AdapterInboundPayload::text("hello from fake adapter"),
            )?;
            context.emit(AdapterEvent::inbound_message(message))?;
            self.started = true;
            Ok(())
        }

        fn stop(&mut self) -> Result<()> {
            self.stopped = true;
            Ok(())
        }

        fn map_identity(&self, external: AdapterExternalIdentity) -> Result<AdapterIdentity> {
            AdapterIdentity::new(
                self.definition.name.clone(),
                self.definition.kind.clone(),
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
            self.updates.push(update);
            Ok(())
        }

        fn acknowledge(&mut self, acknowledgement: AdapterAcknowledgement) -> Result<()> {
            self.acknowledgements.push(acknowledgement);
            Ok(())
        }
    }

    #[test]
    fn crate_info_describes_adapter_layer() {
        let info = crate_info();

        assert_eq!(info.name(), CRATE_NAME);
        assert!(info.purpose().contains("adapters"));
    }

    #[test]
    fn adapter_registry_and_event_emission_are_traced() {
        let buffer = SharedBuffer::default();
        let dispatch = dispatch_with_writer(
            ObserveConfig::new("debug", LogFormat::Human, false),
            buffer.clone(),
        )
        .expect("dispatch");
        let _ = tracing::dispatcher::set_global_default(dispatch.clone());
        tracing::callsite::rebuild_interest_cache();

        tracing::dispatcher::with_default(&dispatch, || {
            let mut registry = AdapterRegistry::new();
            registry
                .register_fn("fake", |definition| {
                    Ok(Box::new(FakeAdapter::new(definition)))
                })
                .expect("register fake factory");
            let definition = AdapterDefinition::new("fake", "fake").expect("definition");
            let _adapter = registry.create(&definition).expect("create fake adapter");
            let sink = Arc::new(RecordingSink::default());
            let context = AdapterContext::new(definition.clone(), sink);
            context
                .emit(AdapterEvent::lifecycle(
                    AdapterLifecycleEvent::new(
                        &definition,
                        AdapterLifecycleState::Running,
                        "fake adapter started",
                    )
                    .expect("lifecycle"),
                ))
                .expect("emit lifecycle");
        });

        let output = buffer.text();
        assert!(output.contains("span.adapter.registry"), "{output}");
        assert!(output.contains("adapter creation started"), "{output}");
        assert!(output.contains("span.adapter.event"), "{output}");
        assert!(output.contains("adapter_kind=fake"));
        assert!(output.contains("event_type=missive.adapter.lifecycle"));
        assert!(output.contains("adapter event emitted"));
    }

    #[test]
    fn adapter_definitions_are_derived_from_config_schema() {
        let config = MissiveConfig::from_toml_str(
            r#"
schema_version = "missive.config.v1"
default_profile = "default"

[profiles.default]

[adapters.fake]
kind = "fake"
enabled = true
session_profile = "default"

[adapters.fake.settings]
mode = "test"

[adapters.disabled]
kind = "fake"
enabled = false
"#,
        )
        .expect("config should parse");

        let definitions = adapter_definitions_from_config(&config).expect("definitions");
        assert_eq!(definitions.len(), 2);
        assert_eq!(definitions[1].name, "fake");
        assert_eq!(definitions[1].kind, "fake");
        assert_eq!(definitions[1].session_profile.as_deref(), Some("default"));
        assert_eq!(definitions[1].settings.get_str("mode"), Some("test"));

        let enabled = enabled_adapter_definitions_from_config(&config).expect("enabled");
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].name, "fake");
    }

    #[test]
    fn registry_creates_fake_adapter_and_emits_inbound_message() -> Result<()> {
        let mut registry = AdapterRegistry::new();
        registry
            .register_fn("fake", |definition| {
                Ok(Box::new(FakeAdapter::new(definition)))
            })
            .expect("register fake factory");
        assert_eq!(registry.kinds(), vec!["fake"]);

        let definition = AdapterDefinition::new("fake", "fake").expect("definition");
        let mut adapter = registry.create(&definition).expect("create fake adapter");
        assert_eq!(adapter.definition().name, "fake");

        let sink = Arc::new(RecordingSink::default());
        let context = AdapterContext::new(definition.clone(), sink.clone());
        adapter.start(context).expect("start fake adapter");

        let update = AdapterOutboundUpdate::new(
            "fake",
            EventId::new("evt/adapter/fake/update-1")?,
            AdapterIdentity::new("fake", "fake", "user-1")?,
            AdapterSession::new("default")?,
            AdapterOutboundUpdateKind::Status,
            json!({"state": "working"}),
        )
        .expect("outbound update");
        adapter.deliver_update(update).expect("deliver update");
        adapter
            .acknowledge(AdapterAcknowledgement::new(
                "fake",
                MessageId::new("msg-fake-1")?,
                AdapterAcknowledgementStatus::Accepted,
            )?)
            .expect("acknowledge");
        adapter.stop().expect("stop fake adapter");

        let events = sink.events();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], AdapterEvent::Lifecycle(_)));
        match &events[1] {
            AdapterEvent::InboundMessage(message) => {
                assert_eq!(message.adapter_name, "fake");
                assert_eq!(message.identity.source_id, "user-1");
                assert_eq!(
                    message
                        .session
                        .target_agent
                        .as_ref()
                        .map(AgentAlias::as_str),
                    Some("echo")
                );
                assert!(message.payload == AdapterInboundPayload::text("hello from fake adapter"));
            }
            other => panic!("unexpected adapter event: {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn registry_rejects_duplicate_factories_missing_kinds_and_disabled_adapters() {
        let mut registry = AdapterRegistry::new();
        registry
            .register_fn("fake", |definition| {
                Ok(Box::new(FakeAdapter::new(definition)))
            })
            .expect("register fake factory");
        let duplicate = registry
            .register_fn("fake", |definition| {
                Ok(Box::new(FakeAdapter::new(definition)))
            })
            .expect_err("duplicate factory should fail");
        assert_eq!(duplicate.category(), ErrorCategory::Config);

        let missing = match registry
            .create(&AdapterDefinition::new("other", "missing").expect("definition"))
        {
            Ok(_) => panic!("missing factory should fail"),
            Err(error) => error,
        };
        assert_eq!(missing.category(), ErrorCategory::Config);

        let mut disabled = AdapterDefinition::new("fake", "fake").expect("definition");
        disabled.enabled = false;
        let error = match registry.create(&disabled) {
            Ok(_) => panic!("disabled adapter should fail"),
            Err(error) => error,
        };
        assert_eq!(error.category(), ErrorCategory::Config);
    }

    #[test]
    fn adapter_context_rejects_cross_adapter_events() {
        let sink = Arc::new(RecordingSink::default());
        let context = AdapterContext::new(
            AdapterDefinition::new("left", "fake").expect("definition"),
            sink,
        );
        let event = AdapterEvent::lifecycle(
            AdapterLifecycleEvent::new(
                &AdapterDefinition::new("right", "fake").expect("definition"),
                AdapterLifecycleState::Running,
                "wrong adapter",
            )
            .expect("lifecycle"),
        );
        let error = context
            .emit(event)
            .expect_err("wrong adapter event should fail");
        assert_eq!(error.category(), ErrorCategory::Validation);
    }
}
