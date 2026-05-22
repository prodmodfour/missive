//! Streaming A2A message command implementation.
//!
//! This module wires `missive stream` to A2A `SendStreamingMessage` over SSE,
//! renders incremental human/NDJSON updates, and persists each stream event as
//! it is parsed from the remote response.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;

use clap::{ArgAction, Args};
use missive_a2a::{
    AgentCard, AgentCardClient, AgentCardFetchOutcome, AuthHeaders, NegotiatedInterface,
    ServiceParameters, StreamMessageClient, StreamMessageEvent, StreamMessageOutcome,
    protocol::{Message, StreamResponse, Task, TaskArtifactUpdateEvent, TaskStatusUpdateEvent},
};
use missive_core::{
    AgentAlias, ContextId, EventId, LoadedConfig, MessageId, Metadata, MissiveError,
    MissiveTimestamp, Result, TaskId,
};
use missive_store::{
    AgentRecord, ContextUpsert, EventInsert, EventRecord, MessageDirection, MessageInsert,
    MessageRecord, MessageRole, Store, StoreTransaction, TaskSource, TaskState as StoreTaskState,
    TaskUpsert,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::agent::{
    AgentRegistry, cache_agent_card, get_existing_agent, negotiate_record_interface,
    open_agent_registry, parse_cached_agent_card,
};
use crate::auth::auth_headers_for_agent;
use crate::output::{OutputMode, redact_json, render_stream_item, render_success};
use crate::send::{
    PreparedSend, SendArgs, new_local_message_id, prepare_send_request, store_message_role,
    store_task_state,
};
use crate::{GlobalArgs, service_parameters_from_config_and_globals};

/// Arguments for `missive stream`.
#[derive(Debug, Clone, Args)]
pub struct StreamArgs {
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

    /// Attempt streaming even when the cached/fetched Agent Card does not advertise it.
    #[arg(long = "force", action = ArgAction::SetTrue)]
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct StreamInterfaceView {
    binding: String,
    protocol_binding: String,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tenant: Option<String>,
    protocol_version: String,
    source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct StreamCapabilityView {
    advertised_streaming: bool,
    forced: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct StreamRequestView {
    message_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    part_count: usize,
    accepted_output_modes: Vec<String>,
    metadata: Metadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct StreamEventPersistenceView {
    event_sequence: i64,
    event_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct StreamEventView {
    profile: String,
    agent: String,
    sequence: u64,
    event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    append: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_chunk: Option<bool>,
    raw: Value,
    persistence: StreamEventPersistenceView,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct StreamPersistenceSummary {
    request_message_id: String,
    event_ids: Vec<String>,
    stream_message_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct StreamOutput {
    profile: String,
    agent: String,
    selected_interface: StreamInterfaceView,
    capability: StreamCapabilityView,
    request: StreamRequestView,
    event_count: u64,
    status_update_count: usize,
    artifact_update_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    final_task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    final_context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    final_state: Option<String>,
    events: Vec<StreamEventView>,
    persistence: StreamPersistenceSummary,
    message: String,
}

#[derive(Debug, Clone)]
struct PersistedStreamEvent {
    event: EventRecord,
    message: Option<MessageRecord>,
}

/// Executes `missive stream`.
pub(crate) fn execute_stream_command<R, W>(
    args: &StreamArgs,
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
    let send_args = args.to_send_args();
    let prepared = prepare_send_request(&send_args, &service_parameters, input)?;
    let mut registry = open_agent_registry(loaded_config, environment)?;
    let profile = registry.profile.clone();
    let alias = AgentAlias::new(args.agent.clone())?;
    let agent = get_existing_agent(&registry.store, &alias)?;
    let auth_headers = auth_headers_for_agent(&registry.store, &agent, globals, environment)?;
    let (agent, card, selected_interface) =
        resolve_stream_interface(&mut registry, agent, &service_parameters, &auth_headers)?;
    let capability = validate_streaming_capability(&card, args.force)?;
    let request_message =
        persist_stream_request(&mut registry.store, &agent, &prepared, &service_parameters)?;

    let client = StreamMessageClient::new()?;
    let mut events = Vec::new();
    let outcome = client.stream_message(
        &selected_interface,
        &prepared.request,
        &service_parameters,
        &auth_headers,
        |event| {
            let persisted = persist_stream_event(
                &mut registry.store,
                &agent,
                &event,
                &prepared.local_metadata,
                &service_parameters,
            )?;
            let view = StreamEventView::from_event(profile.clone(), &agent, &event, &persisted)?;
            render_stream_item(
                writer,
                mode,
                "stream_event",
                event.sequence,
                &view,
                &view.message,
            )?;
            events.push(view);
            Ok(())
        },
    )?;

    let output = StreamOutput::from_parts(StreamOutputParts {
        profile,
        agent: &agent,
        selected_interface: &selected_interface,
        capability,
        prepared: &prepared,
        request_message: &request_message,
        events: &events,
        outcome: &outcome,
    });
    render_stream_result(writer, mode, outcome.event_count, &output)
}

impl StreamArgs {
    fn to_send_args(&self) -> SendArgs {
        SendArgs {
            agent: self.agent.clone(),
            message: self.message.clone(),
            stdin: self.stdin,
            files: self.files.clone(),
            parts: self.parts.clone(),
            metadata: self.metadata.clone(),
            context: self.context.clone(),
            task: self.task.clone(),
            accepted_output_modes: self.accepted_output_modes.clone(),
        }
    }
}

fn resolve_stream_interface(
    registry: &mut AgentRegistry,
    agent: AgentRecord,
    service_parameters: &ServiceParameters,
    auth_headers: &AuthHeaders,
) -> Result<(AgentRecord, AgentCard, NegotiatedInterface)> {
    if let Some(raw_card) = agent.agent_card_json.clone() {
        let card = parse_cached_agent_card(&agent, raw_card)?;
        let selected = negotiate_record_interface(&agent, &card, None)?;
        return Ok((agent, card, selected));
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
            let card = fetch.card.clone();
            let updated = cache_agent_card(
                &registry.store,
                &agent,
                fetch.raw_json,
                fetch.validators,
                MissiveTimestamp::now_utc(),
            )?;
            Ok((updated, card, selected))
        }
        AgentCardFetchOutcome::NotModified(_) => Err(MissiveError::protocol(format!(
            "agent {:?} Agent Card endpoint returned 304 Not Modified without a local cache",
            agent.alias.as_str()
        ))
        .with_help("Run 'missive agent inspect <alias> --refresh' after the remote endpoint returns a full card body.")),
    }
}

fn validate_streaming_capability(card: &AgentCard, force: bool) -> Result<StreamCapabilityView> {
    let advertised_streaming = card.capabilities.streaming.unwrap_or(false);
    if !advertised_streaming && !force {
        return Err(MissiveError::validation(
            "A2A Agent Card does not advertise capabilities.streaming=true",
        )
        .with_help(
            "Use an agent that supports SendStreamingMessage, refresh the Agent Card, or pass --force for interoperability testing.",
        ));
    }

    Ok(StreamCapabilityView {
        advertised_streaming,
        forced: !advertised_streaming && force,
    })
}

fn persist_stream_request(
    store: &mut Store,
    agent: &AgentRecord,
    prepared: &PreparedSend,
    service_parameters: &ServiceParameters,
) -> Result<MessageRecord> {
    store.transaction(|transaction| {
        if let Some(context_id) = &prepared.requested_context_id {
            ensure_context(transaction, context_id, agent)?;
        }
        if let Some(task_id) = &prepared.requested_task_id {
            ensure_task_from_parts(
                transaction,
                agent,
                task_id,
                prepared.requested_context_id.as_ref(),
                StoreTaskState::Submitted,
                None,
                service_parameters,
            )?;
        }

        let mut message = MessageInsert::new(
            prepared.request_message_id.clone(),
            MessageDirection::Request,
            serde_json::to_value(&prepared.request.message).map_err(|error| {
                MissiveError::protocol("encoding outbound A2A stream message for persistence")
                    .with_source(error)
            })?,
        );
        message.agent_alias = Some(agent.alias.clone());
        message.context_id = prepared.requested_context_id.clone();
        message.task_id = prepared.requested_task_id.clone();
        message.role = Some(MessageRole::User);
        message.protocol_message_id = Some(prepared.request.message.message_id.clone());
        message.metadata = prepared.local_metadata.clone();
        transaction.insert_message(&message)
    })
}

fn persist_stream_event(
    store: &mut Store,
    agent: &AgentRecord,
    event: &StreamMessageEvent,
    base_metadata: &Metadata,
    service_parameters: &ServiceParameters,
) -> Result<PersistedStreamEvent> {
    store.transaction(|transaction| {
        let ids = EventIds::from_stream_response(&event.event)?;
        if let Some(context_id) = &ids.context_id {
            ensure_context(transaction, context_id, agent)?;
        }
        ensure_task_placeholder_for_message_fk(transaction, agent, &ids, service_parameters)?;
        let message = insert_stream_event_message(transaction, agent, event, &ids, base_metadata)?;
        persist_task_state_for_event(
            transaction,
            agent,
            event,
            &ids,
            message.as_ref().map(|message| &message.message_id),
            service_parameters,
        )?;
        let journal_event = append_stream_event(transaction, agent, event, &ids, base_metadata)?;

        Ok(PersistedStreamEvent {
            event: journal_event,
            message,
        })
    })
}

#[derive(Debug, Clone)]
struct EventIds {
    task_id: Option<TaskId>,
    context_id: Option<ContextId>,
}

impl EventIds {
    fn from_stream_response(event: &StreamResponse) -> Result<Self> {
        match event {
            StreamResponse::Task(task) => Ok(Self {
                task_id: Some(TaskId::new(task.id.clone())?),
                context_id: Some(ContextId::new(task.context_id.clone())?),
            }),
            StreamResponse::Message(message) => Ok(Self {
                task_id: message
                    .task_id
                    .as_ref()
                    .map(|value| TaskId::new(value.clone()))
                    .transpose()?,
                context_id: message
                    .context_id
                    .as_ref()
                    .map(|value| ContextId::new(value.clone()))
                    .transpose()?,
            }),
            StreamResponse::StatusUpdate(update) => Ok(Self {
                task_id: Some(TaskId::new(update.task_id.clone())?),
                context_id: Some(ContextId::new(update.context_id.clone())?),
            }),
            StreamResponse::ArtifactUpdate(update) => Ok(Self {
                task_id: Some(TaskId::new(update.task_id.clone())?),
                context_id: Some(ContextId::new(update.context_id.clone())?),
            }),
        }
    }
}

fn persist_task_state_for_event(
    transaction: &StoreTransaction<'_>,
    agent: &AgentRecord,
    event: &StreamMessageEvent,
    ids: &EventIds,
    stream_message_id: Option<&MessageId>,
    service_parameters: &ServiceParameters,
) -> Result<()> {
    match &event.event {
        StreamResponse::Task(task) => upsert_remote_task(
            transaction,
            agent,
            task,
            stream_message_id,
            service_parameters,
        ),
        StreamResponse::StatusUpdate(update) => {
            let task_id = ids
                .task_id
                .as_ref()
                .expect("status update ids always include task id");
            let context_id = ids
                .context_id
                .as_ref()
                .expect("status update ids always include context id");
            ensure_task_from_parts(
                transaction,
                agent,
                task_id,
                Some(context_id),
                store_task_state(&update.status.state),
                stream_message_id,
                service_parameters,
            )
        }
        StreamResponse::ArtifactUpdate(_) | StreamResponse::Message(_) => {
            let Some(task_id) = &ids.task_id else {
                return Ok(());
            };
            let state = transaction
                .get_task(task_id)?
                .map(|task| task.state)
                .unwrap_or(StoreTaskState::Unknown);
            ensure_task_from_parts(
                transaction,
                agent,
                task_id,
                ids.context_id.as_ref(),
                state,
                stream_message_id,
                service_parameters,
            )
        }
    }
}

fn ensure_task_placeholder_for_message_fk(
    transaction: &StoreTransaction<'_>,
    agent: &AgentRecord,
    ids: &EventIds,
    service_parameters: &ServiceParameters,
) -> Result<()> {
    let Some(task_id) = &ids.task_id else {
        return Ok(());
    };
    if transaction.get_task(task_id)?.is_none() {
        ensure_task_from_parts(
            transaction,
            agent,
            task_id,
            ids.context_id.as_ref(),
            StoreTaskState::Unknown,
            None,
            service_parameters,
        )?;
    }
    Ok(())
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

fn upsert_remote_task(
    transaction: &StoreTransaction<'_>,
    agent: &AgentRecord,
    task: &Task,
    stream_message_id: Option<&MessageId>,
    service_parameters: &ServiceParameters,
) -> Result<()> {
    let task_id = TaskId::new(task.id.clone())?;
    let context_id = ContextId::new(task.context_id.clone())?;
    ensure_context(transaction, &context_id, agent)?;

    let mut task_upsert = merged_task_upsert(
        transaction,
        agent,
        &task_id,
        Some(&context_id),
        store_task_state(&task.status.state),
        stream_message_id,
        service_parameters,
    )?;
    task_upsert.remote_task_json = Some(serde_json::to_value(task).map_err(|error| {
        MissiveError::protocol("encoding A2A stream task event for persistence").with_source(error)
    })?);
    transaction.upsert_task(&task_upsert)?;
    Ok(())
}

fn ensure_task_from_parts(
    transaction: &StoreTransaction<'_>,
    agent: &AgentRecord,
    task_id: &TaskId,
    context_id: Option<&ContextId>,
    state: StoreTaskState,
    last_message_id: Option<&MessageId>,
    service_parameters: &ServiceParameters,
) -> Result<()> {
    let task_upsert = merged_task_upsert(
        transaction,
        agent,
        task_id,
        context_id,
        state,
        last_message_id,
        service_parameters,
    )?;
    transaction.upsert_task(&task_upsert)?;
    Ok(())
}

fn merged_task_upsert(
    transaction: &StoreTransaction<'_>,
    agent: &AgentRecord,
    task_id: &TaskId,
    context_id: Option<&ContextId>,
    state: StoreTaskState,
    last_message_id: Option<&MessageId>,
    service_parameters: &ServiceParameters,
) -> Result<TaskUpsert> {
    let existing = transaction.get_task(task_id)?;
    let mut task_upsert = TaskUpsert::new(task_id.clone(), agent.alias.clone(), state);
    task_upsert.source = TaskSource::Remote;
    task_upsert.context_id = context_id
        .cloned()
        .or_else(|| existing.as_ref().and_then(|task| task.context_id.clone()));
    task_upsert.remote_task_json = existing
        .as_ref()
        .and_then(|task| task.remote_task_json.clone());
    task_upsert.last_message_id = last_message_id.cloned().or_else(|| {
        existing
            .as_ref()
            .and_then(|task| task.last_message_id.clone())
    });
    task_upsert.completed_at = if matches!(
        state,
        StoreTaskState::Completed | StoreTaskState::Failed | StoreTaskState::Cancelled
    ) {
        Some(MissiveTimestamp::now_utc())
    } else {
        existing.and_then(|task| task.completed_at)
    };
    task_upsert.record_a2a_protocol_version(service_parameters.protocol_version.clone())?;
    Ok(task_upsert)
}

fn insert_stream_event_message(
    transaction: &StoreTransaction<'_>,
    agent: &AgentRecord,
    event: &StreamMessageEvent,
    ids: &EventIds,
    base_metadata: &Metadata,
) -> Result<Option<MessageRecord>> {
    let (content_json, role, protocol_message_id) = stream_event_message_content(event)?;
    let mut row = MessageInsert::new(
        new_local_message_id(),
        MessageDirection::StreamEvent,
        content_json,
    );
    row.agent_alias = Some(agent.alias.clone());
    row.context_id = ids.context_id.clone();
    row.task_id = ids.task_id.clone();
    row.role = role;
    row.ordinal = event.sequence + 1;
    row.protocol_message_id = protocol_message_id;
    row.metadata = stream_metadata(base_metadata, event)?;
    transaction.insert_message(&row).map(Some)
}

fn stream_event_message_content(
    event: &StreamMessageEvent,
) -> Result<(Value, Option<MessageRole>, Option<String>)> {
    match &event.event {
        StreamResponse::Message(message) => Ok((
            serde_json::to_value(message).map_err(|error| {
                MissiveError::protocol("encoding A2A stream message event for persistence")
                    .with_source(error)
            })?,
            store_message_role(&message.role),
            Some(message.message_id.clone()),
        )),
        StreamResponse::StatusUpdate(update) => {
            if let Some(message) = &update.status.message {
                Ok((
                    serde_json::to_value(message).map_err(|error| {
                        MissiveError::protocol("encoding A2A stream status message for persistence")
                            .with_source(error)
                    })?,
                    store_message_role(&message.role),
                    Some(message.message_id.clone()),
                ))
            } else {
                Ok((event.raw_json.clone(), Some(MessageRole::Unknown), None))
            }
        }
        StreamResponse::Task(_) | StreamResponse::ArtifactUpdate(_) => {
            Ok((event.raw_json.clone(), Some(MessageRole::Unknown), None))
        }
    }
}

fn append_stream_event(
    transaction: &StoreTransaction<'_>,
    agent: &AgentRecord,
    event: &StreamMessageEvent,
    ids: &EventIds,
    base_metadata: &Metadata,
) -> Result<EventRecord> {
    let mut journal = EventInsert::new(
        EventId::new(format!(
            "evt/stream/{}",
            missive_a2a::protocol::new_message_id()
        ))?,
        "cli",
        format!("a2a.stream.{}", stream_event_type(&event.event)),
        redact_json(&event.raw_json),
    );
    journal.agent_alias = Some(agent.alias.clone());
    journal.context_id = ids.context_id.clone();
    journal.task_id = ids.task_id.clone();
    journal.metadata = stream_metadata(base_metadata, event)?;
    journal.record_a2a_protocol_version(
        base_metadata
            .get_str(missive_a2a::METADATA_A2A_PROTOCOL_VERSION)
            .unwrap_or("unknown"),
    )?;
    transaction.append_event(&journal)
}

fn stream_metadata(base_metadata: &Metadata, event: &StreamMessageEvent) -> Result<Metadata> {
    let mut metadata = base_metadata.clone();
    metadata.insert("stream.sequence", json!(event.sequence))?;
    metadata.insert_str("stream.event_type", stream_event_type(&event.event))?;
    if let Some(sse_event_type) = &event.sse_event_type {
        metadata.insert_str("stream.sse_event", sse_event_type.clone())?;
    }
    Ok(metadata)
}

fn stream_event_type(event: &StreamResponse) -> &'static str {
    match event {
        StreamResponse::Task(_) => "task",
        StreamResponse::Message(_) => "message",
        StreamResponse::StatusUpdate(_) => "status_update",
        StreamResponse::ArtifactUpdate(_) => "artifact_update",
    }
}

fn render_stream_result<W>(
    writer: &mut W,
    mode: OutputMode,
    next_sequence: u64,
    output: &StreamOutput,
) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Ndjson => render_stream_item(
            writer,
            mode,
            "stream_result",
            next_sequence,
            output,
            &output.message,
        ),
        _ => render_success(writer, mode, "stream_result", output, &output.message),
    }
}

impl From<&NegotiatedInterface> for StreamInterfaceView {
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

impl StreamRequestView {
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

impl StreamEventView {
    fn from_event(
        profile: String,
        agent: &AgentRecord,
        event: &StreamMessageEvent,
        persisted: &PersistedStreamEvent,
    ) -> Result<Self> {
        let details = StreamEventDetails::from_response(&event.event)?;
        let persistence = StreamEventPersistenceView {
            event_sequence: persisted.event.sequence,
            event_id: persisted.event.event_id.as_str().to_owned(),
            message_id: persisted
                .message
                .as_ref()
                .map(|message| message.message_id.as_str().to_owned()),
        };
        let message = details.human_message(agent.alias.as_str());
        Ok(Self {
            profile,
            agent: agent.alias.as_str().to_owned(),
            sequence: event.sequence,
            event_type: details.event_type,
            task_id: details.task_id,
            context_id: details.context_id,
            state: details.state,
            message_id: details.message_id,
            text: details.text,
            artifact_id: details.artifact_id,
            append: details.append,
            last_chunk: details.last_chunk,
            raw: event.raw_json.clone(),
            persistence,
            message,
        })
    }
}

#[derive(Debug, Clone)]
struct StreamEventDetails {
    event_type: String,
    task_id: Option<String>,
    context_id: Option<String>,
    state: Option<String>,
    message_id: Option<String>,
    text: Option<String>,
    artifact_id: Option<String>,
    append: Option<bool>,
    last_chunk: Option<bool>,
}

impl StreamEventDetails {
    fn from_response(event: &StreamResponse) -> Result<Self> {
        match event {
            StreamResponse::Task(task) => Ok(Self {
                event_type: "task".to_owned(),
                task_id: Some(task.id.clone()),
                context_id: Some(task.context_id.clone()),
                state: Some(store_task_state(&task.status.state).as_str().to_owned()),
                message_id: task
                    .status
                    .message
                    .as_ref()
                    .map(|message| message.message_id.clone()),
                text: task
                    .status
                    .message
                    .as_ref()
                    .and_then(Message::text)
                    .map(ToOwned::to_owned),
                artifact_id: None,
                append: None,
                last_chunk: None,
            }),
            StreamResponse::Message(message) => Ok(Self {
                event_type: "message".to_owned(),
                task_id: message.task_id.clone(),
                context_id: message.context_id.clone(),
                state: None,
                message_id: Some(message.message_id.clone()),
                text: message.text().map(ToOwned::to_owned),
                artifact_id: None,
                append: None,
                last_chunk: None,
            }),
            StreamResponse::StatusUpdate(update) => status_update_details(update),
            StreamResponse::ArtifactUpdate(update) => artifact_update_details(update),
        }
    }

    fn human_message(&self, agent: &str) -> String {
        match self.event_type.as_str() {
            "status_update" => format!(
                "{} task {} status {}{}",
                agent,
                self.task_id.as_deref().unwrap_or("unknown"),
                self.state.as_deref().unwrap_or("unknown"),
                self.text
                    .as_deref()
                    .map(|text| format!(": {text}"))
                    .unwrap_or_default()
            ),
            "artifact_update" => format!(
                "{} task {} artifact {}{}{}",
                agent,
                self.task_id.as_deref().unwrap_or("unknown"),
                self.artifact_id.as_deref().unwrap_or("unknown"),
                self.append
                    .map(|append| format!(" append={append}"))
                    .unwrap_or_default(),
                self.last_chunk
                    .map(|last_chunk| format!(" last_chunk={last_chunk}"))
                    .unwrap_or_default()
            ),
            "task" => format!(
                "{} task {} state {}",
                agent,
                self.task_id.as_deref().unwrap_or("unknown"),
                self.state.as_deref().unwrap_or("unknown")
            ),
            _ => format!(
                "{} message {}{}",
                agent,
                self.message_id.as_deref().unwrap_or("unknown"),
                self.text
                    .as_deref()
                    .map(|text| format!(": {text}"))
                    .unwrap_or_default()
            ),
        }
    }
}

fn status_update_details(update: &TaskStatusUpdateEvent) -> Result<StreamEventDetails> {
    Ok(StreamEventDetails {
        event_type: "status_update".to_owned(),
        task_id: Some(update.task_id.clone()),
        context_id: Some(update.context_id.clone()),
        state: Some(store_task_state(&update.status.state).as_str().to_owned()),
        message_id: update
            .status
            .message
            .as_ref()
            .map(|message| message.message_id.clone()),
        text: update
            .status
            .message
            .as_ref()
            .and_then(Message::text)
            .map(ToOwned::to_owned),
        artifact_id: None,
        append: None,
        last_chunk: None,
    })
}

fn artifact_update_details(update: &TaskArtifactUpdateEvent) -> Result<StreamEventDetails> {
    Ok(StreamEventDetails {
        event_type: "artifact_update".to_owned(),
        task_id: Some(update.task_id.clone()),
        context_id: Some(update.context_id.clone()),
        state: None,
        message_id: None,
        text: update
            .artifact
            .parts
            .iter()
            .find_map(|part| part.as_text())
            .map(ToOwned::to_owned),
        artifact_id: Some(update.artifact.artifact_id.clone()),
        append: update.append,
        last_chunk: update.last_chunk,
    })
}

struct StreamOutputParts<'a> {
    profile: String,
    agent: &'a AgentRecord,
    selected_interface: &'a NegotiatedInterface,
    capability: StreamCapabilityView,
    prepared: &'a PreparedSend,
    request_message: &'a MessageRecord,
    events: &'a [StreamEventView],
    outcome: &'a StreamMessageOutcome,
}

impl StreamOutput {
    fn from_parts(parts: StreamOutputParts<'_>) -> Self {
        let StreamOutputParts {
            profile,
            agent,
            selected_interface,
            capability,
            prepared,
            request_message,
            events,
            outcome,
        } = parts;
        let final_task_id = events.iter().rev().find_map(|event| event.task_id.clone());
        let final_context_id = events
            .iter()
            .rev()
            .find_map(|event| event.context_id.clone());
        let final_state = events.iter().rev().find_map(|event| event.state.clone());
        let status_update_count = events
            .iter()
            .filter(|event| event.event_type == "status_update")
            .count();
        let artifact_update_count = events
            .iter()
            .filter(|event| event.event_type == "artifact_update")
            .count();
        let event_ids = events
            .iter()
            .map(|event| event.persistence.event_id.clone())
            .collect();
        let stream_message_ids = events
            .iter()
            .filter_map(|event| event.persistence.message_id.clone())
            .collect();
        let message = format!(
            "Streamed {} event(s) from '{}'{}",
            outcome.event_count,
            agent.alias.as_str(),
            final_state
                .as_deref()
                .map(|state| format!("; final state {state}"))
                .unwrap_or_default()
        );

        Self {
            profile,
            agent: agent.alias.as_str().to_owned(),
            selected_interface: StreamInterfaceView::from(selected_interface),
            capability,
            request: StreamRequestView::from_prepared(prepared),
            event_count: outcome.event_count,
            status_update_count,
            artifact_update_count,
            final_task_id,
            final_context_id,
            final_state,
            events: events.to_vec(),
            persistence: StreamPersistenceSummary {
                request_message_id: request_message.message_id.as_str().to_owned(),
                event_ids,
                stream_message_ids,
            },
            message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_args_convert_to_send_args_for_shared_input_parsing() {
        let args = StreamArgs {
            agent: "echo".to_owned(),
            message: Some("hello".to_owned()),
            stdin: false,
            files: Vec::new(),
            parts: vec!["text=extra".to_owned()],
            metadata: vec!["purpose=test".to_owned()],
            context: Some("ctx-1".to_owned()),
            task: Some("task-1".to_owned()),
            accepted_output_modes: vec!["text/plain".to_owned()],
            force: true,
        };

        let send_args = args.to_send_args();

        assert_eq!(send_args.agent, "echo");
        assert_eq!(send_args.message.as_deref(), Some("hello"));
        assert_eq!(send_args.parts, ["text=extra"]);
        assert_eq!(send_args.metadata, ["purpose=test"]);
        assert_eq!(send_args.context.as_deref(), Some("ctx-1"));
        assert_eq!(send_args.task.as_deref(), Some("task-1"));
        assert_eq!(send_args.accepted_output_modes, ["text/plain"]);
    }
}
