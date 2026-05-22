#![doc = "A2A protocol integration scaffolding for missive."]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::Duration;

pub use missive_core::{
    METADATA_A2A_EXTENSIONS, METADATA_A2A_PROTOCOL_VERSION, METADATA_A2A_SERVICE_PARAMETERS,
};
use missive_core::{Metadata, MissiveError, Result};
use reqwest::StatusCode;
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{
    ETAG, HeaderMap, HeaderName, HeaderValue, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use url::Url;

/// Official A2A Rust SDK protocol types re-exported behind missive's A2A
/// boundary.
///
/// The upstream Cargo package is `a2a-lf`, whose library crate is named `a2a`.
/// Re-exporting the protocol types from this module keeps downstream missive
/// crates from depending on the upstream SDK directly while still avoiding
/// duplicate Message/Task/Artifact/Agent Card models in this workspace.
pub mod protocol {
    pub use a2a::{
        A2AError, AgentCapabilities, AgentCard, AgentCardSignature, AgentExtension, AgentInterface,
        AgentProvider, AgentSkill, Artifact, AuthenticationInfo, CancelTaskRequest,
        DeleteTaskPushNotificationConfigRequest, GetExtendedAgentCardRequest,
        GetTaskPushNotificationConfigRequest, GetTaskRequest, JsonRpcError, JsonRpcId,
        JsonRpcRequest, JsonRpcResponse, ListTaskPushNotificationConfigsRequest,
        ListTaskPushNotificationConfigsResponse, ListTasksRequest, ListTasksResponse, Message,
        Part, PartContent, ProtocolVersion, Role, SVC_PARAM_EXTENSIONS, SVC_PARAM_VERSION,
        SendMessageConfiguration, SendMessageRequest, SendMessageResponse, SubscribeToTaskRequest,
        TRANSPORT_PROTOCOL_GRPC, TRANSPORT_PROTOCOL_HTTP_JSON, TRANSPORT_PROTOCOL_JSONRPC,
        TRANSPORT_PROTOCOL_SLIMRPC, Task, TaskPushNotificationConfig, TaskState, TaskStatus,
        TransportProtocol, VERSION, error_code, methods as jsonrpc_methods, new_message_id,
    };
}

pub use protocol::{
    AgentCapabilities, AgentCard, AgentExtension, AgentInterface, AgentProvider, AgentSkill,
};

/// Cargo package name for this crate.
pub const CRATE_NAME: &str = "missive-a2a";

/// Short description of this crate's target responsibility.
pub const CRATE_PURPOSE: &str = "A2A protocol/client integration and compatibility fixtures";

/// Public Agent Card discovery path defined by A2A.
pub const PUBLIC_AGENT_CARD_PATH: &str = "/.well-known/agent-card.json";

const DEFAULT_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_SEND_TIMEOUT: Duration = Duration::from_secs(30);
const USER_AGENT: &str = concat!("missive/", env!("CARGO_PKG_VERSION"), " a2a-client");
const A2A_JSON_CONTENT_TYPE: &str = "application/a2a+json";
const SEND_MESSAGE_REST_PATH: &str = "message:send";

/// Canonical missive name for the A2A HTTP+JSON protocol binding.
pub const HTTP_JSON_BINDING: &str = "http+json";

/// Canonical missive name for the A2A JSON-RPC protocol binding.
pub const JSON_RPC_BINDING: &str = "json-rpc";

/// Canonical missive name reserved for future A2A gRPC protocol support.
pub const GRPC_BINDING: &str = "grpc";

/// Protocol bindings implemented by missive today, in default preference order.
pub const LOCALLY_SUPPORTED_BINDINGS: &[&str] = &[HTTP_JSON_BINDING, JSON_RPC_BINDING];

/// Protocol bindings recognized by name but intentionally left for future tickets.
pub const PLANNED_BINDINGS: &[&str] = &[GRPC_BINDING];

/// Returns metadata for this crate.
#[must_use]
pub const fn crate_info() -> missive_core::CrateInfo {
    missive_core::CrateInfo::new(CRATE_NAME, CRATE_PURPOSE)
}

/// Redaction marker used by debug rendering for outbound auth headers.
const AUTH_REDACTED: &str = "[REDACTED]";

/// Validated HTTP authentication/authorization headers for outbound A2A requests.
///
/// The raw values are intentionally not serializable and `Debug` renders only
/// redacted header values. Callers resolve secrets from CLI/config/keyring inputs
/// outside this crate, then pass the resulting headers to the A2A transport edge.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct AuthHeaders {
    headers: BTreeMap<String, String>,
}

impl AuthHeaders {
    /// Creates an empty auth header set.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            headers: BTreeMap::new(),
        }
    }

    /// Returns true when there are no auth headers to send.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.headers.is_empty()
    }

    /// Inserts or replaces one header using case-insensitive HTTP header-name
    /// canonicalization.
    pub fn insert(&mut self, name: impl AsRef<str>, value: impl AsRef<str>) -> Result<()> {
        let name = name.as_ref().trim();
        let value = value.as_ref().trim();
        let header_name = auth_header_name(name)?;
        auth_header_value(name, value)?;
        self.headers
            .insert(header_name.as_str().to_owned(), value.to_owned());
        Ok(())
    }

    /// Applies these headers to a blocking reqwest request builder. Header
    /// values are marked sensitive so downstream debug output does not expose
    /// them through reqwest internals.
    pub fn apply_to_blocking_request(&self, request: RequestBuilder) -> Result<RequestBuilder> {
        let mut request = request;
        for (name, value) in &self.headers {
            request = request.header(auth_header_name(name)?, auth_header_value(name, value)?);
        }
        Ok(request)
    }

    /// Returns a deterministic redacted view suitable for debug output and tests.
    #[must_use]
    pub fn redacted(&self) -> BTreeMap<String, String> {
        self.headers
            .iter()
            .map(|(name, value)| (name.clone(), redact_auth_header_value(name, value)))
            .collect()
    }
}

impl fmt::Debug for AuthHeaders {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthHeaders")
            .field("headers", &self.redacted())
            .finish()
    }
}

fn auth_header_name(name: &str) -> Result<HeaderName> {
    if name.is_empty() {
        return Err(MissiveError::validation("auth header name cannot be empty"));
    }
    HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
        MissiveError::validation("auth header name is not a valid HTTP header name")
            .with_source(error)
            .with_help("Use ASCII HTTP header names such as Authorization or X-Api-Key.")
    })
}

fn auth_header_value(name: &str, value: &str) -> Result<HeaderValue> {
    if value.is_empty() {
        return Err(MissiveError::validation(format!(
            "auth header {name:?} value cannot be empty"
        )));
    }
    if value.len() > 8 * 1024 {
        return Err(MissiveError::validation(format!(
            "auth header {name:?} value must be at most 8192 bytes"
        )));
    }
    let mut header_value = HeaderValue::from_str(value).map_err(|error| {
        MissiveError::validation(format!(
            "auth header {name:?} value is not a valid HTTP header value"
        ))
        .with_source(error)
        .with_help("Avoid control characters and newline characters in auth headers.")
    })?;
    header_value.set_sensitive(true);
    Ok(header_value)
}

fn redact_auth_header_value(name: &str, value: &str) -> String {
    let normalized: String = name
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    if normalized == "authorization" || normalized == "proxyauthorization" {
        let trimmed = value.trim();
        if let Some((scheme, _)) = trimmed.split_once(char::is_whitespace) {
            return format!("{scheme} {AUTH_REDACTED}");
        }
    }
    AUTH_REDACTED.to_owned()
}

/// A2A service parameters applied to outbound protocol requests.
///
/// For HTTP-based bindings these values are sent as headers: `A2A-Version` is
/// always present, `A2A-Extensions` is present when extensions are requested,
/// and `extra` entries become additional validated service-parameter headers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct ServiceParameters {
    /// A2A protocol version sent as `A2A-Version`.
    pub protocol_version: String,
    /// Requested A2A extension identifiers sent through `A2A-Extensions`.
    pub extensions: Vec<String>,
    /// Additional service parameters sent as HTTP headers when applicable.
    pub extra: BTreeMap<String, String>,
}

impl Default for ServiceParameters {
    fn default() -> Self {
        Self {
            protocol_version: protocol::VERSION.to_owned(),
            extensions: Vec::new(),
            extra: BTreeMap::new(),
        }
    }
}

impl ServiceParameters {
    /// Creates validated service parameters from explicit parts.
    pub fn new(
        protocol_version: impl Into<String>,
        extensions: Vec<String>,
        extra: BTreeMap<String, String>,
    ) -> Result<Self> {
        let parameters = Self {
            protocol_version: protocol_version.into(),
            extensions,
            extra,
        };
        parameters.validate()?;
        Ok(parameters)
    }

    /// Validates protocol version, extension identifiers, and HTTP header forms.
    pub fn validate(&self) -> Result<()> {
        validate_service_parameter_value("A2A-Version", &self.protocol_version)?;
        validate_protocol_version_text(&self.protocol_version)?;

        let mut extensions = BTreeSet::new();
        for extension in &self.extensions {
            validate_extension_value(extension)?;
            if !extensions.insert(extension) {
                return Err(MissiveError::validation(format!(
                    "duplicate A2A extension {extension:?}"
                ))
                .with_help("List each A2A extension at most once."));
            }
        }

        for (name, value) in &self.extra {
            validate_extra_service_parameter(name, value)?;
        }

        Ok(())
    }

    /// Applies these parameters to a blocking reqwest request builder.
    pub fn apply_to_blocking_request(&self, request: RequestBuilder) -> Result<RequestBuilder> {
        self.validate()?;
        let mut request = request.header(
            protocol::SVC_PARAM_VERSION,
            service_parameter_header_value(protocol::SVC_PARAM_VERSION, &self.protocol_version)?,
        );

        if !self.extensions.is_empty() {
            request = request.header(
                protocol::SVC_PARAM_EXTENSIONS,
                service_parameter_header_value(
                    protocol::SVC_PARAM_EXTENSIONS,
                    &self.extensions.join(", "),
                )?,
            );
        }

        for (name, value) in &self.extra {
            let header_name = service_parameter_header_name(name)?;
            let header_value = service_parameter_header_value(name, value)?;
            request = request.header(header_name, header_value);
        }

        Ok(request)
    }

    /// Records service-parameter metadata on future task/event rows.
    pub fn record_metadata(&self, metadata: &mut Metadata) -> Result<()> {
        metadata.insert_str(METADATA_A2A_PROTOCOL_VERSION, self.protocol_version.clone())?;
        if !self.extensions.is_empty() {
            metadata.insert(
                METADATA_A2A_EXTENSIONS,
                serde_json::json!(self.extensions.clone()),
            )?;
        }
        if !self.extra.is_empty() {
            metadata.insert(
                METADATA_A2A_SERVICE_PARAMETERS,
                serde_json::json!(self.extra.clone()),
            )?;
        }
        Ok(())
    }

    /// Returns metadata containing the service parameters from this request.
    pub fn to_metadata(&self) -> Result<Metadata> {
        let mut metadata = Metadata::new();
        self.record_metadata(&mut metadata)?;
        Ok(metadata)
    }
}

fn validate_protocol_version_text(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 64 {
        return Err(
            MissiveError::validation("A2A protocol version must be 1 to 64 bytes")
                .with_help("Use a short version such as 1.0."),
        );
    }
    if value
        .bytes()
        .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')))
    {
        return Err(MissiveError::validation(
            "A2A protocol version contains unsupported characters",
        )
        .with_help("Use ASCII letters, digits, '.', '-' or '_' only."));
    }
    Ok(())
}

fn validate_extension_value(value: &str) -> Result<()> {
    validate_service_parameter_value("A2A-Extensions", value)?;
    if value.chars().any(char::is_whitespace) || value.contains(',') {
        return Err(MissiveError::validation(
            "A2A extension identifiers must not contain whitespace or commas",
        )
        .with_help("Use compact URI-like extension identifiers."));
    }
    Ok(())
}

fn validate_extra_service_parameter(name: &str, value: &str) -> Result<()> {
    service_parameter_header_name(name)?;
    if is_reserved_service_parameter_name(name) {
        return Err(MissiveError::validation(format!(
            "extra A2A service parameters must not redefine reserved header {name:?}"
        ))
        .with_help("Use protocol_version for A2A-Version and extensions for A2A-Extensions."));
    }
    validate_service_parameter_value(name, value)
}

fn service_parameter_header_name(name: &str) -> Result<HeaderName> {
    HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
        MissiveError::validation(format!(
            "A2A service parameter name {name:?} is not a valid HTTP header name"
        ))
        .with_source(error)
        .with_help("Use ASCII HTTP header names such as A2A-Trace or A2A-Tenant.")
    })
}

fn service_parameter_header_value(name: &str, value: &str) -> Result<HeaderValue> {
    validate_service_parameter_value(name, value)?;
    HeaderValue::from_str(value).map_err(|error| {
        MissiveError::validation(format!(
            "A2A service parameter {name:?} is not a valid HTTP header value"
        ))
        .with_source(error)
        .with_help("Avoid control characters and newline characters in service parameters.")
    })
}

fn validate_service_parameter_value(name: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(MissiveError::validation(format!(
            "A2A service parameter {name:?} cannot be empty"
        )));
    }
    if value.len() > 8 * 1024 {
        return Err(MissiveError::validation(format!(
            "A2A service parameter {name:?} must be at most 8192 bytes"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(MissiveError::validation(format!(
            "A2A service parameter {name:?} cannot contain control characters"
        )));
    }
    Ok(())
}

fn is_reserved_service_parameter_name(name: &str) -> bool {
    name.eq_ignore_ascii_case(protocol::SVC_PARAM_VERSION)
        || name.eq_ignore_ascii_case(protocol::SVC_PARAM_EXTENSIONS)
}

/// Extension helpers around the official SDK Agent Card type.
///
/// `missive-a2a` parses public Agent Cards into the official `a2a-lf`
/// `AgentCard` model, then applies missive-specific validation and compatibility
/// normalization at the edge. This lets future send/task/push features share the
/// upstream protocol structs while preserving the current inspection behavior for
/// older cards that omit `supportedInterfaces`.
pub trait AgentCardExt {
    /// Parses and validates an Agent Card from JSON using the official SDK type.
    fn from_json(value: Value) -> Result<Self>
    where
        Self: Sized;

    /// Returns distinct protocol versions exposed by supported interfaces.
    fn protocol_versions(&self) -> Vec<String>;

    /// Returns a compact parsed summary useful for command output and cache
    /// diagnostics.
    fn summary(&self) -> AgentCardSummary;
}

impl AgentCardExt for AgentCard {
    fn from_json(value: Value) -> Result<Self> {
        if !value.is_object() {
            return Err(
                MissiveError::protocol("A2A Agent Card JSON must be an object").with_help(
                    "Verify that /.well-known/agent-card.json returns an AgentCard object.",
                ),
            );
        }

        let normalized = normalize_agent_card_value(value);
        let card = parse_official_agent_card(normalized).map_err(|error| {
            MissiveError::protocol("malformed A2A Agent Card JSON")
                .with_source(error)
                .with_help(
                    "Verify that /.well-known/agent-card.json follows the A2A AgentCard schema.",
                )
        })?;
        validate_agent_card(&card)?;
        Ok(card)
    }

    fn protocol_versions(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        let mut versions = Vec::new();
        for interface in &self.supported_interfaces {
            if seen.insert(interface.protocol_version.clone()) {
                versions.push(interface.protocol_version.clone());
            }
        }
        versions
    }

    fn summary(&self) -> AgentCardSummary {
        AgentCardSummary {
            name: self.name.clone(),
            description: self.description.clone(),
            provider: self.provider.clone(),
            agent_version: self.version.clone(),
            protocol_versions: self.protocol_versions(),
            documentation_url: self.documentation_url.clone(),
            icon_url: self.icon_url.clone(),
            supported_interfaces: self.supported_interfaces.clone(),
            capabilities: self.capabilities.clone(),
            default_input_modes: self.default_input_modes.clone(),
            default_output_modes: self.default_output_modes.clone(),
            skills: self.skills.clone(),
        }
    }
}

/// Parsed and validated subset of an Agent Card that is stable for command
/// output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentCardSummary {
    /// Human-readable agent name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Optional provider metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<AgentProvider>,
    /// Agent implementation version.
    pub agent_version: String,
    /// Distinct A2A protocol versions from supported interfaces.
    pub protocol_versions: Vec<String>,
    /// Optional documentation URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation_url: Option<String>,
    /// Optional icon URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
    /// Ordered protocol interfaces advertised by the remote agent.
    pub supported_interfaces: Vec<AgentInterface>,
    /// Declared A2A capabilities.
    pub capabilities: AgentCapabilities,
    /// Default input media types.
    pub default_input_modes: Vec<String>,
    /// Default output media types.
    pub default_output_modes: Vec<String>,
    /// Advertised skills.
    pub skills: Vec<AgentSkill>,
}

fn parse_official_agent_card(value: Value) -> std::result::Result<AgentCard, serde_json::Error> {
    match serde_json::from_value::<AgentCard>(value.clone()) {
        Ok(card) => Ok(card),
        Err(primary_error) => {
            let relaxed = relax_optional_security_fields(value.clone());
            if relaxed == value {
                Err(primary_error)
            } else {
                serde_json::from_value::<AgentCard>(relaxed).map_err(|_| primary_error)
            }
        }
    }
}

fn normalize_agent_card_value(mut value: Value) -> Value {
    let Some(object) = value.as_object_mut() else {
        return value;
    };

    move_alias(object, "supportedInterfaces", &["supported_interfaces"]);
    move_alias(object, "documentationUrl", &["documentation_url"]);
    move_alias(object, "securitySchemes", &["security_schemes"]);
    move_alias(object, "securityRequirements", &["security_requirements"]);
    move_alias(object, "defaultInputModes", &["default_input_modes"]);
    move_alias(object, "defaultOutputModes", &["default_output_modes"]);
    move_alias(object, "iconUrl", &["icon_url"]);

    object
        .entry("supportedInterfaces".to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));

    if let Some(interfaces) = object
        .get_mut("supportedInterfaces")
        .and_then(Value::as_array_mut)
    {
        for interface in interfaces {
            if let Some(interface) = interface.as_object_mut() {
                move_alias(
                    interface,
                    "protocolBinding",
                    &["protocol_binding", "transport"],
                );
                move_alias(interface, "protocolVersion", &["protocol_version"]);
            }
        }
    }

    if let Some(capabilities) = object
        .get_mut("capabilities")
        .and_then(Value::as_object_mut)
    {
        move_alias(capabilities, "pushNotifications", &["push_notifications"]);
        move_alias(
            capabilities,
            "stateTransitionHistory",
            &["state_transition_history"],
        );
        move_alias(capabilities, "extendedAgentCard", &["extended_agent_card"]);
    }

    if let Some(skills) = object.get_mut("skills").and_then(Value::as_array_mut) {
        for skill in skills {
            if let Some(skill) = skill.as_object_mut() {
                move_alias(skill, "inputModes", &["input_modes"]);
                move_alias(skill, "outputModes", &["output_modes"]);
                move_alias(skill, "securityRequirements", &["security_requirements"]);
            }
        }
    }

    value
}

fn move_alias(object: &mut Map<String, Value>, canonical: &str, aliases: &[&str]) {
    if object.contains_key(canonical) {
        return;
    }
    for alias in aliases {
        if let Some(value) = object.remove(*alias) {
            object.insert(canonical.to_owned(), value);
            return;
        }
    }
}

fn relax_optional_security_fields(mut value: Value) -> Value {
    let Some(object) = value.as_object_mut() else {
        return value;
    };

    object.remove("securitySchemes");
    object.remove("securityRequirements");
    object.remove("signatures");

    if let Some(skills) = object.get_mut("skills").and_then(Value::as_array_mut) {
        for skill in skills {
            if let Some(skill) = skill.as_object_mut() {
                skill.remove("securityRequirements");
            }
        }
    }

    value
}

fn validate_agent_card(card: &AgentCard) -> Result<()> {
    validate_non_empty("Agent Card name", &card.name)?;
    validate_non_empty("Agent Card description", &card.description)?;
    validate_non_empty("Agent Card version", &card.version)?;
    if card.skills.is_empty() {
        return Err(
            MissiveError::protocol("A2A Agent Card does not declare any skills")
                .with_help("Public Agent Cards must include at least one skills entry."),
        );
    }
    for (index, interface) in card.supported_interfaces.iter().enumerate() {
        validate_agent_interface(interface, index)?;
    }
    for (index, skill) in card.skills.iter().enumerate() {
        validate_agent_skill(skill, index)?;
    }
    Ok(())
}

fn validate_agent_interface(interface: &AgentInterface, index: usize) -> Result<()> {
    validate_non_empty(
        format!("Agent Card supportedInterfaces[{index}].url"),
        &interface.url,
    )?;
    validate_non_empty(
        format!("Agent Card supportedInterfaces[{index}].protocolBinding"),
        &interface.protocol_binding,
    )?;
    validate_non_empty(
        format!("Agent Card supportedInterfaces[{index}].protocolVersion"),
        &interface.protocol_version,
    )?;

    match Url::parse(&interface.url) {
        Ok(parsed) if parsed.host_str().is_some() => Ok(()),
        _ if canonical_protocol_binding(&interface.protocol_binding) == GRPC_BINDING
            && looks_like_grpc_authority(&interface.url) =>
        {
            Ok(())
        }
        Ok(_) => Err(MissiveError::protocol(format!(
            "A2A Agent Card supportedInterfaces[{index}].url must include a host"
        ))),
        Err(error) => Err(MissiveError::protocol(format!(
            "A2A Agent Card supportedInterfaces[{index}].url is not an absolute URL"
        ))
        .with_source(error)
        .with_help(
            "AgentInterface.url must be an absolute URL such as https://agent.example/a2a.",
        )),
    }
}

fn looks_like_grpc_authority(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && !value.contains("://")
        && !value.starts_with('/')
        && !value
            .chars()
            .any(|character| character.is_ascii_whitespace() || character.is_control())
}

fn validate_agent_skill(skill: &AgentSkill, index: usize) -> Result<()> {
    validate_non_empty(format!("Agent Card skills[{index}].id"), &skill.id)?;
    validate_non_empty(format!("Agent Card skills[{index}].name"), &skill.name)
}

/// Source used when an A2A interface negotiation result was selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NegotiatedInterfaceSource {
    /// Selected from the Agent Card's `supportedInterfaces` array.
    AgentCard,
    /// Selected from an explicit registry/config interface URL because the card
    /// did not declare `supportedInterfaces`.
    RegistryOverride,
    /// Selected from the registered base URL as a compatibility fallback.
    BaseUrlFallback,
}

impl NegotiatedInterfaceSource {
    /// Stable string used by command output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentCard => "agent_card",
            Self::RegistryOverride => "registry_override",
            Self::BaseUrlFallback => "base_url_fallback",
        }
    }
}

/// Result of A2A protocol/interface negotiation for one agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct NegotiatedInterface {
    /// Canonical missive binding name, for example `http+json` or `json-rpc`.
    pub binding: String,
    /// Binding spelling from the Agent Card, or the canonical binding for
    /// fallback interfaces.
    pub protocol_binding: String,
    /// Endpoint URL selected for future protocol calls.
    pub url: String,
    /// Optional tenant identifier advertised by the Agent Card.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// A2A protocol version declared by the interface, or `unknown` when using
    /// a compatibility fallback with no declared interface metadata.
    pub protocol_version: String,
    /// Where this selection came from.
    pub source: NegotiatedInterfaceSource,
}

/// Caller-provided options for A2A interface negotiation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct InterfaceNegotiationOptions {
    /// Ordered preferred bindings. Empty means missive's default local order.
    pub preferred_bindings: Vec<String>,
    /// Optional explicit binding override from `--binding`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding_override: Option<String>,
    /// Explicit fallback interface URLs keyed by binding name. These are used
    /// only when the Agent Card omits `supportedInterfaces`.
    pub fallback_interface_urls: BTreeMap<String, String>,
    /// Registered base URL used as a legacy HTTP+JSON fallback when no explicit
    /// interface URL is available and the card omits `supportedInterfaces`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_base_url: Option<String>,
}

impl Default for InterfaceNegotiationOptions {
    fn default() -> Self {
        Self {
            preferred_bindings: default_binding_preference(),
            binding_override: None,
            fallback_interface_urls: BTreeMap::new(),
            fallback_base_url: None,
        }
    }
}

/// Returns missive's default local A2A binding preference order.
#[must_use]
pub fn default_binding_preference() -> Vec<String> {
    LOCALLY_SUPPORTED_BINDINGS
        .iter()
        .map(|binding| (*binding).to_owned())
        .collect()
}

/// Returns true when missive can currently use the canonical binding.
#[must_use]
pub fn is_locally_supported_binding(binding: &str) -> bool {
    LOCALLY_SUPPORTED_BINDINGS.contains(&binding)
}

/// Returns the comma-separated local support list for diagnostics.
#[must_use]
pub fn locally_supported_bindings_text() -> String {
    LOCALLY_SUPPORTED_BINDINGS.join(", ")
}

/// Canonicalizes A2A protocol binding names for comparison and command output.
///
/// A2A Agent Cards use values such as `HTTP+JSON`, `JSONRPC`, and `gRPC`, while
/// missive's CLI/config identifiers are lowercase (`http+json`, `json-rpc`,
/// `grpc`). Unknown names are lowercased and otherwise preserved so diagnostics
/// can report what the remote card advertised.
#[must_use]
pub fn canonical_protocol_binding(value: &str) -> String {
    let folded = value.trim().to_ascii_lowercase();
    let compact = folded
        .chars()
        .filter(|character| !matches!(character, '+' | '-' | '_'))
        .collect::<String>();
    match compact.as_str() {
        "httpjson" => HTTP_JSON_BINDING.to_owned(),
        "jsonrpc" => JSON_RPC_BINDING.to_owned(),
        "grpc" => GRPC_BINDING.to_owned(),
        _ => folded,
    }
}

/// Selects the first mutually supported A2A interface using the caller's
/// preference order and optional explicit binding override.
///
/// `HTTP+JSON` and `JSONRPC` are implemented today. `gRPC` is recognized for
/// diagnostics and future extension points but is not selected until a later
/// implementation ticket adds local support.
pub fn negotiate_agent_interface(
    card: &AgentCard,
    options: &InterfaceNegotiationOptions,
) -> Result<NegotiatedInterface> {
    let preferences = normalized_preferences(&options.preferred_bindings)?;
    if let Some(override_binding) = options.binding_override.as_deref() {
        let binding = normalize_requested_binding("binding override", override_binding)?;
        ensure_locally_supported(&binding)?;
        return negotiate_requested_binding(card, options, &binding);
    }

    let mut locally_supported_preference = Vec::new();
    for binding in &preferences {
        if is_locally_supported_binding(binding) {
            locally_supported_preference.push(binding.clone());
        }
    }

    if locally_supported_preference.is_empty() {
        return Err(MissiveError::transport(format!(
            "A2A interface negotiation cannot proceed because binding preference [{}] contains no locally supported bindings; missive supports locally: {}",
            preferences.join(", "),
            locally_supported_bindings_text()
        ))
        .with_help(local_support_help(None)));
    }

    for binding in &locally_supported_preference {
        if let Some(interface) = first_card_interface(card, binding) {
            return Ok(interface_from_agent_card(interface));
        }
    }

    if card.supported_interfaces.is_empty() {
        return negotiate_missing_interfaces_fallback(options, &locally_supported_preference, None);
    }

    Err(no_mutual_interface_error(card))
}

fn normalized_preferences(values: &[String]) -> Result<Vec<String>> {
    let values = if values.is_empty() {
        default_binding_preference()
    } else {
        values.to_vec()
    };
    let mut seen = BTreeSet::new();
    let mut normalized = Vec::new();
    for value in values {
        let binding = normalize_requested_binding("binding preference", &value)?;
        if seen.insert(binding.clone()) {
            normalized.push(binding);
        }
    }
    Ok(normalized)
}

fn normalize_requested_binding(label: &str, value: &str) -> Result<String> {
    let binding = canonical_protocol_binding(value);
    if binding.is_empty() {
        return Err(MissiveError::validation(format!("{label} cannot be empty"))
            .with_help(local_support_help(None)));
    }
    if binding.bytes().any(|byte| {
        !(byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || matches!(byte, b'+' | b'-' | b'_' | b'.'))
    }) {
        return Err(MissiveError::validation(format!(
            "{label} {value:?} is not a valid protocol binding name"
        ))
        .with_help(local_support_help(None)));
    }
    Ok(binding)
}

fn ensure_locally_supported(binding: &str) -> Result<()> {
    if is_locally_supported_binding(binding) {
        return Ok(());
    }

    Err(MissiveError::transport(format!(
        "A2A binding {binding:?} is not supported locally; missive supports locally: {}",
        locally_supported_bindings_text()
    ))
    .with_help(local_support_help(Some(binding))))
}

fn negotiate_requested_binding(
    card: &AgentCard,
    options: &InterfaceNegotiationOptions,
    binding: &str,
) -> Result<NegotiatedInterface> {
    if let Some(interface) = first_card_interface(card, binding) {
        return Ok(interface_from_agent_card(interface));
    }

    if card.supported_interfaces.is_empty() {
        return negotiate_missing_interfaces_fallback(
            options,
            &[binding.to_owned()],
            Some(binding),
        );
    }

    Err(MissiveError::transport(format!(
        "A2A binding override {binding:?} is not advertised by the Agent Card; missive supports locally: {}; remote advertised: {}",
        locally_supported_bindings_text(),
        remote_bindings_text(card)
    ))
    .with_help("Choose a binding advertised in supportedInterfaces, or refresh/update the Agent Card."))
}

fn first_card_interface<'a>(card: &'a AgentCard, binding: &str) -> Option<&'a AgentInterface> {
    card.supported_interfaces
        .iter()
        .find(|interface| canonical_protocol_binding(&interface.protocol_binding) == binding)
}

fn interface_from_agent_card(interface: &AgentInterface) -> NegotiatedInterface {
    NegotiatedInterface {
        binding: canonical_protocol_binding(&interface.protocol_binding),
        protocol_binding: interface.protocol_binding.clone(),
        url: interface.url.clone(),
        tenant: interface.tenant.clone(),
        protocol_version: interface.protocol_version.clone(),
        source: NegotiatedInterfaceSource::AgentCard,
    }
}

fn negotiate_missing_interfaces_fallback(
    options: &InterfaceNegotiationOptions,
    bindings: &[String],
    requested_binding: Option<&str>,
) -> Result<NegotiatedInterface> {
    for binding in bindings {
        if let Some(interface) = fallback_interface_for_binding(options, binding)? {
            return Ok(interface);
        }
    }

    let requested = requested_binding
        .map(|binding| format!(" for requested binding {binding:?}"))
        .unwrap_or_default();
    Err(MissiveError::transport(format!(
        "A2A Agent Card does not declare supportedInterfaces and no registry/base-URL fallback interface is available{requested}; missive supports locally: {}",
        locally_supported_bindings_text()
    ))
    .with_help("Add an explicit agent interface URL, use the HTTP+JSON base-URL fallback, or update the remote Agent Card."))
}

fn fallback_interface_for_binding(
    options: &InterfaceNegotiationOptions,
    binding: &str,
) -> Result<Option<NegotiatedInterface>> {
    for (candidate, url) in &options.fallback_interface_urls {
        if canonical_protocol_binding(candidate) == binding {
            validate_interface_url("registry fallback interface URL", url)?;
            return Ok(Some(NegotiatedInterface {
                binding: binding.to_owned(),
                protocol_binding: binding.to_owned(),
                url: url.clone(),
                tenant: None,
                protocol_version: "unknown".to_owned(),
                source: NegotiatedInterfaceSource::RegistryOverride,
            }));
        }
    }

    if binding == HTTP_JSON_BINDING
        && let Some(url) = &options.fallback_base_url
    {
        validate_interface_url("base URL fallback interface", url)?;
        return Ok(Some(NegotiatedInterface {
            binding: binding.to_owned(),
            protocol_binding: binding.to_owned(),
            url: url.clone(),
            tenant: None,
            protocol_version: "unknown".to_owned(),
            source: NegotiatedInterfaceSource::BaseUrlFallback,
        }));
    }

    Ok(None)
}

fn validate_interface_url(label: &str, value: &str) -> Result<()> {
    let parsed = Url::parse(value).map_err(|error| {
        MissiveError::validation(format!("{label} must be an absolute URL"))
            .with_source(error)
            .with_help("Use an absolute http(s) URL such as https://agent.example/a2a.")
    })?;
    if parsed.host_str().is_none() {
        return Err(
            MissiveError::validation(format!("{label} must include a host"))
                .with_help("Use an absolute http(s) URL such as https://agent.example/a2a."),
        );
    }
    Ok(())
}

fn no_mutual_interface_error(card: &AgentCard) -> MissiveError {
    MissiveError::transport(format!(
        "no mutually supported A2A interface could be negotiated; missive supports locally: {}; remote advertised: {}",
        locally_supported_bindings_text(),
        remote_bindings_text(card)
    ))
    .with_help("Use an agent that advertises HTTP+JSON or JSONRPC, or add local support for another binding in a future extension.")
}

fn remote_bindings_text(card: &AgentCard) -> String {
    let mut bindings = BTreeSet::new();
    for interface in &card.supported_interfaces {
        bindings.insert(canonical_protocol_binding(&interface.protocol_binding));
    }
    if bindings.is_empty() {
        "none".to_owned()
    } else {
        bindings.into_iter().collect::<Vec<_>>().join(", ")
    }
}

fn local_support_help(binding: Option<&str>) -> String {
    let mut help = format!(
        "Choose one of the locally supported A2A bindings: {}.",
        locally_supported_bindings_text()
    );
    if binding.is_some_and(|binding| PLANNED_BINDINGS.contains(&binding)) {
        help.push_str(" gRPC is recognized by the negotiation layer but is not implemented yet.");
    }
    help
}

/// Cache validators captured from Agent Card HTTP response headers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentCardCacheValidators {
    /// HTTP ETag header value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    /// HTTP Last-Modified header value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
}

impl AgentCardCacheValidators {
    /// Returns true when no HTTP validators are available.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.etag.is_none() && self.last_modified.is_none()
    }
}

/// Result of an Agent Card fetch request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCardFetchOutcome {
    /// A fresh card body was fetched and parsed.
    Fetched(Box<AgentCardFetch>),
    /// The remote endpoint returned `304 Not Modified` for the provided cache
    /// validators.
    NotModified(AgentCardNotModified),
}

/// Fresh Agent Card fetch response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentCardFetch {
    /// URL fetched by the resolver.
    pub url: String,
    /// HTTP status code.
    pub status: u16,
    /// Parsed Agent Card.
    pub card: AgentCard,
    /// Raw public Agent Card JSON value.
    pub raw_json: Value,
    /// Response cache validators.
    pub validators: AgentCardCacheValidators,
}

/// HTTP `304 Not Modified` response metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentCardNotModified {
    /// URL fetched by the resolver.
    pub url: String,
    /// HTTP status code, always 304.
    pub status: u16,
    /// Response cache validators, if the server echoed/updated any.
    pub validators: AgentCardCacheValidators,
}

/// Blocking public Agent Card discovery client.
#[derive(Debug, Clone)]
pub struct AgentCardClient {
    client: Client,
}

impl AgentCardClient {
    /// Creates a client with a bounded timeout and a missive user agent.
    pub fn new() -> Result<Self> {
        Self::with_timeout(DEFAULT_DISCOVERY_TIMEOUT)
    }

    /// Creates a client with a caller-provided timeout.
    pub fn with_timeout(timeout: Duration) -> Result<Self> {
        let client = Client::builder()
            .timeout(timeout)
            .user_agent(USER_AGENT)
            .build()
            .map_err(|error| {
                MissiveError::transport("building A2A Agent Card HTTP client")
                    .with_source(error)
                    .with_help("Check local TLS certificate roots and HTTP client configuration.")
            })?;
        Ok(Self { client })
    }

    /// Fetches `/.well-known/agent-card.json` from a registered agent base URL.
    pub fn fetch_public_agent_card(
        &self,
        base_url: &str,
        validators: Option<&AgentCardCacheValidators>,
    ) -> Result<AgentCardFetchOutcome> {
        self.fetch_public_agent_card_with_service_parameters(
            base_url,
            validators,
            &ServiceParameters::default(),
        )
    }

    /// Fetches `/.well-known/agent-card.json` with explicit A2A service parameters.
    pub fn fetch_public_agent_card_with_service_parameters(
        &self,
        base_url: &str,
        validators: Option<&AgentCardCacheValidators>,
        service_parameters: &ServiceParameters,
    ) -> Result<AgentCardFetchOutcome> {
        self.fetch_public_agent_card_with_service_parameters_and_auth(
            base_url,
            validators,
            service_parameters,
            &AuthHeaders::new(),
        )
    }

    /// Fetches `/.well-known/agent-card.json` with explicit A2A service
    /// parameters and resolved authentication headers.
    pub fn fetch_public_agent_card_with_service_parameters_and_auth(
        &self,
        base_url: &str,
        validators: Option<&AgentCardCacheValidators>,
        service_parameters: &ServiceParameters,
        auth_headers: &AuthHeaders,
    ) -> Result<AgentCardFetchOutcome> {
        let discovery_url = public_agent_card_url(base_url)?;
        let mut request = self
            .client
            .get(discovery_url.clone())
            .header("Accept", "application/json");
        request = service_parameters.apply_to_blocking_request(request)?;
        request = auth_headers.apply_to_blocking_request(request)?;

        if let Some(validators) = validators {
            if let Some(etag) = &validators.etag {
                request = request.header(IF_NONE_MATCH, etag);
            }
            if let Some(last_modified) = &validators.last_modified {
                request = request.header(IF_MODIFIED_SINCE, last_modified);
            }
        }

        let response = request.send().map_err(|error| {
            MissiveError::transport(format!(
                "fetching A2A Agent Card from {discovery_url} failed"
            ))
            .with_source(error)
            .with_help("Verify the agent base URL, local network access, and TLS configuration.")
        })?;

        let status = response.status();
        let headers = response.headers().clone();
        if status == StatusCode::NOT_MODIFIED {
            return Ok(AgentCardFetchOutcome::NotModified(AgentCardNotModified {
                url: discovery_url.to_string(),
                status: status.as_u16(),
                validators: validators_from_headers(&headers),
            }));
        }
        if !status.is_success() {
            let body = response.text().map_err(|error| {
                MissiveError::transport(format!(
                    "reading A2A Agent Card error response from {discovery_url} failed"
                ))
                .with_source(error)
            })?;
            if response_reports_unsupported_version(&body) {
                return Err(unsupported_protocol_version_error(
                    &service_parameters.protocol_version,
                    &discovery_url,
                    status,
                ));
            }
            return Err(MissiveError::transport(format!(
                "A2A Agent Card discovery returned HTTP {status} for {discovery_url}"
            ))
            .with_help(
                "Ensure the agent serves /.well-known/agent-card.json and retry with --refresh after fixing the endpoint.",
            ));
        }

        let body = response.text().map_err(|error| {
            MissiveError::transport(format!(
                "reading A2A Agent Card response from {discovery_url} failed"
            ))
            .with_source(error)
        })?;
        let raw_json = serde_json::from_str::<Value>(&body).map_err(|error| {
            MissiveError::protocol(format!(
                "A2A Agent Card response from {discovery_url} is not valid JSON"
            ))
            .with_source(error)
            .with_help("Verify that /.well-known/agent-card.json returns a JSON object.")
        })?;
        if !raw_json.is_object() {
            return Err(MissiveError::protocol(format!(
                "A2A Agent Card response from {discovery_url} must be a JSON object"
            ))
            .with_help("Verify that /.well-known/agent-card.json returns an AgentCard object."));
        }
        let card = AgentCard::from_json(raw_json.clone())?;

        Ok(AgentCardFetchOutcome::Fetched(Box::new(AgentCardFetch {
            url: discovery_url.to_string(),
            status: status.as_u16(),
            card,
            raw_json,
            validators: validators_from_headers(&headers),
        })))
    }
}

impl Default for AgentCardClient {
    fn default() -> Self {
        Self::new().expect("default Agent Card HTTP client should build")
    }
}

/// Result of a non-streaming A2A `SendMessage` request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SendMessageOutcome {
    /// URL called by the selected transport.
    pub url: String,
    /// HTTP status code returned by the transport.
    pub status: u16,
    /// Negotiated interface used for the call.
    pub interface: NegotiatedInterface,
    /// Parsed A2A response.
    pub response: protocol::SendMessageResponse,
    /// Raw response JSON after unwrapping any JSON-RPC envelope.
    pub raw_json: Value,
}

/// Blocking A2A client for non-streaming message sends.
#[derive(Debug, Clone)]
pub struct SendMessageClient {
    client: Client,
}

impl SendMessageClient {
    /// Creates a client with a bounded timeout and missive user agent.
    pub fn new() -> Result<Self> {
        Self::with_timeout(DEFAULT_SEND_TIMEOUT)
    }

    /// Creates a client with a caller-provided timeout.
    pub fn with_timeout(timeout: Duration) -> Result<Self> {
        let client = Client::builder()
            .timeout(timeout)
            .user_agent(USER_AGENT)
            .build()
            .map_err(|error| {
                MissiveError::transport("building A2A send HTTP client")
                    .with_source(error)
                    .with_help("Check local TLS certificate roots and HTTP client configuration.")
            })?;
        Ok(Self { client })
    }

    /// Sends one non-streaming A2A message using the negotiated interface.
    pub fn send_message(
        &self,
        interface: &NegotiatedInterface,
        request: &protocol::SendMessageRequest,
        service_parameters: &ServiceParameters,
        auth_headers: &AuthHeaders,
    ) -> Result<SendMessageOutcome> {
        match interface.binding.as_str() {
            HTTP_JSON_BINDING => self.send_http_json(
                interface,
                request,
                service_parameters,
                auth_headers,
            ),
            JSON_RPC_BINDING => self.send_json_rpc(
                interface,
                request,
                service_parameters,
                auth_headers,
            ),
            binding => Err(MissiveError::transport(format!(
                "A2A send cannot use unsupported negotiated binding {binding:?}; missive supports locally: {}",
                locally_supported_bindings_text()
            ))
            .with_help(local_support_help(Some(binding)))),
        }
    }

    fn send_http_json(
        &self,
        interface: &NegotiatedInterface,
        request: &protocol::SendMessageRequest,
        service_parameters: &ServiceParameters,
        auth_headers: &AuthHeaders,
    ) -> Result<SendMessageOutcome> {
        let endpoint = rest_endpoint_url(&interface.url, SEND_MESSAGE_REST_PATH)?;
        let request_body = request_with_interface_tenant(request, interface);
        let mut http_request = self
            .client
            .post(endpoint.clone())
            .header("Accept", A2A_JSON_CONTENT_TYPE)
            .header("Content-Type", A2A_JSON_CONTENT_TYPE)
            .json(&request_body);
        http_request = service_parameters.apply_to_blocking_request(http_request)?;
        http_request = auth_headers.apply_to_blocking_request(http_request)?;

        let response = http_request.send().map_err(|error| {
            MissiveError::transport(format!("sending A2A message to {endpoint} failed"))
                .with_source(error)
                .with_help("Verify the selected Agent Card interface URL, local network access, and TLS configuration.")
        })?;
        let status = response.status();
        let body = response.text().map_err(|error| {
            MissiveError::transport(format!("reading A2A send response from {endpoint} failed"))
                .with_source(error)
        })?;
        if !status.is_success() {
            if response_reports_unsupported_version(&body) {
                return Err(unsupported_protocol_version_error(
                    &service_parameters.protocol_version,
                    &endpoint,
                    status,
                ));
            }
            return Err(MissiveError::transport(format!(
                "A2A send returned HTTP {status} for {endpoint}"
            ))
            .with_help("Inspect the remote agent logs or retry after refreshing its Agent Card."));
        }

        let raw_json = parse_response_json(&body, &endpoint, "A2A send response")?;
        let response = serde_json::from_value::<protocol::SendMessageResponse>(raw_json.clone())
            .map_err(|error| {
                MissiveError::protocol(format!(
                    "A2A send response from {endpoint} is not a SendMessageResponse"
                ))
                .with_source(error)
                .with_help("Expected a JSON object with exactly one of 'task' or 'message'.")
            })?;

        Ok(SendMessageOutcome {
            url: endpoint.to_string(),
            status: status.as_u16(),
            interface: interface.clone(),
            response,
            raw_json,
        })
    }

    fn send_json_rpc(
        &self,
        interface: &NegotiatedInterface,
        request: &protocol::SendMessageRequest,
        service_parameters: &ServiceParameters,
        auth_headers: &AuthHeaders,
    ) -> Result<SendMessageOutcome> {
        let endpoint = validate_absolute_url("JSON-RPC interface URL", &interface.url)?;
        let request_body = request_with_interface_tenant(request, interface);
        let params = serde_json::to_value(&request_body).map_err(|error| {
            MissiveError::protocol("encoding SendMessageRequest as JSON-RPC params")
                .with_source(error)
        })?;
        let rpc_request = protocol::JsonRpcRequest::new(
            protocol::JsonRpcId::String(request_body.message.message_id.clone()),
            protocol::jsonrpc_methods::SEND_MESSAGE,
            Some(params),
        );
        let mut http_request = self
            .client
            .post(endpoint.clone())
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .json(&rpc_request);
        http_request = service_parameters.apply_to_blocking_request(http_request)?;
        http_request = auth_headers.apply_to_blocking_request(http_request)?;

        let response = http_request.send().map_err(|error| {
            MissiveError::transport(format!(
                "sending A2A JSON-RPC message to {endpoint} failed"
            ))
            .with_source(error)
            .with_help("Verify the selected Agent Card JSON-RPC interface URL, local network access, and TLS configuration.")
        })?;
        let status = response.status();
        let body = response.text().map_err(|error| {
            MissiveError::transport(format!(
                "reading A2A JSON-RPC send response from {endpoint} failed"
            ))
            .with_source(error)
        })?;
        if !status.is_success() {
            if response_reports_unsupported_version(&body) {
                return Err(unsupported_protocol_version_error(
                    &service_parameters.protocol_version,
                    &endpoint,
                    status,
                ));
            }
            return Err(MissiveError::transport(format!(
                "A2A JSON-RPC send returned HTTP {status} for {endpoint}"
            ))
            .with_help("Inspect the remote agent logs or retry after refreshing its Agent Card."));
        }

        let raw_rpc = parse_response_json(&body, &endpoint, "A2A JSON-RPC send response")?;
        let rpc_response = serde_json::from_value::<protocol::JsonRpcResponse>(raw_rpc.clone())
            .map_err(|error| {
                MissiveError::protocol(format!(
                    "A2A JSON-RPC send response from {endpoint} is not a JSON-RPC response"
                ))
                .with_source(error)
            })?;
        if let Some(error) = rpc_response.error {
            if error.code == protocol::error_code::VERSION_NOT_SUPPORTED {
                return Err(unsupported_protocol_version_error(
                    &service_parameters.protocol_version,
                    &endpoint,
                    status,
                ));
            }
            return Err(MissiveError::protocol(format!(
                "A2A JSON-RPC SendMessage failed with code {}: {}",
                error.code, error.message
            ))
            .with_help("Inspect the remote agent error data and retry if appropriate."));
        }
        let raw_json = rpc_response.result.ok_or_else(|| {
            MissiveError::protocol(format!(
                "A2A JSON-RPC send response from {endpoint} did not include result or error"
            ))
        })?;
        let response = serde_json::from_value::<protocol::SendMessageResponse>(raw_json.clone())
            .map_err(|error| {
                MissiveError::protocol(format!(
                    "A2A JSON-RPC SendMessage result from {endpoint} is not a SendMessageResponse"
                ))
                .with_source(error)
                .with_help("Expected a JSON object with exactly one of 'task' or 'message'.")
            })?;

        Ok(SendMessageOutcome {
            url: endpoint.to_string(),
            status: status.as_u16(),
            interface: interface.clone(),
            response,
            raw_json,
        })
    }
}

impl Default for SendMessageClient {
    fn default() -> Self {
        Self::new().expect("default A2A send HTTP client should build")
    }
}

fn request_with_interface_tenant(
    request: &protocol::SendMessageRequest,
    interface: &NegotiatedInterface,
) -> protocol::SendMessageRequest {
    let mut request = request.clone();
    if request.tenant.is_none() {
        request.tenant.clone_from(&interface.tenant);
    }
    request
}

fn rest_endpoint_url(base_url: &str, suffix: &str) -> Result<Url> {
    let mut parsed = validate_absolute_url("HTTP+JSON interface URL", base_url)?;
    parsed.set_query(None);
    parsed.set_fragment(None);
    let mut endpoint = parsed.to_string().trim_end_matches('/').to_owned();
    endpoint.push('/');
    endpoint.push_str(suffix.trim_start_matches('/'));
    Url::parse(&endpoint).map_err(|error| {
        MissiveError::validation(format!(
            "could not resolve A2A REST endpoint {suffix:?} against interface URL {base_url:?}"
        ))
        .with_source(error)
    })
}

fn validate_absolute_url(label: &str, value: &str) -> Result<Url> {
    let parsed = Url::parse(value).map_err(|error| {
        MissiveError::validation(format!("{label} must be an absolute URL"))
            .with_source(error)
            .with_help("Use an absolute http(s) URL such as https://agent.example/a2a.")
    })?;
    if parsed.host_str().is_none() {
        return Err(
            MissiveError::validation(format!("{label} must include a host"))
                .with_help("Use an absolute http(s) URL such as https://agent.example/a2a."),
        );
    }
    Ok(parsed)
}

fn parse_response_json(body: &str, url: &Url, label: &str) -> Result<Value> {
    serde_json::from_str::<Value>(body).map_err(|error| {
        MissiveError::protocol(format!("{label} from {url} is not valid JSON"))
            .with_source(error)
            .with_help("Verify that the remote A2A endpoint returns a JSON response body.")
    })
}

fn response_reports_unsupported_version(body: &str) -> bool {
    serde_json::from_str::<Value>(body)
        .ok()
        .is_some_and(|value| json_contains_unsupported_version(&value))
}

fn json_contains_unsupported_version(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            object.get("code").and_then(Value::as_i64)
                == Some(i64::from(protocol::error_code::VERSION_NOT_SUPPORTED))
                || object
                    .get("reason")
                    .and_then(Value::as_str)
                    .is_some_and(|reason| reason == "VERSION_NOT_SUPPORTED")
                || object.values().any(json_contains_unsupported_version)
        }
        Value::Array(values) => values.iter().any(json_contains_unsupported_version),
        Value::String(text) => text == "VERSION_NOT_SUPPORTED",
        _ => false,
    }
}

fn unsupported_protocol_version_error(
    requested_version: &str,
    url: &Url,
    status: StatusCode,
) -> MissiveError {
    MissiveError::protocol(format!(
        "A2A protocol version {requested_version:?} is not supported by {url} (HTTP {status})"
    ))
    .with_help("Choose a protocol_version supported by the remote Agent Card or retry without --protocol-version.")
}

/// Resolves the public Agent Card URL for an agent base URL.
pub fn public_agent_card_url(base_url: &str) -> Result<Url> {
    let parsed = Url::parse(base_url).map_err(|error| {
        MissiveError::validation("agent base URL must be an absolute URL")
            .with_source(error)
            .with_help("Use an absolute http(s) URL such as https://agent.example.")
    })?;
    parsed.join(PUBLIC_AGENT_CARD_PATH).map_err(|error| {
        MissiveError::validation(format!(
            "could not resolve {PUBLIC_AGENT_CARD_PATH} against agent base URL {base_url:?}"
        ))
        .with_source(error)
    })
}

fn validators_from_headers(headers: &HeaderMap) -> AgentCardCacheValidators {
    AgentCardCacheValidators {
        etag: header_to_string(headers, ETAG),
        last_modified: header_to_string(headers, LAST_MODIFIED),
    }
}

fn header_to_string(headers: &HeaderMap, name: reqwest::header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

fn validate_non_empty(label: impl AsRef<str>, value: &str) -> Result<()> {
    let label = label.as_ref();
    if value.trim().is_empty() {
        return Err(MissiveError::protocol(format!("{label} cannot be empty")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn valid_card() -> Value {
        json!({
            "name": "Echo Agent",
            "description": "Replies with whatever it receives.",
            "supportedInterfaces": [
                {
                    "url": "http://127.0.0.1:8080/a2a",
                    "protocolBinding": "HTTP+JSON",
                    "protocolVersion": "1.0"
                },
                {
                    "url": "http://127.0.0.1:8080/rpc",
                    "protocolBinding": "JSONRPC",
                    "protocolVersion": "1.0"
                }
            ],
            "provider": {
                "url": "https://example.test/provider",
                "organization": "Example Agents"
            },
            "version": "2026.5.0",
            "capabilities": {
                "streaming": true,
                "pushNotifications": true,
                "extendedAgentCard": false
            },
            "defaultInputModes": ["text/plain"],
            "defaultOutputModes": ["text/plain", "application/json"],
            "skills": [
                {
                    "id": "echo",
                    "name": "Echo",
                    "description": "Echoes text",
                    "tags": ["test"],
                    "inputModes": ["text/plain"],
                    "outputModes": ["text/plain"]
                }
            ]
        })
    }

    #[test]
    fn crate_info_describes_a2a_layer() {
        let info = crate_info();

        assert_eq!(info.name(), CRATE_NAME);
        assert!(info.purpose().contains("A2A"));
    }

    #[test]
    fn auth_headers_apply_to_requests_and_debug_redacts_values() {
        let client = Client::builder().build().expect("client");
        let mut auth_headers = AuthHeaders::new();
        auth_headers
            .insert("Authorization", "Bearer value-hidden-in-output")
            .expect("authorization header");
        auth_headers
            .insert("X-Api-Key", "value-hidden-in-output")
            .expect("api key header");

        let request = auth_headers
            .apply_to_blocking_request(client.get("http://127.0.0.1:8080/a2a"))
            .expect("auth headers should apply")
            .build()
            .expect("request should build");

        assert_eq!(
            request
                .headers()
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some("Bearer value-hidden-in-output")
        );
        assert_eq!(
            request
                .headers()
                .get("x-api-key")
                .and_then(|value| value.to_str().ok()),
            Some("value-hidden-in-output")
        );
        let rendered = format!("{auth_headers:?}");
        assert!(!rendered.contains("value-hidden-in-output"));
        assert!(rendered.contains("Bearer [REDACTED]"));
    }

    #[test]
    fn service_parameters_apply_default_version_header() {
        let client = Client::builder().build().expect("client");
        let request = ServiceParameters::default()
            .apply_to_blocking_request(client.get("http://127.0.0.1:8080/a2a"))
            .expect("service parameters should apply")
            .build()
            .expect("request should build");

        assert_eq!(
            request
                .headers()
                .get(protocol::SVC_PARAM_VERSION)
                .and_then(|value| value.to_str().ok()),
            Some(protocol::VERSION)
        );
        assert!(
            !request
                .headers()
                .contains_key(protocol::SVC_PARAM_EXTENSIONS)
        );
    }

    #[test]
    fn service_parameters_apply_extensions_and_extra_headers() {
        let client = Client::builder().build().expect("client");
        let parameters = ServiceParameters::new(
            "1.1",
            vec!["urn:example:one".to_owned(), "urn:example:two".to_owned()],
            BTreeMap::from([("A2A-Trace".to_owned(), "trace-1".to_owned())]),
        )
        .expect("valid service parameters");
        let request = parameters
            .apply_to_blocking_request(client.get("http://127.0.0.1:8080/a2a"))
            .expect("service parameters should apply")
            .build()
            .expect("request should build");

        assert_eq!(
            request
                .headers()
                .get(protocol::SVC_PARAM_VERSION)
                .and_then(|value| value.to_str().ok()),
            Some("1.1")
        );
        assert_eq!(
            request
                .headers()
                .get(protocol::SVC_PARAM_EXTENSIONS)
                .and_then(|value| value.to_str().ok()),
            Some("urn:example:one, urn:example:two")
        );
        assert_eq!(
            request
                .headers()
                .get("A2A-Trace")
                .and_then(|value| value.to_str().ok()),
            Some("trace-1")
        );
    }

    #[test]
    fn service_parameters_record_task_and_event_metadata_shape() {
        let parameters = ServiceParameters::new(
            "1.1",
            vec!["urn:example:one".to_owned()],
            BTreeMap::from([("A2A-Trace".to_owned(), "trace-1".to_owned())]),
        )
        .expect("valid service parameters");
        let metadata = parameters.to_metadata().expect("metadata");

        assert_eq!(metadata.get_str(METADATA_A2A_PROTOCOL_VERSION), Some("1.1"));
        assert_eq!(
            metadata.get(METADATA_A2A_EXTENSIONS),
            Some(&json!(["urn:example:one"]))
        );
        assert_eq!(
            metadata.get(METADATA_A2A_SERVICE_PARAMETERS),
            Some(&json!({"A2A-Trace": "trace-1"}))
        );
    }

    #[test]
    fn service_parameters_reject_reserved_extra_headers() {
        let error = ServiceParameters::new(
            "1.0",
            Vec::new(),
            BTreeMap::from([("A2A-Version".to_owned(), "2.0".to_owned())]),
        )
        .expect_err("reserved extra header should fail");

        assert!(error.to_string().contains("reserved header"));
    }

    #[test]
    fn unsupported_version_response_detection_is_specific_to_a2a_error() {
        assert!(response_reports_unsupported_version(
            r#"{"error":{"code":-32009,"message":"version not supported"}}"#
        ));
        assert!(response_reports_unsupported_version(
            r#"{"error":{"data":[{"value":{"reason":"VERSION_NOT_SUPPORTED"}}]}}"#
        ));
        assert!(!response_reports_unsupported_version(
            r#"{"error":{"code":-32004,"message":"unsupported operation"}}"#
        ));
        assert!(!response_reports_unsupported_version(
            "VERSION_NOT_SUPPORTED"
        ));
    }

    #[test]
    fn public_agent_card_url_resolves_at_origin_well_known_path() {
        let url =
            public_agent_card_url("http://127.0.0.1:8080/nested/path").expect("discovery URL");

        assert_eq!(
            url.as_str(),
            "http://127.0.0.1:8080/.well-known/agent-card.json"
        );
    }

    #[test]
    fn agent_card_parses_current_a2a_shape() {
        let card = AgentCard::from_json(valid_card()).expect("valid card");
        let summary = card.summary();

        assert_eq!(summary.name, "Echo Agent");
        assert_eq!(summary.agent_version, "2026.5.0");
        assert_eq!(summary.protocol_versions, vec!["1.0"]);
        assert_eq!(summary.supported_interfaces.len(), 2);
        assert_eq!(summary.skills[0].id, "echo");
        assert_eq!(summary.capabilities.streaming, Some(true));
        assert_eq!(summary.capabilities.push_notifications, Some(true));
    }

    #[test]
    fn agent_card_parses_snake_case_fixture_aliases() {
        let card = json!({
            "name": "Alias Agent",
            "description": "Uses proto-style fixture keys.",
            "supported_interfaces": [
                {
                    "url": "http://127.0.0.1:8080/a2a",
                    "protocol_binding": "HTTP+JSON",
                    "protocol_version": "1.0"
                }
            ],
            "version": "1.0.0",
            "documentation_url": "https://example.test/docs",
            "capabilities": {
                "push_notifications": true,
                "extended_agent_card": true
            },
            "default_input_modes": ["text/plain"],
            "default_output_modes": ["text/plain"],
            "skills": [
                {
                    "id": "echo",
                    "name": "Echo",
                    "description": "Echoes text",
                    "tags": ["test"],
                    "input_modes": ["text/plain"],
                    "output_modes": ["text/plain"]
                }
            ]
        });

        let card = AgentCard::from_json(card).expect("snake_case aliases parse");

        assert_eq!(
            card.documentation_url.as_deref(),
            Some("https://example.test/docs")
        );
        assert_eq!(card.supported_interfaces[0].protocol_binding, "HTTP+JSON");
        assert_eq!(card.capabilities.push_notifications, Some(true));
        assert_eq!(
            card.skills[0].input_modes.as_deref(),
            Some(vec!["text/plain".to_owned()].as_slice())
        );
    }

    #[test]
    fn malformed_agent_card_shape_is_rejected() {
        let error = AgentCard::from_json(json!({"name": "missing required fields"}))
            .expect_err("invalid card should fail");

        assert!(error.to_string().contains("malformed A2A Agent Card JSON"));
    }

    #[test]
    fn agent_card_without_interfaces_is_allowed_for_negotiation_fallback() {
        let mut card = valid_card();
        card.as_object_mut()
            .expect("object")
            .remove("supportedInterfaces");

        let card = AgentCard::from_json(card).expect("legacy card should parse");

        assert!(card.supported_interfaces.is_empty());
    }

    #[test]
    fn negotiation_respects_preference_order_over_card_order() {
        let mut card = valid_card();
        card.as_object_mut().expect("object").insert(
            "supportedInterfaces".to_owned(),
            json!([
                {
                    "url": "http://127.0.0.1:8080/rpc",
                    "protocolBinding": "JSONRPC",
                    "protocolVersion": "1.0"
                },
                {
                    "url": "http://127.0.0.1:8080/a2a",
                    "protocolBinding": "HTTP+JSON",
                    "protocolVersion": "1.0"
                }
            ]),
        );
        let card = AgentCard::from_json(card).expect("valid card");
        let options = InterfaceNegotiationOptions {
            preferred_bindings: vec![HTTP_JSON_BINDING.to_owned(), JSON_RPC_BINDING.to_owned()],
            ..InterfaceNegotiationOptions::default()
        };

        let selected = negotiate_agent_interface(&card, &options).expect("selected interface");

        assert_eq!(selected.binding, HTTP_JSON_BINDING);
        assert_eq!(selected.protocol_binding, "HTTP+JSON");
        assert_eq!(selected.url, "http://127.0.0.1:8080/a2a");
        assert_eq!(selected.source, NegotiatedInterfaceSource::AgentCard);
    }

    #[test]
    fn negotiation_allows_explicit_binding_override() {
        let card = AgentCard::from_json(valid_card()).expect("valid card");
        let options = InterfaceNegotiationOptions {
            binding_override: Some("JSONRPC".to_owned()),
            ..InterfaceNegotiationOptions::default()
        };

        let selected = negotiate_agent_interface(&card, &options).expect("selected interface");

        assert_eq!(selected.binding, JSON_RPC_BINDING);
        assert_eq!(selected.protocol_binding, "JSONRPC");
        assert_eq!(selected.url, "http://127.0.0.1:8080/rpc");
    }

    #[test]
    fn negotiation_rejects_unsupported_remote_bindings_with_local_support_list() {
        let mut card = valid_card();
        card.as_object_mut().expect("object").insert(
            "supportedInterfaces".to_owned(),
            json!([
                {
                    "url": "http://127.0.0.1:8080/grpc",
                    "protocolBinding": "gRPC",
                    "protocolVersion": "1.0"
                }
            ]),
        );
        let card = AgentCard::from_json(card).expect("valid card");

        let error = negotiate_agent_interface(&card, &InterfaceNegotiationOptions::default())
            .expect_err("grpc-only card is not locally supported yet");

        assert!(
            error
                .to_string()
                .contains("supports locally: http+json, json-rpc")
        );
        assert!(error.to_string().contains("remote advertised: grpc"));
    }

    #[test]
    fn negotiation_rejects_unsupported_override_with_local_support_list() {
        let card = AgentCard::from_json(valid_card()).expect("valid card");
        let options = InterfaceNegotiationOptions {
            binding_override: Some("grpc".to_owned()),
            ..InterfaceNegotiationOptions::default()
        };

        let error = negotiate_agent_interface(&card, &options)
            .expect_err("grpc override is not locally supported yet");

        assert!(
            error
                .to_string()
                .contains("binding \"grpc\" is not supported locally")
        );
        assert!(
            error
                .to_string()
                .contains("supports locally: http+json, json-rpc")
        );
        assert!(
            error
                .help()
                .expect("help text")
                .contains("gRPC is recognized")
        );
    }

    #[test]
    fn negotiation_falls_back_when_supported_interfaces_are_missing() {
        let mut card = valid_card();
        card.as_object_mut()
            .expect("object")
            .remove("supportedInterfaces");
        let card = AgentCard::from_json(card).expect("legacy card should parse");
        let options = InterfaceNegotiationOptions {
            fallback_base_url: Some("http://127.0.0.1:8080".to_owned()),
            ..InterfaceNegotiationOptions::default()
        };

        let selected = negotiate_agent_interface(&card, &options).expect("fallback interface");

        assert_eq!(selected.binding, HTTP_JSON_BINDING);
        assert_eq!(selected.url, "http://127.0.0.1:8080");
        assert_eq!(selected.protocol_version, "unknown");
        assert_eq!(selected.source, NegotiatedInterfaceSource::BaseUrlFallback);
    }

    #[test]
    fn negotiation_fallback_uses_explicit_registry_interface_for_override() {
        let mut card = valid_card();
        card.as_object_mut()
            .expect("object")
            .insert("supportedInterfaces".to_owned(), json!([]));
        let card = AgentCard::from_json(card).expect("legacy card should parse");
        let options = InterfaceNegotiationOptions {
            binding_override: Some(JSON_RPC_BINDING.to_owned()),
            fallback_interface_urls: BTreeMap::from([(
                JSON_RPC_BINDING.to_owned(),
                "http://127.0.0.1:8080/rpc".to_owned(),
            )]),
            fallback_base_url: Some("http://127.0.0.1:8080".to_owned()),
            ..InterfaceNegotiationOptions::default()
        };

        let selected = negotiate_agent_interface(&card, &options).expect("fallback interface");

        assert_eq!(selected.binding, JSON_RPC_BINDING);
        assert_eq!(selected.url, "http://127.0.0.1:8080/rpc");
        assert_eq!(selected.source, NegotiatedInterfaceSource::RegistryOverride);
    }
}
