//! A2A task get/list/wait/cancel command implementation.
//!
//! The task command reads and filters local SQLite task state, can refresh that
//! state from a remote A2A agent through `GetTask`/`ListTasks`, and can request
//! remote task cancellation through `CancelTask`.

use std::cmp;
use std::collections::BTreeMap;
use std::io::Write;
use std::thread;
use std::time::{Duration, Instant};

use clap::{ArgAction, Args, Subcommand};
use missive_a2a::{
    NegotiatedInterface, ServiceParameters, TaskClient,
    protocol::{CancelTaskRequest, GetTaskRequest, ListTasksRequest, Message, Task},
};
use missive_core::{
    AgentAlias, ContextId, LoadedConfig, MessageId, Metadata, MissiveError, MissiveExitCode,
    MissiveTimestamp, Result, TaskId,
};
use missive_store::{
    AgentRecord, ArtifactRecord, ContextUpsert, Store, StoreTransaction, TaskRecord, TaskSource,
    TaskState, TaskUpsert,
};
use serde::Serialize;
use serde_json::Value;

use crate::agent::{AgentRegistry, get_existing_agent, open_agent_registry};
use crate::artifact::{
    ArtifactSummaryView, TaskArtifactCommands, execute_task_artifact_command,
    first_artifact_text_from_records, persist_task_artifacts,
};
use crate::auth::auth_headers_for_agent;
use crate::output::{OutputMode, redact_text, render_success};
use crate::send::{resolve_send_interface, store_task_state};
use crate::{GlobalArgs, service_parameters_from_config_and_globals};

const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_WAIT_INTERVAL: Duration = Duration::from_secs(1);
const MAX_POLL_SLEEP: Duration = Duration::from_secs(5);

/// Task subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum TaskCommands {
    /// Show one local task, optionally refreshing it from its remote agent.
    Get(TaskGetArgs),
    /// List local tasks or one remote agent's tasks with filters.
    List(TaskListArgs),
    /// Poll a task until it completes, fails, is cancelled, needs input, or times out.
    Wait(TaskWaitArgs),
    /// Request remote cancellation for one task.
    Cancel(TaskCancelArgs),
    /// List, show, save, or export artifacts persisted for a task.
    Artifact {
        /// Artifact operation to run.
        #[command(subcommand)]
        command: TaskArtifactCommands,
    },
}

impl TaskCommands {
    /// Stable subcommand spelling used in structured output messages.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Get(_) => "get",
            Self::List(_) => "list",
            Self::Wait(_) => "wait",
            Self::Cancel(_) => "cancel",
            Self::Artifact { .. } => "artifact",
        }
    }
}

/// Arguments for `missive task get`.
#[derive(Debug, Clone, Args)]
pub struct TaskGetArgs {
    /// A2A task id.
    pub task_id: String,

    /// Agent alias to use when refreshing the task from a remote agent.
    #[arg(long = "agent", value_name = "ALIAS")]
    pub agent: Option<String>,

    /// Fetch the task from the remote A2A agent before rendering.
    #[arg(long = "remote", action = ArgAction::SetTrue)]
    pub remote: bool,

    /// Require the local task row to have this source.
    #[arg(long = "source", value_name = "SOURCE", value_parser = parse_task_source_arg)]
    pub source: Option<TaskSource>,

    /// Request this many history messages from the remote agent.
    #[arg(long = "history-length", value_name = "N")]
    pub history_length: Option<i32>,
}

/// Arguments for `missive task list`.
#[derive(Debug, Clone, Args)]
pub struct TaskListArgs {
    /// Filter by agent alias. Required with --remote.
    #[arg(long = "agent", value_name = "ALIAS")]
    pub agent: Option<String>,

    /// Filter by A2A context id.
    #[arg(long = "context", value_name = "CONTEXT_ID")]
    pub context: Option<String>,

    /// Filter by mapped local task state.
    #[arg(long = "state", value_name = "STATE", value_parser = parse_task_state_arg)]
    pub state: Option<TaskState>,

    /// Filter to tasks updated after this RFC3339 timestamp.
    #[arg(long = "updated-after", value_name = "RFC3339")]
    pub updated_after: Option<String>,

    /// Filter by local task source: remote, local, or gateway.
    #[arg(long = "source", value_name = "SOURCE", value_parser = parse_task_source_arg)]
    pub source: Option<TaskSource>,

    /// Query the selected remote agent with A2A ListTasks before rendering.
    #[arg(long = "remote", action = ArgAction::SetTrue)]
    pub remote: bool,

    /// Remote ListTasks page size.
    #[arg(long = "page-size", value_name = "N")]
    pub page_size: Option<i32>,

    /// Remote ListTasks page token.
    #[arg(long = "page-token", value_name = "TOKEN")]
    pub page_token: Option<String>,

    /// Request this many history messages from the remote agent.
    #[arg(long = "history-length", value_name = "N")]
    pub history_length: Option<i32>,

    /// Ask the remote agent to include artifacts in listed task payloads.
    #[arg(long = "include-artifacts", action = ArgAction::SetTrue)]
    pub include_artifacts: bool,
}

/// Arguments for `missive task wait`.
#[derive(Debug, Clone, Args)]
pub struct TaskWaitArgs {
    /// A2A task id.
    pub task_id: String,

    /// Agent alias to poll. If omitted, missive uses the local task row's agent.
    #[arg(long = "agent", value_name = "ALIAS")]
    pub agent: Option<String>,

    /// Poll only the local SQLite row instead of calling remote A2A GetTask.
    #[arg(long = "local", action = ArgAction::SetTrue)]
    pub local: bool,

    /// Poll interval such as 500ms, 2s, or 1m.
    #[arg(long = "interval", value_name = "DURATION")]
    pub interval: Option<String>,

    /// Request this many history messages from the remote agent.
    #[arg(long = "history-length", value_name = "N")]
    pub history_length: Option<i32>,
}

/// Arguments for `missive task cancel`.
#[derive(Debug, Clone, Args)]
pub struct TaskCancelArgs {
    /// A2A task id.
    pub task_id: String,

    /// Agent alias to cancel against. If omitted, missive uses the local task row's agent.
    #[arg(long = "agent", value_name = "ALIAS")]
    pub agent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TaskInterfaceView {
    binding: String,
    protocol_binding: String,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tenant: Option<String>,
    protocol_version: String,
    source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct TaskView {
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
    #[serde(skip_serializing_if = "Option::is_none")]
    status_message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status_timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    artifact_count: usize,
    artifacts: Vec<ArtifactSummaryView>,
    history_count: usize,
    metadata: Metadata,
    created_at: String,
    updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_task: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TaskFiltersView {
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_after: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct TaskGetOutput {
    profile: String,
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    selected_interface: Option<TaskInterfaceView>,
    task: TaskView,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct TaskListOutput {
    profile: String,
    source: String,
    filters: TaskFiltersView,
    count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    selected_interface: Option<TaskInterfaceView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_page_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    page_size: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_size: Option<i32>,
    tasks: Vec<TaskView>,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct TaskCancelOutput {
    profile: String,
    selected_interface: TaskInterfaceView,
    task: TaskView,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct TaskWaitOutput {
    profile: String,
    task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    status: String,
    attempts: u64,
    elapsed_ms: u128,
    timeout_ms: u128,
    interval_ms: u128,
    timed_out: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    selected_interface: Option<TaskInterfaceView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task: Option<TaskView>,
    message: String,
}

#[derive(Debug, Clone)]
struct ParsedTaskFilters {
    agent: Option<AgentAlias>,
    context_id: Option<ContextId>,
    state: Option<TaskState>,
    updated_after: Option<MissiveTimestamp>,
    source: Option<TaskSource>,
}

#[derive(Debug, Clone)]
struct RemoteTaskResult {
    agent: AgentRecord,
    selected_interface: NegotiatedInterface,
    record: TaskRecord,
}

#[derive(Debug, Clone)]
struct RemoteTaskListResult {
    selected_interface: NegotiatedInterface,
    records: Vec<TaskRecord>,
    next_page_token: String,
    page_size: i32,
    total_size: i32,
}

/// Executes one task subcommand.
pub(crate) fn execute_task_command<W>(
    command: &TaskCommands,
    globals: &GlobalArgs,
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let service_parameters = service_parameters_from_config_and_globals(loaded_config, globals)?;
    let mut registry = open_agent_registry(loaded_config, environment)?;

    match command {
        TaskCommands::Get(args) => get_task(
            args,
            &mut registry,
            globals,
            environment,
            &service_parameters,
            mode,
            writer,
        ),
        TaskCommands::List(args) => list_tasks(
            args,
            &mut registry,
            globals,
            environment,
            &service_parameters,
            mode,
            writer,
        ),
        TaskCommands::Wait(args) => wait_task(
            args,
            globals,
            &mut registry,
            environment,
            &service_parameters,
            mode,
            writer,
        ),
        TaskCommands::Cancel(args) => cancel_task(
            args,
            &mut registry,
            globals,
            environment,
            &service_parameters,
            mode,
            writer,
        ),
        TaskCommands::Artifact { command } => {
            execute_task_artifact_command(command, &mut registry, mode, writer)
        }
    }
}

fn get_task<W>(
    args: &TaskGetArgs,
    registry: &mut AgentRegistry,
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
    service_parameters: &ServiceParameters,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let task_id = TaskId::new(args.task_id.clone())?;
    if args.remote
        && args
            .source
            .is_some_and(|source| source != TaskSource::Remote)
    {
        return Err(MissiveError::validation(
            "missive task get --remote can only be combined with --source remote",
        ));
    }

    let output = if args.remote {
        let result = refresh_remote_task(
            registry,
            args.agent.as_deref(),
            &task_id,
            args.history_length,
            globals,
            environment,
            service_parameters,
        )?;
        let view = task_view_from_store(&registry.store, &result.record)?;
        TaskGetOutput {
            profile: registry.profile.clone(),
            source: "remote".to_owned(),
            selected_interface: Some(TaskInterfaceView::from(&result.selected_interface)),
            message: format!(
                "Fetched task '{}' from remote agent '{}'",
                task_id.as_str(),
                result.agent.alias.as_str()
            ),
            task: view,
        }
    } else {
        let record = get_existing_task(&registry.store, &task_id)?;
        ensure_optional_agent_filter(&record, args.agent.as_deref())?;
        if let Some(source) = args.source
            && record.source != source
        {
            return Err(MissiveError::validation(format!(
                "task {:?} has source {:?}, not {:?}",
                task_id.as_str(),
                record.source.as_str(),
                source.as_str()
            )));
        }
        let view = task_view_from_store(&registry.store, &record)?;
        TaskGetOutput {
            profile: registry.profile.clone(),
            source: "local".to_owned(),
            selected_interface: None,
            message: format!("Showing local task '{}'", task_id.as_str()),
            task: view,
        }
    };

    render_task_get(writer, mode, &output)
}

fn list_tasks<W>(
    args: &TaskListArgs,
    registry: &mut AgentRegistry,
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
    service_parameters: &ServiceParameters,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let filters = ParsedTaskFilters::from_list_args(args)?;
    if args.remote
        && filters
            .source
            .is_some_and(|source| source != TaskSource::Remote)
    {
        return Err(MissiveError::validation(
            "missive task list --remote can only be combined with --source remote",
        ));
    }

    let output = if args.remote {
        let result = list_remote_tasks(
            registry,
            args,
            &filters,
            globals,
            environment,
            service_parameters,
        )?;
        let tasks = result
            .records
            .iter()
            .filter(|record| filters.matches(record))
            .map(|record| task_view_from_store(&registry.store, record))
            .collect::<Result<Vec<_>>>()?;
        TaskListOutput {
            profile: registry.profile.clone(),
            source: "remote".to_owned(),
            filters: TaskFiltersView::from_filters(&filters),
            count: tasks.len(),
            selected_interface: Some(TaskInterfaceView::from(&result.selected_interface)),
            next_page_token: (!result.next_page_token.is_empty()).then_some(result.next_page_token),
            page_size: Some(result.page_size),
            total_size: Some(result.total_size),
            message: format!("Listed {} remote task(s)", tasks.len()),
            tasks,
        }
    } else {
        let tasks = registry
            .store
            .list_tasks()?
            .into_iter()
            .filter(|record| filters.matches(record))
            .map(|record| task_view_from_store(&registry.store, &record))
            .collect::<Result<Vec<_>>>()?;
        TaskListOutput {
            profile: registry.profile.clone(),
            source: "local".to_owned(),
            filters: TaskFiltersView::from_filters(&filters),
            count: tasks.len(),
            selected_interface: None,
            next_page_token: None,
            page_size: None,
            total_size: None,
            message: format!("Listed {} local task(s)", tasks.len()),
            tasks,
        }
    };

    render_task_list(writer, mode, &output)
}

fn wait_task<W>(
    args: &TaskWaitArgs,
    globals: &GlobalArgs,
    registry: &mut AgentRegistry,
    environment: &BTreeMap<String, String>,
    service_parameters: &ServiceParameters,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let task_id = TaskId::new(args.task_id.clone())?;
    let timeout = match globals.timeout.as_deref() {
        Some(value) => parse_duration_arg("--timeout", value)?,
        None => DEFAULT_WAIT_TIMEOUT,
    };
    let interval = match args.interval.as_deref() {
        Some(value) => parse_duration_arg("--interval", value)?,
        None => DEFAULT_WAIT_INTERVAL,
    };
    let interval = cmp::min(interval, MAX_POLL_SLEEP);
    let started = Instant::now();
    let mut attempts = 0_u64;

    loop {
        attempts += 1;
        let (record, selected_interface) = if args.local {
            (get_existing_task(&registry.store, &task_id)?, None)
        } else {
            let result = refresh_remote_task(
                registry,
                args.agent.as_deref(),
                &task_id,
                args.history_length,
                globals,
                environment,
                service_parameters,
            )?;
            (
                result.record,
                Some(TaskInterfaceView::from(&result.selected_interface)),
            )
        };
        let state = record.state;
        let agent = Some(record.agent_alias.as_str().to_owned());
        let task = Some(task_view_from_store(&registry.store, &record)?);

        if wait_state_is_decisive(state) {
            let status = state.as_str().to_owned();
            let output = TaskWaitOutput::from_parts(TaskWaitParts {
                profile: registry.profile.clone(),
                task_id: task_id.as_str().to_owned(),
                agent,
                status,
                attempts,
                started,
                timeout,
                interval,
                timed_out: false,
                selected_interface,
                task,
            });
            render_task_wait(writer, mode, &output)?;
            return wait_exit_for_state(&task_id, state);
        }

        if started.elapsed() >= timeout {
            let output = TaskWaitOutput::from_parts(TaskWaitParts {
                profile: registry.profile.clone(),
                task_id: task_id.as_str().to_owned(),
                agent,
                status: "timeout".to_owned(),
                attempts,
                started,
                timeout,
                interval,
                timed_out: true,
                selected_interface,
                task,
            });
            render_task_wait(writer, mode, &output)?;
            return Err(MissiveError::orchestration(format!(
                "task {:?} did not reach a decisive state before timeout",
                task_id.as_str()
            ))
            .with_help("Increase --timeout, lower --interval, or inspect the remote task state.")
            .with_exit_code(MissiveExitCode::TaskTimeout));
        }

        let remaining = timeout
            .checked_sub(started.elapsed())
            .unwrap_or_else(|| Duration::from_millis(1));
        thread::sleep(cmp::min(interval, remaining));
    }
}

fn cancel_task<W>(
    args: &TaskCancelArgs,
    registry: &mut AgentRegistry,
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
    service_parameters: &ServiceParameters,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let task_id = TaskId::new(args.task_id.clone())?;
    let agent = resolve_agent_for_task(&registry.store, args.agent.as_deref(), &task_id)?;
    let auth_headers = auth_headers_for_agent(&registry.store, &agent, globals, environment)?;
    let (agent, selected_interface) =
        resolve_send_interface(registry, agent, service_parameters, &auth_headers)?;
    let request = CancelTaskRequest {
        id: task_id.as_str().to_owned(),
        metadata: None,
        tenant: selected_interface.tenant.clone(),
    };
    let outcome = TaskClient::new()?.cancel_task(
        &selected_interface,
        &request,
        service_parameters,
        &auth_headers,
    )?;
    let record = persist_remote_task(
        &mut registry.store,
        &agent,
        &outcome.task,
        service_parameters,
    )?;
    let view = task_view_from_store(&registry.store, &record)?;
    let output = TaskCancelOutput {
        profile: registry.profile.clone(),
        selected_interface: TaskInterfaceView::from(&selected_interface),
        message: format!(
            "Requested cancellation for task '{}' on agent '{}'",
            task_id.as_str(),
            agent.alias.as_str()
        ),
        task: view,
    };

    render_task_cancel(writer, mode, &output)
}

fn refresh_remote_task(
    registry: &mut AgentRegistry,
    agent_arg: Option<&str>,
    task_id: &TaskId,
    history_length: Option<i32>,
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
    service_parameters: &ServiceParameters,
) -> Result<RemoteTaskResult> {
    validate_non_negative_i32("--history-length", history_length)?;
    let agent = resolve_agent_for_task(&registry.store, agent_arg, task_id)?;
    let auth_headers = auth_headers_for_agent(&registry.store, &agent, globals, environment)?;
    let (agent, selected_interface) =
        resolve_send_interface(registry, agent, service_parameters, &auth_headers)?;
    let request = GetTaskRequest {
        id: task_id.as_str().to_owned(),
        history_length,
        tenant: selected_interface.tenant.clone(),
    };
    let outcome = TaskClient::new()?.get_task(
        &selected_interface,
        &request,
        service_parameters,
        &auth_headers,
    )?;
    let record = persist_remote_task(
        &mut registry.store,
        &agent,
        &outcome.task,
        service_parameters,
    )?;

    Ok(RemoteTaskResult {
        agent,
        selected_interface,
        record,
    })
}

fn list_remote_tasks(
    registry: &mut AgentRegistry,
    args: &TaskListArgs,
    filters: &ParsedTaskFilters,
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
    service_parameters: &ServiceParameters,
) -> Result<RemoteTaskListResult> {
    validate_positive_i32("--page-size", args.page_size)?;
    validate_non_negative_i32("--history-length", args.history_length)?;
    let agent_alias = filters.agent.as_ref().ok_or_else(|| {
        MissiveError::validation("missive task list --remote requires --agent <alias>")
            .with_help("Remote A2A ListTasks is scoped to one registered agent.")
    })?;
    let agent = get_existing_agent(&registry.store, agent_alias)?;
    let auth_headers = auth_headers_for_agent(&registry.store, &agent, globals, environment)?;
    let (agent, selected_interface) =
        resolve_send_interface(registry, agent, service_parameters, &auth_headers)?;
    let request = ListTasksRequest {
        context_id: filters
            .context_id
            .as_ref()
            .map(|context_id| context_id.as_str().to_owned()),
        status: filters.state.map(protocol_task_state),
        page_size: args.page_size,
        page_token: args.page_token.clone(),
        history_length: args.history_length,
        status_timestamp_after: filters.updated_after.map(MissiveTimestamp::as_datetime),
        include_artifacts: args.include_artifacts.then_some(true),
        tenant: selected_interface.tenant.clone(),
    };
    let outcome = TaskClient::new()?.list_tasks(
        &selected_interface,
        &request,
        service_parameters,
        &auth_headers,
    )?;
    let mut records = Vec::with_capacity(outcome.response.tasks.len());
    for task in &outcome.response.tasks {
        records.push(persist_remote_task(
            &mut registry.store,
            &agent,
            task,
            service_parameters,
        )?);
    }

    Ok(RemoteTaskListResult {
        selected_interface,
        records,
        next_page_token: outcome.response.next_page_token,
        page_size: outcome.response.page_size,
        total_size: outcome.response.total_size,
    })
}

fn resolve_agent_for_task(
    store: &Store,
    agent_arg: Option<&str>,
    task_id: &TaskId,
) -> Result<AgentRecord> {
    if let Some(agent) = agent_arg {
        let alias = AgentAlias::new(agent.to_owned())?;
        return get_existing_agent(store, &alias);
    }

    let task = store.get_task(task_id)?.ok_or_else(|| {
        MissiveError::validation(format!(
            "task {:?} is not in the local store and no --agent was supplied",
            task_id.as_str()
        ))
        .with_help("Pass --agent <alias> or run 'missive task list' to see locally known tasks.")
    })?;
    get_existing_agent(store, &task.agent_alias)
}

fn get_existing_task(store: &Store, task_id: &TaskId) -> Result<TaskRecord> {
    store.get_task(task_id)?.ok_or_else(|| {
        MissiveError::validation(format!("task {:?} is not known locally", task_id.as_str()))
            .with_help("Use 'missive send' or 'missive stream' to create task rows, or pass --remote --agent to fetch from an A2A agent.")
    })
}

fn task_view_from_store(store: &Store, record: &TaskRecord) -> Result<TaskView> {
    let artifacts = store.list_artifacts_for_task(&record.task_id)?;
    Ok(TaskView::from_record_and_artifacts(record, &artifacts))
}

fn ensure_optional_agent_filter(record: &TaskRecord, agent: Option<&str>) -> Result<()> {
    let Some(agent) = agent else {
        return Ok(());
    };
    let alias = AgentAlias::new(agent.to_owned())?;
    if record.agent_alias != alias {
        return Err(MissiveError::validation(format!(
            "task {:?} belongs to agent {:?}, not {:?}",
            record.task_id.as_str(),
            record.agent_alias.as_str(),
            alias.as_str()
        )));
    }
    Ok(())
}

fn persist_remote_task(
    store: &mut Store,
    agent: &AgentRecord,
    task: &Task,
    service_parameters: &ServiceParameters,
) -> Result<TaskRecord> {
    store
        .transaction(|transaction| upsert_remote_task(transaction, agent, task, service_parameters))
}

fn upsert_remote_task(
    transaction: &StoreTransaction<'_>,
    agent: &AgentRecord,
    task: &Task,
    service_parameters: &ServiceParameters,
) -> Result<TaskRecord> {
    let task_id = TaskId::new(task.id.clone())?;
    let context_id = ContextId::new(task.context_id.clone())?;
    if transaction.get_context(&context_id)?.is_none() {
        let mut context = ContextUpsert::new(context_id.clone());
        context.agent_alias = Some(agent.alias.clone());
        transaction.upsert_context(&context)?;
    }

    let mut upsert = TaskUpsert::new(
        task_id,
        agent.alias.clone(),
        store_task_state(&task.status.state),
    );
    upsert.source = TaskSource::Remote;
    upsert.context_id = Some(context_id);
    upsert.remote_task_json = Some(serde_json::to_value(task).map_err(|error| {
        MissiveError::protocol("encoding A2A task for local persistence").with_source(error)
    })?);
    upsert.last_message_id = task
        .status
        .message
        .as_ref()
        .map(|message| MessageId::new(message.message_id.clone()))
        .transpose()?;
    upsert.record_a2a_protocol_version(service_parameters.protocol_version.clone())?;
    if matches!(
        upsert.state,
        TaskState::Completed | TaskState::Failed | TaskState::Cancelled
    ) {
        upsert.completed_at = Some(MissiveTimestamp::now_utc());
    }
    let record = transaction.upsert_task(&upsert)?;
    persist_task_artifacts(transaction, task)?;
    Ok(record)
}

fn protocol_task_state(state: TaskState) -> missive_a2a::protocol::TaskState {
    match state {
        TaskState::Submitted => missive_a2a::protocol::TaskState::Submitted,
        TaskState::Working => missive_a2a::protocol::TaskState::Working,
        TaskState::InputRequired => missive_a2a::protocol::TaskState::InputRequired,
        TaskState::Completed => missive_a2a::protocol::TaskState::Completed,
        TaskState::Failed => missive_a2a::protocol::TaskState::Failed,
        TaskState::Cancelled => missive_a2a::protocol::TaskState::Canceled,
        TaskState::Unknown => missive_a2a::protocol::TaskState::Unspecified,
    }
}

impl ParsedTaskFilters {
    fn from_list_args(args: &TaskListArgs) -> Result<Self> {
        Ok(Self {
            agent: args
                .agent
                .as_ref()
                .map(|value| AgentAlias::new(value.clone()))
                .transpose()?,
            context_id: args
                .context
                .as_ref()
                .map(|value| ContextId::new(value.clone()))
                .transpose()?,
            state: args.state,
            updated_after: args
                .updated_after
                .as_ref()
                .map(|value| value.parse::<MissiveTimestamp>())
                .transpose()?,
            source: args.source,
        })
    }

    fn matches(&self, record: &TaskRecord) -> bool {
        if self
            .agent
            .as_ref()
            .is_some_and(|agent| &record.agent_alias != agent)
        {
            return false;
        }
        if self
            .context_id
            .as_ref()
            .is_some_and(|context_id| record.context_id.as_ref() != Some(context_id))
        {
            return false;
        }
        if self.state.is_some_and(|state| record.state != state) {
            return false;
        }
        if self
            .updated_after
            .is_some_and(|timestamp| record.updated_at <= timestamp)
        {
            return false;
        }
        if self.source.is_some_and(|source| record.source != source) {
            return false;
        }
        true
    }
}

impl TaskFiltersView {
    fn from_filters(filters: &ParsedTaskFilters) -> Self {
        Self {
            agent: filters
                .agent
                .as_ref()
                .map(|agent| agent.as_str().to_owned()),
            context_id: filters
                .context_id
                .as_ref()
                .map(|context_id| context_id.as_str().to_owned()),
            state: filters.state.map(|state| state.as_str().to_owned()),
            updated_after: filters.updated_after.map(MissiveTimestamp::to_rfc3339),
            source: filters.source.map(|source| source.as_str().to_owned()),
        }
    }
}

impl TaskView {
    fn from_record_and_artifacts(record: &TaskRecord, artifacts: &[ArtifactRecord]) -> Self {
        let parsed = record
            .remote_task_json
            .as_ref()
            .and_then(|value| serde_json::from_value::<Task>(value.clone()).ok());
        let status_message = parsed
            .as_ref()
            .and_then(|task| task.status.message.as_ref());
        let artifact_views = artifacts
            .iter()
            .map(ArtifactSummaryView::from_record)
            .collect::<Vec<_>>();
        let parsed_artifact_count = parsed
            .as_ref()
            .and_then(|task| task.artifacts.as_ref())
            .map_or(0, Vec::len);
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
            status_message_id: status_message.map(|message| message.message_id.clone()),
            status_timestamp: parsed
                .as_ref()
                .and_then(|task| task.status.timestamp.as_ref())
                .map(|timestamp| timestamp.to_rfc3339()),
            text: status_message
                .and_then(Message::text)
                .map(ToOwned::to_owned)
                .or_else(|| first_artifact_text_from_records(artifacts))
                .or_else(|| parsed.as_ref().and_then(first_artifact_text)),
            artifact_count: if artifact_views.is_empty() {
                parsed_artifact_count
            } else {
                artifact_views.len()
            },
            artifacts: artifact_views,
            history_count: parsed
                .as_ref()
                .and_then(|task| task.history.as_ref())
                .map_or(0, Vec::len),
            metadata: record.metadata.clone(),
            created_at: record.created_at.to_rfc3339(),
            updated_at: record.updated_at.to_rfc3339(),
            completed_at: record.completed_at.map(MissiveTimestamp::to_rfc3339),
            raw_task: record.remote_task_json.clone(),
        }
    }
}

fn first_artifact_text(task: &Task) -> Option<String> {
    task.artifacts
        .as_ref()?
        .iter()
        .flat_map(|artifact| artifact.parts.iter())
        .find_map(|part| part.as_text())
        .map(ToOwned::to_owned)
}

impl From<&NegotiatedInterface> for TaskInterfaceView {
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

struct TaskWaitParts {
    profile: String,
    task_id: String,
    agent: Option<String>,
    status: String,
    attempts: u64,
    started: Instant,
    timeout: Duration,
    interval: Duration,
    timed_out: bool,
    selected_interface: Option<TaskInterfaceView>,
    task: Option<TaskView>,
}

impl TaskWaitOutput {
    fn from_parts(parts: TaskWaitParts) -> Self {
        let elapsed = parts.started.elapsed();
        let message = if parts.timed_out {
            format!(
                "Timed out waiting for task '{}' after {} attempt(s)",
                parts.task_id, parts.attempts
            )
        } else {
            format!(
                "Task '{}' reached state {} after {} attempt(s)",
                parts.task_id, parts.status, parts.attempts
            )
        };
        Self {
            profile: parts.profile,
            task_id: parts.task_id,
            agent: parts.agent,
            status: parts.status,
            attempts: parts.attempts,
            elapsed_ms: elapsed.as_millis(),
            timeout_ms: parts.timeout.as_millis(),
            interval_ms: parts.interval.as_millis(),
            timed_out: parts.timed_out,
            selected_interface: parts.selected_interface,
            task: parts.task,
            message,
        }
    }
}

fn render_task_get<W>(writer: &mut W, mode: OutputMode, output: &TaskGetOutput) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => write_task_human(writer, &output.task),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, "task_get", output, &output.message)
        }
    }
}

fn render_task_list<W>(writer: &mut W, mode: OutputMode, output: &TaskListOutput) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => write_task_list_human(writer, output),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, "task_list", output, &output.message)
        }
    }
}

fn render_task_cancel<W>(writer: &mut W, mode: OutputMode, output: &TaskCancelOutput) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => {
            writeln!(writer, "{}", redact_text(&output.message))
                .map_err(|error| MissiveError::io("writing task cancel output", error))?;
            write_task_human(writer, &output.task)
        }
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, "task_cancel", output, &output.message)
        }
    }
}

fn render_task_wait<W>(writer: &mut W, mode: OutputMode, output: &TaskWaitOutput) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => {
            writeln!(writer, "{}", redact_text(&output.message))
                .map_err(|error| MissiveError::io("writing task wait output", error))?;
            if let Some(task) = &output.task {
                write_task_human(writer, task)?;
            }
            Ok(())
        }
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, "task_wait", output, &output.message)
        }
    }
}

fn write_task_list_human<W>(writer: &mut W, output: &TaskListOutput) -> Result<()>
where
    W: Write,
{
    if output.tasks.is_empty() {
        return writeln!(
            writer,
            "No {} tasks matched for profile '{}'.",
            redact_text(&output.source),
            redact_text(&output.profile)
        )
        .map_err(|error| MissiveError::io("writing task list output", error));
    }

    writeln!(
        writer,
        "Tasks for profile '{}' ({}):",
        redact_text(&output.profile),
        redact_text(&output.source)
    )
    .map_err(|error| MissiveError::io("writing task list output", error))?;
    for task in &output.tasks {
        writeln!(
            writer,
            "  {}  agent={}  state={}  source={}  context={}  updated={}",
            redact_text(&task.task_id),
            redact_text(&task.agent),
            redact_text(&task.state),
            redact_text(&task.source),
            task.context_id
                .as_deref()
                .map(redact_text)
                .unwrap_or_else(|| "-".to_owned()),
            redact_text(&task.updated_at)
        )
        .map_err(|error| MissiveError::io("writing task list output", error))?;
    }
    Ok(())
}

fn write_task_human<W>(writer: &mut W, task: &TaskView) -> Result<()>
where
    W: Write,
{
    writeln!(writer, "Task {}", redact_text(&task.task_id))
        .map_err(|error| MissiveError::io("writing task output", error))?;
    writeln!(writer, "  agent: {}", redact_text(&task.agent))
        .map_err(|error| MissiveError::io("writing task output", error))?;
    writeln!(writer, "  state: {}", redact_text(&task.state))
        .map_err(|error| MissiveError::io("writing task output", error))?;
    writeln!(writer, "  source: {}", redact_text(&task.source))
        .map_err(|error| MissiveError::io("writing task output", error))?;
    writeln!(
        writer,
        "  context: {}",
        task.context_id
            .as_deref()
            .map(redact_text)
            .unwrap_or_else(|| "-".to_owned())
    )
    .map_err(|error| MissiveError::io("writing task output", error))?;
    if let Some(text) = &task.text {
        writeln!(writer, "  text: {}", redact_text(text))
            .map_err(|error| MissiveError::io("writing task output", error))?;
    }
    writeln!(writer, "  artifacts: {}", task.artifact_count)
        .map_err(|error| MissiveError::io("writing task output", error))?;
    for artifact in &task.artifacts {
        writeln!(
            writer,
            "    {}  kind={}  version={}  name={}  mime={}",
            redact_text(&artifact.artifact_id),
            redact_text(&artifact.kind),
            artifact.version,
            artifact
                .name
                .as_deref()
                .map(redact_text)
                .unwrap_or_else(|| "-".to_owned()),
            artifact
                .mime_type
                .as_deref()
                .map(redact_text)
                .unwrap_or_else(|| "-".to_owned())
        )
        .map_err(|error| MissiveError::io("writing task output", error))?;
    }
    writeln!(writer, "  history: {}", task.history_count)
        .map_err(|error| MissiveError::io("writing task output", error))?;
    writeln!(writer, "  updated_at: {}", redact_text(&task.updated_at))
        .map_err(|error| MissiveError::io("writing task output", error))
}

fn wait_state_is_decisive(state: TaskState) -> bool {
    matches!(
        state,
        TaskState::Completed | TaskState::Failed | TaskState::Cancelled | TaskState::InputRequired
    )
}

fn wait_exit_for_state(task_id: &TaskId, state: TaskState) -> Result<()> {
    match state {
        TaskState::Completed => Ok(()),
        TaskState::Failed => Err(MissiveError::orchestration(format!(
            "task {:?} reached failed state",
            task_id.as_str()
        ))
        .with_exit_code(MissiveExitCode::TaskFailed)),
        TaskState::Cancelled => Err(MissiveError::orchestration(format!(
            "task {:?} reached cancelled state",
            task_id.as_str()
        ))
        .with_exit_code(MissiveExitCode::TaskCancelled)),
        TaskState::InputRequired => Err(MissiveError::orchestration(format!(
            "task {:?} requires input",
            task_id.as_str()
        ))
        .with_help("Send follow-up input for the same task/context before waiting again.")
        .with_exit_code(MissiveExitCode::TaskInputRequired)),
        TaskState::Submitted | TaskState::Working | TaskState::Unknown => Ok(()),
    }
}

fn parse_task_state_arg(value: &str) -> std::result::Result<TaskState, String> {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "submitted" => Ok(TaskState::Submitted),
        "working" => Ok(TaskState::Working),
        "input_required" => Ok(TaskState::InputRequired),
        "completed" => Ok(TaskState::Completed),
        "failed" => Ok(TaskState::Failed),
        "cancelled" | "canceled" => Ok(TaskState::Cancelled),
        "unknown" => Ok(TaskState::Unknown),
        _ => Err(
            "expected submitted, working, input-required, completed, failed, cancelled, or unknown"
                .to_owned(),
        ),
    }
}

fn parse_task_source_arg(value: &str) -> std::result::Result<TaskSource, String> {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "remote" => Ok(TaskSource::Remote),
        "local" => Ok(TaskSource::Local),
        "gateway" => Ok(TaskSource::Gateway),
        _ => Err("expected remote, local, or gateway".to_owned()),
    }
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
        .with_help("Use values such as 500ms, 2s, 5m, or 1h."));
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

fn validate_positive_i32(flag: &str, value: Option<i32>) -> Result<()> {
    if value.is_some_and(|value| value <= 0) {
        return Err(MissiveError::validation(format!(
            "{flag} must be greater than zero"
        )));
    }
    Ok(())
}

fn validate_non_negative_i32(flag: &str, value: Option<i32>) -> Result<()> {
    if value.is_some_and(|value| value < 0) {
        return Err(MissiveError::validation(format!(
            "{flag} must be greater than or equal to zero"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_state_parser_accepts_canceled_aliases() {
        assert_eq!(
            parse_task_state_arg("input-required"),
            Ok(TaskState::InputRequired)
        );
        assert_eq!(
            parse_task_state_arg("input_required"),
            Ok(TaskState::InputRequired)
        );
        assert_eq!(parse_task_state_arg("cancelled"), Ok(TaskState::Cancelled));
        assert_eq!(parse_task_state_arg("canceled"), Ok(TaskState::Cancelled));
    }

    #[test]
    fn duration_parser_accepts_expected_suffixes() {
        assert_eq!(
            parse_duration_arg("--interval", "250ms").expect("duration"),
            Duration::from_millis(250)
        );
        assert_eq!(
            parse_duration_arg("--interval", "2s").expect("duration"),
            Duration::from_secs(2)
        );
        assert!(parse_duration_arg("--interval", "0s").is_err());
        assert!(parse_duration_arg("--interval", "2").is_err());
    }
}
