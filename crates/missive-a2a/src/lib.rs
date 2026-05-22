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
    /// remote agent's preferred interface.
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
        if self.supported_interfaces.is_empty() {
            return Err(MissiveError::protocol(
                "A2A Agent Card does not declare supportedInterfaces",
            )
            .with_help("Public Agent Cards must include at least one supportedInterfaces entry."));
        }
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
    fn agent_card_without_interfaces_is_rejected() {
        let mut card = valid_card();
        card.as_object_mut()
            .expect("object")
            .insert("supportedInterfaces".to_owned(), json!([]));

        let error = AgentCard::from_json(card).expect_err("missing interfaces should fail");

        assert!(
            error
                .to_string()
                .contains("does not declare supportedInterfaces")
        );
    }
}
