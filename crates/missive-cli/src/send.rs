//! Non-streaming A2A send command implementation.
//!
//! This module wires `missive send` to the negotiated A2A HTTP+JSON/JSON-RPC
//! interface for one registered agent, persists the request/response linkage in
//! SQLite, and renders a stable `send_result` output envelope.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;

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
use crate::auth::auth_headers_for_agent;
use crate::output::{OutputMode, render_success};
use crate::{GlobalArgs, service_parameters_from_config_and_globals};

const TEXT_PART_PREFIX: &str = "text=";
const MAX_TEXT_INPUT_BYTES: usize = 1024 * 1024;

/// Arguments for `missive send`.
#[derive(Debug, Clone, Args)]
pub struct SendArgs {
    /// Registered agent alias to call.
    pub agent: String,
    /// Text message to send. Omit when using --stdin, --file, or --part.
    pub message: Option<String>,

    /// Read one text message part from standard input.
    #[arg(long = "stdin", action = ArgAction::SetTrue)]
    pub stdin: bool,

    /// Read one UTF-8 text message part from this file; repeatable.
    #[arg(long = "file", value_name = "PATH")]
    pub files: Vec<PathBuf>,

    /// Add a message part, currently text=VALUE; repeatable.
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
struct PreparedSend {
    request: SendMessageRequest,
    request_message_id: MessageId,
    requested_context_id: Option<ContextId>,
    requested_task_id: Option<TaskId>,
    local_metadata: Metadata,
    accepted_output_modes: Vec<String>,
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
    let prepared = prepare_send_request(args, &service_parameters, input)?;
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

fn prepare_send_request<R>(
    args: &SendArgs,
    service_parameters: &ServiceParameters,
    input: &mut R,
) -> Result<PreparedSend>
where
    R: Read,
{
    let parts = read_text_parts(args, input)?;
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

    let mut message = Message::new(Role::User, parts);
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

    let mut local_metadata = request_metadata.clone();
    local_metadata.merge(service_parameters.to_metadata()?);

    Ok(PreparedSend {
        request,
        request_message_id,
        requested_context_id,
        requested_task_id,
        local_metadata,
        accepted_output_modes,
    })
}

fn read_text_parts<R>(args: &SendArgs, input: &mut R) -> Result<Vec<Part>>
where
    R: Read,
{
    let mut parts = Vec::new();

    if let Some(message) = args.message.as_deref() {
        parts.push(text_part("message", message)?);
    }

    if args.stdin {
        let mut text = String::new();
        input
            .take((MAX_TEXT_INPUT_BYTES + 1) as u64)
            .read_to_string(&mut text)
            .map_err(|error| MissiveError::io("reading --stdin text", error))?;
        parts.push(text_part("--stdin", &text)?);
    }

    for path in &args.files {
        let bytes = fs::read(path).map_err(|error| {
            MissiveError::io(format!("reading --file {}", path.display()), error)
        })?;
        if bytes.len() > MAX_TEXT_INPUT_BYTES {
            return Err(MissiveError::validation(format!(
                "--file {} is {} bytes, but the current text input limit is {MAX_TEXT_INPUT_BYTES} bytes",
                path.display(),
                bytes.len()
            ))
            .with_help("Use a smaller UTF-8 text file for this ticket; richer file parts land in a later message-parts ticket."));
        }
        let text = String::from_utf8(bytes).map_err(|error| {
            MissiveError::validation(format!(
                "--file {} must contain UTF-8 text for this send implementation",
                path.display()
            ))
            .with_source(error)
            .with_help("Binary/file-byte parts are implemented by a later message-parts ticket.")
        })?;
        parts.push(text_part(&format!("--file {}", path.display()), &text)?);
    }

    for part in &args.parts {
        let text = part.strip_prefix(TEXT_PART_PREFIX).ok_or_else(|| {
            MissiveError::validation(format!(
                "--part value {part:?} is not supported by this ticket"
            ))
            .with_help("Use --part text=VALUE. File, JSON, and byte parts are implemented by a later message-parts ticket.")
        })?;
        parts.push(text_part("--part text=", text)?);
    }

    if parts.is_empty() {
        return Err(MissiveError::validation(
            "missive send requires a message, --stdin, --file, or --part text=VALUE",
        ));
    }

    Ok(parts)
}

fn text_part(source: &str, text: &str) -> Result<Part> {
    if text.is_empty() {
        return Err(MissiveError::validation(format!(
            "{source} cannot be empty for missive send"
        )));
    }
    if text.len() > MAX_TEXT_INPUT_BYTES {
        return Err(MissiveError::validation(format!(
            "{source} is {} bytes, but the current text input limit is {MAX_TEXT_INPUT_BYTES} bytes",
            text.len()
        ))
        .with_help("Use smaller text input for this ticket; streaming and richer file parts land later."));
    }
    Ok(Part::text(text.to_owned()))
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

fn metadata_hash_map(metadata: &Metadata) -> HashMap<String, Value> {
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

fn resolve_send_interface(
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

fn store_message_role(role: &Role) -> Option<MessageRole> {
    Some(match role {
        Role::User => MessageRole::User,
        Role::Agent => MessageRole::Agent,
        Role::Unspecified => MessageRole::Unknown,
    })
}

fn store_task_state(state: &missive_a2a::protocol::TaskState) -> TaskState {
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

fn new_local_message_id() -> MessageId {
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
            parts: Vec::new(),
            metadata: Vec::new(),
            context: None,
            task: None,
            accepted_output_modes: Vec::new(),
        };

        let error = read_text_parts(&args, &mut std::io::empty()).expect_err("missing input");

        assert!(error.to_string().contains("requires a message"));
    }
}
