#![doc = "A2A protocol integration scaffolding for missive."]

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use missive_core::{MissiveError, Result};
use reqwest::StatusCode;
use reqwest::blocking::Client;
use reqwest::header::{ETAG, HeaderMap, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

/// Cargo package name for this crate.
pub const CRATE_NAME: &str = "missive-a2a";

/// Short description of this crate's target responsibility.
pub const CRATE_PURPOSE: &str = "A2A protocol/client integration and compatibility fixtures";

/// Public Agent Card discovery path defined by A2A.
pub const PUBLIC_AGENT_CARD_PATH: &str = "/.well-known/agent-card.json";

const DEFAULT_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(30);
const USER_AGENT: &str = concat!("missive/", env!("CARGO_PKG_VERSION"), " a2a-card-discovery");

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

/// A2A Agent Card as published at `/.well-known/agent-card.json`.
///
/// This type intentionally models the public discovery surface needed by the
/// current `missive agent inspect` command. It follows the A2A v1 lower-camel
/// JSON names while accepting snake_case aliases for fixtures generated from the
/// normative proto names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCard {
    /// Human-readable agent name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Ordered list of supported protocol interfaces. The first entry is the
    /// remote agent's preferred interface. Older/pre-release Agent Cards may
    /// omit this field; interface negotiation has a registry/base-URL fallback
    /// for that compatibility case.
    #[serde(default, alias = "supported_interfaces")]
    pub supported_interfaces: Vec<AgentInterface>,
    /// Optional service provider metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<AgentProvider>,
    /// Agent implementation version.
    pub version: String,
    /// Optional documentation URL.
    #[serde(
        default,
        alias = "documentation_url",
        skip_serializing_if = "Option::is_none"
    )]
    pub documentation_url: Option<String>,
    /// Declared A2A capabilities.
    pub capabilities: AgentCapabilities,
    /// Declared security scheme descriptions.
    #[serde(
        default,
        alias = "security_schemes",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub security_schemes: BTreeMap<String, Value>,
    /// Declared security requirements.
    #[serde(
        default,
        alias = "security_requirements",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub security_requirements: Vec<Value>,
    /// Default input media types.
    #[serde(default, alias = "default_input_modes")]
    pub default_input_modes: Vec<String>,
    /// Default output media types.
    #[serde(default, alias = "default_output_modes")]
    pub default_output_modes: Vec<String>,
    /// Skills advertised by the agent.
    #[serde(default)]
    pub skills: Vec<AgentSkill>,
    /// Optional signature records for the Agent Card.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signatures: Vec<Value>,
    /// Optional icon URL.
    #[serde(default, alias = "icon_url", skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
}

impl AgentCard {
    /// Parses and validates an Agent Card from JSON.
    pub fn from_json(value: Value) -> Result<Self> {
        let card = serde_json::from_value::<Self>(value).map_err(|error| {
            MissiveError::protocol("malformed A2A Agent Card JSON")
                .with_source(error)
                .with_help(
                    "Verify that /.well-known/agent-card.json follows the A2A AgentCard schema.",
                )
        })?;
        card.validate()?;
        Ok(card)
    }

    /// Returns distinct protocol versions exposed by supported interfaces.
    #[must_use]
    pub fn protocol_versions(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        let mut versions = Vec::new();
        for interface in &self.supported_interfaces {
            if seen.insert(interface.protocol_version.clone()) {
                versions.push(interface.protocol_version.clone());
            }
        }
        versions
    }

    /// Returns a compact parsed summary useful for command output and cache
    /// diagnostics.
    #[must_use]
    pub fn summary(&self) -> AgentCardSummary {
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

    fn validate(&self) -> Result<()> {
        validate_non_empty("Agent Card name", &self.name)?;
        validate_non_empty("Agent Card description", &self.description)?;
        validate_non_empty("Agent Card version", &self.version)?;
        if self.skills.is_empty() {
            return Err(
                MissiveError::protocol("A2A Agent Card does not declare any skills")
                    .with_help("Public Agent Cards must include at least one skills entry."),
            );
        }
        for (index, interface) in self.supported_interfaces.iter().enumerate() {
            interface.validate(index)?;
        }
        for (index, skill) in self.skills.iter().enumerate() {
            skill.validate(index)?;
        }
        Ok(())
    }
}

/// Service provider metadata from an Agent Card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProvider {
    /// Provider website or documentation URL.
    pub url: String,
    /// Provider organization name.
    pub organization: String,
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

/// Capability set declared by an Agent Card.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    /// Whether streaming message/task updates are supported.
    #[serde(default)]
    pub streaming: bool,
    /// Whether task push notifications are supported.
    #[serde(default, alias = "push_notifications")]
    pub push_notifications: bool,
    /// Whether task status history is available.
    #[serde(default, alias = "state_transition_history")]
    pub state_transition_history: bool,
    /// Whether authenticated extended Agent Cards are supported.
    #[serde(default, alias = "extended_agent_card")]
    pub extended_agent_card: bool,
    /// Declared protocol extensions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<AgentExtension>,
    /// Unknown capability fields preserved for forward compatibility.
    #[serde(flatten)]
    pub additional: BTreeMap<String, Value>,
}

/// A2A extension declaration inside Agent Card capabilities.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentExtension {
    /// Extension URI.
    pub uri: String,
    /// Optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether clients must understand this extension to interact safely.
    #[serde(default)]
    pub required: bool,
    /// Extension-specific parameters.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, Value>,
}

/// A concrete protocol interface from an Agent Card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInterface {
    /// URL where this interface is available.
    pub url: String,
    /// Protocol binding, for example `HTTP+JSON`, `JSONRPC`, or `GRPC`.
    #[serde(alias = "protocol_binding", alias = "transport")]
    pub protocol_binding: String,
    /// Optional tenant identifier required by this interface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// A2A protocol version exposed by this interface.
    #[serde(alias = "protocol_version")]
    pub protocol_version: String,
}

impl AgentInterface {
    fn validate(&self, index: usize) -> Result<()> {
        validate_non_empty(
            format!("Agent Card supportedInterfaces[{index}].url"),
            &self.url,
        )?;
        validate_non_empty(
            format!("Agent Card supportedInterfaces[{index}].protocolBinding"),
            &self.protocol_binding,
        )?;
        validate_non_empty(
            format!("Agent Card supportedInterfaces[{index}].protocolVersion"),
            &self.protocol_version,
        )?;
        let parsed = Url::parse(&self.url).map_err(|error| {
            MissiveError::protocol(format!(
                "A2A Agent Card supportedInterfaces[{index}].url is not an absolute URL"
            ))
            .with_source(error)
            .with_help(
                "AgentInterface.url must be an absolute URL such as https://agent.example/a2a.",
            )
        })?;
        if parsed.host_str().is_none() {
            return Err(MissiveError::protocol(format!(
                "A2A Agent Card supportedInterfaces[{index}].url must include a host"
            )));
        }
        Ok(())
    }
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

/// Skill metadata declared by an Agent Card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSkill {
    /// Stable skill id.
    pub id: String,
    /// Human-readable skill name.
    pub name: String,
    /// Skill description.
    #[serde(default)]
    pub description: String,
    /// Keywords describing this skill.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Example prompts or tasks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
    /// Skill-specific input media types.
    #[serde(default, alias = "input_modes", skip_serializing_if = "Vec::is_empty")]
    pub input_modes: Vec<String>,
    /// Skill-specific output media types.
    #[serde(default, alias = "output_modes", skip_serializing_if = "Vec::is_empty")]
    pub output_modes: Vec<String>,
    /// Skill-specific security requirements.
    #[serde(
        default,
        alias = "security_requirements",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub security_requirements: Vec<Value>,
}

impl AgentSkill {
    fn validate(&self, index: usize) -> Result<()> {
        validate_non_empty(format!("Agent Card skills[{index}].id"), &self.id)?;
        validate_non_empty(format!("Agent Card skills[{index}].name"), &self.name)
    }
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
        let discovery_url = public_agent_card_url(base_url)?;
        let mut request = self
            .client
            .get(discovery_url.clone())
            .header("Accept", "application/json");

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
        assert!(summary.capabilities.streaming);
        assert!(summary.capabilities.push_notifications);
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
