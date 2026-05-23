//! Event journal listing, tailing, replay, and export commands.
//!
//! The event command exposes the append-only SQLite `events` table in stable
//! human, JSON, and NDJSON forms. Producers in other CLI modules use the helper
//! functions in this module to create redacted CLI-sourced journal records.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::thread;
use std::time::{Duration, Instant};

use clap::{Args, Subcommand};
use missive_core::{
    AgentAlias, ContextId, EventId, LoadedConfig, Metadata, MissiveError, MissiveTimestamp, Result,
    TaskId,
};
use missive_store::{
    EventInsert, EventRecord, ProcessLock, ProcessLockKind, StatePathResolver, Store,
};
use serde::Serialize;
use serde_json::Value;

use crate::GlobalArgs;
use crate::output::{OutputMode, redact_json, redact_text, render_stream_item, render_success};

const DEFAULT_TAIL_POLL_INTERVAL: Duration = Duration::from_millis(500);
const MAX_TAIL_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Event journal subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum EventsCommands {
    /// List event journal records that match optional filters.
    List(EventListArgs),
    /// Follow new event journal records as they are appended.
    Tail(EventTailArgs),
    /// Reconstruct task/context summaries from matching event records.
    Replay(EventReplayArgs),
    /// Export matching event records, including one-record-per-line NDJSON.
    Export(EventExportArgs),
}

impl EventsCommands {
    /// Stable subcommand spelling used in structured output messages.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::List(_) => "list",
            Self::Tail(_) => "tail",
            Self::Replay(_) => "replay",
            Self::Export(_) => "export",
        }
    }
}

/// Shared event selector options.
#[derive(Debug, Clone, Default, Args)]
pub struct EventSelectorArgs {
    /// Filter by linked agent alias.
    #[arg(long = "agent", value_name = "ALIAS")]
    pub agent: Option<String>,

    /// Filter by linked A2A context id.
    #[arg(long = "context", value_name = "CONTEXT_ID")]
    pub context: Option<String>,

    /// Filter by linked A2A task id.
    #[arg(long = "task", value_name = "TASK_ID")]
    pub task: Option<String>,

    /// Filter by exact event source, for example cli, gateway, or adapter:local.
    #[arg(long = "source", value_name = "SOURCE")]
    pub source: Option<String>,

    /// Filter by exact event type, for example a2a.task.updated.
    #[arg(long = "type", value_name = "TYPE")]
    pub event_type: Option<String>,

    /// Filter to events at or after this RFC3339 timestamp.
    #[arg(long = "since", value_name = "RFC3339")]
    pub since: Option<String>,
}

/// Arguments for `missive events list`.
#[derive(Debug, Clone, Args)]
pub struct EventListArgs {
    /// Event filters.
    #[command(flatten)]
    pub selector: EventSelectorArgs,

    /// Return events with sequence greater than this value.
    #[arg(long = "after-sequence", value_name = "N")]
    pub after_sequence: Option<i64>,

    /// Maximum number of events to return.
    #[arg(long = "limit", value_name = "N")]
    pub limit: Option<usize>,
}

/// Arguments for `missive events tail`.
#[derive(Debug, Clone, Args)]
pub struct EventTailArgs {
    /// Event filters.
    #[command(flatten)]
    pub selector: EventSelectorArgs,

    /// Start after this sequence. Defaults to the current end of the journal.
    #[arg(long = "from-sequence", value_name = "N")]
    pub from_sequence: Option<i64>,

    /// Maximum number of matching events to print before exiting.
    #[arg(long = "limit", value_name = "N")]
    pub limit: Option<usize>,

    /// Poll interval while following, such as 100ms, 1s, or 1m.
    #[arg(long = "poll-interval", value_name = "DURATION")]
    pub poll_interval: Option<String>,
}

/// Arguments for `missive events replay`.
#[derive(Debug, Clone, Args)]
pub struct EventReplayArgs {
    /// Event filters.
    #[command(flatten)]
    pub selector: EventSelectorArgs,

    /// Replay events with sequence greater than this value.
    #[arg(long = "after-sequence", value_name = "N")]
    pub after_sequence: Option<i64>,

    /// Maximum number of events to replay.
    #[arg(long = "limit", value_name = "N")]
    pub limit: Option<usize>,
}

/// Arguments for `missive events export`.
#[derive(Debug, Clone, Args)]
pub struct EventExportArgs {
    /// Event filters.
    #[command(flatten)]
    pub selector: EventSelectorArgs,

    /// Export events with sequence greater than this value.
    #[arg(long = "after-sequence", value_name = "N")]
    pub after_sequence: Option<i64>,

    /// Maximum number of events to export.
    #[arg(long = "limit", value_name = "N")]
    pub limit: Option<usize>,
}

#[derive(Debug)]
struct EventStore {
    store: Store,
    profile: String,
}

#[derive(Debug, Clone)]
struct ParsedEventFilters {
    agent: Option<AgentAlias>,
    context_id: Option<ContextId>,
    task_id: Option<TaskId>,
    source: Option<String>,
    event_type: Option<String>,
    since: Option<MissiveTimestamp>,
    after_sequence: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct EventFilterView {
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    event_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    since: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    after_sequence: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct EventRecordView {
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
    #[serde(skip_serializing_if = "Option::is_none")]
    group_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gateway_job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    adapter_binding_id: Option<String>,
    payload: Value,
    metadata: Metadata,
    redacted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct EventListOutput {
    profile: String,
    filters: EventFilterView,
    count: usize,
    events: Vec<EventRecordView>,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct EventTailOutput {
    profile: String,
    filters: EventFilterView,
    from_sequence: i64,
    emitted: usize,
    last_seen_sequence: i64,
    timed_out: bool,
    events: Vec<EventRecordView>,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct EventExportOutput {
    profile: String,
    filters: EventFilterView,
    count: usize,
    events: Vec<EventRecordView>,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct EventReplayOutput {
    profile: String,
    filters: EventFilterView,
    event_count: usize,
    context_count: usize,
    task_count: usize,
    event_types: BTreeMap<String, usize>,
    contexts: Vec<ReplayContextView>,
    tasks: Vec<ReplayTaskView>,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ReplayContextView {
    context_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    task_ids: Vec<String>,
    event_count: usize,
    first_sequence: i64,
    last_sequence: i64,
    first_timestamp: String,
    last_timestamp: String,
    last_event_type: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ReplayTaskView {
    task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    event_count: usize,
    first_sequence: i64,
    last_sequence: i64,
    first_timestamp: String,
    last_timestamp: String,
    last_event_type: String,
}

#[derive(Debug, Clone)]
struct ReplayContextAccumulator {
    context_id: String,
    agent: Option<String>,
    state: Option<String>,
    name: Option<String>,
    task_ids: BTreeSet<String>,
    event_count: usize,
    first_sequence: i64,
    last_sequence: i64,
    first_timestamp: MissiveTimestamp,
    last_timestamp: MissiveTimestamp,
    last_event_type: String,
}

#[derive(Debug, Clone)]
struct ReplayTaskAccumulator {
    task_id: String,
    agent: Option<String>,
    context_id: Option<String>,
    state: Option<String>,
    event_count: usize,
    first_sequence: i64,
    last_sequence: i64,
    first_timestamp: MissiveTimestamp,
    last_timestamp: MissiveTimestamp,
    last_event_type: String,
}

/// Creates a stable event id for CLI-generated journal records.
pub(crate) fn new_event_id(prefix: &str) -> Result<EventId> {
    EventId::new(format!(
        "evt/{prefix}/{}",
        missive_a2a::protocol::new_message_id()
    ))
}

/// Creates a redacted CLI-sourced event insert with a generated event id.
pub(crate) fn new_cli_event(event_type: &str, payload: Value) -> Result<EventInsert> {
    Ok(EventInsert::new(
        new_event_id(event_type)?,
        "cli",
        event_type,
        redact_json(&payload),
    ))
}

/// Executes one event journal subcommand.
pub(crate) fn execute_events_command<W>(
    command: &EventsCommands,
    globals: &GlobalArgs,
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let mut event_store = open_event_store(loaded_config, environment)?;

    match command {
        EventsCommands::List(args) => list_events(args, &event_store, mode, writer),
        EventsCommands::Tail(args) => tail_events(args, globals, &mut event_store, mode, writer),
        EventsCommands::Replay(args) => replay_events(args, &event_store, mode, writer),
        EventsCommands::Export(args) => export_events(args, &event_store, mode, writer),
    }
}

fn open_event_store(
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
) -> Result<EventStore> {
    let resolver = StatePathResolver::new().with_env(
        environment
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    let paths = resolver.resolve_loaded(loaded_config)?;
    paths.ensure_directories()?;

    let store = {
        let _migration_lock = ProcessLock::acquire(&paths, ProcessLockKind::StateMutation)?;
        Store::open(paths.database_path())?
    };
    Ok(EventStore {
        store,
        profile: loaded_config.selected_profile.clone(),
    })
}

fn list_events<W>(
    args: &EventListArgs,
    event_store: &EventStore,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let filters = ParsedEventFilters::from_parts(&args.selector, args.after_sequence)?;
    let records = filtered_events(&event_store.store, &filters, args.limit)?;
    let events = records.iter().map(EventRecordView::from_record).collect();
    let output = EventListOutput {
        profile: event_store.profile.clone(),
        filters: EventFilterView::from_filters(&filters, args.limit),
        count: records.len(),
        events,
        message: format!("Listed {} event(s)", records.len()),
    };

    render_event_list(writer, mode, &output)
}

fn tail_events<W>(
    args: &EventTailArgs,
    globals: &GlobalArgs,
    event_store: &mut EventStore,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let selector = ParsedEventFilters::from_parts(&args.selector, None)?;
    let poll_interval = match args.poll_interval.as_deref() {
        Some(value) => parse_duration_arg("--poll-interval", value)?,
        None => DEFAULT_TAIL_POLL_INTERVAL,
    };
    let poll_interval = poll_interval.min(MAX_TAIL_POLL_INTERVAL);
    let timeout = globals
        .timeout
        .as_deref()
        .map(|value| parse_duration_arg("--timeout", value))
        .transpose()?;

    let mut last_seen_sequence = match args.from_sequence {
        Some(sequence) => validate_non_negative_sequence("--from-sequence", sequence)?,
        None => event_store
            .store
            .list_events()?
            .last()
            .map_or(0, |event| event.sequence),
    };
    let from_sequence = last_seen_sequence;
    let started = Instant::now();
    let mut emitted = 0_usize;
    let mut collected = Vec::new();
    let mut timed_out = false;

    loop {
        let all_new = event_store
            .store
            .list_events()?
            .into_iter()
            .filter(|event| event.sequence > last_seen_sequence)
            .collect::<Vec<_>>();
        if let Some(max_sequence) = all_new.last().map(|event| event.sequence) {
            last_seen_sequence = max_sequence;
        }

        for event in all_new.into_iter().filter(|event| selector.matches(event)) {
            let view = EventRecordView::from_record(&event);
            render_event_item(writer, mode, &view)?;
            collected.push(view);
            emitted += 1;
            if args.limit.is_some_and(|limit| emitted >= limit) {
                return render_event_tail_summary(
                    writer,
                    mode,
                    event_store.profile.clone(),
                    &selector,
                    args.limit,
                    from_sequence,
                    emitted,
                    last_seen_sequence,
                    false,
                    collected,
                );
            }
        }

        if timeout.is_some_and(|timeout| started.elapsed() >= timeout) {
            timed_out = true;
            break;
        }

        if args.limit == Some(0) {
            break;
        }

        thread::sleep(poll_interval);
    }

    render_event_tail_summary(
        writer,
        mode,
        event_store.profile.clone(),
        &selector,
        args.limit,
        from_sequence,
        emitted,
        last_seen_sequence,
        timed_out,
        collected,
    )
}

fn replay_events<W>(
    args: &EventReplayArgs,
    event_store: &EventStore,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let filters = ParsedEventFilters::from_parts(&args.selector, args.after_sequence)?;
    let records = filtered_events(&event_store.store, &filters, args.limit)?;
    let replay = replay_records(&event_store.profile, &filters, args.limit, &records);

    render_event_replay(writer, mode, &replay)
}

fn export_events<W>(
    args: &EventExportArgs,
    event_store: &EventStore,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let filters = ParsedEventFilters::from_parts(&args.selector, args.after_sequence)?;
    let records = filtered_events(&event_store.store, &filters, args.limit)?;
    let events = records
        .iter()
        .map(EventRecordView::from_record)
        .collect::<Vec<_>>();
    let output = EventExportOutput {
        profile: event_store.profile.clone(),
        filters: EventFilterView::from_filters(&filters, args.limit),
        count: events.len(),
        message: format!("Exported {} event(s)", events.len()),
        events,
    };

    render_event_export(writer, mode, &output)
}

fn filtered_events(
    store: &Store,
    filters: &ParsedEventFilters,
    limit: Option<usize>,
) -> Result<Vec<EventRecord>> {
    if limit == Some(0) {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for record in store.list_events()? {
        if filters.matches(&record) {
            records.push(record);
            if limit.is_some_and(|limit| records.len() >= limit) {
                break;
            }
        }
    }
    Ok(records)
}

/// Replays event records into the same stable JSON summary used by the CLI.
///
/// This hidden helper keeps the `cargo-fuzz` event-replay target on the real
/// replay implementation without requiring it to create profile directories or
/// SQLite databases for each generated input.
#[doc(hidden)]
pub fn replay_event_records_for_fuzzing(records: &[EventRecord]) -> Result<Value> {
    let filters = ParsedEventFilters::empty();
    let replay = replay_records("fuzz", &filters, Some(records.len()), records);
    serde_json::to_value(replay).map_err(|error| {
        MissiveError::orchestration("failed to serialize fuzz event replay summary")
            .with_source(error)
    })
}

fn replay_records(
    profile: &str,
    filters: &ParsedEventFilters,
    limit: Option<usize>,
    records: &[EventRecord],
) -> EventReplayOutput {
    let mut event_types = BTreeMap::<String, usize>::new();
    let mut contexts = BTreeMap::<String, ReplayContextAccumulator>::new();
    let mut tasks = BTreeMap::<String, ReplayTaskAccumulator>::new();

    for record in records {
        *event_types.entry(record.event_type.clone()).or_default() += 1;
        apply_context_replay(record, &mut contexts);
        apply_task_replay(record, &mut tasks, &mut contexts);
    }

    let contexts = contexts
        .into_values()
        .map(ReplayContextView::from)
        .collect::<Vec<_>>();
    let tasks = tasks
        .into_values()
        .map(ReplayTaskView::from)
        .collect::<Vec<_>>();
    EventReplayOutput {
        profile: profile.to_owned(),
        filters: EventFilterView::from_filters(filters, limit),
        event_count: records.len(),
        context_count: contexts.len(),
        task_count: tasks.len(),
        event_types,
        message: format!(
            "Replayed {} event(s) into {} context summary(s) and {} task summary(s)",
            records.len(),
            contexts.len(),
            tasks.len()
        ),
        contexts,
        tasks,
    }
}

fn apply_context_replay(
    record: &EventRecord,
    contexts: &mut BTreeMap<String, ReplayContextAccumulator>,
) {
    let Some(context_id) = record.context_id.as_ref().map(ToString::to_string) else {
        return;
    };
    let context = contexts
        .entry(context_id.clone())
        .or_insert_with(|| ReplayContextAccumulator::new(record, context_id));
    context.observe(record);
    if context.agent.is_none() {
        context.agent = record.agent_alias.as_ref().map(ToString::to_string);
    }
    if let Some(state) = string_at_paths(
        &record.payload_json,
        &[&["context", "state"], &["context_state"], &["state"]],
    ) {
        context.state = Some(state);
    }
    if let Some(name) = string_at_paths(&record.payload_json, &[&["context", "name"], &["name"]]) {
        context.name = Some(name);
    }
}

fn apply_task_replay(
    record: &EventRecord,
    tasks: &mut BTreeMap<String, ReplayTaskAccumulator>,
    contexts: &mut BTreeMap<String, ReplayContextAccumulator>,
) {
    let Some(task_id) = record.task_id.as_ref().map(ToString::to_string) else {
        return;
    };
    let task = tasks
        .entry(task_id.clone())
        .or_insert_with(|| ReplayTaskAccumulator::new(record, task_id.clone()));
    task.observe(record);
    if task.agent.is_none() {
        task.agent = record.agent_alias.as_ref().map(ToString::to_string);
    }
    if task.context_id.is_none() {
        task.context_id = record
            .context_id
            .as_ref()
            .map(ToString::to_string)
            .or_else(|| {
                string_at_paths(
                    &record.payload_json,
                    &[
                        &["context_id"],
                        &["contextId"],
                        &["task", "context_id"],
                        &["task", "contextId"],
                    ],
                )
            });
    }
    if let Some(state) = state_from_payload(&record.payload_json) {
        task.state = Some(normalize_state(&state));
    }

    if let Some(context_id) = &task.context_id {
        let context = contexts
            .entry(context_id.clone())
            .or_insert_with(|| ReplayContextAccumulator::new(record, context_id.clone()));
        context.observe(record);
        context.task_ids.insert(task_id);
        if context.agent.is_none() {
            context.agent = record.agent_alias.as_ref().map(ToString::to_string);
        }
    }
}

fn state_from_payload(payload: &Value) -> Option<String> {
    string_at_paths(
        payload,
        &[
            &["state"],
            &["task", "state"],
            &["task", "status", "state"],
            &["status", "state"],
            &["response", "state"],
            &["response", "task", "status", "state"],
        ],
    )
}

fn string_at_paths(payload: &Value, paths: &[&[&str]]) -> Option<String> {
    for path in paths {
        let mut current = payload;
        let mut found = true;
        for segment in *path {
            match current.get(*segment) {
                Some(value) => current = value,
                None => {
                    found = false;
                    break;
                }
            }
        }
        if found && let Some(value) = current.as_str() {
            return Some(value.to_owned());
        }
    }
    None
}

fn normalize_state(value: &str) -> String {
    let trimmed = value.trim();
    let without_prefix = trimmed
        .strip_prefix("TASK_STATE_")
        .or_else(|| trimmed.strip_prefix("task_state_"))
        .unwrap_or(trimmed);
    without_prefix.to_ascii_lowercase().replace('-', "_")
}

fn render_event_list<W>(writer: &mut W, mode: OutputMode, output: &EventListOutput) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => write_event_list_human(writer, &output.profile, &output.events),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, "events_list", output, &output.message)
        }
    }
}

fn render_event_replay<W>(
    writer: &mut W,
    mode: OutputMode,
    output: &EventReplayOutput,
) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => write_replay_human(writer, output),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, "events_replay", output, &output.message)
        }
    }
}

fn render_event_export<W>(
    writer: &mut W,
    mode: OutputMode,
    output: &EventExportOutput,
) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => write_event_list_human(writer, &output.profile, &output.events),
        OutputMode::Json => render_success(writer, mode, "events_export", output, &output.message),
        OutputMode::Ndjson => {
            for event in &output.events {
                render_event_item(writer, mode, event)?;
            }
            Ok(())
        }
        OutputMode::Quiet => Ok(()),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_event_tail_summary<W>(
    writer: &mut W,
    mode: OutputMode,
    profile: String,
    filters: &ParsedEventFilters,
    limit: Option<usize>,
    from_sequence: i64,
    emitted: usize,
    last_seen_sequence: i64,
    timed_out: bool,
    events: Vec<EventRecordView>,
) -> Result<()>
where
    W: Write,
{
    let message = if emitted == 0 {
        if timed_out {
            "No matching events arrived before the tail timeout".to_owned()
        } else {
            "No matching events were emitted".to_owned()
        }
    } else {
        format!("Tailed {emitted} event(s)")
    };
    let output = EventTailOutput {
        profile,
        filters: EventFilterView::from_filters(filters, limit),
        from_sequence,
        emitted,
        last_seen_sequence,
        timed_out,
        events,
        message,
    };

    match mode {
        OutputMode::Human if emitted == 0 => writeln!(writer, "{}", redact_text(&output.message))
            .map_err(|error| MissiveError::io("writing event tail output", error)),
        OutputMode::Json => render_success(writer, mode, "events_tail", &output, &output.message),
        OutputMode::Human | OutputMode::Ndjson | OutputMode::Quiet => Ok(()),
    }
}

fn render_event_item<W>(writer: &mut W, mode: OutputMode, event: &EventRecordView) -> Result<()>
where
    W: Write,
{
    render_stream_item(
        writer,
        mode,
        "event_record",
        event_sequence(event.sequence)?,
        event,
        &format_event_human_line(event),
    )
}

fn write_event_list_human<W>(
    writer: &mut W,
    profile: &str,
    events: &[EventRecordView],
) -> Result<()>
where
    W: Write,
{
    writeln!(
        writer,
        "Events for profile '{}' ({}):",
        redact_text(profile),
        events.len()
    )
    .map_err(|error| MissiveError::io("writing event list output", error))?;
    for event in events {
        writeln!(writer, "  {}", format_event_human_line(event))
            .map_err(|error| MissiveError::io("writing event list output", error))?;
    }
    Ok(())
}

fn write_replay_human<W>(writer: &mut W, output: &EventReplayOutput) -> Result<()>
where
    W: Write,
{
    writeln!(
        writer,
        "Replayed {} event(s) for profile '{}': {} context(s), {} task(s)",
        output.event_count,
        redact_text(&output.profile),
        output.context_count,
        output.task_count
    )
    .map_err(|error| MissiveError::io("writing event replay output", error))?;
    for context in &output.contexts {
        writeln!(
            writer,
            "  context {} events={} tasks={} last={}",
            redact_text(&context.context_id),
            context.event_count,
            context.task_ids.len(),
            redact_text(&context.last_event_type)
        )
        .map_err(|error| MissiveError::io("writing event replay output", error))?;
    }
    for task in &output.tasks {
        writeln!(
            writer,
            "  task {} state={} events={} last={}",
            redact_text(&task.task_id),
            task.state
                .as_deref()
                .map(redact_text)
                .unwrap_or_else(|| "-".to_owned()),
            task.event_count,
            redact_text(&task.last_event_type)
        )
        .map_err(|error| MissiveError::io("writing event replay output", error))?;
    }
    Ok(())
}

fn format_event_human_line(event: &EventRecordView) -> String {
    let mut parts = vec![
        format!("#{}", event.sequence),
        event.timestamp.clone(),
        event.source.clone(),
        event.event_type.clone(),
        format!("id={}", event.event_id),
    ];
    if let Some(agent) = &event.agent {
        parts.push(format!("agent={agent}"));
    }
    if let Some(context_id) = &event.context_id {
        parts.push(format!("context={context_id}"));
    }
    if let Some(task_id) = &event.task_id {
        parts.push(format!("task={task_id}"));
    }
    if let Some(group_name) = &event.group_name {
        parts.push(format!("group={group_name}"));
    }
    if let Some(gateway_job_id) = &event.gateway_job_id {
        parts.push(format!("job={gateway_job_id}"));
    }
    redact_text(&parts.join("  "))
}

fn event_sequence(sequence: i64) -> Result<u64> {
    u64::try_from(sequence).map_err(|error| {
        MissiveError::storage(format!(
            "event sequence {sequence} cannot be rendered as NDJSON"
        ))
        .with_source(error)
    })
}

fn parse_duration_arg(flag: &str, value: &str) -> Result<Duration> {
    let value = value.trim();
    let (number, multiplier) = if let Some(number) = value.strip_suffix("ms") {
        (number, 1_u64)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1_000_u64)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60_000_u64)
    } else if let Some(number) = value.strip_suffix('h') {
        (number, 3_600_000_u64)
    } else {
        return Err(MissiveError::validation(format!(
            "{flag} value {value:?} must use a duration suffix: ms, s, m, or h"
        ))
        .with_help("Use values such as 100ms, 2s, 5m, or 1h."));
    };
    let number = number.parse::<u64>().map_err(|error| {
        MissiveError::validation(format!("{flag} value {value:?} has an invalid number"))
            .with_source(error)
            .with_help("Use a positive whole number followed by ms, s, m, or h.")
    })?;
    if number == 0 {
        return Err(MissiveError::validation(format!(
            "{flag} value {value:?} must be greater than zero"
        )));
    }
    let millis = number
        .checked_mul(multiplier)
        .ok_or_else(|| MissiveError::validation(format!("{flag} value {value:?} is too large")))?;
    Ok(Duration::from_millis(millis))
}

fn validate_non_negative_sequence(flag: &str, sequence: i64) -> Result<i64> {
    if sequence < 0 {
        return Err(MissiveError::validation(format!(
            "{flag} must be greater than or equal to zero"
        )));
    }
    Ok(sequence)
}

impl ParsedEventFilters {
    fn empty() -> Self {
        Self {
            agent: None,
            context_id: None,
            task_id: None,
            source: None,
            event_type: None,
            since: None,
            after_sequence: None,
        }
    }

    fn from_parts(selector: &EventSelectorArgs, after_sequence: Option<i64>) -> Result<Self> {
        Ok(Self {
            agent: selector
                .agent
                .as_ref()
                .map(|value| AgentAlias::new(value.clone()))
                .transpose()?,
            context_id: selector
                .context
                .as_ref()
                .map(|value| ContextId::new(value.clone()))
                .transpose()?,
            task_id: selector
                .task
                .as_ref()
                .map(|value| TaskId::new(value.clone()))
                .transpose()?,
            source: selector.source.clone(),
            event_type: selector.event_type.clone(),
            since: selector
                .since
                .as_ref()
                .map(|value| value.parse::<MissiveTimestamp>())
                .transpose()?,
            after_sequence: after_sequence
                .map(|sequence| validate_non_negative_sequence("--after-sequence", sequence))
                .transpose()?,
        })
    }

    fn matches(&self, event: &EventRecord) -> bool {
        if self
            .agent
            .as_ref()
            .is_some_and(|agent| event.agent_alias.as_ref() != Some(agent))
        {
            return false;
        }
        if self
            .context_id
            .as_ref()
            .is_some_and(|context_id| event.context_id.as_ref() != Some(context_id))
        {
            return false;
        }
        if self
            .task_id
            .as_ref()
            .is_some_and(|task_id| event.task_id.as_ref() != Some(task_id))
        {
            return false;
        }
        if self
            .source
            .as_ref()
            .is_some_and(|source| &event.source != source)
        {
            return false;
        }
        if self
            .event_type
            .as_ref()
            .is_some_and(|event_type| &event.event_type != event_type)
        {
            return false;
        }
        if self.since.is_some_and(|since| event.timestamp < since) {
            return false;
        }
        if self
            .after_sequence
            .is_some_and(|sequence| event.sequence <= sequence)
        {
            return false;
        }
        true
    }
}

impl EventFilterView {
    fn from_filters(filters: &ParsedEventFilters, limit: Option<usize>) -> Self {
        Self {
            agent: filters.agent.as_ref().map(ToString::to_string),
            context_id: filters.context_id.as_ref().map(ToString::to_string),
            task_id: filters.task_id.as_ref().map(ToString::to_string),
            source: filters.source.clone(),
            event_type: filters.event_type.clone(),
            since: filters.since.map(MissiveTimestamp::to_rfc3339),
            after_sequence: filters.after_sequence,
            limit,
        }
    }
}

impl EventRecordView {
    fn from_record(record: &EventRecord) -> Self {
        Self {
            sequence: record.sequence,
            event_id: record.event_id.as_str().to_owned(),
            timestamp: record.timestamp.to_rfc3339(),
            source: record.source.clone(),
            event_type: record.event_type.clone(),
            agent: record.agent_alias.as_ref().map(ToString::to_string),
            context_id: record.context_id.as_ref().map(ToString::to_string),
            task_id: record.task_id.as_ref().map(ToString::to_string),
            group_name: record.group_name.as_ref().map(ToString::to_string),
            gateway_job_id: record.gateway_job_id.as_ref().map(ToString::to_string),
            adapter_binding_id: record.adapter_binding_id.as_ref().map(ToString::to_string),
            payload: redact_json(&record.payload_json),
            metadata: record.metadata.clone(),
            redacted: record.redacted,
        }
    }
}

impl ReplayContextAccumulator {
    fn new(record: &EventRecord, context_id: String) -> Self {
        Self {
            context_id,
            agent: record.agent_alias.as_ref().map(ToString::to_string),
            state: None,
            name: None,
            task_ids: BTreeSet::new(),
            event_count: 0,
            first_sequence: record.sequence,
            last_sequence: record.sequence,
            first_timestamp: record.timestamp,
            last_timestamp: record.timestamp,
            last_event_type: record.event_type.clone(),
        }
    }

    fn observe(&mut self, record: &EventRecord) {
        self.event_count += 1;
        self.last_sequence = record.sequence;
        self.last_timestamp = record.timestamp;
        self.last_event_type = record.event_type.clone();
    }
}

impl ReplayTaskAccumulator {
    fn new(record: &EventRecord, task_id: String) -> Self {
        Self {
            task_id,
            agent: record.agent_alias.as_ref().map(ToString::to_string),
            context_id: record.context_id.as_ref().map(ToString::to_string),
            state: None,
            event_count: 0,
            first_sequence: record.sequence,
            last_sequence: record.sequence,
            first_timestamp: record.timestamp,
            last_timestamp: record.timestamp,
            last_event_type: record.event_type.clone(),
        }
    }

    fn observe(&mut self, record: &EventRecord) {
        self.event_count += 1;
        self.last_sequence = record.sequence;
        self.last_timestamp = record.timestamp;
        self.last_event_type = record.event_type.clone();
    }
}

impl From<ReplayContextAccumulator> for ReplayContextView {
    fn from(context: ReplayContextAccumulator) -> Self {
        Self {
            context_id: context.context_id,
            agent: context.agent,
            state: context.state,
            name: context.name,
            task_ids: context.task_ids.into_iter().collect(),
            event_count: context.event_count,
            first_sequence: context.first_sequence,
            last_sequence: context.last_sequence,
            first_timestamp: context.first_timestamp.to_rfc3339(),
            last_timestamp: context.last_timestamp.to_rfc3339(),
            last_event_type: context.last_event_type,
        }
    }
}

impl From<ReplayTaskAccumulator> for ReplayTaskView {
    fn from(task: ReplayTaskAccumulator) -> Self {
        Self {
            task_id: task.task_id,
            agent: task.agent,
            context_id: task.context_id,
            state: task.state,
            event_count: task.event_count,
            first_sequence: task.first_sequence,
            last_sequence: task.last_sequence,
            first_timestamp: task.first_timestamp.to_rfc3339(),
            last_timestamp: task.last_timestamp.to_rfc3339(),
            last_event_type: task.last_event_type,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn normalize_a2a_task_states() {
        assert_eq!(normalize_state("TASK_STATE_COMPLETED"), "completed");
        assert_eq!(normalize_state("input-required"), "input_required");
    }

    #[test]
    fn state_search_handles_a2a_payload_shapes() {
        assert_eq!(
            state_from_payload(&json!({"task": {"status": {"state": "TASK_STATE_WORKING"}}})),
            Some("TASK_STATE_WORKING".to_owned())
        );
        assert_eq!(
            state_from_payload(&json!({"status": {"state": "completed"}})),
            Some("completed".to_owned())
        );
    }
}
