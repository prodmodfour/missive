//! Non-streaming A2A send command implementation.
//!
//! This module wires `missive send` to the negotiated A2A HTTP+JSON/JSON-RPC
//! interface for one registered agent, persists the request/response linkage in
//! SQLite, and renders a stable `send_result` output envelope.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use clap::{ArgAction, Args};
use missive_a2a::{
    AgentCardClient, AgentCardFetchOutcome, AuthHeaders, NegotiatedInterface, SendMessageClient,
    SendMessageOutcome, ServiceParameters,
    protocol::{
        Message, Part, Role, SendMessageConfiguration, SendMessageRequest, SendMessageResponse,
        Task,
    },
};
use missive_core::{
    AgentAlias, ContextId, LoadedConfig, MessageId, Metadata, MissiveError, MissiveTimestamp,
    Result, TaskId,
};
use missive_store::{
    AgentRecord, ContextUpsert, MessageDirection, MessageInsert, MessageRecord, MessageRole, Store,
    StoreTransaction, TaskSource, TaskState, TaskUpsert,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::agent::{
    AgentRegistry, cache_agent_card, get_existing_agent, negotiate_record_interface,
    open_agent_registry, parse_cached_agent_card,
};
use crate::artifact::persist_task_artifacts;
use crate::auth::auth_headers_for_agent;
use crate::output::{OutputMode, render_success};
use crate::{GlobalArgs, service_parameters_from_config_and_globals};

const TEXT_PART_PREFIX: &str = "text=";
#[cfg(test)]
const DEFAULT_MESSAGE_INPUT_LIMIT_BYTES: u64 = 1024 * 1024;
const JSON_PART_DEFAULT_MEDIA_TYPE: &str = "application/json";

/// Arguments for `missive send`.
#[derive(Debug, Clone, Args)]
pub struct SendArgs {
    /// Registered agent alias to call.
    pub agent: String,
    /// Text message to send. Omit when using --stdin, --file, --file-bytes, --json-part, or --part.
    pub message: Option<String>,

    /// Read one UTF-8 text message part from standard input.
    #[arg(long = "stdin", action = ArgAction::SetTrue)]
    pub stdin: bool,

    /// Attach one safe local file reference part without embedding bytes; repeatable.
    #[arg(long = "file", value_name = "PATH")]
    pub files: Vec<PathBuf>,

    /// Embed one safe local file as an A2A raw byte part; repeatable.
    #[arg(long = "file-bytes", value_name = "PATH")]
    pub file_bytes: Vec<PathBuf>,

    /// Add one A2A structured data part from an inline JSON value; repeatable.
    #[arg(long = "json-part", value_name = "JSON")]
    pub json_parts: Vec<String>,

    /// Apply MIME/media type metadata to file, byte, JSON, or text parts.
    #[arg(long = "mime", value_name = "MIME", action = ArgAction::Append)]
    pub mime: Vec<String>,

    /// Add a message text part as text=VALUE; repeatable.
    #[arg(long = "part", value_name = "text=VALUE")]
    pub parts: Vec<String>,

    /// Add non-secret A2A request metadata as KEY=VALUE; VALUE may be JSON.
    #[arg(long = "metadata", value_name = "KEY=VALUE")]
    pub metadata: Vec<String>,

    /// Continue or create this A2A context id.
    #[arg(long = "context", value_name = "CONTEXT_ID")]
    pub context: Option<String>,

    /// Continue this A2A task id when the remote protocol state allows it.
    #[arg(long = "task", value_name = "TASK_ID")]
    pub task: Option<String>,

    /// Accepted response MIME/output mode; repeatable.
    #[arg(long = "accepted-output-mode", value_name = "MIME")]
    pub accepted_output_modes: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedSend {
    pub(crate) request: SendMessageRequest,
    pub(crate) request_message_id: MessageId,
    pub(crate) requested_context_id: Option<ContextId>,
    pub(crate) requested_task_id: Option<TaskId>,
    pub(crate) local_metadata: Metadata,
    pub(crate) accepted_output_modes: Vec<String>,
    pub(crate) part_summaries: Vec<MessagePartSummary>,
    pub(crate) local_input_bytes: u64,
    pub(crate) request_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct MessagePartSummary {
    kind: String,
    source: String,
    local_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    media_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SendInterfaceView {
    binding: String,
    protocol_binding: String,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tenant: Option<String>,
    protocol_version: String,
    source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct SendRequestView {
    message_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    part_count: usize,
    parts: Vec<MessagePartSummary>,
    local_input_bytes: u64,
    request_bytes: u64,
    accepted_output_modes: Vec<String>,
    metadata: Metadata,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct SendResponseView {
    shape: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    raw: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SendPersistenceView {
    request_message_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct SendOutput {
    profile: String,
    agent: String,
    selected_interface: SendInterfaceView,
    request: SendRequestView,
    response: SendResponseView,
    persistence: SendPersistenceView,
    message: String,
}

#[derive(Debug, Clone)]
struct PersistedSend {
    request_message: MessageRecord,
    response_message: Option<MessageRecord>,
    task_id: Option<TaskId>,
    context_id: Option<ContextId>,
}

/// Executes `missive send`.
pub(crate) fn execute_send_command<R, W>(
    args: &SendArgs,
    globals: &GlobalArgs,
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
    mode: OutputMode,
    input: &mut R,
    writer: &mut W,
) -> Result<()>
where
    R: Read,
    W: Write,
{
    let service_parameters = service_parameters_from_config_and_globals(loaded_config, globals)?;
    let max_request_bytes = message_part_limit_bytes(loaded_config)?;
    let prepared = prepare_send_request(args, &service_parameters, max_request_bytes, input)?;
    let mut registry = open_agent_registry(loaded_config, environment)?;
    let alias = AgentAlias::new(args.agent.clone())?;
    let agent = get_existing_agent(&registry.store, &alias)?;
    let auth_headers = auth_headers_for_agent(&registry.store, &agent, globals, environment)?;
    let (agent, selected_interface) =
        resolve_send_interface(&registry, agent, &service_parameters, &auth_headers)?;

    let client = SendMessageClient::new()?;
    let outcome = client.send_message(
        &selected_interface,
        &prepared.request,
        &service_parameters,
        &auth_headers,
    )?;
    let persisted = persist_send(
        &mut registry.store,
        &agent,
        &prepared,
        &outcome,
        &service_parameters,
    )?;
    let output = SendOutput::from_parts(registry.profile, &agent, &prepared, &outcome, &persisted);

    render_success(writer, mode, "send_result", &output, &output.message)
}

pub(crate) fn prepare_send_request<R>(
    args: &SendArgs,
    service_parameters: &ServiceParameters,
    max_request_bytes: u64,
    input: &mut R,
) -> Result<PreparedSend>
where
    R: Read,
{
    let built_parts = read_message_parts(args, max_request_bytes, input)?;
    let request_metadata = parse_metadata(&args.metadata)?;
    let requested_context_id = args
        .context
        .as_ref()
        .map(|value| ContextId::new(value.clone()))
        .transpose()?;
    let requested_task_id = args
        .task
        .as_ref()
        .map(|value| TaskId::new(value.clone()))
        .transpose()?;
    let accepted_output_modes = validate_output_modes(&args.accepted_output_modes)?;

    let mut message = Message::new(Role::User, built_parts.parts);
    message.context_id = requested_context_id
        .as_ref()
        .map(|context_id| context_id.as_str().to_owned());
    message.task_id = requested_task_id
        .as_ref()
        .map(|task_id| task_id.as_str().to_owned());

    let request_message_id = MessageId::new(message.message_id.clone())?;
    let configuration = (!accepted_output_modes.is_empty()).then_some(SendMessageConfiguration {
        accepted_output_modes: Some(accepted_output_modes.clone()),
        task_push_notification_config: None,
        history_length: None,
        return_immediately: None,
    });
    let request = SendMessageRequest {
        message,
        configuration,
        metadata: (!request_metadata.is_empty()).then(|| metadata_hash_map(&request_metadata)),
        tenant: None,
    };
    let request_bytes = serialized_request_bytes(&request)?;
    enforce_size_limit(
        "serialized A2A SendMessage request",
        request_bytes,
        max_request_bytes,
    )?;

    let mut local_metadata = request_metadata.clone();
    local_metadata.merge(service_parameters.to_metadata()?);

    Ok(PreparedSend {
        request,
        request_message_id,
        requested_context_id,
        requested_task_id,
        local_metadata,
        accepted_output_modes,
        part_summaries: built_parts.summaries,
        local_input_bytes: built_parts.local_input_bytes,
        request_bytes,
    })
}

pub(crate) fn message_part_limit_bytes(loaded_config: &LoadedConfig) -> Result<u64> {
    let profile = loaded_config.selected_profile_config()?;
    Ok(profile
        .qos
        .as_ref()
        .map_or(loaded_config.config.qos.max_request_bytes, |qos| {
            qos.max_request_bytes
        }))
}

#[derive(Debug)]
struct BuiltMessageParts {
    parts: Vec<Part>,
    summaries: Vec<MessagePartSummary>,
    local_input_bytes: u64,
}

#[derive(Debug)]
struct PartSpec {
    part: Part,
    summary: MessagePartSummary,
    default_mime_target: bool,
}

#[derive(Debug)]
struct LocalFileInput {
    canonical_path: PathBuf,
    file_url: String,
    filename: String,
    len: u64,
}

#[derive(Debug)]
struct InputBudget {
    limit: u64,
    used: u64,
}

impl InputBudget {
    const fn new(limit: u64) -> Self {
        Self { limit, used: 0 }
    }

    fn add(&mut self, source: &str, bytes: u64) -> Result<()> {
        enforce_size_limit(source, bytes, self.limit)?;
        let next = self
            .used
            .checked_add(bytes)
            .ok_or_else(|| MissiveError::validation("message input byte accounting overflowed"))?;
        if next > self.limit {
            return Err(MissiveError::validation(format!(
                "message inputs are {next} bytes in total, exceeding the selected profile qos.max_request_bytes limit of {} bytes",
                self.limit
            ))
            .with_help("Use smaller inputs, lower fidelity attachments, file references, or raise qos.max_request_bytes in the selected profile."));
        }
        self.used = next;
        Ok(())
    }

    fn remaining_with_sentinel(&self) -> u64 {
        self.limit.saturating_sub(self.used).saturating_add(1)
    }
}

fn read_message_parts<R>(
    args: &SendArgs,
    max_request_bytes: u64,
    input: &mut R,
) -> Result<BuiltMessageParts>
where
    R: Read,
{
    let mut budget = InputBudget::new(max_request_bytes);
    let mut specs = Vec::new();

    if let Some(message) = args.message.as_deref() {
        specs.push(text_part_spec("message", message, &mut budget)?);
    }

    if args.stdin {
        let mut text = String::new();
        input
            .take(budget.remaining_with_sentinel())
            .read_to_string(&mut text)
            .map_err(|error| MissiveError::io("reading --stdin text", error))?;
        specs.push(text_part_spec("--stdin", &text, &mut budget)?);
    }

    for path in &args.files {
        specs.push(file_reference_part_spec(path, &mut budget)?);
    }

    for path in &args.file_bytes {
        specs.push(file_bytes_part_spec(path, &mut budget)?);
    }

    for raw_json in &args.json_parts {
        specs.push(json_part_spec(raw_json, &mut budget)?);
    }

    for part in &args.parts {
        let text = part.strip_prefix(TEXT_PART_PREFIX).ok_or_else(|| {
            MissiveError::validation(format!("--part value {part:?} is not supported"))
                .with_help("Use --part text=VALUE, or use --json-part/--file/--file-bytes for structured and file content.")
        })?;
        specs.push(text_part_spec("--part text=", text, &mut budget)?);
    }

    if specs.is_empty() {
        return Err(MissiveError::validation(
            "missive send requires a message, --stdin, --file, --file-bytes, --json-part, or --part text=VALUE",
        ));
    }

    apply_mime_assignments(&mut specs, &args.mime)?;

    let mut parts = Vec::with_capacity(specs.len());
    let mut summaries = Vec::with_capacity(specs.len());
    for spec in specs {
        parts.push(spec.part);
        summaries.push(spec.summary);
    }

    Ok(BuiltMessageParts {
        parts,
        summaries,
        local_input_bytes: budget.used,
    })
}

fn text_part_spec(source: &str, text: &str, budget: &mut InputBudget) -> Result<PartSpec> {
    if text.is_empty() {
        return Err(MissiveError::validation(format!(
            "{source} cannot be empty for message input"
        )));
    }
    let bytes = usize_to_u64(text.len(), source)?;
    budget.add(source, bytes)?;
    Ok(PartSpec {
        part: Part::text(text.to_owned()),
        summary: MessagePartSummary {
            kind: "text".to_owned(),
            source: source.to_owned(),
            local_bytes: bytes,
            filename: None,
            media_type: None,
        },
        default_mime_target: false,
    })
}

fn file_reference_part_spec(path: &Path, budget: &mut InputBudget) -> Result<PartSpec> {
    let file = validate_local_file("--file", path, budget.limit)?;
    budget.add(&format!("--file {}", path.display()), file.len)?;
    let filename = file.filename;
    Ok(PartSpec {
        part: Part::url(file.file_url).with_filename(filename.clone()),
        summary: MessagePartSummary {
            kind: "file_reference".to_owned(),
            source: "--file".to_owned(),
            local_bytes: file.len,
            filename: Some(filename),
            media_type: None,
        },
        default_mime_target: true,
    })
}

fn file_bytes_part_spec(path: &Path, budget: &mut InputBudget) -> Result<PartSpec> {
    let file = validate_local_file("--file-bytes", path, budget.limit)?;
    let bytes = fs::read(&file.canonical_path).map_err(|error| {
        MissiveError::io(
            format!("reading --file-bytes {}", file.canonical_path.display()),
            error,
        )
    })?;
    let byte_count = usize_to_u64(bytes.len(), "--file-bytes")?;
    budget.add(&format!("--file-bytes {}", path.display()), byte_count)?;
    let filename = file.filename;
    Ok(PartSpec {
        part: Part::raw(bytes).with_filename(filename.clone()),
        summary: MessagePartSummary {
            kind: "file_bytes".to_owned(),
            source: "--file-bytes".to_owned(),
            local_bytes: byte_count,
            filename: Some(filename),
            media_type: None,
        },
        default_mime_target: true,
    })
}

fn json_part_spec(raw_json: &str, budget: &mut InputBudget) -> Result<PartSpec> {
    if raw_json.trim().is_empty() {
        return Err(MissiveError::validation("--json-part cannot be empty"));
    }
    let byte_count = usize_to_u64(raw_json.len(), "--json-part")?;
    budget.add("--json-part", byte_count)?;
    let value = serde_json::from_str::<Value>(raw_json).map_err(|error| {
        MissiveError::validation("--json-part must be valid JSON")
            .with_source(error)
            .with_help("Pass an inline JSON value such as '{\"kind\":\"example\"}' or '[1,2,3]'.")
    })?;
    Ok(PartSpec {
        part: Part::data(value).with_media_type(JSON_PART_DEFAULT_MEDIA_TYPE),
        summary: MessagePartSummary {
            kind: "data".to_owned(),
            source: "--json-part".to_owned(),
            local_bytes: byte_count,
            filename: None,
            media_type: Some(JSON_PART_DEFAULT_MEDIA_TYPE.to_owned()),
        },
        default_mime_target: true,
    })
}

fn validate_local_file(flag: &str, path: &Path, max_request_bytes: u64) -> Result<LocalFileInput> {
    if path.as_os_str().is_empty() {
        return Err(MissiveError::validation(format!(
            "{flag} path cannot be empty"
        )));
    }
    let canonical_path = fs::canonicalize(path).map_err(|error| {
        MissiveError::io(format!("resolving {flag} {}", path.display()), error).with_help(
            "Use an existing local file path. Directories and missing paths are rejected.",
        )
    })?;
    let metadata = fs::metadata(&canonical_path).map_err(|error| {
        MissiveError::io(
            format!("reading metadata for {flag} {}", canonical_path.display()),
            error,
        )
    })?;
    if !metadata.is_file() {
        return Err(MissiveError::validation(format!(
            "{flag} {} is not a regular file",
            canonical_path.display()
        ))
        .with_help(
            "Use a regular local file; directories, sockets, and special files are rejected.",
        ));
    }
    enforce_size_limit(
        &format!("{flag} {}", canonical_path.display()),
        metadata.len(),
        max_request_bytes,
    )?;
    let filename = canonical_path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty() && !value.chars().any(char::is_control))
        .ok_or_else(|| {
            MissiveError::validation(format!(
                "{flag} {} does not have a safe UTF-8 filename",
                canonical_path.display()
            ))
            .with_help(
                "Use a file whose final path component is valid UTF-8 without control characters.",
            )
        })?
        .to_owned();
    let file_url = url::Url::from_file_path(&canonical_path)
        .map_err(|()| {
            MissiveError::validation(format!(
                "{flag} {} cannot be represented as a file:// URL",
                canonical_path.display()
            ))
        })?
        .to_string();

    Ok(LocalFileInput {
        canonical_path,
        file_url,
        filename,
        len: metadata.len(),
    })
}

fn apply_mime_assignments(specs: &mut [PartSpec], values: &[String]) -> Result<()> {
    let media_types = values
        .iter()
        .map(|value| validate_mime_value(value))
        .collect::<Result<Vec<_>>>()?;
    if media_types.is_empty() {
        return Ok(());
    }

    let default_targets = specs
        .iter()
        .enumerate()
        .filter_map(|(index, spec)| spec.default_mime_target.then_some(index))
        .collect::<Vec<_>>();
    let target_indices = if default_targets.is_empty() {
        (0..specs.len()).collect::<Vec<_>>()
    } else {
        default_targets
    };

    if media_types.len() == 1 {
        let media_type = &media_types[0];
        for index in target_indices {
            set_part_media_type(&mut specs[index], media_type);
        }
        return Ok(());
    }

    if media_types.len() == target_indices.len() {
        for (index, media_type) in target_indices.into_iter().zip(media_types.iter()) {
            set_part_media_type(&mut specs[index], media_type);
        }
        return Ok(());
    }

    if media_types.len() == specs.len() {
        for (spec, media_type) in specs.iter_mut().zip(media_types.iter()) {
            set_part_media_type(spec, media_type);
        }
        return Ok(());
    }

    Err(MissiveError::validation(format!(
        "--mime was provided {} times for {} message parts and {} file/JSON parts",
        media_types.len(),
        specs.len(),
        target_indices.len()
    ))
    .with_help("Pass one --mime value for all file/JSON parts, one value per file/JSON part, or one value per message part."))
}

fn set_part_media_type(spec: &mut PartSpec, media_type: &str) {
    spec.part.media_type = Some(media_type.to_owned());
    spec.summary.media_type = Some(media_type.to_owned());
}

fn validate_mime_value(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(MissiveError::validation("--mime values cannot be empty"));
    }
    if value.len() > 128
        || value.chars().any(char::is_control)
        || value.chars().any(char::is_whitespace)
        || !value.contains('/')
    {
        return Err(MissiveError::validation(format!(
            "--mime value {value:?} is not a valid compact MIME media type"
        ))
        .with_help("Use values such as text/plain, application/json, image/png, or application/octet-stream."));
    }
    Ok(value.to_owned())
}

fn enforce_size_limit(source: &str, bytes: u64, limit: u64) -> Result<()> {
    if bytes > limit {
        return Err(MissiveError::validation(format!(
            "{source} is {bytes} bytes, exceeding the selected profile qos.max_request_bytes limit of {limit} bytes"
        ))
        .with_help("Use a smaller input or raise qos.max_request_bytes in the selected profile. Streaming/chunked file upload is not implemented yet."));
    }
    Ok(())
}

fn serialized_request_bytes(request: &SendMessageRequest) -> Result<u64> {
    let bytes = serde_json::to_vec(request).map_err(|error| {
        MissiveError::protocol("encoding A2A SendMessage request for size validation")
            .with_source(error)
    })?;
    usize_to_u64(bytes.len(), "serialized A2A SendMessage request")
}

fn usize_to_u64(value: usize, source: &str) -> Result<u64> {
    u64::try_from(value).map_err(|error| {
        MissiveError::validation(format!("{source} byte count does not fit into u64"))
            .with_source(error)
    })
}

fn parse_metadata(values: &[String]) -> Result<Metadata> {
    let mut metadata = Metadata::new();
    for value in values {
        let (key, raw_value) = split_key_value("--metadata", value)?;
        let parsed = serde_json::from_str::<Value>(raw_value)
            .unwrap_or_else(|_| Value::String(raw_value.to_owned()));
        metadata.insert(key.to_owned(), parsed)?;
    }
    Ok(metadata)
}

pub(crate) fn metadata_hash_map(metadata: &Metadata) -> HashMap<String, Value> {
    metadata
        .iter()
        .map(|(key, value)| (key.to_owned(), value.clone()))
        .collect()
}

fn validate_output_modes(values: &[String]) -> Result<Vec<String>> {
    let mut modes = Vec::new();
    for value in values {
        let value = value.trim();
        if value.is_empty() {
            return Err(MissiveError::validation(
                "--accepted-output-mode values cannot be empty",
            ));
        }
        if value.len() > 128 || value.chars().any(char::is_control) {
            return Err(MissiveError::validation(format!(
                "--accepted-output-mode value {value:?} is not a valid short MIME/output mode"
            )));
        }
        modes.push(value.to_owned());
    }
    Ok(modes)
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

pub(crate) fn resolve_send_interface(
    registry: &AgentRegistry,
    agent: AgentRecord,
    service_parameters: &ServiceParameters,
    auth_headers: &AuthHeaders,
) -> Result<(AgentRecord, NegotiatedInterface)> {
    if let Some(raw_card) = agent.agent_card_json.clone() {
        let card = parse_cached_agent_card(&agent, raw_card)?;
        let selected = negotiate_record_interface(&agent, &card, None)?;
        return Ok((agent, selected));
    }

    let client = AgentCardClient::new()?;
    let outcome = client.fetch_public_agent_card_with_service_parameters_and_auth(
        &agent.base_url,
        None,
        service_parameters,
        auth_headers,
    )?;
    match outcome {
        AgentCardFetchOutcome::Fetched(fetch) => {
            let selected = negotiate_record_interface(&agent, &fetch.card, None)?;
            let updated = cache_agent_card(
                &registry.store,
                &agent,
                fetch.raw_json,
                fetch.validators,
                MissiveTimestamp::now_utc(),
            )?;
            Ok((updated, selected))
        }
        AgentCardFetchOutcome::NotModified(_) => Err(MissiveError::protocol(format!(
            "agent {:?} Agent Card endpoint returned 304 Not Modified without a local cache",
            agent.alias.as_str()
        ))
        .with_help("Run 'missive agent inspect <alias> --refresh' after the remote endpoint returns a full card body.")),
    }
}

fn persist_send(
    store: &mut Store,
    agent: &AgentRecord,
    prepared: &PreparedSend,
    outcome: &SendMessageOutcome,
    service_parameters: &ServiceParameters,
) -> Result<PersistedSend> {
    store.transaction(|transaction| {
        let response_ids = ResponseIds::from_outcome(outcome)?;
        let effective_context_id = response_ids
            .context_id
            .clone()
            .or_else(|| prepared.requested_context_id.clone());
        let effective_task_id = response_ids
            .task_id
            .clone()
            .or_else(|| prepared.requested_task_id.clone());

        if let Some(context_id) = &effective_context_id {
            ensure_context(transaction, context_id, agent)?;
        }

        let response_message_id = response_ids.response_message_id.clone();
        if let Some(task) = task_response(outcome) {
            upsert_remote_task(
                transaction,
                agent,
                task,
                response_message_id.as_ref(),
                service_parameters,
            )?;
        } else if let Some(task_id) = &effective_task_id {
            ensure_task_placeholder(
                transaction,
                agent,
                task_id,
                effective_context_id.as_ref(),
                service_parameters,
            )?;
        }

        let request_message = insert_request_message(
            transaction,
            agent,
            prepared,
            effective_context_id.as_ref(),
            effective_task_id.as_ref(),
        )?;
        let response_message = insert_response_message(
            transaction,
            agent,
            outcome,
            effective_context_id.as_ref(),
            effective_task_id.as_ref(),
            response_message_id.as_ref(),
            &prepared.local_metadata,
        )?;

        Ok(PersistedSend {
            request_message,
            response_message,
            task_id: effective_task_id,
            context_id: effective_context_id,
        })
    })
}

#[derive(Debug, Clone)]
struct ResponseIds {
    context_id: Option<ContextId>,
    task_id: Option<TaskId>,
    response_message_id: Option<MessageId>,
}

impl ResponseIds {
    fn from_outcome(outcome: &SendMessageOutcome) -> Result<Self> {
        match &outcome.response {
            SendMessageResponse::Message(message) => Ok(Self {
                context_id: message
                    .context_id
                    .as_ref()
                    .map(|value| ContextId::new(value.clone()))
                    .transpose()?,
                task_id: message
                    .task_id
                    .as_ref()
                    .map(|value| TaskId::new(value.clone()))
                    .transpose()?,
                response_message_id: Some(MessageId::new(message.message_id.clone())?),
            }),
            SendMessageResponse::Task(task) => {
                let status_message_id = task
                    .status
                    .message
                    .as_ref()
                    .map(|message| MessageId::new(message.message_id.clone()))
                    .transpose()?;
                Ok(Self {
                    context_id: Some(ContextId::new(task.context_id.clone())?),
                    task_id: Some(TaskId::new(task.id.clone())?),
                    response_message_id: Some(
                        status_message_id.unwrap_or_else(new_local_message_id),
                    ),
                })
            }
        }
    }
}

fn ensure_context(
    transaction: &StoreTransaction<'_>,
    context_id: &ContextId,
    agent: &AgentRecord,
) -> Result<()> {
    if transaction.get_context(context_id)?.is_none() {
        let mut context = ContextUpsert::new(context_id.clone());
        context.agent_alias = Some(agent.alias.clone());
        transaction.upsert_context(&context)?;
    }
    Ok(())
}

fn ensure_task_placeholder(
    transaction: &StoreTransaction<'_>,
    agent: &AgentRecord,
    task_id: &TaskId,
    context_id: Option<&ContextId>,
    service_parameters: &ServiceParameters,
) -> Result<()> {
    if transaction.get_task(task_id)?.is_none() {
        let mut task = TaskUpsert::new(task_id.clone(), agent.alias.clone(), TaskState::Submitted);
        task.source = TaskSource::Remote;
        task.context_id = context_id.cloned();
        task.record_a2a_protocol_version(service_parameters.protocol_version.clone())?;
        transaction.upsert_task(&task)?;
    }
    Ok(())
}

fn upsert_remote_task(
    transaction: &StoreTransaction<'_>,
    agent: &AgentRecord,
    task: &Task,
    last_message_id: Option<&MessageId>,
    service_parameters: &ServiceParameters,
) -> Result<()> {
    let task_id = TaskId::new(task.id.clone())?;
    let context_id = ContextId::new(task.context_id.clone())?;
    ensure_context(transaction, &context_id, agent)?;

    let mut task_upsert = TaskUpsert::new(
        task_id,
        agent.alias.clone(),
        store_task_state(&task.status.state),
    );
    task_upsert.context_id = Some(context_id);
    task_upsert.remote_task_json = Some(serde_json::to_value(task).map_err(|error| {
        MissiveError::protocol("encoding A2A task response for persistence").with_source(error)
    })?);
    task_upsert.last_message_id = last_message_id.cloned();
    task_upsert.record_a2a_protocol_version(service_parameters.protocol_version.clone())?;
    if matches!(
        task_upsert.state,
        TaskState::Completed | TaskState::Failed | TaskState::Cancelled
    ) {
        task_upsert.completed_at = Some(MissiveTimestamp::now_utc());
    }
    transaction.upsert_task(&task_upsert)?;
    persist_task_artifacts(transaction, task)?;
    Ok(())
}

fn insert_request_message(
    transaction: &StoreTransaction<'_>,
    agent: &AgentRecord,
    prepared: &PreparedSend,
    context_id: Option<&ContextId>,
    task_id: Option<&TaskId>,
) -> Result<MessageRecord> {
    let mut message = MessageInsert::new(
        prepared.request_message_id.clone(),
        MessageDirection::Request,
        serde_json::to_value(&prepared.request.message).map_err(|error| {
            MissiveError::protocol("encoding outbound A2A message for persistence")
                .with_source(error)
        })?,
    );
    message.agent_alias = Some(agent.alias.clone());
    message.context_id = context_id.cloned();
    message.task_id = task_id.cloned();
    message.role = Some(MessageRole::User);
    message.protocol_message_id = Some(prepared.request.message.message_id.clone());
    message.metadata = prepared.local_metadata.clone();
    transaction.insert_message(&message)
}

fn insert_response_message(
    transaction: &StoreTransaction<'_>,
    agent: &AgentRecord,
    outcome: &SendMessageOutcome,
    context_id: Option<&ContextId>,
    task_id: Option<&TaskId>,
    response_message_id: Option<&MessageId>,
    metadata: &Metadata,
) -> Result<Option<MessageRecord>> {
    let Some(message_id) = response_message_id else {
        return Ok(None);
    };
    let (content_json, role, protocol_message_id) = match &outcome.response {
        SendMessageResponse::Message(message) => (
            serde_json::to_value(message).map_err(|error| {
                MissiveError::protocol("encoding A2A response message for persistence")
                    .with_source(error)
            })?,
            store_message_role(&message.role),
            Some(message.message_id.clone()),
        ),
        SendMessageResponse::Task(task) => {
            if let Some(message) = &task.status.message {
                (
                    serde_json::to_value(message).map_err(|error| {
                        MissiveError::protocol("encoding A2A task status message for persistence")
                            .with_source(error)
                    })?,
                    store_message_role(&message.role),
                    Some(message.message_id.clone()),
                )
            } else {
                (
                    json!({ "task": outcome.raw_json.clone() }),
                    Some(MessageRole::Unknown),
                    None,
                )
            }
        }
    };

    let mut row = MessageInsert::new(message_id.clone(), MessageDirection::Response, content_json);
    row.agent_alias = Some(agent.alias.clone());
    row.context_id = context_id.cloned();
    row.task_id = task_id.cloned();
    row.role = role;
    row.ordinal = 1;
    row.protocol_message_id = protocol_message_id;
    row.metadata = metadata.clone();
    transaction.insert_message(&row).map(Some)
}

fn task_response(outcome: &SendMessageOutcome) -> Option<&Task> {
    match &outcome.response {
        SendMessageResponse::Task(task) => Some(task),
        SendMessageResponse::Message(_) => None,
    }
}

pub(crate) fn store_message_role(role: &Role) -> Option<MessageRole> {
    Some(match role {
        Role::User => MessageRole::User,
        Role::Agent => MessageRole::Agent,
        Role::Unspecified => MessageRole::Unknown,
    })
}

pub(crate) fn store_task_state(state: &missive_a2a::protocol::TaskState) -> TaskState {
    match state {
        missive_a2a::protocol::TaskState::Submitted => TaskState::Submitted,
        missive_a2a::protocol::TaskState::Working => TaskState::Working,
        missive_a2a::protocol::TaskState::Completed => TaskState::Completed,
        missive_a2a::protocol::TaskState::Failed | missive_a2a::protocol::TaskState::Rejected => {
            TaskState::Failed
        }
        missive_a2a::protocol::TaskState::Canceled => TaskState::Cancelled,
        missive_a2a::protocol::TaskState::InputRequired
        | missive_a2a::protocol::TaskState::AuthRequired => TaskState::InputRequired,
        missive_a2a::protocol::TaskState::Unspecified => TaskState::Unknown,
    }
}

pub(crate) fn new_local_message_id() -> MessageId {
    MessageId::new(missive_a2a::protocol::new_message_id())
        .expect("official generated message id should be valid")
}

impl From<&NegotiatedInterface> for SendInterfaceView {
    fn from(interface: &NegotiatedInterface) -> Self {
        Self {
            binding: interface.binding.clone(),
            protocol_binding: interface.protocol_binding.clone(),
            url: interface.url.clone(),
            tenant: interface.tenant.clone(),
            protocol_version: interface.protocol_version.clone(),
            source: interface.source.as_str().to_owned(),
        }
    }
}

impl SendRequestView {
    fn from_prepared(prepared: &PreparedSend) -> Self {
        Self {
            message_id: prepared.request_message_id.as_str().to_owned(),
            context_id: prepared
                .requested_context_id
                .as_ref()
                .map(|context_id| context_id.as_str().to_owned()),
            task_id: prepared
                .requested_task_id
                .as_ref()
                .map(|task_id| task_id.as_str().to_owned()),
            part_count: prepared.request.message.parts.len(),
            parts: prepared.part_summaries.clone(),
            local_input_bytes: prepared.local_input_bytes,
            request_bytes: prepared.request_bytes,
            accepted_output_modes: prepared.accepted_output_modes.clone(),
            metadata: prepared.local_metadata.clone(),
        }
    }
}

impl SendResponseView {
    fn from_outcome(outcome: &SendMessageOutcome) -> Self {
        match &outcome.response {
            SendMessageResponse::Message(message) => Self {
                shape: "message".to_owned(),
                message_id: Some(message.message_id.clone()),
                task_id: message.task_id.clone(),
                context_id: message.context_id.clone(),
                state: None,
                text: message.text().map(ToOwned::to_owned),
                raw: outcome.raw_json.clone(),
            },
            SendMessageResponse::Task(task) => Self {
                shape: "task".to_owned(),
                message_id: task
                    .status
                    .message
                    .as_ref()
                    .map(|message| message.message_id.clone()),
                task_id: Some(task.id.clone()),
                context_id: Some(task.context_id.clone()),
                state: Some(store_task_state(&task.status.state).as_str().to_owned()),
                text: task
                    .status
                    .message
                    .as_ref()
                    .and_then(Message::text)
                    .map(ToOwned::to_owned),
                raw: outcome.raw_json.clone(),
            },
        }
    }
}

impl SendPersistenceView {
    fn from_persisted(persisted: &PersistedSend) -> Self {
        Self {
            request_message_id: persisted.request_message.message_id.as_str().to_owned(),
            response_message_id: persisted
                .response_message
                .as_ref()
                .map(|message| message.message_id.as_str().to_owned()),
            task_id: persisted
                .task_id
                .as_ref()
                .map(|task_id| task_id.as_str().to_owned()),
            context_id: persisted
                .context_id
                .as_ref()
                .map(|context_id| context_id.as_str().to_owned()),
        }
    }
}

impl SendOutput {
    fn from_parts(
        profile: String,
        agent: &AgentRecord,
        prepared: &PreparedSend,
        outcome: &SendMessageOutcome,
        persisted: &PersistedSend,
    ) -> Self {
        let response = SendResponseView::from_outcome(outcome);
        let persistence = SendPersistenceView::from_persisted(persisted);
        let message = match response.shape.as_str() {
            "task" => format!(
                "Sent message to '{}' and recorded task {}{}",
                agent.alias.as_str(),
                response.task_id.as_deref().unwrap_or("unknown"),
                response
                    .context_id
                    .as_deref()
                    .map(|context_id| format!(" in context {context_id}"))
                    .unwrap_or_default()
            ),
            _ => format!(
                "Sent message to '{}' and received message {}{}",
                agent.alias.as_str(),
                response.message_id.as_deref().unwrap_or("unknown"),
                response
                    .context_id
                    .as_deref()
                    .map(|context_id| format!(" in context {context_id}"))
                    .unwrap_or_default()
            ),
        };

        Self {
            profile,
            agent: agent.alias.as_str().to_owned(),
            selected_interface: SendInterfaceView::from(&outcome.interface),
            request: SendRequestView::from_prepared(prepared),
            response,
            persistence,
            message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_values_parse_as_json_when_possible() {
        let metadata = parse_metadata(&[
            "plain=value".to_owned(),
            "count=2".to_owned(),
            "structured={\"ok\":true}".to_owned(),
        ])
        .expect("metadata");

        assert_eq!(metadata.get_str("plain"), Some("value"));
        assert_eq!(metadata.get("count"), Some(&json!(2)));
        assert_eq!(metadata.get("structured"), Some(&json!({"ok": true})));
    }

    #[test]
    fn text_parts_require_some_input() {
        let args = SendArgs {
            agent: "echo".to_owned(),
            message: None,
            stdin: false,
            files: Vec::new(),
            file_bytes: Vec::new(),
            json_parts: Vec::new(),
            mime: Vec::new(),
            parts: Vec::new(),
            metadata: Vec::new(),
            context: None,
            task: None,
            accepted_output_modes: Vec::new(),
        };

        let error = read_message_parts(
            &args,
            DEFAULT_MESSAGE_INPUT_LIMIT_BYTES,
            &mut std::io::empty(),
        )
        .expect_err("missing input");

        assert!(error.to_string().contains("requires a message"));
    }
}
