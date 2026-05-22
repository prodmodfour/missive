//! Configuration discovery, schema validation, and redacted rendering.
//!
//! The configuration layer is intentionally protocol- and store-agnostic. It
//! discovers a TOML file, validates the public schema used by later crates, and
//! exposes redacted rendering helpers so diagnostics and future `doctor` output
//! never need to print secret-like values.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use url::Url;

use crate::{Metadata, MissiveError, Result, parse_config_routing_policy};

/// Current configuration schema marker.
pub const CONFIG_SCHEMA_VERSION: &str = "missive.config.v1";

/// Default A2A protocol version used when config and CLI do not override it.
pub const DEFAULT_A2A_PROTOCOL_VERSION: &str = "1.0";

/// Environment variable that points at one explicit configuration file.
pub const ENV_CONFIG: &str = "MISSIVE_CONFIG";

/// Environment variable that enables repository-local config discovery.
pub const ENV_REPOSITORY_CONFIG: &str = "MISSIVE_REPO_CONFIG";

/// Filename checked when repository-local config discovery is explicitly enabled.
pub const REPOSITORY_CONFIG_FILE: &str = "missive.toml";

/// Alternate hidden filename checked for repository-local config discovery.
pub const REPOSITORY_DOT_CONFIG_FILE: &str = ".missive.toml";

/// Redaction marker used by config rendering.
pub const CONFIG_REDACTED: &str = "[REDACTED]";

const DEFAULT_PROFILE: &str = "default";
const DEFAULT_GATEWAY_BIND: &str = "127.0.0.1:7347";
const DEFAULT_TIMEOUT: &str = "30s";
const DEFAULT_CONNECT_TIMEOUT: &str = "10s";
const DEFAULT_RETRY_BACKOFF: &str = "250ms";
const DEFAULT_MAX_REQUEST_BYTES: u64 = 1024 * 1024;
const DEFAULT_CONCURRENCY: u16 = 4;
const DEFAULT_BUSY_QUEUE_DEPTH: u16 = 32;
const NAMED_CONFIG_IDENTIFIER_MAX_BYTES: usize = 63;
const CONFIG_IDENTIFIER_HELP: &str =
    "Use lowercase ASCII letters or digits, with '-', '_' or '.' only in the middle.";

/// Top-level missive configuration file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MissiveConfig {
    /// Stable config schema version.
    pub schema_version: String,
    /// Profile selected when `--profile` is not provided.
    pub default_profile: String,
    /// Named profiles that scope command defaults and future state.
    pub profiles: BTreeMap<String, ProfileConfig>,
    /// Config-seeded A2A agent entries.
    pub agents: BTreeMap<String, AgentConfig>,
    /// Named authentication references. Raw token values are intentionally not
    /// part of this schema; use env or keyring references instead.
    pub auth_refs: BTreeMap<String, AuthRefConfig>,
    /// Local storage defaults.
    pub storage: StorageConfig,
    /// CLI output defaults used when no output flag overrides them.
    pub output: OutputConfig,
    /// A2A protocol service-parameter defaults.
    pub protocol: ProtocolConfig,
    /// Local gateway defaults.
    pub gateway: GatewayConfig,
    /// Routing defaults for dry-run route planning.
    pub routing: RoutingConfig,
    /// Adapter definitions for future gateway/adapters work.
    pub adapters: BTreeMap<String, AdapterConfig>,
    /// Quality-of-service defaults for transport and orchestration calls.
    pub qos: QosConfig,
}

impl Default for MissiveConfig {
    fn default() -> Self {
        let mut profiles = BTreeMap::new();
        profiles.insert(DEFAULT_PROFILE.to_owned(), ProfileConfig::default());

        Self {
            schema_version: CONFIG_SCHEMA_VERSION.to_owned(),
            default_profile: DEFAULT_PROFILE.to_owned(),
            profiles,
            agents: BTreeMap::new(),
            auth_refs: BTreeMap::new(),
            storage: StorageConfig::default(),
            output: OutputConfig::default(),
            protocol: ProtocolConfig::default(),
            gateway: GatewayConfig::default(),
            routing: RoutingConfig::default(),
            adapters: BTreeMap::new(),
            qos: QosConfig::default(),
        }
    }
}

impl MissiveConfig {
    /// Parses and validates TOML configuration from a string.
    pub fn from_toml_str(input: &str) -> Result<Self> {
        parse_toml_config(input, "inline configuration")
    }

    /// Loads, parses, and validates TOML configuration from a file.
    pub fn from_path(path: &Path) -> Result<Self> {
        let input = fs::read_to_string(path).map_err(|error| {
            MissiveError::io(format!("reading configuration {}", path.display()), error)
                .with_help("Check the path passed with --config or MISSIVE_CONFIG.")
        })?;

        parse_toml_config(&input, &path.display().to_string())
    }

    /// Validates schema-level and cross-reference constraints.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != CONFIG_SCHEMA_VERSION {
            return Err(MissiveError::config(format!(
                "unsupported config schema_version {:?}; expected {CONFIG_SCHEMA_VERSION:?}",
                self.schema_version
            ))
            .with_help(
                "Update the config file to the schema documented in docs/configuration.md.",
            ));
        }

        validate_named_config_identifier("default_profile", &self.default_profile)?;
        if self.profiles.is_empty() {
            return Err(
                MissiveError::config("configuration must define at least one profile").with_help(
                    "Add [profiles.default] or set default_profile to an existing profile.",
                ),
            );
        }
        if !self.profiles.contains_key(&self.default_profile) {
            return Err(MissiveError::config(format!(
                "default_profile {:?} does not match any [profiles.<name>] entry",
                self.default_profile
            ))
            .with_help(
                "Set default_profile to an existing profile name or add the missing profile.",
            ));
        }

        for (name, profile) in &self.profiles {
            validate_named_config_identifier("profile name", name)?;
            profile.validate(name, self)?;
        }

        for (alias, agent) in &self.agents {
            validate_named_config_identifier("agent alias", alias)?;
            agent.validate(alias, self)?;
        }

        for (name, auth_ref) in &self.auth_refs {
            validate_named_config_identifier("auth ref name", name)?;
            auth_ref.validate(name)?;
        }

        self.storage.validate()?;
        self.output.validate()?;
        self.protocol.validate("protocol")?;
        self.gateway.validate()?;
        self.routing.validate("routing")?;
        self.qos.validate("qos")?;

        for (name, adapter) in &self.adapters {
            validate_named_config_identifier("adapter name", name)?;
            adapter.validate(name, self)?;
        }

        Ok(())
    }

    /// Returns the selected profile after checking that it exists.
    pub fn profile(&self, name: &str) -> Result<&ProfileConfig> {
        validate_named_config_identifier("profile name", name)?;
        self.profiles.get(name).ok_or_else(|| {
            MissiveError::config(format!("profile {name:?} is not defined"))
                .with_help("Choose an existing [profiles.<name>] entry or update --profile.")
        })
    }

    /// Renders this configuration as a JSON value with secret-like values redacted.
    pub fn to_redacted_json(&self) -> Result<Value> {
        let value = serde_json::to_value(self).map_err(|error| {
            MissiveError::orchestration("failed to serialize configuration for redacted rendering")
                .with_source(error)
                .with_help(
                    "Report this as a missive bug; the config schema should be serializable.",
                )
        })?;

        Ok(redact_config_json(&value))
    }

    /// Renders this configuration as pretty JSON with secret-like values redacted.
    pub fn to_redacted_pretty_json(&self) -> Result<String> {
        let value = self.to_redacted_json()?;
        serde_json::to_string_pretty(&value).map_err(|error| {
            MissiveError::orchestration("failed to render redacted configuration JSON")
                .with_source(error)
        })
    }
}

/// Profile-scoped command defaults and future state boundaries.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProfileConfig {
    /// Human-facing profile description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Agent alias used when a command accepts an optional agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_agent: Option<String>,
    /// Optional profile-specific storage override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage: Option<StorageConfig>,
    /// Optional profile-specific output override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputConfig>,
    /// Optional profile-specific A2A protocol/service-parameter override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<ProtocolConfig>,
    /// Optional profile-specific gateway override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gateway: Option<GatewayConfig>,
    /// Optional profile-specific routing default override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routing: Option<RoutingConfig>,
    /// Optional profile-specific quality-of-service override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qos: Option<QosConfig>,
    /// Profile metadata for automation and future routing policies.
    pub metadata: Metadata,
}

impl ProfileConfig {
    fn validate(&self, name: &str, config: &MissiveConfig) -> Result<()> {
        if let Some(description) = &self.description {
            validate_optional_text("profile description", description)?;
        }

        if let Some(default_agent) = &self.default_agent {
            validate_named_config_identifier("profile default_agent", default_agent)?;
            if !config.agents.contains_key(default_agent) {
                return Err(MissiveError::config(format!(
                    "profile {name:?} references missing default_agent {default_agent:?}"
                ))
                .with_help(
                    "Add [agents.<alias>] for the default agent or choose an existing alias.",
                ));
            }
        }

        if let Some(storage) = &self.storage {
            storage.validate()?;
        }
        if let Some(output) = &self.output {
            output.validate()?;
        }
        if let Some(protocol) = &self.protocol {
            protocol.validate(&format!("profiles.{name}.protocol"))?;
        }
        if let Some(gateway) = &self.gateway {
            gateway.validate()?;
        }
        if let Some(routing) = &self.routing {
            routing.validate(&format!("profiles.{name}.routing"))?;
        }
        if let Some(qos) = &self.qos {
            qos.validate(&format!("profiles.{name}.qos"))?;
        }

        Ok(())
    }
}

/// Config-seeded A2A agent defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    /// Base URL used for Agent Card discovery and transport defaults.
    pub base_url: String,
    /// Explicit transport/interface URLs keyed by binding name, for example
    /// `"http+json" = "https://agent.example/a2a"`.
    #[serde(alias = "interfaces")]
    pub interface_urls: BTreeMap<String, String>,
    /// Ordered binding preference for this agent.
    pub binding_preference: Vec<String>,
    /// Named auth reference used for this agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_ref: Option<String>,
    /// User-supplied tags for selection and grouping.
    pub tags: Vec<String>,
    /// Human notes about this agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Arbitrary non-secret metadata.
    pub metadata: Metadata,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            interface_urls: BTreeMap::new(),
            binding_preference: vec!["http+json".to_owned(), "json-rpc".to_owned()],
            auth_ref: None,
            tags: Vec::new(),
            notes: None,
            metadata: Metadata::default(),
        }
    }
}

impl AgentConfig {
    fn validate(&self, alias: &str, config: &MissiveConfig) -> Result<()> {
        validate_http_url(&format!("agents.{alias}.base_url"), &self.base_url)?;

        for (binding, url) in &self.interface_urls {
            validate_transport_binding(&format!("agents.{alias}.interface_urls"), binding)?;
            validate_http_url(&format!("agents.{alias}.interface_urls.{binding}"), url)?;
        }

        for binding in &self.binding_preference {
            validate_transport_binding(&format!("agents.{alias}.binding_preference"), binding)?;
        }

        if let Some(auth_ref) = &self.auth_ref {
            validate_named_config_identifier(&format!("agents.{alias}.auth_ref"), auth_ref)?;
            if !config.auth_refs.contains_key(auth_ref) {
                return Err(MissiveError::config(format!(
                    "agent {alias:?} references missing auth_ref {auth_ref:?}"
                ))
                .with_help("Add [auth_refs.<name>] or remove the agent auth_ref."));
            }
        }

        for tag in &self.tags {
            validate_named_config_identifier(&format!("agents.{alias}.tags"), tag)?;
        }

        if let Some(notes) = &self.notes {
            validate_optional_text(&format!("agents.{alias}.notes"), notes)?;
        }

        Ok(())
    }
}

/// Authentication reference kinds supported by the configuration schema.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthRefKind {
    /// Token or header value read from an environment variable.
    #[default]
    Env,
    /// Token read from the platform keyring where available.
    Keyring,
}

/// Named authentication reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthRefConfig {
    /// Where the secret is resolved from. Raw secret values are not accepted.
    pub kind: AuthRefKind,
    /// Environment variable name for `kind = "env"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    /// Keyring service name for `kind = "keyring"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyring_service: Option<String>,
    /// Keyring account name for `kind = "keyring"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyring_account: Option<String>,
    /// HTTP header populated by future auth handling.
    pub header: String,
    /// Optional auth scheme prefix such as `Bearer`.
    pub scheme: String,
}

impl Default for AuthRefConfig {
    fn default() -> Self {
        Self {
            kind: AuthRefKind::Env,
            env: None,
            keyring_service: None,
            keyring_account: None,
            header: "Authorization".to_owned(),
            scheme: "Bearer".to_owned(),
        }
    }
}

impl AuthRefConfig {
    fn validate(&self, name: &str) -> Result<()> {
        validate_header_name(&format!("auth_refs.{name}.header"), &self.header)?;
        validate_auth_scheme(&format!("auth_refs.{name}.scheme"), &self.scheme)?;

        match self.kind {
            AuthRefKind::Env => {
                let env_name = self.env.as_deref().ok_or_else(|| {
                    MissiveError::config(format!(
                        "auth_refs.{name}.env is required for kind = \"env\""
                    ))
                    .with_help(
                        "Set env to the name of an environment variable, not to a token value.",
                    )
                })?;
                validate_env_var_name(&format!("auth_refs.{name}.env"), env_name)?;
            }
            AuthRefKind::Keyring => {
                validate_required_text(
                    &format!("auth_refs.{name}.keyring_service"),
                    self.keyring_service.as_deref(),
                )?;
                validate_required_text(
                    &format!("auth_refs.{name}.keyring_account"),
                    self.keyring_account.as_deref(),
                )?;
            }
        }

        Ok(())
    }
}

/// Storage backend names understood by the schema.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageBackend {
    /// SQLite local state. Repository APIs and migrations are implemented later.
    #[default]
    Sqlite,
}

/// Local storage configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageConfig {
    /// Storage backend. Only `sqlite` is accepted by the current schema.
    pub backend: StorageBackend,
    /// Optional database path override. If omitted, later state-path code chooses
    /// an XDG-compatible per-profile path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database_path: Option<PathBuf>,
}

impl StorageConfig {
    fn validate(&self) -> Result<()> {
        match self.backend {
            StorageBackend::Sqlite => {}
        }

        if let Some(path) = &self.database_path {
            validate_config_path("storage.database_path", path)?;
        }

        Ok(())
    }
}

/// CLI output formats available as config defaults.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    /// Human-readable output.
    #[default]
    Human,
    /// One JSON document.
    Json,
    /// Newline-delimited JSON.
    Ndjson,
    /// Suppress non-error output.
    Quiet,
}

impl OutputFormat {
    /// Stable string used in command summaries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Json => "json",
            Self::Ndjson => "ndjson",
            Self::Quiet => "quiet",
        }
    }
}

/// Terminal color behavior.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorMode {
    /// Auto-detect terminal color support.
    #[default]
    Auto,
    /// Always use color where supported.
    Always,
    /// Never use color.
    Never,
}

/// CLI output defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutputConfig {
    /// Default output format when `--json`, `--ndjson`, and `--quiet` are absent.
    pub format: OutputFormat,
    /// Color behavior for future human diagnostics.
    pub color: ColorMode,
    /// Whether config/log renderers should redact secret-like values.
    pub redact_secrets: bool,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            format: OutputFormat::Human,
            color: ColorMode::Auto,
            redact_secrets: true,
        }
    }
}

impl OutputConfig {
    fn validate(&self) -> Result<()> {
        if !self.redact_secrets {
            return Err(
                MissiveError::config("output.redact_secrets cannot be disabled").with_help(
                    "missive always redacts known secret-like config and output fields.",
                ),
            );
        }

        Ok(())
    }
}

/// A2A protocol service-parameter defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProtocolConfig {
    /// A2A protocol version sent as the `A2A-Version` service parameter.
    pub protocol_version: String,
    /// Extension identifiers sent through `A2A-Extensions` when non-empty.
    pub extensions: Vec<String>,
    /// Additional non-auth service parameters sent as HTTP headers when applicable.
    pub service_parameters: BTreeMap<String, String>,
}

impl Default for ProtocolConfig {
    fn default() -> Self {
        Self {
            protocol_version: DEFAULT_A2A_PROTOCOL_VERSION.to_owned(),
            extensions: Vec::new(),
            service_parameters: BTreeMap::new(),
        }
    }
}

impl ProtocolConfig {
    fn validate(&self, prefix: &str) -> Result<()> {
        validate_protocol_version(
            &format!("{prefix}.protocol_version"),
            &self.protocol_version,
        )?;

        let mut extensions = BTreeSet::new();
        for extension in &self.extensions {
            validate_a2a_extension(&format!("{prefix}.extensions"), extension)?;
            if !extensions.insert(extension) {
                return Err(MissiveError::config(format!(
                    "{prefix}.extensions contains duplicate extension {extension:?}"
                ))
                .with_help("List each A2A extension at most once."));
            }
        }

        for (name, value) in &self.service_parameters {
            validate_header_name(&format!("{prefix}.service_parameters"), name)?;
            if is_reserved_a2a_service_parameter(name) {
                return Err(MissiveError::config(format!(
                    "{prefix}.service_parameters must not redefine reserved A2A service parameter {name:?}"
                ))
                .with_help(
                    "Use protocol_version for A2A-Version and extensions for A2A-Extensions.",
                ));
            }
            validate_service_parameter_value(
                &format!("{prefix}.service_parameters.{name}"),
                value,
            )?;
        }

        Ok(())
    }
}

/// How gateway/adapters handle new input from the same source while an
/// operation is already in flight.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BusyInputMode {
    /// Keep the active operation running and queue the new input for later.
    #[default]
    Queue,
    /// Cancel local waits/subscriptions and request remote task cancellation
    /// where possible before the new input is started.
    Interrupt,
    /// Append the input to the active task/context when A2A state allows it.
    Steer,
}

impl BusyInputMode {
    /// Stable lowercase string representation used in config docs and output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::Interrupt => "interrupt",
            Self::Steer => "steer",
        }
    }
}

/// Busy-input policy shared by gateway, sessions, and future adapters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BusyInputConfig {
    /// Primary busy-input mode for a source.
    pub mode: BusyInputMode,
    /// Fallback mode used when `mode = "steer"` but the active task/context
    /// cannot accept follow-up input.
    pub unsupported_steer_fallback: BusyInputMode,
    /// Whether interrupt mode should request remote A2A `CancelTask` when the
    /// active operation has a cancellable remote task id.
    pub interrupt_remote_cancel: bool,
    /// Maximum number of queued follow-up inputs per source.
    pub max_queue_depth: u16,
}

impl Default for BusyInputConfig {
    fn default() -> Self {
        Self {
            mode: BusyInputMode::Queue,
            unsupported_steer_fallback: BusyInputMode::Queue,
            interrupt_remote_cancel: true,
            max_queue_depth: DEFAULT_BUSY_QUEUE_DEPTH,
        }
    }
}

impl BusyInputConfig {
    fn validate(&self, prefix: &str) -> Result<()> {
        validate_positive_u16(&format!("{prefix}.max_queue_depth"), self.max_queue_depth)?;
        if self.unsupported_steer_fallback == BusyInputMode::Steer {
            return Err(MissiveError::config(format!(
                "{prefix}.unsupported_steer_fallback must be queue or interrupt"
            ))
            .with_help(
                "Use queue to preserve the follow-up input or interrupt to cancel the active task when steering is unsupported.",
            ));
        }
        Ok(())
    }
}

/// Gateway defaults used by later daemon tickets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GatewayConfig {
    /// Whether the profile expects a gateway to run by default.
    pub enabled: bool,
    /// Local bind address for gateway HTTP endpoints.
    pub bind_address: String,
    /// Optional externally reachable URL used for webhooks and docs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_base_url: Option<String>,
    /// Maximum number of concurrent gateway-managed jobs.
    pub job_concurrency: u16,
    /// Busy-input behavior for gateway-managed sources in this profile.
    pub busy_input: BusyInputConfig,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind_address: DEFAULT_GATEWAY_BIND.to_owned(),
            public_base_url: None,
            job_concurrency: DEFAULT_CONCURRENCY,
            busy_input: BusyInputConfig::default(),
        }
    }
}

impl GatewayConfig {
    fn validate(&self) -> Result<()> {
        self.bind_address.parse::<SocketAddr>().map_err(|_| {
            MissiveError::config(format!(
                "gateway.bind_address must be an IP socket address such as {DEFAULT_GATEWAY_BIND:?}"
            ))
            .with_help("Use an explicit IP address and port, for example 127.0.0.1:7347.")
        })?;

        if let Some(url) = &self.public_base_url {
            validate_http_url("gateway.public_base_url", url)?;
        }

        validate_positive_u16("gateway.job_concurrency", self.job_concurrency)?;
        self.busy_input.validate("gateway.busy_input")?;

        Ok(())
    }
}

/// Routing defaults used by dry-run route planning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RoutingConfig {
    /// Policy used when `missive route explain` is not given an explicit policy
    /// and the selected group has no routing policy label.
    pub default_policy: String,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            default_policy: "direct".to_owned(),
        }
    }
}

impl RoutingConfig {
    fn validate(&self, prefix: &str) -> Result<()> {
        parse_config_routing_policy(&format!("{prefix}.default_policy"), &self.default_policy)?;
        Ok(())
    }
}

/// Adapter definition used by later gateway adapter tickets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AdapterConfig {
    /// Adapter kind such as `stdio`, `file`, or `http`.
    pub kind: String,
    /// Whether this adapter is enabled for the profile.
    pub enabled: bool,
    /// Optional profile name used for messages entering this adapter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_profile: Option<String>,
    /// Adapter/source-specific busy-input override. When omitted, the selected
    /// profile's gateway busy-input policy applies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub busy_input: Option<BusyInputConfig>,
    /// Adapter-specific non-secret settings.
    pub settings: Metadata,
}

impl Default for AdapterConfig {
    fn default() -> Self {
        Self {
            kind: String::new(),
            enabled: true,
            session_profile: None,
            busy_input: None,
            settings: Metadata::default(),
        }
    }
}

impl AdapterConfig {
    fn validate(&self, name: &str, config: &MissiveConfig) -> Result<()> {
        validate_named_config_identifier(&format!("adapters.{name}.kind"), &self.kind)?;

        if let Some(profile) = &self.session_profile {
            validate_named_config_identifier(&format!("adapters.{name}.session_profile"), profile)?;
            if !config.profiles.contains_key(profile) {
                return Err(MissiveError::config(format!(
                    "adapter {name:?} references missing session_profile {profile:?}"
                ))
                .with_help("Choose an existing [profiles.<name>] entry for this adapter."));
            }
        }

        if let Some(busy_input) = &self.busy_input {
            busy_input.validate(&format!("adapters.{name}.busy_input"))?;
        }

        Ok(())
    }
}

/// Quality-of-service defaults for later transport and orchestration work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QosConfig {
    /// Default overall command timeout.
    pub timeout: String,
    /// Connection timeout for outbound HTTP transports.
    pub connect_timeout: String,
    /// Number of retry attempts for retryable operations.
    pub retry_attempts: u8,
    /// Backoff between retry attempts.
    pub retry_backoff: String,
    /// Maximum request payload bytes accepted by local parsing and adapters.
    pub max_request_bytes: u64,
    /// Maximum local concurrency for profile-scoped work.
    pub concurrency: u16,
}

impl Default for QosConfig {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT.to_owned(),
            connect_timeout: DEFAULT_CONNECT_TIMEOUT.to_owned(),
            retry_attempts: 2,
            retry_backoff: DEFAULT_RETRY_BACKOFF.to_owned(),
            max_request_bytes: DEFAULT_MAX_REQUEST_BYTES,
            concurrency: DEFAULT_CONCURRENCY,
        }
    }
}

impl QosConfig {
    fn validate(&self, prefix: &str) -> Result<()> {
        validate_duration(&format!("{prefix}.timeout"), &self.timeout)?;
        validate_duration(&format!("{prefix}.connect_timeout"), &self.connect_timeout)?;
        validate_duration(&format!("{prefix}.retry_backoff"), &self.retry_backoff)?;

        if self.retry_attempts > 10 {
            return Err(MissiveError::config(format!(
                "{prefix}.retry_attempts must be at most 10"
            ))
            .with_help("Use a small bounded retry count so CLI commands fail deterministically."));
        }
        if self.max_request_bytes == 0 {
            return Err(MissiveError::config(format!(
                "{prefix}.max_request_bytes must be greater than zero"
            )));
        }
        validate_positive_u16(&format!("{prefix}.concurrency"), self.concurrency)?;

        Ok(())
    }
}

/// Discovered configuration source kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigSourceKind {
    /// Path passed through the CLI `--config` flag.
    ExplicitPath,
    /// Path provided by `MISSIVE_CONFIG`.
    Environment,
    /// Repository-local `missive.toml` or `.missive.toml` requested explicitly.
    RepositoryLocal,
    /// XDG-compatible user or system configuration path.
    Xdg,
    /// Built-in defaults because no config file was discovered.
    BuiltInDefault,
}

impl ConfigSourceKind {
    /// Stable string used in command summaries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExplicitPath => "explicit_path",
            Self::Environment => "environment",
            Self::RepositoryLocal => "repository_local",
            Self::Xdg => "xdg",
            Self::BuiltInDefault => "built_in_default",
        }
    }
}

/// Location selected by configuration discovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigSource {
    /// Source category.
    pub kind: ConfigSourceKind,
    /// Concrete path when a file backed the source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

impl ConfigSource {
    fn path(kind: ConfigSourceKind, path: PathBuf) -> Self {
        Self {
            kind,
            path: Some(path),
        }
    }

    fn built_in_default() -> Self {
        Self {
            kind: ConfigSourceKind::BuiltInDefault,
            path: None,
        }
    }
}

/// Configuration plus source and selected-profile metadata.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LoadedConfig {
    /// Parsed and validated configuration.
    pub config: MissiveConfig,
    /// Source selected by discovery.
    pub source: ConfigSource,
    /// Selected profile name after applying CLI/config defaults.
    pub selected_profile: String,
}

impl LoadedConfig {
    /// Returns the selected profile definition.
    pub fn selected_profile_config(&self) -> Result<&ProfileConfig> {
        self.config.profile(&self.selected_profile)
    }

    /// Returns the effective output format after applying the selected profile override.
    pub fn output_format(&self) -> Result<OutputFormat> {
        let profile = self.selected_profile_config()?;
        Ok(profile
            .output
            .as_ref()
            .map_or(self.config.output.format, |output| output.format))
    }

    /// Returns the effective A2A protocol/service-parameter config for the selected profile.
    pub fn protocol_config(&self) -> Result<ProtocolConfig> {
        let profile = self.selected_profile_config()?;
        Ok(profile
            .protocol
            .clone()
            .unwrap_or_else(|| self.config.protocol.clone()))
    }

    /// Returns the effective gateway defaults for the selected profile.
    pub fn gateway_config(&self) -> Result<GatewayConfig> {
        let profile = self.selected_profile_config()?;
        Ok(profile
            .gateway
            .clone()
            .unwrap_or_else(|| self.config.gateway.clone()))
    }

    /// Returns the effective busy-input policy for the selected profile and
    /// optional adapter/source name.
    ///
    /// The profile's gateway busy-input policy is the base. If `source_name`
    /// names a configured adapter and that adapter has a `busy_input` override,
    /// the adapter/source override wins.
    pub fn busy_input_config_for_source(
        &self,
        source_name: Option<&str>,
    ) -> Result<BusyInputConfig> {
        if let Some(source_name) = source_name {
            validate_named_config_identifier("busy input source name", source_name)?;
            if let Some(adapter) = self.config.adapters.get(source_name) {
                if let Some(busy_input) = &adapter.busy_input {
                    return Ok(busy_input.clone());
                }
            }
        }
        Ok(self.gateway_config()?.busy_input)
    }

    /// Returns the effective routing defaults for the selected profile.
    pub fn routing_config(&self) -> Result<RoutingConfig> {
        let profile = self.selected_profile_config()?;
        Ok(profile
            .routing
            .clone()
            .unwrap_or_else(|| self.config.routing.clone()))
    }

    /// Renders the loaded config, source, and profile with secret-like values redacted.
    pub fn to_redacted_json(&self) -> Result<Value> {
        let value = serde_json::to_value(self).map_err(|error| {
            MissiveError::orchestration("failed to serialize loaded configuration")
                .with_source(error)
        })?;

        Ok(redact_config_json(&value))
    }
}

/// Testable configuration discovery context.
#[derive(Debug, Clone)]
pub struct ConfigDiscovery {
    explicit_path: Option<PathBuf>,
    selected_profile: Option<String>,
    allow_repository_config: bool,
    env: BTreeMap<String, String>,
    current_dir: PathBuf,
}

impl Default for ConfigDiscovery {
    fn default() -> Self {
        Self {
            explicit_path: None,
            selected_profile: None,
            allow_repository_config: false,
            env: BTreeMap::new(),
            current_dir: PathBuf::from("."),
        }
    }
}

impl ConfigDiscovery {
    /// Creates discovery using no environment and the current directory `.`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates discovery using process environment and current working directory.
    pub fn from_process() -> Result<Self> {
        let current_dir = env::current_dir().map_err(|error| {
            MissiveError::io("reading current directory for config discovery", error)
        })?;

        Ok(Self::new()
            .with_env(env::vars())
            .with_current_dir(current_dir))
    }

    /// Sets the `--config` path.
    #[must_use]
    pub fn with_explicit_path(mut self, path: Option<PathBuf>) -> Self {
        self.explicit_path = path;
        self
    }

    /// Sets the selected profile name.
    #[must_use]
    pub fn with_selected_profile(mut self, profile: Option<String>) -> Self {
        self.selected_profile = profile;
        self
    }

    /// Enables or disables repository-local discovery.
    #[must_use]
    pub const fn with_repository_config(mut self, enabled: bool) -> Self {
        self.allow_repository_config = enabled;
        self
    }

    /// Sets an environment map for deterministic tests.
    #[must_use]
    pub fn with_env<I, K, V>(mut self, env: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.env = env
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect();
        self
    }

    /// Sets the working directory used to resolve relative paths and repository config.
    #[must_use]
    pub fn with_current_dir(mut self, current_dir: PathBuf) -> Self {
        self.current_dir = current_dir;
        self
    }

    /// Returns the path selected by discovery, if any.
    pub fn discover_path(&self) -> Result<Option<ConfigSource>> {
        if let Some(path) = &self.explicit_path {
            return Ok(Some(ConfigSource::path(
                ConfigSourceKind::ExplicitPath,
                self.resolve_path(path)?,
            )));
        }

        if let Some(path) = self.env.get(ENV_CONFIG) {
            if path.trim().is_empty() {
                return Err(
                    MissiveError::config(format!("{ENV_CONFIG} is set but empty")).with_help(
                        "Unset MISSIVE_CONFIG or set it to a TOML configuration file path.",
                    ),
                );
            }
            return Ok(Some(ConfigSource::path(
                ConfigSourceKind::Environment,
                self.resolve_path(Path::new(path))?,
            )));
        }

        if self.repository_config_requested() {
            if let Some(path) = self.find_repository_config() {
                return Ok(Some(ConfigSource::path(
                    ConfigSourceKind::RepositoryLocal,
                    path,
                )));
            }
        }

        if let Some(path) = self.find_xdg_config() {
            return Ok(Some(ConfigSource::path(ConfigSourceKind::Xdg, path)));
        }

        Ok(None)
    }

    /// Loads the discovered config file or built-in defaults, then validates the selected profile.
    pub fn load(&self) -> Result<LoadedConfig> {
        let source = self
            .discover_path()?
            .unwrap_or_else(ConfigSource::built_in_default);
        let config = if let Some(path) = &source.path {
            MissiveConfig::from_path(path)?
        } else {
            let config = MissiveConfig::default();
            config.validate()?;
            config
        };

        let selected_profile = self
            .selected_profile
            .clone()
            .unwrap_or_else(|| config.default_profile.clone());
        config.profile(&selected_profile)?;

        Ok(LoadedConfig {
            config,
            source,
            selected_profile,
        })
    }

    fn resolve_path(&self, path: &Path) -> Result<PathBuf> {
        if path.as_os_str().is_empty() {
            return Err(MissiveError::config("configuration path cannot be empty")
                .with_help("Pass a non-empty path to --config or MISSIVE_CONFIG."));
        }

        if path.is_absolute() {
            Ok(path.to_path_buf())
        } else {
            Ok(self.current_dir.join(path))
        }
    }

    fn repository_config_requested(&self) -> bool {
        self.allow_repository_config
            || self
                .env
                .get(ENV_REPOSITORY_CONFIG)
                .is_some_and(|value| env_flag_enabled(value))
    }

    fn find_repository_config(&self) -> Option<PathBuf> {
        let mut directory = self.current_dir.as_path();

        loop {
            for filename in [REPOSITORY_CONFIG_FILE, REPOSITORY_DOT_CONFIG_FILE] {
                let candidate = directory.join(filename);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }

            if directory.join(".git").exists() {
                return None;
            }

            directory = directory.parent()?;
        }
    }

    fn find_xdg_config(&self) -> Option<PathBuf> {
        self.xdg_config_candidates()
            .into_iter()
            .find(|candidate| candidate.is_file())
    }

    fn xdg_config_candidates(&self) -> Vec<PathBuf> {
        let mut candidates = Vec::new();

        if let Some(home) = self
            .env
            .get("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
        {
            push_config_candidates(&mut candidates, Path::new(home));
        } else if let Some(home) = self.env.get("HOME").filter(|value| !value.is_empty()) {
            push_config_candidates(&mut candidates, &Path::new(home).join(".config"));
        }

        let system_dirs = self
            .env
            .get("XDG_CONFIG_DIRS")
            .map(String::as_str)
            .unwrap_or("/etc/xdg");
        for directory in system_dirs.split(':').filter(|part| !part.is_empty()) {
            push_config_candidates(&mut candidates, Path::new(directory));
        }

        candidates
    }
}

fn push_config_candidates(candidates: &mut Vec<PathBuf>, root: &Path) {
    candidates.push(root.join("missive").join("config.toml"));
    candidates.push(root.join("missive.toml"));
}

fn parse_toml_config(input: &str, label: &str) -> Result<MissiveConfig> {
    let config: MissiveConfig = toml::from_str(input).map_err(|error| {
        let location = error
            .span()
            .map(|span| line_column(input, span.start))
            .map(|(line, column)| format!(" near line {line}, column {column}"))
            .unwrap_or_default();
        MissiveError::config(format!(
            "failed to parse TOML configuration at {label}{location}"
        ))
        .with_help("Check TOML syntax and the schema documented in docs/configuration.md.")
    })?;

    config.validate()?;
    Ok(config)
}

fn line_column(input: &str, byte_index: usize) -> (usize, usize) {
    let mut line = 1;
    let mut column = 1;

    for (index, character) in input.char_indices() {
        if index >= byte_index {
            break;
        }
        if character == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }

    (line, column)
}

fn validate_named_config_identifier(kind: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return invalid_config_identifier(kind, "value cannot be empty");
    }
    if value.len() > NAMED_CONFIG_IDENTIFIER_MAX_BYTES {
        return invalid_config_identifier(
            kind,
            format!(
                "value is {} bytes, but the maximum is {NAMED_CONFIG_IDENTIFIER_MAX_BYTES}",
                value.len()
            ),
        );
    }

    let bytes = value.as_bytes();
    if !is_ascii_lower_alphanumeric(bytes[0]) {
        return invalid_config_identifier(
            kind,
            "value must start with a lowercase ASCII letter or digit",
        );
    }
    if !is_ascii_lower_alphanumeric(bytes[bytes.len() - 1]) {
        return invalid_config_identifier(
            kind,
            "value must end with a lowercase ASCII letter or digit",
        );
    }

    for byte in bytes {
        if is_ascii_lower_alphanumeric(*byte) || matches!(*byte, b'-' | b'_' | b'.') {
            continue;
        }
        return invalid_config_identifier(
            kind,
            "value must contain only lowercase ASCII letters, digits, '-', '_' or '.'",
        );
    }

    Ok(())
}

fn invalid_config_identifier(kind: &str, reason: impl Into<String>) -> Result<()> {
    Err(
        MissiveError::config(format!("invalid {kind}: {}", reason.into()))
            .with_help(CONFIG_IDENTIFIER_HELP),
    )
}

fn validate_transport_binding(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(
            MissiveError::config(format!("{field} binding cannot be empty"))
                .with_help("Use bindings such as http+json, json-rpc, or grpc."),
        );
    }
    if value.len() > 64 {
        return Err(MissiveError::config(format!("{field} binding is too long"))
            .with_help("Use short transport binding names such as http+json or json-rpc."));
    }

    let bytes = value.as_bytes();
    if !is_ascii_lower_alphanumeric(bytes[0])
        || !is_ascii_lower_alphanumeric(bytes[bytes.len() - 1])
    {
        return Err(MissiveError::config(format!(
            "{field} binding must start and end with a lowercase ASCII letter or digit"
        ))
        .with_help("Use bindings such as http+json, json-rpc, or grpc."));
    }

    for byte in bytes {
        if is_ascii_lower_alphanumeric(*byte) || matches!(*byte, b'+' | b'-' | b'_' | b'.') {
            continue;
        }
        return Err(MissiveError::config(format!(
            "{field} binding contains unsupported characters"
        ))
        .with_help("Use lowercase ASCII letters, digits, '+', '-', '_' or '.'."));
    }

    Ok(())
}

fn validate_protocol_version(field: &str, value: &str) -> Result<()> {
    validate_required_text(field, Some(value))?;
    if value.len() > 64 {
        return Err(
            MissiveError::config(format!("{field} must be at most 64 bytes"))
                .with_help("Use a short A2A protocol version such as 1.0."),
        );
    }
    if value
        .bytes()
        .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')))
    {
        return Err(
            MissiveError::config(format!("{field} contains unsupported characters"))
                .with_help("Use ASCII letters, digits, '.', '-' or '_' only."),
        );
    }
    Ok(())
}

fn validate_a2a_extension(field: &str, value: &str) -> Result<()> {
    validate_required_text(field, Some(value))?;
    if value.len() > 512 {
        return Err(MissiveError::config(format!(
            "{field} extension identifier must be at most 512 bytes"
        )));
    }
    if value.chars().any(char::is_whitespace) || value.contains(',') {
        return Err(MissiveError::config(format!(
            "{field} extension identifiers must not contain whitespace or commas"
        ))
        .with_help("Use compact URI-like extension identifiers."));
    }
    Ok(())
}

fn validate_service_parameter_value(field: &str, value: &str) -> Result<()> {
    validate_required_text(field, Some(value))?;
    if value.len() > 8 * 1024 {
        return Err(MissiveError::config(format!(
            "{field} service parameter value must be at most 8192 bytes"
        )));
    }
    Ok(())
}

fn is_reserved_a2a_service_parameter(value: &str) -> bool {
    value.eq_ignore_ascii_case("A2A-Version") || value.eq_ignore_ascii_case("A2A-Extensions")
}

fn validate_http_url(field: &str, value: &str) -> Result<()> {
    validate_required_text(field, Some(value))?;
    if value.chars().any(char::is_whitespace) || value.chars().any(char::is_control) {
        return Err(MissiveError::config(format!(
            "{field} must be an HTTP(S) URL without whitespace or control characters"
        ))
        .with_help("Use a URL such as https://agent.example or http://127.0.0.1:8080."));
    }

    let parsed = Url::parse(value).map_err(|_| {
        MissiveError::config(format!("{field} must be a valid absolute HTTP(S) URL"))
            .with_help("Use a URL such as https://agent.example or http://127.0.0.1:8080.")
    })?;

    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(MissiveError::config(format!(
            "{field} must use http or https and include a host"
        ))
        .with_help("Use a URL such as https://agent.example or http://127.0.0.1:8080."));
    }

    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(
            MissiveError::config(format!("{field} must not include embedded credentials"))
                .with_help(
                    "Use auth_refs for authentication material instead of username/password URLs.",
                ),
        );
    }

    Ok(())
}

fn validate_header_name(field: &str, value: &str) -> Result<()> {
    validate_required_text(field, Some(value))?;

    if value
        .bytes()
        .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'-')))
    {
        return Err(
            MissiveError::config(format!("{field} must be an HTTP header name"))
                .with_help("Use ASCII letters, digits, and '-' only, for example Authorization."),
        );
    }

    Ok(())
}

fn validate_auth_scheme(field: &str, value: &str) -> Result<()> {
    validate_required_text(field, Some(value))?;

    if value
        .bytes()
        .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
    {
        return Err(
            MissiveError::config(format!("{field} must be an auth scheme token"))
                .with_help("Use a scheme such as Bearer, Basic, Token, or ApiKey."),
        );
    }

    Ok(())
}

fn validate_env_var_name(field: &str, value: &str) -> Result<()> {
    validate_required_text(field, Some(value))?;

    let bytes = value.as_bytes();
    if !(bytes[0].is_ascii_alphabetic() || bytes[0] == b'_') {
        return Err(MissiveError::config(format!(
            "{field} must start with an ASCII letter or underscore"
        ))
        .with_help("Set env to an environment variable name, not to a token value."));
    }

    if bytes
        .iter()
        .any(|byte| !(byte.is_ascii_alphanumeric() || *byte == b'_'))
    {
        return Err(MissiveError::config(format!(
            "{field} must contain only ASCII letters, digits, and underscore"
        ))
        .with_help("Set env to an environment variable name, not to a token value."));
    }

    Ok(())
}

fn validate_config_path(field: &str, path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() {
        return Err(MissiveError::config(format!("{field} cannot be empty")));
    }

    let path_text = path.to_string_lossy();
    if path_text.chars().any(char::is_control) {
        return Err(MissiveError::config(format!(
            "{field} cannot contain control characters"
        )));
    }

    Ok(())
}

fn validate_optional_text(field: &str, value: &str) -> Result<()> {
    if value.chars().any(char::is_control) {
        return Err(MissiveError::config(format!(
            "{field} cannot contain control characters"
        )));
    }

    Ok(())
}

fn validate_required_text(field: &str, value: Option<&str>) -> Result<()> {
    let value = value.ok_or_else(|| MissiveError::config(format!("{field} is required")))?;
    if value.trim().is_empty() {
        return Err(MissiveError::config(format!("{field} cannot be empty")));
    }
    validate_optional_text(field, value)
}

fn validate_duration(field: &str, value: &str) -> Result<()> {
    validate_required_text(field, Some(value))?;

    let split_at = value
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(value.len());
    let (number, unit) = value.split_at(split_at);

    if number.is_empty() || unit.is_empty() {
        return Err(MissiveError::config(format!(
            "{field} must include a positive integer and unit"
        ))
        .with_help("Use duration strings such as 250ms, 30s, 2m, or 1h."));
    }

    let amount = number.parse::<u64>().map_err(|_| {
        MissiveError::config(format!("{field} duration amount is invalid"))
            .with_help("Use duration strings such as 250ms, 30s, 2m, or 1h.")
    })?;
    if amount == 0 || !matches!(unit, "ms" | "s" | "m" | "h") {
        return Err(MissiveError::config(format!(
            "{field} must use a positive duration with unit ms, s, m, or h"
        ))
        .with_help("Use duration strings such as 250ms, 30s, 2m, or 1h."));
    }

    Ok(())
}

fn validate_positive_u16(field: &str, value: u16) -> Result<()> {
    if value == 0 {
        return Err(MissiveError::config(format!(
            "{field} must be greater than zero"
        )));
    }

    Ok(())
}

const fn is_ascii_lower_alphanumeric(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

fn env_flag_enabled(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn redact_config_json(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(redact_config_object(object, false)),
        Value::Array(items) => Value::Array(items.iter().map(redact_config_json).collect()),
        Value::String(text) => Value::String(redact_config_text(text)),
        other => other.clone(),
    }
}

fn redact_config_object(object: &Map<String, Value>, headers_context: bool) -> Map<String, Value> {
    let mut redacted = Map::new();

    for (key, value) in object {
        let lower_key = key.to_ascii_lowercase();
        let child_headers_context = lower_key == "headers" || lower_key == "http_headers";
        let value = if headers_context {
            redact_config_header_value(key, value)
        } else if is_secret_key(key) {
            Value::String(CONFIG_REDACTED.to_owned())
        } else if child_headers_context {
            match value {
                Value::Object(headers) => Value::Object(redact_config_object(headers, true)),
                other => redact_config_json(other),
            }
        } else {
            redact_config_json(value)
        };

        redacted.insert(key.clone(), value);
    }

    redacted
}

fn redact_config_header_value(name: &str, value: &Value) -> Value {
    match value {
        Value::String(text) => {
            if is_secret_key(name) {
                Value::String(redact_header_value(text))
            } else {
                Value::String(redact_config_text(text))
            }
        }
        other if is_secret_key(name) => {
            let _ = other;
            Value::String(CONFIG_REDACTED.to_owned())
        }
        other => redact_config_json(other),
    }
}

fn redact_config_text(input: &str) -> String {
    let mut output = input.to_owned();

    for scheme in ["Bearer", "Basic", "Token", "ApiKey"] {
        output = redact_after_auth_scheme(&output, scheme);
    }

    output
}

fn redact_after_auth_scheme(input: &str, scheme: &str) -> String {
    let needle = format!("{scheme} ");
    let mut output = String::with_capacity(input.len());
    let mut remaining = input;

    while let Some(index) = find_ascii_case_insensitive(remaining, &needle) {
        let prefix_end = index + needle.len();
        output.push_str(&remaining[..prefix_end]);
        remaining = &remaining[prefix_end..];

        let secret_end = remaining
            .find(|character: char| {
                character.is_whitespace() || character == ',' || character == ';'
            })
            .unwrap_or(remaining.len());

        if secret_end > 0 {
            output.push_str(CONFIG_REDACTED);
        }
        remaining = &remaining[secret_end..];
    }

    output.push_str(remaining);
    output
}

fn redact_header_value(value: &str) -> String {
    let trimmed = value.trim();
    if let Some((scheme, _)) = trimmed.split_once(char::is_whitespace) {
        if is_auth_scheme(scheme) {
            return format!("{scheme} {CONFIG_REDACTED}");
        }
    }

    CONFIG_REDACTED.to_owned()
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())
}

fn is_auth_scheme(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "bearer" | "basic" | "token" | "apikey" | "api-key"
    )
}

fn is_secret_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();

    matches!(
        normalized.as_str(),
        "authorization"
            | "proxyauthorization"
            | "cookie"
            | "setcookie"
            | "apikey"
            | "xapikey"
            | "xauthtoken"
            | "xcsrftoken"
            | "token"
            | "secret"
            | "password"
            | "passwd"
            | "credential"
            | "credentials"
            | "clientsecret"
            | "refreshtoken"
            | "accesstoken"
            | "accesskey"
            | "privatekey"
            | "sessiontoken"
    ) || normalized.ends_with("token")
        || normalized.ends_with("secret")
        || normalized.ends_with("password")
        || normalized.ends_with("apikey")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::ErrorCategory;

    const VALID_MINIMAL: &str = r#"
schema_version = "missive.config.v1"
default_profile = "default"

[profiles.default]
description = "Local development defaults"
default_agent = "echo"

[agents.echo]
base_url = "http://127.0.0.1:8080"
auth_ref = "example-env"
tags = ["local", "mock"]

[agents.echo.interface_urls]
"http+json" = "http://127.0.0.1:8080/a2a"

[auth_refs.example-env]
kind = "env"
env = "MISSIVE_EXAMPLE_TOKEN"
header = "Authorization"
scheme = "Bearer"
"#;

    #[test]
    fn default_config_validates_and_selects_default_profile() {
        let config = MissiveConfig::default();

        config.validate().expect("default config should validate");
        assert_eq!(
            config.protocol.protocol_version,
            DEFAULT_A2A_PROTOCOL_VERSION
        );
        assert_eq!(
            config.profile("default").expect("profile"),
            &ProfileConfig::default()
        );
    }

    #[test]
    fn protocol_config_parses_and_profile_override_wins() {
        let config = MissiveConfig::from_toml_str(
            r#"
schema_version = "missive.config.v1"
default_profile = "default"

[protocol]
protocol_version = "1.0"
extensions = ["urn:example:base"]

[protocol.service_parameters]
A2A-Trace = "base-trace"

[profiles.default]

[profiles.default.protocol]
protocol_version = "1.1"
extensions = ["urn:example:profile"]

[profiles.default.protocol.service_parameters]
A2A-Tenant = "tenant-a"
"#,
        )
        .expect("protocol config should parse");
        let loaded = LoadedConfig {
            config,
            source: ConfigSource::built_in_default(),
            selected_profile: "default".to_owned(),
        };

        let protocol = loaded.protocol_config().expect("effective protocol");

        assert_eq!(protocol.protocol_version, "1.1");
        assert_eq!(protocol.extensions, vec!["urn:example:profile"]);
        assert_eq!(
            protocol
                .service_parameters
                .get("A2A-Tenant")
                .map(String::as_str),
            Some("tenant-a")
        );
    }

    #[test]
    fn protocol_config_rejects_reserved_service_parameter_names() {
        let error = MissiveConfig::from_toml_str(
            r#"
schema_version = "missive.config.v1"
default_profile = "default"

[profiles.default]

[protocol.service_parameters]
A2A-Version = "2.0"
"#,
        )
        .expect_err("reserved service parameter should fail");

        assert_eq!(error.category(), ErrorCategory::Config);
        assert!(error.to_string().contains("reserved A2A service parameter"));
    }

    #[test]
    fn routing_config_parses_profile_override_and_rejects_invalid_policy() {
        let config = MissiveConfig::from_toml_str(
            r#"
schema_version = "missive.config.v1"
default_profile = "default"

[routing]
default_policy = "broadcast"

[profiles.default]

[profiles.default.routing]
default_policy = "weighted"
"#,
        )
        .expect("routing config should parse");
        let loaded = LoadedConfig {
            config,
            source: ConfigSource::built_in_default(),
            selected_profile: "default".to_owned(),
        };

        assert_eq!(
            loaded.routing_config().expect("routing").default_policy,
            "weighted"
        );

        let error = MissiveConfig::from_toml_str(
            r#"
schema_version = "missive.config.v1"
default_profile = "default"

[routing]
default_policy = "least-latency"

[profiles.default]
"#,
        )
        .expect_err("invalid routing policy should fail");

        assert_eq!(error.category(), ErrorCategory::Config);
        assert!(error.to_string().contains("routing.default_policy"));
        assert!(error.help().is_some());
    }

    #[test]
    fn busy_input_config_parses_profile_and_source_overrides() {
        let config = MissiveConfig::from_toml_str(
            r#"
schema_version = "missive.config.v1"
default_profile = "default"

[gateway.busy_input]
mode = "queue"
unsupported_steer_fallback = "queue"
interrupt_remote_cancel = true
max_queue_depth = 4

[profiles.default]

[profiles.default.gateway]
enabled = true
bind_address = "127.0.0.1:7348"
job_concurrency = 2

[profiles.default.gateway.busy_input]
mode = "interrupt"
unsupported_steer_fallback = "queue"
interrupt_remote_cancel = false
max_queue_depth = 8

[adapters.stdio]
kind = "stdio"
enabled = true

[adapters.stdio.busy_input]
mode = "steer"
unsupported_steer_fallback = "interrupt"
interrupt_remote_cancel = true
max_queue_depth = 3
"#,
        )
        .expect("busy input config should parse");
        let loaded = LoadedConfig {
            config,
            source: ConfigSource::built_in_default(),
            selected_profile: "default".to_owned(),
        };

        let profile_policy = loaded
            .busy_input_config_for_source(None)
            .expect("profile busy input");
        assert_eq!(profile_policy.mode, BusyInputMode::Interrupt);
        assert!(!profile_policy.interrupt_remote_cancel);
        assert_eq!(profile_policy.max_queue_depth, 8);

        let source_policy = loaded
            .busy_input_config_for_source(Some("stdio"))
            .expect("source busy input");
        assert_eq!(source_policy.mode, BusyInputMode::Steer);
        assert_eq!(
            source_policy.unsupported_steer_fallback,
            BusyInputMode::Interrupt
        );
        assert_eq!(source_policy.max_queue_depth, 3);
    }

    #[test]
    fn busy_input_config_rejects_recursive_fallback_and_empty_queue() {
        let recursive = MissiveConfig::from_toml_str(
            r#"
schema_version = "missive.config.v1"
default_profile = "default"

[profiles.default]

[gateway.busy_input]
mode = "steer"
unsupported_steer_fallback = "steer"
"#,
        )
        .expect_err("recursive steer fallback should fail");
        assert_eq!(recursive.category(), ErrorCategory::Config);
        assert!(recursive.to_string().contains("unsupported_steer_fallback"));

        let empty_queue = MissiveConfig::from_toml_str(
            r#"
schema_version = "missive.config.v1"
default_profile = "default"

[profiles.default]

[gateway.busy_input]
max_queue_depth = 0
"#,
        )
        .expect_err("empty busy input queue should fail");
        assert_eq!(empty_queue.category(), ErrorCategory::Config);
        assert!(empty_queue.to_string().contains("max_queue_depth"));
    }

    #[test]
    fn minimal_config_parses_and_validates() {
        let config = MissiveConfig::from_toml_str(VALID_MINIMAL).expect("valid config");

        assert_eq!(config.default_profile, "default");
        assert!(config.agents.contains_key("echo"));
        assert_eq!(
            config.auth_refs["example-env"].env.as_deref(),
            Some("MISSIVE_EXAMPLE_TOKEN")
        );
    }

    #[test]
    fn config_examples_load_successfully() {
        let examples_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("examples/config");
        let mut loaded = Vec::new();

        for entry in fs::read_dir(&examples_dir).expect("examples/config should exist") {
            let entry = entry.expect("example entry");
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("toml") {
                continue;
            }

            MissiveConfig::from_path(&path)
                .unwrap_or_else(|error| panic!("{} should load: {error}", path.display()));
            loaded.push(path);
        }

        assert!(
            !loaded.is_empty(),
            "at least one config example should be tested"
        );
    }

    #[test]
    fn invalid_configs_have_actionable_diagnostics() {
        let error = MissiveConfig::from_toml_str(
            r#"
schema_version = "missive.config.v1"
default_profile = "missing"

[profiles.default]
"#,
        )
        .expect_err("missing profile should fail");

        assert_eq!(error.category(), ErrorCategory::Config);
        assert!(error.to_string().contains("default_profile"));
        assert!(error.help().is_some());
    }

    #[test]
    fn unknown_fields_fail_without_echoing_values() {
        let hidden = "value-hidden-in-output";
        let error = MissiveConfig::from_toml_str(&format!(
            r#"
schema_version = "missive.config.v1"
default_profile = "default"
secret_token = "{hidden}"

[profiles.default]
"#
        ))
        .expect_err("unknown field should fail");
        let rendered = format!("{} {:?}", error, error.to_report());

        assert!(rendered.contains("failed to parse TOML configuration"));
        assert!(!rendered.contains(hidden));
    }

    #[test]
    fn embedded_url_credentials_are_rejected_without_echoing_secret() {
        let hidden = "value-hidden-in-output";
        let error = MissiveConfig::from_toml_str(&format!(
            r#"
schema_version = "missive.config.v1"
default_profile = "default"

[profiles.default]
default_agent = "echo"

[agents.echo]
base_url = "https://user:{hidden}@agent.example"
"#
        ))
        .expect_err("embedded URL credentials should fail");
        let rendered = format!("{} {:?}", error, error.to_report());

        assert!(rendered.contains("embedded credentials"));
        assert!(!rendered.contains(hidden));
    }

    #[test]
    fn config_redacted_rendering_hides_secret_like_metadata() {
        let hidden = "value-hidden-in-output";
        let config = MissiveConfig::from_toml_str(&format!(
            r#"
schema_version = "missive.config.v1"
default_profile = "default"

[profiles.default]
default_agent = "echo"

[agents.echo]
base_url = "http://127.0.0.1:8080"
tags = ["local"]

[agents.echo.metadata]
token = "{hidden}"
public_note = "Bearer {hidden}"
"#
        ))
        .expect("config should load");

        let redacted = config.to_redacted_pretty_json().expect("redacted JSON");

        assert!(!redacted.contains(hidden));
        assert!(redacted.contains(CONFIG_REDACTED));
    }

    #[test]
    fn explicit_config_path_wins_over_environment_and_xdg() {
        let temp = tempdir().expect("tempdir");
        let explicit_path = temp.path().join("explicit.toml");
        let env_path = temp.path().join("env.toml");
        let xdg_dir = temp.path().join("xdg").join("missive");
        fs::create_dir_all(&xdg_dir).expect("xdg dir");
        fs::write(&explicit_path, VALID_MINIMAL).expect("write explicit");
        fs::write(&env_path, VALID_MINIMAL.replace("echo", "env-agent")).expect("write env");
        fs::write(xdg_dir.join("config.toml"), VALID_MINIMAL).expect("write xdg");

        let loaded = ConfigDiscovery::new()
            .with_current_dir(temp.path().to_path_buf())
            .with_explicit_path(Some(explicit_path.clone()))
            .with_env([
                (ENV_CONFIG.to_owned(), env_path.display().to_string()),
                (
                    "XDG_CONFIG_HOME".to_owned(),
                    temp.path().join("xdg").display().to_string(),
                ),
            ])
            .load()
            .expect("config should load");

        assert_eq!(loaded.source.kind, ConfigSourceKind::ExplicitPath);
        assert_eq!(loaded.source.path.as_deref(), Some(explicit_path.as_path()));
    }

    #[test]
    fn missive_config_environment_path_is_discovered() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("missive.toml");
        fs::write(&config_path, VALID_MINIMAL).expect("write config");

        let loaded = ConfigDiscovery::new()
            .with_current_dir(temp.path().to_path_buf())
            .with_env([(ENV_CONFIG.to_owned(), config_path.display().to_string())])
            .load()
            .expect("config should load");

        assert_eq!(loaded.source.kind, ConfigSourceKind::Environment);
        assert_eq!(loaded.selected_profile, "default");
    }

    #[test]
    fn xdg_config_home_is_discovered_after_env_precedence() {
        let temp = tempdir().expect("tempdir");
        let config_dir = temp.path().join("xdg").join("missive");
        fs::create_dir_all(&config_dir).expect("create xdg config dir");
        let config_path = config_dir.join("config.toml");
        fs::write(&config_path, VALID_MINIMAL).expect("write config");

        let loaded = ConfigDiscovery::new()
            .with_current_dir(temp.path().to_path_buf())
            .with_env([(
                "XDG_CONFIG_HOME".to_owned(),
                temp.path().join("xdg").display().to_string(),
            )])
            .load()
            .expect("config should load");

        assert_eq!(loaded.source.kind, ConfigSourceKind::Xdg);
        assert_eq!(loaded.source.path.as_deref(), Some(config_path.as_path()));
    }

    #[test]
    fn repository_config_requires_explicit_request() {
        let temp = tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        let nested = repo.join("crates").join("demo");
        fs::create_dir_all(repo.join(".git")).expect("create git dir");
        fs::create_dir_all(&nested).expect("create nested dir");
        let config_path = repo.join(REPOSITORY_CONFIG_FILE);
        fs::write(&config_path, VALID_MINIMAL).expect("write config");

        let default_loaded = ConfigDiscovery::new()
            .with_current_dir(nested.clone())
            .load()
            .expect("built-in default should load");
        assert_eq!(default_loaded.source.kind, ConfigSourceKind::BuiltInDefault);

        let repo_loaded = ConfigDiscovery::new()
            .with_current_dir(nested)
            .with_repository_config(true)
            .load()
            .expect("repo config should load");

        assert_eq!(repo_loaded.source.kind, ConfigSourceKind::RepositoryLocal);
        assert_eq!(
            repo_loaded.source.path.as_deref(),
            Some(config_path.as_path())
        );
    }

    #[test]
    fn profile_selection_is_validated() {
        let loaded = ConfigDiscovery::new()
            .with_selected_profile(Some("missing".to_owned()))
            .load()
            .expect_err("missing profile should fail");

        assert_eq!(loaded.category(), ErrorCategory::Config);
        assert!(loaded.to_string().contains("profile"));
    }

    #[test]
    fn loaded_config_redaction_includes_source_without_secret_values() {
        let config = MissiveConfig::from_toml_str(VALID_MINIMAL).expect("valid config");
        let loaded = LoadedConfig {
            config,
            source: ConfigSource::path(ConfigSourceKind::ExplicitPath, PathBuf::from("demo.toml")),
            selected_profile: "default".to_owned(),
        };

        let value = loaded.to_redacted_json().expect("redacted loaded config");

        assert_eq!(value["source"]["kind"], json!("explicit_path"));
        assert_eq!(value["selected_profile"], json!("default"));
    }
}
