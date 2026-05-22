//! Agent registry CLI commands.
//!
//! This module implements the first non-skeletal CLI behavior for `missive`:
//! local profile-scoped agent registry management backed by the SQLite store.
//! A2A Agent Card discovery and remote inspection remain intentionally out of
//! scope for this ticket.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use clap::{Args, Subcommand};
use missive_core::{
    AgentAlias, LoadedConfig, Metadata, MissiveError, Result, TransportName,
    config::{AgentConfig, AuthRefConfig, AuthRefKind as ConfigAuthRefKind},
};
use missive_store::{
    AgentRecord, AgentSource, AgentUpsert, AuthRefKind as StoreAuthRefKind, AuthRefUpsert,
    AuthSecretStorage, ProcessLock, ProcessLockKind, StatePathResolver, Store,
};
use serde::Serialize;
use serde_json::Value;
use url::Url;

use crate::output::{OutputMode, redact_json, redact_text, render_success};

const NAMED_IDENTIFIER_MAX_BYTES: usize = 63;
const NAMED_IDENTIFIER_HELP: &str =
    "Use lowercase ASCII letters or digits, with '-', '_' or '.' only in the middle.";
const NOTES_MAX_BYTES: usize = 8 * 1024;

/// Agent registry subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum AgentCommands {
    /// Add one writable local agent registry entry.
    Add(AgentAddArgs),
    /// Remove one writable local agent registry entry.
    Remove(AgentAliasArgs),
    /// List local and config-seeded agent registry entries.
    List,
    /// Show one local or config-seeded agent registry entry.
    Show(AgentAliasArgs),
    /// Rename one writable local agent registry entry.
    Rename(AgentRenameArgs),
}

impl AgentCommands {
    /// Stable command spelling used in structured output messages.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Add(_) => "add",
            Self::Remove(_) => "remove",
            Self::List => "list",
            Self::Show(_) => "show",
            Self::Rename(_) => "rename",
        }
    }
}

/// Arguments for `missive agent add`.
#[derive(Debug, Clone, Args)]
pub struct AgentAddArgs {
    /// Agent alias used by other missive commands.
    pub alias: String,
    /// Base URL used for public Agent Card discovery.
    pub base_url: String,
    /// Explicit interface URL as BINDING=URL, for example http+json=http://127.0.0.1:8080/a2a.
    #[arg(long = "interface", value_name = "BINDING=URL")]
    pub interfaces: Vec<String>,
    /// Ordered binding preference; repeat to override the default http+json,json-rpc order.
    #[arg(long = "binding-preference", value_name = "BINDING")]
    pub binding_preference: Vec<String>,
    /// Named auth reference from configuration.
    #[arg(long = "auth-ref", value_name = "NAME")]
    pub auth_ref: Option<String>,
    /// Selection tag; repeat for multiple tags.
    #[arg(long = "tag", value_name = "TAG")]
    pub tags: Vec<String>,
    /// Human notes for this agent.
    #[arg(long = "notes", value_name = "TEXT")]
    pub notes: Option<String>,
    /// Non-secret metadata entry as KEY=VALUE; VALUE is parsed as JSON when possible.
    #[arg(long = "metadata", value_name = "KEY=VALUE")]
    pub metadata: Vec<String>,
}

/// Arguments for commands that take a single alias.
#[derive(Debug, Clone, Args)]
pub struct AgentAliasArgs {
    /// Agent alias.
    pub alias: String,
}

/// Arguments for `missive agent rename`.
#[derive(Debug, Clone, Args)]
pub struct AgentRenameArgs {
    /// Existing agent alias.
    pub alias: String,
    /// New agent alias.
    pub new_alias: String,
}

#[derive(Debug)]
struct AgentRegistry {
    store: Store,
    profile: String,
    _lock: ProcessLock,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct AgentView {
    alias: String,
    source: String,
    base_url: String,
    interface_urls: BTreeMap<String, String>,
    binding_preference: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    auth_ref: Option<String>,
    tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
    metadata: Metadata,
    read_only: bool,
    created_at: String,
    updated_at: String,
}

impl AgentView {
    fn from_record(record: &AgentRecord) -> Self {
        Self {
            alias: record.alias.as_str().to_owned(),
            source: record.source.as_str().to_owned(),
            base_url: record.base_url.clone(),
            interface_urls: record
                .interface_urls
                .iter()
                .map(|(binding, url)| (binding.as_str().to_owned(), url.clone()))
                .collect(),
            binding_preference: record
                .binding_preference
                .iter()
                .map(|binding| binding.as_str().to_owned())
                .collect(),
            auth_ref: record.auth_ref_name.clone(),
            tags: record.tags.clone(),
            notes: record.notes.clone(),
            metadata: record.metadata.clone(),
            read_only: record.read_only,
            created_at: record.created_at.to_rfc3339(),
            updated_at: record.updated_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct AgentListOutput {
    profile: String,
    count: usize,
    agents: Vec<AgentView>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct AgentShowOutput {
    profile: String,
    agent: AgentView,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct AgentActionOutput {
    profile: String,
    action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_alias: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<AgentView>,
    message: String,
}

/// Executes one agent registry subcommand.
pub(crate) fn execute_agent_command<W>(
    command: &AgentCommands,
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let registry = open_agent_registry(loaded_config, environment)?;

    match command {
        AgentCommands::Add(args) => add_agent(args, loaded_config, registry, mode, writer),
        AgentCommands::Remove(args) => remove_agent(args, registry, mode, writer),
        AgentCommands::List => list_agents(registry, mode, writer),
        AgentCommands::Show(args) => show_agent(args, registry, mode, writer),
        AgentCommands::Rename(args) => rename_agent(args, registry, mode, writer),
    }
}

fn open_agent_registry(
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
) -> Result<AgentRegistry> {
    let resolver = StatePathResolver::new().with_env(
        environment
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    let paths = resolver.resolve_loaded(loaded_config)?;
    paths.ensure_directories()?;
    let lock = ProcessLock::acquire(&paths, ProcessLockKind::StateMutation)?;
    let store = Store::open(paths.database_path())?;
    sync_config_auth_refs(&store, loaded_config)?;
    sync_config_agents(&store, loaded_config)?;

    Ok(AgentRegistry {
        store,
        profile: loaded_config.selected_profile.clone(),
        _lock: lock,
    })
}

fn sync_config_auth_refs(store: &Store, loaded_config: &LoadedConfig) -> Result<()> {
    for (name, auth_ref) in &loaded_config.config.auth_refs {
        store.upsert_auth_ref(&config_auth_ref_upsert(name, auth_ref))?;
    }

    Ok(())
}

fn config_auth_ref_upsert(name: &str, auth_ref: &AuthRefConfig) -> AuthRefUpsert {
    let (kind, secret_storage) = match auth_ref.kind {
        ConfigAuthRefKind::Env => (StoreAuthRefKind::Env, AuthSecretStorage::Env),
        ConfigAuthRefKind::Keyring => (StoreAuthRefKind::Keyring, AuthSecretStorage::Keyring),
    };

    AuthRefUpsert {
        name: name.to_owned(),
        kind,
        header_name: auth_ref.header.clone(),
        scheme: Some(auth_ref.scheme.clone()),
        env_var: auth_ref.env.clone(),
        keyring_service: auth_ref.keyring_service.clone(),
        keyring_account: auth_ref.keyring_account.clone(),
        secret_storage,
        metadata: Metadata::new(),
    }
}

fn sync_config_agents(store: &Store, loaded_config: &LoadedConfig) -> Result<()> {
    for (alias, agent) in &loaded_config.config.agents {
        store.upsert_agent(&config_agent_upsert(alias, agent)?)?;
    }

    Ok(())
}

fn config_agent_upsert(alias: &str, agent: &AgentConfig) -> Result<AgentUpsert> {
    let alias = AgentAlias::new(alias.to_owned())?;
    let mut input = AgentUpsert::new(alias, agent.base_url.clone());
    input.source = AgentSource::ConfigSeed;
    input.interface_urls = transport_url_map(&agent.interface_urls)?;
    input.binding_preference = transport_list(&agent.binding_preference)?;
    input.auth_ref_name = agent.auth_ref.clone();
    input.tags = agent.tags.clone();
    input.notes = agent.notes.clone();
    input.metadata = agent.metadata.clone();
    input.read_only = true;
    Ok(input)
}

fn add_agent<W>(
    args: &AgentAddArgs,
    loaded_config: &LoadedConfig,
    registry: AgentRegistry,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let alias = parse_alias(&args.alias)?;
    ensure_agent_absent(&registry.store, &alias)?;
    validate_http_url("agent base_url", &args.base_url)?;

    let mut input = AgentUpsert::new(alias.clone(), args.base_url.clone());
    input.interface_urls = parse_interface_urls(&args.interfaces)?;
    if !args.binding_preference.is_empty() {
        input.binding_preference = parse_binding_preference(&args.binding_preference)?;
    }
    input.auth_ref_name = parse_auth_ref(args.auth_ref.as_deref(), loaded_config)?;
    input.tags = parse_tags(&args.tags)?;
    input.notes = parse_notes(args.notes.as_deref())?;
    input.metadata = parse_metadata(&args.metadata)?;

    let record = registry.store.upsert_agent(&input)?;
    let view = AgentView::from_record(&record);
    let message = format!(
        "Added agent '{}' with base URL {}",
        view.alias, view.base_url
    );
    let output = AgentActionOutput {
        profile: registry.profile,
        action: "add".to_owned(),
        previous_alias: None,
        agent: Some(view),
        message,
    };

    render_agent_action(writer, mode, "agent_add", &output)
}

fn remove_agent<W>(
    args: &AgentAliasArgs,
    registry: AgentRegistry,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let alias = parse_alias(&args.alias)?;
    let record = get_existing_agent(&registry.store, &alias)?;
    ensure_writable(&record, "remove")?;
    let view = AgentView::from_record(&record);
    if !registry.store.delete_agent(&alias)? {
        return Err(MissiveError::storage(format!(
            "agent {:?} disappeared before it could be removed",
            alias.as_str()
        )));
    }

    let message = format!("Removed agent '{}'", alias.as_str());
    let output = AgentActionOutput {
        profile: registry.profile,
        action: "remove".to_owned(),
        previous_alias: None,
        agent: Some(view),
        message,
    };

    render_agent_action(writer, mode, "agent_remove", &output)
}

fn list_agents<W>(registry: AgentRegistry, mode: OutputMode, writer: &mut W) -> Result<()>
where
    W: Write,
{
    let records = registry.store.list_agents()?;
    let agents = records
        .iter()
        .map(AgentView::from_record)
        .collect::<Vec<_>>();
    let output = AgentListOutput {
        profile: registry.profile,
        count: agents.len(),
        agents,
    };

    render_agent_list(writer, mode, &output)
}

fn show_agent<W>(
    args: &AgentAliasArgs,
    registry: AgentRegistry,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let alias = parse_alias(&args.alias)?;
    let record = get_existing_agent(&registry.store, &alias)?;
    let output = AgentShowOutput {
        profile: registry.profile,
        agent: AgentView::from_record(&record),
    };

    render_agent_show(writer, mode, &output)
}

fn rename_agent<W>(
    args: &AgentRenameArgs,
    mut registry: AgentRegistry,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let old_alias = parse_alias(&args.alias)?;
    let new_alias = parse_alias(&args.new_alias)?;
    if old_alias == new_alias {
        return Err(
            MissiveError::validation("agent rename requires a different new alias")
                .with_help("Choose a new alias that does not match the current alias."),
        );
    }

    let old_record = get_existing_agent(&registry.store, &old_alias)?;
    ensure_writable(&old_record, "rename")?;
    ensure_agent_absent(&registry.store, &new_alias)?;

    let new_record = registry.store.transaction(|transaction| {
        let mut input = agent_upsert_from_record(&old_record, new_alias.clone());
        input.read_only = false;
        let new_record = transaction.upsert_agent(&input)?;
        if !transaction.delete_agent(&old_alias)? {
            return Err(MissiveError::storage(format!(
                "agent {:?} disappeared before it could be renamed",
                old_alias.as_str()
            )));
        }
        Ok(new_record)
    })?;

    let view = AgentView::from_record(&new_record);
    let message = format!(
        "Renamed agent '{}' to '{}'",
        old_alias.as_str(),
        new_alias.as_str()
    );
    let output = AgentActionOutput {
        profile: registry.profile,
        action: "rename".to_owned(),
        previous_alias: Some(old_alias.into_string()),
        agent: Some(view),
        message,
    };

    render_agent_action(writer, mode, "agent_rename", &output)
}

fn get_existing_agent(store: &Store, alias: &AgentAlias) -> Result<AgentRecord> {
    store.get_agent(alias)?.ok_or_else(|| {
        MissiveError::validation(format!("agent {:?} is not registered", alias.as_str()))
            .with_help("Run 'missive agent list' to see registered aliases.")
    })
}

fn ensure_agent_absent(store: &Store, alias: &AgentAlias) -> Result<()> {
    if let Some(record) = store.get_agent(alias)? {
        let detail = if record.read_only {
            "a read-only config-seeded entry"
        } else {
            "an existing local entry"
        };
        return Err(MissiveError::validation(format!(
            "agent alias {:?} already exists as {detail}",
            alias.as_str()
        ))
        .with_help("Choose a different alias or remove/rename the existing writable entry."));
    }

    Ok(())
}

fn ensure_writable(record: &AgentRecord, action: &str) -> Result<()> {
    if record.read_only {
        return Err(MissiveError::validation(format!(
            "agent {:?} is read-only and cannot be {action}d",
            record.alias.as_str()
        ))
        .with_help(
            "Config-seeded agents come from the loaded missive config; edit the config file instead.",
        ));
    }

    Ok(())
}

fn agent_upsert_from_record(record: &AgentRecord, alias: AgentAlias) -> AgentUpsert {
    AgentUpsert {
        alias,
        source: record.source,
        base_url: record.base_url.clone(),
        interface_urls: record.interface_urls.clone(),
        binding_preference: record.binding_preference.clone(),
        auth_ref_name: record.auth_ref_name.clone(),
        tags: record.tags.clone(),
        notes: record.notes.clone(),
        metadata: record.metadata.clone(),
        agent_card_json: record.agent_card_json.clone(),
        agent_card_etag: record.agent_card_etag.clone(),
        agent_card_last_modified: record.agent_card_last_modified.clone(),
        agent_card_fetched_at: record.agent_card_fetched_at,
        read_only: record.read_only,
    }
}

fn parse_alias(value: &str) -> Result<AgentAlias> {
    AgentAlias::new(value.to_owned())
}

fn parse_interface_urls(values: &[String]) -> Result<BTreeMap<TransportName, String>> {
    let mut interfaces = BTreeMap::new();
    for value in values {
        let (binding, url) = split_key_value("--interface", value)?;
        let binding = TransportName::new(binding.to_owned())?;
        validate_http_url("agent interface URL", url)?;
        if interfaces.insert(binding.clone(), url.to_owned()).is_some() {
            return Err(MissiveError::validation(format!(
                "duplicate interface binding {:?}",
                binding.as_str()
            ))
            .with_help("Pass each --interface binding at most once."));
        }
    }
    Ok(interfaces)
}

fn parse_binding_preference(values: &[String]) -> Result<Vec<TransportName>> {
    let mut seen = BTreeSet::new();
    let mut preference = Vec::new();
    for value in values {
        let binding = TransportName::new(value.to_owned())?;
        if !seen.insert(binding.clone()) {
            return Err(MissiveError::validation(format!(
                "duplicate binding preference {:?}",
                binding.as_str()
            ))
            .with_help("Pass each --binding-preference value at most once."));
        }
        preference.push(binding);
    }
    Ok(preference)
}

fn parse_auth_ref(value: Option<&str>, loaded_config: &LoadedConfig) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    validate_named_cli_identifier("auth ref", value)?;
    if !loaded_config.config.auth_refs.contains_key(value) {
        return Err(MissiveError::validation(format!(
            "auth ref {value:?} is not defined in the loaded config"
        ))
        .with_help("Add [auth_refs.<name>] to the selected config file or omit --auth-ref."));
    }
    Ok(Some(value.to_owned()))
}

fn parse_tags(values: &[String]) -> Result<Vec<String>> {
    let mut tags = Vec::with_capacity(values.len());
    for value in values {
        validate_named_cli_identifier("tag", value)?;
        tags.push(value.clone());
    }
    Ok(tags)
}

fn parse_notes(value: Option<&str>) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.len() > NOTES_MAX_BYTES {
        return Err(MissiveError::validation(format!(
            "agent notes are {} bytes, but the maximum is {NOTES_MAX_BYTES}",
            value.len()
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(MissiveError::validation(
            "agent notes cannot contain control characters",
        ));
    }
    Ok(Some(value.to_owned()))
}

fn parse_metadata(values: &[String]) -> Result<Metadata> {
    let mut metadata = Metadata::new();
    for value in values {
        let (key, raw_value) = split_key_value("--metadata", value)?;
        let parsed_value =
            serde_json::from_str(raw_value).unwrap_or_else(|_| Value::String(raw_value.to_owned()));
        if metadata.insert(key.to_owned(), parsed_value)?.is_some() {
            return Err(
                MissiveError::validation(format!("duplicate metadata key {key:?}"))
                    .with_help("Pass each --metadata key at most once."),
            );
        }
    }
    Ok(metadata)
}

fn transport_url_map(values: &BTreeMap<String, String>) -> Result<BTreeMap<TransportName, String>> {
    values
        .iter()
        .map(|(binding, url)| Ok((TransportName::new(binding.clone())?, url.clone())))
        .collect()
}

fn transport_list(values: &[String]) -> Result<Vec<TransportName>> {
    values
        .iter()
        .map(|binding| TransportName::new(binding.clone()))
        .collect()
}

fn split_key_value<'a>(flag: &str, value: &'a str) -> Result<(&'a str, &'a str)> {
    let Some((key, raw_value)) = value.split_once('=') else {
        return Err(MissiveError::validation(format!(
            "{flag} value {value:?} must use KEY=VALUE syntax"
        )));
    };
    if key.is_empty() || raw_value.is_empty() {
        return Err(MissiveError::validation(format!(
            "{flag} value {value:?} must include a non-empty key and value"
        )));
    }
    Ok((key, raw_value))
}

fn validate_http_url(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(MissiveError::validation(format!("{field} cannot be empty")));
    }
    if value.chars().any(char::is_whitespace) || value.chars().any(char::is_control) {
        return Err(MissiveError::validation(format!(
            "{field} must be an HTTP(S) URL without whitespace or control characters"
        ))
        .with_help("Use a URL such as https://agent.example or http://127.0.0.1:8080."));
    }

    let parsed = Url::parse(value).map_err(|error| {
        MissiveError::validation(format!("{field} must be a valid absolute HTTP(S) URL"))
            .with_source(error)
            .with_help("Use a URL such as https://agent.example or http://127.0.0.1:8080.")
    })?;

    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(MissiveError::validation(format!(
            "{field} must use http or https and include a host"
        ))
        .with_help("Use a URL such as https://agent.example or http://127.0.0.1:8080."));
    }

    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(MissiveError::validation(format!(
            "{field} must not include embedded credentials"
        ))
        .with_help(
            "Use auth refs for authentication material instead of username/password URLs.",
        ));
    }

    Ok(())
}

fn validate_named_cli_identifier(kind: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return invalid_named_cli_identifier(kind, "value cannot be empty");
    }
    if value.len() > NAMED_IDENTIFIER_MAX_BYTES {
        return invalid_named_cli_identifier(
            kind,
            format!(
                "value is {} bytes, but the maximum is {NAMED_IDENTIFIER_MAX_BYTES}",
                value.len()
            ),
        );
    }

    let bytes = value.as_bytes();
    if !is_ascii_lower_alphanumeric(bytes[0]) {
        return invalid_named_cli_identifier(
            kind,
            "value must start with a lowercase ASCII letter or digit",
        );
    }
    if !is_ascii_lower_alphanumeric(bytes[bytes.len() - 1]) {
        return invalid_named_cli_identifier(
            kind,
            "value must end with a lowercase ASCII letter or digit",
        );
    }

    for byte in bytes {
        if is_ascii_lower_alphanumeric(*byte) || matches!(*byte, b'-' | b'_' | b'.') {
            continue;
        }
        return invalid_named_cli_identifier(
            kind,
            "value must contain only lowercase ASCII letters, digits, '-', '_' or '.'",
        );
    }

    Ok(())
}

fn invalid_named_cli_identifier(kind: &str, reason: impl Into<String>) -> Result<()> {
    Err(
        MissiveError::validation(format!("invalid {kind}: {}", reason.into()))
            .with_help(NAMED_IDENTIFIER_HELP),
    )
}

const fn is_ascii_lower_alphanumeric(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

fn render_agent_list<W>(writer: &mut W, mode: OutputMode, output: &AgentListOutput) -> Result<()>
where
    W: Write,
{
    let message = if output.count == 0 {
        format!("No agents registered for profile '{}'.", output.profile)
    } else {
        format!(
            "Listed {} agent{} for profile '{}'",
            output.count,
            if output.count == 1 { "" } else { "s" },
            output.profile
        )
    };

    match mode {
        OutputMode::Human => write_agent_list_human(writer, output),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, "agent_list", output, &message)
        }
    }
}

fn write_agent_list_human<W>(writer: &mut W, output: &AgentListOutput) -> Result<()>
where
    W: Write,
{
    if output.agents.is_empty() {
        return writeln!(
            writer,
            "No agents registered for profile '{}'.",
            redact_text(&output.profile)
        )
        .map_err(|error| MissiveError::io("writing agent list output", error));
    }

    writeln!(
        writer,
        "Agents for profile '{}':",
        redact_text(&output.profile)
    )
    .map_err(|error| MissiveError::io("writing agent list output", error))?;
    for agent in &output.agents {
        let tags = if agent.tags.is_empty() {
            "-".to_owned()
        } else {
            agent.tags.join(",")
        };
        let read_only = if agent.read_only { " read-only" } else { "" };
        writeln!(
            writer,
            "  {}  {}{}  {}  tags={}",
            redact_text(&agent.alias),
            redact_text(&agent.source),
            read_only,
            redact_text(&agent.base_url),
            redact_text(&tags),
        )
        .map_err(|error| MissiveError::io("writing agent list output", error))?;
    }

    Ok(())
}

fn render_agent_show<W>(writer: &mut W, mode: OutputMode, output: &AgentShowOutput) -> Result<()>
where
    W: Write,
{
    let message = format!("Showing agent '{}'", output.agent.alias);
    match mode {
        OutputMode::Human => write_agent_show_human(writer, output),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, "agent_show", output, &message)
        }
    }
}

fn write_agent_show_human<W>(writer: &mut W, output: &AgentShowOutput) -> Result<()>
where
    W: Write,
{
    let agent = &output.agent;
    writeln!(writer, "Agent {}", redact_text(&agent.alias))
        .map_err(|error| MissiveError::io("writing agent show output", error))?;
    writeln!(writer, "  profile: {}", redact_text(&output.profile))
        .map_err(|error| MissiveError::io("writing agent show output", error))?;
    writeln!(writer, "  source: {}", redact_text(&agent.source))
        .map_err(|error| MissiveError::io("writing agent show output", error))?;
    writeln!(writer, "  read_only: {}", agent.read_only)
        .map_err(|error| MissiveError::io("writing agent show output", error))?;
    writeln!(writer, "  base_url: {}", redact_text(&agent.base_url))
        .map_err(|error| MissiveError::io("writing agent show output", error))?;
    write_map(writer, "  interface_urls", &agent.interface_urls)?;
    writeln!(
        writer,
        "  binding_preference: {}",
        if agent.binding_preference.is_empty() {
            "-".to_owned()
        } else {
            redact_text(&agent.binding_preference.join(", "))
        }
    )
    .map_err(|error| MissiveError::io("writing agent show output", error))?;
    writeln!(
        writer,
        "  auth_ref: {}",
        agent
            .auth_ref
            .as_deref()
            .map(redact_text)
            .unwrap_or_else(|| "-".to_owned())
    )
    .map_err(|error| MissiveError::io("writing agent show output", error))?;
    writeln!(
        writer,
        "  tags: {}",
        if agent.tags.is_empty() {
            "-".to_owned()
        } else {
            redact_text(&agent.tags.join(", "))
        }
    )
    .map_err(|error| MissiveError::io("writing agent show output", error))?;
    writeln!(
        writer,
        "  notes: {}",
        agent
            .notes
            .as_deref()
            .map(redact_text)
            .unwrap_or_else(|| "-".to_owned())
    )
    .map_err(|error| MissiveError::io("writing agent show output", error))?;
    writeln!(writer, "  metadata: {}", metadata_json(&agent.metadata)?)
        .map_err(|error| MissiveError::io("writing agent show output", error))?;
    writeln!(writer, "  created_at: {}", agent.created_at)
        .map_err(|error| MissiveError::io("writing agent show output", error))?;
    writeln!(writer, "  updated_at: {}", agent.updated_at)
        .map_err(|error| MissiveError::io("writing agent show output", error))?;

    Ok(())
}

fn write_map<W>(writer: &mut W, label: &str, values: &BTreeMap<String, String>) -> Result<()>
where
    W: Write,
{
    if values.is_empty() {
        writeln!(writer, "{label}: -")
            .map_err(|error| MissiveError::io("writing agent show output", error))?;
        return Ok(());
    }

    writeln!(writer, "{label}:")
        .map_err(|error| MissiveError::io("writing agent show output", error))?;
    for (key, value) in values {
        writeln!(writer, "    {}: {}", redact_text(key), redact_text(value))
            .map_err(|error| MissiveError::io("writing agent show output", error))?;
    }
    Ok(())
}

fn render_agent_action<W>(
    writer: &mut W,
    mode: OutputMode,
    kind: &str,
    output: &AgentActionOutput,
) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => writeln!(writer, "{}", redact_text(&output.message))
            .map_err(|error| MissiveError::io("writing agent action output", error)),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, kind, output, &output.message)
        }
    }
}

fn metadata_json(metadata: &Metadata) -> Result<String> {
    let value = serde_json::to_value(metadata).map_err(|error| {
        MissiveError::orchestration("failed to encode metadata for human output").with_source(error)
    })?;
    serde_json::to_string(&redact_json(&value)).map_err(|error| {
        MissiveError::orchestration("failed to render metadata for human output").with_source(error)
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn metadata_values_parse_json_or_string() {
        let metadata = parse_metadata(&[
            "role=planner".to_owned(),
            "priority=2".to_owned(),
            "enabled=true".to_owned(),
        ])
        .expect("metadata");

        assert_eq!(metadata.get("role"), Some(&json!("planner")));
        assert_eq!(metadata.get("priority"), Some(&json!(2)));
        assert_eq!(metadata.get("enabled"), Some(&json!(true)));
    }

    #[test]
    fn duplicate_interface_bindings_are_rejected() {
        let error = parse_interface_urls(&[
            "http+json=http://127.0.0.1:1/a2a".to_owned(),
            "http+json=http://127.0.0.1:2/a2a".to_owned(),
        ])
        .expect_err("duplicate binding should fail");

        assert!(error.to_string().contains("duplicate interface binding"));
    }

    #[test]
    fn http_urls_reject_embedded_credentials() {
        let error = validate_http_url("agent base_url", "https://user:pass@example.test")
            .expect_err("credentials should fail");

        assert!(
            error
                .to_string()
                .contains("must not include embedded credentials")
        );
    }
}
