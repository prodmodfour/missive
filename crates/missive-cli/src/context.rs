//! Context continuity command implementation.
//!
//! This module implements local A2A context management for `missive context`.
//! Contexts are profile-scoped SQLite rows that can be named for humans while
//! preserving the opaque A2A `contextId` used by send, stream, task, message,
//! and event records.

use std::collections::BTreeMap;
use std::io::Write;

use clap::{Args, Subcommand};
use missive_core::{
    AgentAlias, ContextId, LoadedConfig, Metadata, MissiveError, MissiveTimestamp, Result,
};
use missive_store::{
    ContextRecord, ContextState, ContextUpsert, EventRecord, MessageRecord, Store, TaskRecord,
};
use serde::Serialize;
use serde_json::Value;

use crate::agent::{get_existing_agent, open_agent_registry};
use crate::output::{OutputMode, redact_json, redact_text, render_success};

const CONTEXT_NAME_MAX_BYTES: usize = 128;
const CONTEXT_SUMMARY_MAX_BYTES: usize = 8 * 1024;
const CONTEXT_NAME_HELP: &str =
    "Use a non-empty context name without control characters; quote names that contain spaces.";
const CONTEXT_SUMMARY_HELP: &str = "Use a concise non-secret context summary.";
const PARENT_CONTEXT_ID_METADATA_KEY: &str = "missive.context.parent_id";
const PARENT_CONTEXT_NAME_METADATA_KEY: &str = "missive.context.parent_name";
const FORKED_AT_METADATA_KEY: &str = "missive.context.forked_at";

/// Context subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum ContextCommands {
    /// Create a local A2A context row.
    Create(ContextCreateArgs),
    /// List local contexts with optional filters.
    List(ContextListArgs),
    /// Show one context selected by id or unique name.
    Show(ContextSelectorArgs),
    /// Fork one context and record its parent linkage.
    Fork(ContextForkArgs),
    /// Mark one context closed while retaining history.
    Close(ContextCloseArgs),
    /// Export one context with linked tasks, messages, and events.
    Export(ContextExportArgs),
}

impl ContextCommands {
    /// Stable subcommand spelling used in structured output messages.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Create(_) => "create",
            Self::List(_) => "list",
            Self::Show(_) => "show",
            Self::Fork(_) => "fork",
            Self::Close(_) => "close",
            Self::Export(_) => "export",
        }
    }
}

/// Arguments for `missive context create`.
#[derive(Debug, Clone, Args)]
pub struct ContextCreateArgs {
    /// Explicit A2A context id. If omitted, missive generates a local UUIDv7 id.
    #[arg(long = "id", value_name = "CONTEXT_ID")]
    pub id: Option<String>,

    /// Human-friendly context name.
    #[arg(long = "name", value_name = "NAME")]
    pub name: Option<String>,

    /// Default/owning agent alias for this context.
    #[arg(long = "agent", value_name = "ALIAS")]
    pub agent: Option<String>,

    /// Non-secret context summary.
    #[arg(long = "summary", value_name = "TEXT")]
    pub summary: Option<String>,

    /// Non-secret metadata entry as KEY=VALUE; VALUE is parsed as JSON when possible.
    #[arg(long = "metadata", value_name = "KEY=VALUE")]
    pub metadata: Vec<String>,
}

/// Arguments for commands that select one context.
#[derive(Debug, Clone, Args)]
pub struct ContextSelectorArgs {
    /// Context id or unique human-friendly context name.
    pub context: String,
}

/// Arguments for `missive context list`.
#[derive(Debug, Clone, Args)]
pub struct ContextListArgs {
    /// Filter by default/owning agent alias.
    #[arg(long = "agent", value_name = "ALIAS")]
    pub agent: Option<String>,

    /// Filter by exact human-friendly context name.
    #[arg(long = "name", value_name = "NAME")]
    pub name: Option<String>,

    /// Filter by lifecycle state: open, closed, or archived.
    #[arg(long = "state", value_name = "STATE", value_parser = parse_context_state_arg)]
    pub state: Option<ContextState>,

    /// Filter by parent context id.
    #[arg(long = "parent", value_name = "CONTEXT_ID")]
    pub parent: Option<String>,
}

/// Arguments for `missive context fork`.
#[derive(Debug, Clone, Args)]
pub struct ContextForkArgs {
    /// Source context id or unique human-friendly context name.
    pub context: String,

    /// Explicit child A2A context id. If omitted, missive generates a local UUIDv7 id.
    #[arg(long = "id", value_name = "CONTEXT_ID")]
    pub id: Option<String>,

    /// Human-friendly child context name.
    #[arg(long = "name", value_name = "NAME")]
    pub name: Option<String>,

    /// Default/owning agent alias for the child. Defaults to the parent context's agent.
    #[arg(long = "agent", value_name = "ALIAS")]
    pub agent: Option<String>,

    /// Non-secret child context summary.
    #[arg(long = "summary", value_name = "TEXT")]
    pub summary: Option<String>,

    /// Non-secret metadata entry as KEY=VALUE; VALUE is parsed as JSON when possible.
    #[arg(long = "metadata", value_name = "KEY=VALUE")]
    pub metadata: Vec<String>,
}

/// Arguments for `missive context close`.
#[derive(Debug, Clone, Args)]
pub struct ContextCloseArgs {
    /// Context id or unique human-friendly context name.
    pub context: String,

    /// Optional final non-secret summary to store with the context.
    #[arg(long = "summary", value_name = "TEXT")]
    pub summary: Option<String>,
}

/// Arguments for `missive context export`.
#[derive(Debug, Clone, Args)]
pub struct ContextExportArgs {
    /// Context id or unique human-friendly context name.
    pub context: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
struct ContextCounts {
    message_count: usize,
    task_count: usize,
    event_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ContextView {
    context_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_context_id: Option<String>,
    state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    metadata: Metadata,
    counts: ContextCounts,
    created_at: String,
    updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    closed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ContextActionOutput {
    profile: String,
    action: String,
    context: ContextView,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ContextListOutput {
    profile: String,
    filters: ContextFiltersView,
    count: usize,
    contexts: Vec<ContextView>,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ContextShowOutput {
    profile: String,
    context: ContextView,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ContextForkOutput {
    profile: String,
    parent: ContextView,
    context: ContextView,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ContextFiltersView {
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_context_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ContextExportOutput {
    profile: String,
    exported_at: String,
    redacted: bool,
    context: ContextView,
    counts: ContextCounts,
    tasks: Vec<TaskExportView>,
    messages: Vec<MessageExportView>,
    events: Vec<EventExportView>,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct TaskExportView {
    task_id: String,
    agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_id: Option<String>,
    state: String,
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    protocol_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_message_id: Option<String>,
    metadata: Metadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_task: Option<Value>,
    created_at: String,
    updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct MessageExportView {
    message_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    direction: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    ordinal: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    protocol_message_id: Option<String>,
    content: Value,
    metadata: Metadata,
    created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct EventExportView {
    sequence: i64,
    event_id: String,
    timestamp: String,
    source: String,
    event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    payload: Value,
    metadata: Metadata,
    redacted: bool,
}

#[derive(Debug, Clone)]
struct ParsedContextFilters {
    agent: Option<AgentAlias>,
    name: Option<String>,
    state: Option<ContextState>,
    parent_context_id: Option<ContextId>,
}

#[derive(Debug, Clone)]
struct ContextRelatedRecords {
    tasks: Vec<TaskRecord>,
    messages: Vec<MessageRecord>,
    events: Vec<EventRecord>,
}

/// Executes one context subcommand.
pub(crate) fn execute_context_command<W>(
    command: &ContextCommands,
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
        ContextCommands::Create(args) => {
            create_context(args, registry.profile, registry.store, mode, writer)
        }
        ContextCommands::List(args) => {
            list_contexts(args, registry.profile, registry.store, mode, writer)
        }
        ContextCommands::Show(args) => {
            show_context(args, registry.profile, registry.store, mode, writer)
        }
        ContextCommands::Fork(args) => {
            fork_context(args, registry.profile, registry.store, mode, writer)
        }
        ContextCommands::Close(args) => {
            close_context(args, registry.profile, registry.store, mode, writer)
        }
        ContextCommands::Export(args) => {
            export_context(args, registry.profile, registry.store, mode, writer)
        }
    }
}

fn create_context<W>(
    args: &ContextCreateArgs,
    profile: String,
    store: Store,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let context_id = parse_or_generate_context_id(args.id.as_deref())?;
    ensure_context_absent(&store, &context_id)?;
    let name = parse_context_name(args.name.as_deref())?;
    ensure_context_name_available(&store, name.as_deref(), None)?;
    let agent_alias = resolve_optional_agent(&store, args.agent.as_deref())?;
    let summary = parse_summary(args.summary.as_deref())?;
    let metadata = parse_metadata(&args.metadata)?;

    let mut upsert = ContextUpsert::new(context_id.clone());
    upsert.agent_alias = agent_alias;
    upsert.name = name;
    upsert.summary = summary;
    upsert.metadata = metadata;

    let record = store.upsert_context(&upsert)?;
    let view = ContextView::from_record(&store, &record)?;
    let output = ContextActionOutput {
        profile,
        action: "create".to_owned(),
        message: format!("Created context '{}'", record.context_id.as_str()),
        context: view,
    };

    render_context_action(writer, mode, "context_create", &output)
}

fn list_contexts<W>(
    args: &ContextListArgs,
    profile: String,
    store: Store,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let filters = ParsedContextFilters::from_list_args(args)?;
    let contexts = store
        .list_contexts()?
        .into_iter()
        .filter(|record| filters.matches(record))
        .map(|record| ContextView::from_record(&store, &record))
        .collect::<Result<Vec<_>>>()?;
    let output = ContextListOutput {
        profile,
        filters: ContextFiltersView::from_filters(&filters),
        count: contexts.len(),
        message: format!("Listed {} context(s)", contexts.len()),
        contexts,
    };

    render_context_list(writer, mode, &output)
}

fn show_context<W>(
    args: &ContextSelectorArgs,
    profile: String,
    store: Store,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let record = resolve_context_selector(&store, &args.context)?;
    let view = ContextView::from_record(&store, &record)?;
    let output = ContextShowOutput {
        profile,
        message: format!("Showing context '{}'", record.context_id.as_str()),
        context: view,
    };

    render_context_show(writer, mode, &output)
}

fn fork_context<W>(
    args: &ContextForkArgs,
    profile: String,
    store: Store,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let parent = resolve_context_selector(&store, &args.context)?;
    let child_id = parse_or_generate_context_id(args.id.as_deref())?;
    ensure_context_absent(&store, &child_id)?;
    let name = parse_context_name(args.name.as_deref())?;
    ensure_context_name_available(&store, name.as_deref(), None)?;
    let agent_alias = match args.agent.as_deref() {
        Some(agent) => resolve_optional_agent(&store, Some(agent))?,
        None => parent.agent_alias.clone(),
    };
    let summary = parse_summary(args.summary.as_deref())?;
    let mut metadata = parse_metadata(&args.metadata)?;
    record_parent_metadata(&mut metadata, &parent)?;

    let mut upsert = ContextUpsert::new(child_id.clone());
    upsert.agent_alias = agent_alias;
    upsert.name = name;
    upsert.parent_context_id = Some(parent.context_id.clone());
    upsert.summary = summary;
    upsert.metadata = metadata;

    let child = store.upsert_context(&upsert)?;
    let parent_view = ContextView::from_record(&store, &parent)?;
    let child_view = ContextView::from_record(&store, &child)?;
    let output = ContextForkOutput {
        profile,
        message: format!(
            "Forked context '{}' from parent '{}'",
            child.context_id.as_str(),
            parent.context_id.as_str()
        ),
        parent: parent_view,
        context: child_view,
    };

    render_context_fork(writer, mode, &output)
}

fn close_context<W>(
    args: &ContextCloseArgs,
    profile: String,
    store: Store,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let record = resolve_context_selector(&store, &args.context)?;
    let summary = parse_summary(args.summary.as_deref())?.or(record.summary.clone());
    let mut upsert = ContextUpsert::new(record.context_id.clone());
    upsert.agent_alias = record.agent_alias.clone();
    upsert.name = record.name.clone();
    upsert.parent_context_id = record.parent_context_id.clone();
    upsert.state = ContextState::Closed;
    upsert.summary = summary;
    upsert.metadata = record.metadata.clone();
    upsert.closed_at = record
        .closed_at
        .or_else(|| Some(MissiveTimestamp::now_utc()));

    let closed = store.upsert_context(&upsert)?;
    let view = ContextView::from_record(&store, &closed)?;
    let output = ContextActionOutput {
        profile,
        action: "close".to_owned(),
        message: format!("Closed context '{}'", closed.context_id.as_str()),
        context: view,
    };

    render_context_action(writer, mode, "context_close", &output)
}

fn export_context<W>(
    args: &ContextExportArgs,
    profile: String,
    store: Store,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let record = resolve_context_selector(&store, &args.context)?;
    let related = related_records(&store, &record.context_id)?;
    let counts = ContextCounts::from_related(&related);
    let view = ContextView::from_record_and_counts(&record, counts.clone());
    let output = ContextExportOutput {
        profile,
        exported_at: MissiveTimestamp::now_utc().to_rfc3339(),
        redacted: true,
        context: view,
        counts: counts.clone(),
        tasks: related
            .tasks
            .iter()
            .map(TaskExportView::from_record)
            .collect(),
        messages: related
            .messages
            .iter()
            .map(MessageExportView::from_record)
            .collect(),
        events: related
            .events
            .iter()
            .map(EventExportView::from_record)
            .collect(),
        message: format!(
            "Exported context '{}' with {} message(s), {} task(s), and {} event(s)",
            record.context_id.as_str(),
            counts.message_count,
            counts.task_count,
            counts.event_count
        ),
    };

    render_context_export(writer, mode, &output)
}

fn parse_or_generate_context_id(id: Option<&str>) -> Result<ContextId> {
    match id {
        Some(value) => ContextId::new(value.to_owned()),
        None => ContextId::new(missive_a2a::protocol::new_context_id()),
    }
}

fn ensure_context_absent(store: &Store, context_id: &ContextId) -> Result<()> {
    if store.get_context(context_id)?.is_some() {
        return Err(MissiveError::validation(format!(
            "context {:?} already exists",
            context_id.as_str()
        ))
        .with_help(
            "Choose another --id or omit --id so missive can generate a fresh context id.",
        ));
    }
    Ok(())
}

fn ensure_context_name_available(
    store: &Store,
    name: Option<&str>,
    except: Option<&ContextId>,
) -> Result<()> {
    let Some(name) = name else {
        return Ok(());
    };
    let duplicate = store.list_contexts()?.into_iter().find(|context| {
        context.name.as_deref() == Some(name)
            && except.is_none_or(|except| &context.context_id != except)
    });
    if let Some(context) = duplicate {
        return Err(MissiveError::validation(format!(
            "context name {:?} is already used by context {:?}",
            name,
            context.context_id.as_str()
        ))
        .with_help("Use a unique --name or select contexts by their A2A context id."));
    }
    Ok(())
}

fn resolve_optional_agent(store: &Store, agent: Option<&str>) -> Result<Option<AgentAlias>> {
    let Some(agent) = agent else {
        return Ok(None);
    };
    let alias = AgentAlias::new(agent.to_owned())?;
    get_existing_agent(store, &alias)?;
    Ok(Some(alias))
}

fn resolve_context_selector(store: &Store, selector: &str) -> Result<ContextRecord> {
    if let Ok(context_id) = ContextId::new(selector.to_owned())
        && let Some(record) = store.get_context(&context_id)?
    {
        return Ok(record);
    }

    let matches = store
        .list_contexts()?
        .into_iter()
        .filter(|context| context.name.as_deref() == Some(selector))
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [record] => Ok(record.clone()),
        [] => Err(MissiveError::validation(format!(
            "context selector {:?} did not match a known context id or unique name",
            selector
        ))
        .with_help("Run 'missive context list' to see locally known context ids and names.")),
        duplicates => Err(MissiveError::validation(format!(
            "context name {:?} is ambiguous across {} contexts",
            selector,
            duplicates.len()
        ))
        .with_help("Select the context by its A2A context id instead of name.")),
    }
}

fn record_parent_metadata(metadata: &mut Metadata, parent: &ContextRecord) -> Result<()> {
    metadata.insert_str(PARENT_CONTEXT_ID_METADATA_KEY, parent.context_id.as_str())?;
    if let Some(name) = &parent.name {
        metadata.insert_str(PARENT_CONTEXT_NAME_METADATA_KEY, name.clone())?;
    }
    metadata.insert_str(
        FORKED_AT_METADATA_KEY,
        MissiveTimestamp::now_utc().to_rfc3339(),
    )?;
    Ok(())
}

fn related_records(store: &Store, context_id: &ContextId) -> Result<ContextRelatedRecords> {
    let tasks = store
        .list_tasks()?
        .into_iter()
        .filter(|task| task.context_id.as_ref() == Some(context_id))
        .collect();
    let messages = store
        .list_messages()?
        .into_iter()
        .filter(|message| message.context_id.as_ref() == Some(context_id))
        .collect();
    let events = store
        .list_events()?
        .into_iter()
        .filter(|event| event.context_id.as_ref() == Some(context_id))
        .collect();

    Ok(ContextRelatedRecords {
        tasks,
        messages,
        events,
    })
}

fn parse_context_name(value: Option<&str>) -> Result<Option<String>> {
    value
        .map(|value| {
            validate_optional_text("--name", value, CONTEXT_NAME_MAX_BYTES, CONTEXT_NAME_HELP)
        })
        .transpose()
}

fn parse_summary(value: Option<&str>) -> Result<Option<String>> {
    value
        .map(|value| {
            validate_optional_text(
                "--summary",
                value,
                CONTEXT_SUMMARY_MAX_BYTES,
                CONTEXT_SUMMARY_HELP,
            )
        })
        .transpose()
}

fn validate_optional_text(flag: &str, value: &str, max_bytes: usize, help: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(
            MissiveError::validation(format!("{flag} value cannot be empty")).with_help(help),
        );
    }
    if value.len() > max_bytes {
        return Err(MissiveError::validation(format!(
            "{flag} value is {} bytes, but the maximum is {max_bytes}",
            value.len()
        ))
        .with_help(help));
    }
    if value.chars().any(char::is_control) {
        return Err(MissiveError::validation(format!(
            "{flag} value cannot contain control characters"
        ))
        .with_help(help));
    }
    Ok(value.to_owned())
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

fn parse_context_state_arg(value: &str) -> std::result::Result<ContextState, String> {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "open" => Ok(ContextState::Open),
        "closed" => Ok(ContextState::Closed),
        "archived" => Ok(ContextState::Archived),
        _ => Err("expected open, closed, or archived".to_owned()),
    }
}

impl ParsedContextFilters {
    fn from_list_args(args: &ContextListArgs) -> Result<Self> {
        Ok(Self {
            agent: args
                .agent
                .as_ref()
                .map(|value| AgentAlias::new(value.clone()))
                .transpose()?,
            name: parse_context_name(args.name.as_deref())?,
            state: args.state,
            parent_context_id: args
                .parent
                .as_ref()
                .map(|value| ContextId::new(value.clone()))
                .transpose()?,
        })
    }

    fn matches(&self, record: &ContextRecord) -> bool {
        if self
            .agent
            .as_ref()
            .is_some_and(|agent| record.agent_alias.as_ref() != Some(agent))
        {
            return false;
        }
        if self
            .name
            .as_ref()
            .is_some_and(|name| record.name.as_ref() != Some(name))
        {
            return false;
        }
        if self.state.is_some_and(|state| record.state != state) {
            return false;
        }
        if self
            .parent_context_id
            .as_ref()
            .is_some_and(|parent| record.parent_context_id.as_ref() != Some(parent))
        {
            return false;
        }
        true
    }
}

impl ContextFiltersView {
    fn from_filters(filters: &ParsedContextFilters) -> Self {
        Self {
            agent: filters
                .agent
                .as_ref()
                .map(|agent| agent.as_str().to_owned()),
            name: filters.name.clone(),
            state: filters.state.map(|state| state.as_str().to_owned()),
            parent_context_id: filters
                .parent_context_id
                .as_ref()
                .map(|context_id| context_id.as_str().to_owned()),
        }
    }
}

impl ContextCounts {
    fn from_related(related: &ContextRelatedRecords) -> Self {
        Self {
            message_count: related.messages.len(),
            task_count: related.tasks.len(),
            event_count: related.events.len(),
        }
    }
}

impl ContextView {
    fn from_record(store: &Store, record: &ContextRecord) -> Result<Self> {
        let related = related_records(store, &record.context_id)?;
        Ok(Self::from_record_and_counts(
            record,
            ContextCounts::from_related(&related),
        ))
    }

    fn from_record_and_counts(record: &ContextRecord, counts: ContextCounts) -> Self {
        Self {
            context_id: record.context_id.as_str().to_owned(),
            name: record.name.clone(),
            agent: record
                .agent_alias
                .as_ref()
                .map(|agent| agent.as_str().to_owned()),
            parent_context_id: record
                .parent_context_id
                .as_ref()
                .map(|context_id| context_id.as_str().to_owned()),
            state: record.state.as_str().to_owned(),
            summary: record.summary.clone(),
            metadata: record.metadata.clone(),
            counts,
            created_at: record.created_at.to_rfc3339(),
            updated_at: record.updated_at.to_rfc3339(),
            closed_at: record.closed_at.map(MissiveTimestamp::to_rfc3339),
        }
    }
}

impl TaskExportView {
    fn from_record(record: &TaskRecord) -> Self {
        Self {
            task_id: record.task_id.as_str().to_owned(),
            agent: record.agent_alias.as_str().to_owned(),
            context_id: record
                .context_id
                .as_ref()
                .map(|context_id| context_id.as_str().to_owned()),
            state: record.state.as_str().to_owned(),
            source: record.source.as_str().to_owned(),
            protocol_version: record.protocol_version.clone(),
            last_message_id: record
                .last_message_id
                .as_ref()
                .map(|message_id| message_id.as_str().to_owned()),
            metadata: record.metadata.clone(),
            remote_task: record.remote_task_json.as_ref().map(redact_json),
            created_at: record.created_at.to_rfc3339(),
            updated_at: record.updated_at.to_rfc3339(),
            completed_at: record.completed_at.map(MissiveTimestamp::to_rfc3339),
        }
    }
}

impl MessageExportView {
    fn from_record(record: &MessageRecord) -> Self {
        Self {
            message_id: record.message_id.as_str().to_owned(),
            agent: record
                .agent_alias
                .as_ref()
                .map(|agent| agent.as_str().to_owned()),
            context_id: record
                .context_id
                .as_ref()
                .map(|context_id| context_id.as_str().to_owned()),
            task_id: record
                .task_id
                .as_ref()
                .map(|task_id| task_id.as_str().to_owned()),
            direction: record.direction.as_str().to_owned(),
            role: record.role.map(|role| role.as_str().to_owned()),
            ordinal: record.ordinal,
            protocol_message_id: record.protocol_message_id.clone(),
            content: redact_json(&record.content_json),
            metadata: record.metadata.clone(),
            created_at: record.created_at.to_rfc3339(),
        }
    }
}

impl EventExportView {
    fn from_record(record: &EventRecord) -> Self {
        Self {
            sequence: record.sequence,
            event_id: record.event_id.as_str().to_owned(),
            timestamp: record.timestamp.to_rfc3339(),
            source: record.source.clone(),
            event_type: record.event_type.clone(),
            agent: record
                .agent_alias
                .as_ref()
                .map(|agent| agent.as_str().to_owned()),
            context_id: record
                .context_id
                .as_ref()
                .map(|context_id| context_id.as_str().to_owned()),
            task_id: record
                .task_id
                .as_ref()
                .map(|task_id| task_id.as_str().to_owned()),
            payload: redact_json(&record.payload_json),
            metadata: record.metadata.clone(),
            redacted: record.redacted,
        }
    }
}

fn render_context_action<W>(
    writer: &mut W,
    mode: OutputMode,
    kind: &str,
    output: &ContextActionOutput,
) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => write_context_human(writer, &output.context, Some(&output.message)),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, kind, output, &output.message)
        }
    }
}

fn render_context_list<W>(
    writer: &mut W,
    mode: OutputMode,
    output: &ContextListOutput,
) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => write_context_list_human(writer, output),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, "context_list", output, &output.message)
        }
    }
}

fn render_context_show<W>(
    writer: &mut W,
    mode: OutputMode,
    output: &ContextShowOutput,
) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => write_context_human(writer, &output.context, None),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, "context_show", output, &output.message)
        }
    }
}

fn render_context_fork<W>(
    writer: &mut W,
    mode: OutputMode,
    output: &ContextForkOutput,
) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => write_context_human(writer, &output.context, Some(&output.message)),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, "context_fork", output, &output.message)
        }
    }
}

fn render_context_export<W>(
    writer: &mut W,
    mode: OutputMode,
    output: &ContextExportOutput,
) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => {
            writeln!(writer, "{}", redact_text(&output.message))
                .map_err(|error| MissiveError::io("writing context export output", error))?;
            writeln!(writer, "  redacted: true")
                .map_err(|error| MissiveError::io("writing context export output", error))?;
            writeln!(
                writer,
                "  use --json or --ndjson to emit the full export payload"
            )
            .map_err(|error| MissiveError::io("writing context export output", error))
        }
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, "context_export", output, &output.message)
        }
    }
}

fn write_context_list_human<W>(writer: &mut W, output: &ContextListOutput) -> Result<()>
where
    W: Write,
{
    if output.contexts.is_empty() {
        return writeln!(
            writer,
            "No contexts matched for profile '{}'.",
            redact_text(&output.profile)
        )
        .map_err(|error| MissiveError::io("writing context list output", error));
    }

    writeln!(
        writer,
        "Contexts for profile '{}':",
        redact_text(&output.profile)
    )
    .map_err(|error| MissiveError::io("writing context list output", error))?;
    for context in &output.contexts {
        writeln!(
            writer,
            "  {}  state={}  name={}  agent={}  parent={}  messages={}  tasks={}  events={}",
            redact_text(&context.context_id),
            redact_text(&context.state),
            context
                .name
                .as_deref()
                .map(redact_text)
                .unwrap_or_else(|| "-".to_owned()),
            context
                .agent
                .as_deref()
                .map(redact_text)
                .unwrap_or_else(|| "-".to_owned()),
            context
                .parent_context_id
                .as_deref()
                .map(redact_text)
                .unwrap_or_else(|| "-".to_owned()),
            context.counts.message_count,
            context.counts.task_count,
            context.counts.event_count
        )
        .map_err(|error| MissiveError::io("writing context list output", error))?;
    }
    Ok(())
}

fn write_context_human<W>(
    writer: &mut W,
    context: &ContextView,
    message: Option<&str>,
) -> Result<()>
where
    W: Write,
{
    if let Some(message) = message {
        writeln!(writer, "{}", redact_text(message))
            .map_err(|error| MissiveError::io("writing context output", error))?;
    }
    writeln!(writer, "Context {}", redact_text(&context.context_id))
        .map_err(|error| MissiveError::io("writing context output", error))?;
    writeln!(
        writer,
        "  name: {}",
        context
            .name
            .as_deref()
            .map(redact_text)
            .unwrap_or_else(|| "-".to_owned())
    )
    .map_err(|error| MissiveError::io("writing context output", error))?;
    writeln!(
        writer,
        "  agent: {}",
        context
            .agent
            .as_deref()
            .map(redact_text)
            .unwrap_or_else(|| "-".to_owned())
    )
    .map_err(|error| MissiveError::io("writing context output", error))?;
    writeln!(writer, "  state: {}", redact_text(&context.state))
        .map_err(|error| MissiveError::io("writing context output", error))?;
    writeln!(
        writer,
        "  parent: {}",
        context
            .parent_context_id
            .as_deref()
            .map(redact_text)
            .unwrap_or_else(|| "-".to_owned())
    )
    .map_err(|error| MissiveError::io("writing context output", error))?;
    if let Some(summary) = &context.summary {
        writeln!(writer, "  summary: {}", redact_text(summary))
            .map_err(|error| MissiveError::io("writing context output", error))?;
    }
    writeln!(writer, "  messages: {}", context.counts.message_count)
        .map_err(|error| MissiveError::io("writing context output", error))?;
    writeln!(writer, "  tasks: {}", context.counts.task_count)
        .map_err(|error| MissiveError::io("writing context output", error))?;
    writeln!(writer, "  events: {}", context.counts.event_count)
        .map_err(|error| MissiveError::io("writing context output", error))?;
    writeln!(writer, "  updated_at: {}", redact_text(&context.updated_at))
        .map_err(|error| MissiveError::io("writing context output", error))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn context_state_parser_accepts_expected_values() {
        assert_eq!(parse_context_state_arg("open"), Ok(ContextState::Open));
        assert_eq!(parse_context_state_arg("closed"), Ok(ContextState::Closed));
        assert_eq!(
            parse_context_state_arg("archived"),
            Ok(ContextState::Archived)
        );
        assert!(parse_context_state_arg("done").is_err());
    }

    #[test]
    fn context_name_validation_rejects_empty_or_control_values() {
        assert!(parse_context_name(Some("planning round")).is_ok());
        assert!(parse_context_name(Some("   ")).is_err());
        assert!(parse_context_name(Some("bad\nname")).is_err());
    }

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
}
