//! Gateway background job commands.
//!
//! `missive job` is the CLI control surface for durable gateway-managed
//! communication jobs.  Starting a job records a `gateway_jobs` row; a running
//! `missive gateway run` process scans queued rows, executes send/stream/wait or
//! local reduce work, stores result JSON, and appends events.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::thread;
use std::time::{Duration, Instant};

use clap::{ArgAction, Args, Subcommand};
use missive_a2a::{
    ServiceParameters, TaskClient,
    protocol::{self, CancelTaskRequest},
};
use missive_core::{
    AgentAlias, ContextId, GroupName, LoadedConfig, Metadata, MissiveError, MissiveTimestamp,
    Result, TaskId,
};
use missive_gateway::{
    BACKGROUND_JOB_KIND_REDUCE, BACKGROUND_JOB_KIND_SEND, BACKGROUND_JOB_KIND_STREAM,
    BACKGROUND_JOB_KIND_WAIT, is_background_job_kind,
};
use missive_store::{
    ContextUpsert, GatewayJobId, GatewayJobRecord, GatewayJobState, GatewayJobUpsert, StatePaths,
    Store, TaskSource, TaskState, TaskUpsert,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::agent::{get_existing_agent, open_agent_registry};
use crate::auth::auth_headers_for_agent;
use crate::events::new_cli_event;
use crate::output::{OutputMode, redact_json, render_success};
use crate::send::{SendArgs, message_part_limit_bytes, prepare_send_request};
use crate::stream::StreamArgs;
use crate::task::TaskWaitArgs;
use crate::{GlobalArgs, service_parameters_from_config_and_globals};

const JOB_SCHEMA: &str = "missive.job.v1";
const DEFAULT_ATTACH_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_ATTACH_INTERVAL: Duration = Duration::from_millis(250);
const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_WAIT_INTERVAL: Duration = Duration::from_secs(1);
const MAX_ATTACH_SLEEP: Duration = Duration::from_secs(5);

/// Background job subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum JobCommands {
    /// Enqueue a background send, stream, wait, or reduce operation for the gateway.
    Start {
        /// Operation to enqueue.
        #[command(subcommand)]
        command: Box<JobStartCommands>,
    },
    /// List gateway-managed background jobs.
    List(JobListArgs),
    /// Show one background job and its latest result.
    Show(JobShowArgs),
    /// Cancel a queued/running/retrying job, optionally cancelling its remote A2A task.
    Cancel(JobCancelArgs),
}

impl JobCommands {
    /// Stable subcommand spelling used in structured output messages.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Start { .. } => "start",
            Self::List(_) => "list",
            Self::Show(_) => "show",
            Self::Cancel(_) => "cancel",
        }
    }
}

/// Operations accepted by `missive job start`.
#[derive(Debug, Clone, Subcommand)]
pub enum JobStartCommands {
    /// Enqueue a background A2A SendMessage operation.
    Send(JobStartSendArgs),
    /// Enqueue a background A2A SendStreamingMessage operation.
    Stream(JobStartStreamArgs),
    /// Enqueue a background task wait operation.
    Wait(JobStartWaitArgs),
    /// Enqueue a background local reduce operation over persisted group outputs.
    Reduce(JobStartReduceArgs),
}

/// Shared options for `job start` operations.
#[derive(Debug, Clone, Args)]
pub struct JobStartOptions {
    /// Maximum attempts before the gateway marks the job failed.
    #[arg(long = "max-attempts", value_name = "N", default_value_t = 1)]
    pub max_attempts: u32,

    /// Wait for the queued job to reach a terminal state and print the final row.
    #[arg(long = "attach", action = ArgAction::SetTrue)]
    pub attach: bool,

    /// Timeout for --attach, for example 30s, 2m, or 1h.
    #[arg(long = "attach-timeout", value_name = "DURATION")]
    pub attach_timeout: Option<String>,

    /// Request remote A2A task cancellation when this job is cancelled and a task id is known.
    #[arg(long = "cancel-remote-on-cancel", action = ArgAction::SetTrue)]
    pub cancel_remote_on_cancel: bool,
}

/// Arguments for `missive job start send`.
#[derive(Debug, Clone, Args)]
pub struct JobStartSendArgs {
    #[command(flatten)]
    pub send: SendArgs,
    #[command(flatten)]
    pub options: JobStartOptions,
}

/// Arguments for `missive job start stream`.
#[derive(Debug, Clone, Args)]
pub struct JobStartStreamArgs {
    #[command(flatten)]
    pub stream: StreamArgs,
    #[command(flatten)]
    pub options: JobStartOptions,
}

/// Arguments for `missive job start wait`.
#[derive(Debug, Clone, Args)]
pub struct JobStartWaitArgs {
    #[command(flatten)]
    pub wait: TaskWaitArgs,
    #[command(flatten)]
    pub options: JobStartOptions,
}

/// Arguments for `missive job start reduce`.
#[derive(Debug, Clone, Args)]
pub struct JobStartReduceArgs {
    /// Local group to reduce.
    pub group: String,

    /// Shared A2A context id whose local group outputs should be reduced.
    #[arg(long = "context", value_name = "CONTEXT_ID")]
    pub context: String,

    /// Local deterministic reduce strategy: summarise, summarize, merge, rank, or vote.
    #[arg(
        long = "strategy",
        value_name = "STRATEGY",
        default_value = "summarise"
    )]
    pub strategy: String,

    #[command(flatten)]
    pub options: JobStartOptions,
}

/// Arguments for `missive job list`.
#[derive(Debug, Clone, Args)]
pub struct JobListArgs {
    /// Filter by job kind: send, stream, wait, or reduce.
    #[arg(long = "kind", value_name = "KIND", value_parser = parse_background_job_kind)]
    pub kind: Option<String>,

    /// Filter by job state.
    #[arg(long = "state", value_name = "STATE", value_parser = parse_gateway_job_state)]
    pub state: Option<GatewayJobState>,

    /// Filter by agent alias.
    #[arg(long = "agent", value_name = "ALIAS")]
    pub agent: Option<String>,

    /// Filter by context id.
    #[arg(long = "context", value_name = "CONTEXT_ID")]
    pub context: Option<String>,

    /// Limit the number of rows rendered after filtering.
    #[arg(long = "limit", value_name = "N")]
    pub limit: Option<usize>,
}

/// Arguments for `missive job show`.
#[derive(Debug, Clone, Args)]
pub struct JobShowArgs {
    /// Gateway job id.
    pub job_id: String,
}

/// Arguments for `missive job cancel`.
#[derive(Debug, Clone, Args)]
pub struct JobCancelArgs {
    /// Gateway job id.
    pub job_id: String,

    /// Also request remote A2A CancelTask when the job has or records a task id.
    #[arg(long = "remote", action = ArgAction::SetTrue)]
    pub remote: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct JobView {
    job_id: String,
    kind: String,
    state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    group: Option<String>,
    retry_count: u32,
    max_attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_run_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    locked_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    locked_until: Option<String>,
    created_at: String,
    updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    completed_at: Option<String>,
    request: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    metadata: Metadata,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct JobStartOutput {
    profile: String,
    attached: bool,
    job: JobView,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct JobListOutput {
    profile: String,
    count: usize,
    filters: JobFiltersView,
    jobs: Vec<JobView>,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct JobFiltersView {
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct JobShowOutput {
    profile: String,
    job: JobView,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct JobCancelOutput {
    profile: String,
    remote_requested: bool,
    remote_cancelled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_result: Option<Value>,
    job: JobView,
    message: String,
}

/// Executes one job command.
pub(crate) fn execute_job_command<R, W>(
    command: &JobCommands,
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
    match command {
        JobCommands::Start { command } => start_job(
            command,
            globals,
            loaded_config,
            environment,
            mode,
            input,
            writer,
        ),
        JobCommands::List(args) => list_jobs(args, loaded_config, environment, mode, writer),
        JobCommands::Show(args) => show_job(args, loaded_config, environment, mode, writer),
        JobCommands::Cancel(args) => {
            cancel_job(args, globals, loaded_config, environment, mode, writer)
        }
    }
}

fn start_job<R, W>(
    command: &JobStartCommands,
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
    let registry = open_agent_registry(loaded_config, environment)?;
    let (upsert, options) = match command {
        JobStartCommands::Send(args) => {
            let alias = AgentAlias::new(args.send.agent.clone())?;
            get_existing_agent(&registry.store, &alias)?;
            let prepared =
                prepare_send_request(&args.send, &service_parameters, max_request_bytes, input)?;
            (
                send_or_stream_job_upsert(
                    BACKGROUND_JOB_KIND_SEND,
                    &args.send.agent,
                    &prepared,
                    &args.options,
                    &service_parameters,
                )?,
                &args.options,
            )
        }
        JobStartCommands::Stream(args) => {
            let alias = AgentAlias::new(args.stream.agent.clone())?;
            get_existing_agent(&registry.store, &alias)?;
            let send_args = send_args_from_stream_args(&args.stream);
            let prepared =
                prepare_send_request(&send_args, &service_parameters, max_request_bytes, input)?;
            (
                stream_job_upsert(&args.stream, &prepared, &args.options, &service_parameters)?,
                &args.options,
            )
        }
        JobStartCommands::Wait(args) => (
            wait_job_upsert(&args.wait, globals, &args.options, &service_parameters)?,
            &args.options,
        ),
        JobStartCommands::Reduce(args) => {
            let group_name = GroupName::new(args.group.clone())?;
            if registry.store.get_group(&group_name)?.is_none() {
                return Err(MissiveError::validation(format!(
                    "group {:?} is not known locally",
                    group_name.as_str()
                ))
                .with_help(
                    "Create the group with 'missive group create' before enqueuing a reduce job.",
                ));
            }
            (
                reduce_job_upsert(args, &args.options, &service_parameters)?,
                &args.options,
            )
        }
    };

    ensure_job_links(&registry.store, &upsert, &service_parameters)?;
    let job = registry.store.upsert_gateway_job(&upsert)?;
    append_job_enqueued_event(&registry.store, &job, &service_parameters)?;
    let profile = registry.profile.clone();
    let state_paths = registry.state_paths.clone();
    drop(registry);

    let final_job = if options.attach {
        wait_for_job_terminal(
            &state_paths,
            &job.gateway_job_id,
            options.attach_timeout.as_deref(),
        )?
    } else {
        job
    };
    let view = JobView::from_record(&final_job);
    let attached = options.attach;
    let message = if attached {
        format!(
            "Background job {} reached state {}",
            view.job_id, view.state
        )
    } else {
        format!(
            "Queued background {} job {}; run 'missive gateway run' to execute it",
            view.kind, view.job_id
        )
    };
    let output = JobStartOutput {
        profile,
        attached,
        job: view,
        message,
    };
    render_success(writer, mode, "job_start", &output, &output.message)
}

fn list_jobs<W>(
    args: &JobListArgs,
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let registry = open_agent_registry(loaded_config, environment)?;
    let filters = ParsedJobFilters::from_args(args)?;
    let mut jobs = registry
        .store
        .list_gateway_jobs()?
        .into_iter()
        .filter(|job| is_background_job_kind(&job.kind))
        .filter(|job| filters.matches(job))
        .collect::<Vec<_>>();
    if let Some(limit) = filters.limit {
        jobs.truncate(limit);
    }
    let views = jobs.iter().map(JobView::from_record).collect::<Vec<_>>();
    let output = JobListOutput {
        profile: registry.profile,
        count: views.len(),
        filters: JobFiltersView::from_filters(&filters),
        message: format!("Listed {} background job(s)", views.len()),
        jobs: views,
    };
    render_success(writer, mode, "job_list", &output, &output.message)
}

fn show_job<W>(
    args: &JobShowArgs,
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let registry = open_agent_registry(loaded_config, environment)?;
    let job_id = GatewayJobId::new(args.job_id.clone())?;
    let job = get_existing_background_job(&registry.store, &job_id)?;
    let view = JobView::from_record(&job);
    let output = JobShowOutput {
        profile: registry.profile,
        message: format!("Showing background job {}", view.job_id),
        job: view,
    };
    render_success(writer, mode, "job_show", &output, &output.message)
}

fn cancel_job<W>(
    args: &JobCancelArgs,
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
    let job_id = GatewayJobId::new(args.job_id.clone())?;
    let job = get_existing_background_job(&registry.store, &job_id)?;
    let policy_remote = job
        .request_json
        .get("cancel_remote_on_cancel")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let remote_requested = args.remote || policy_remote;
    let remote_result = if remote_requested {
        Some(cancel_remote_task_for_job(
            &mut registry.store,
            &job,
            globals,
            environment,
            &service_parameters,
        )?)
    } else {
        None
    };

    let mut upsert = job_to_upsert(&job);
    upsert.state = GatewayJobState::Cancelled;
    upsert.locked_by = None;
    upsert.locked_until = None;
    upsert.next_run_at = None;
    upsert.completed_at = Some(MissiveTimestamp::now_utc());
    upsert.result_json = Some(json!({
        "status": "cancelled",
        "cancelled_by": "cli",
        "remote_requested": remote_requested,
        "remote_result": remote_result,
    }));
    upsert
        .metadata
        .insert_str("gateway.job.cancelled_by", "cli".to_owned())?;
    let cancelled = registry.store.upsert_gateway_job(&upsert)?;
    append_job_cancelled_event(
        &registry.store,
        &cancelled,
        remote_requested,
        remote_result.as_ref(),
        &service_parameters,
    )?;
    let view = JobView::from_record(&cancelled);
    let message = if remote_requested && remote_result.is_some() {
        format!(
            "Cancelled background job {} and requested remote task cancellation",
            view.job_id
        )
    } else {
        format!("Cancelled background job {} locally", view.job_id)
    };
    let output = JobCancelOutput {
        profile: registry.profile,
        remote_requested,
        remote_cancelled: remote_result.is_some(),
        remote_result,
        job: view,
        message,
    };
    render_success(writer, mode, "job_cancel", &output, &output.message)
}

fn send_or_stream_job_upsert(
    kind: &str,
    agent: &str,
    prepared: &crate::send::PreparedSend,
    options: &JobStartOptions,
    service_parameters: &ServiceParameters,
) -> Result<GatewayJobUpsert> {
    validate_job_start_options(options)?;
    let job_id = GatewayJobId::new(format!(
        "job/{kind}/{}",
        safe_identifier_fragment(prepared.request_message_id.as_str(), 64)
    ))?;
    let mut job = GatewayJobUpsert::new(
        job_id,
        kind,
        json!({
            "schema": JOB_SCHEMA,
            "operation": kind,
            "agent": agent,
            "request": &prepared.request,
            "request_message_id": prepared.request_message_id.as_str(),
            "requested_context_id": prepared.requested_context_id.as_ref().map(ContextId::as_str),
            "requested_task_id": prepared.requested_task_id.as_ref().map(TaskId::as_str),
            "part_count": prepared.part_summaries.len(),
            "parts": &prepared.part_summaries,
            "local_input_bytes": prepared.local_input_bytes,
            "request_bytes": prepared.request_bytes,
            "accepted_output_modes": &prepared.accepted_output_modes,
            "cancel_remote_on_cancel": options.cancel_remote_on_cancel,
        }),
    );
    job.agent_alias = Some(AgentAlias::new(agent.to_owned())?);
    job.context_id = prepared.requested_context_id.clone();
    job.task_id = prepared.requested_task_id.clone();
    job.max_attempts = options.max_attempts;
    job.metadata = job_metadata(kind, service_parameters)?;
    Ok(job)
}

fn stream_job_upsert(
    args: &StreamArgs,
    prepared: &crate::send::PreparedSend,
    options: &JobStartOptions,
    service_parameters: &ServiceParameters,
) -> Result<GatewayJobUpsert> {
    let mut job = send_or_stream_job_upsert(
        BACKGROUND_JOB_KIND_STREAM,
        &args.agent,
        prepared,
        options,
        service_parameters,
    )?;
    if let Some(object) = job.request_json.as_object_mut() {
        object.insert("force".to_owned(), json!(args.force));
    }
    Ok(job)
}

fn wait_job_upsert(
    args: &TaskWaitArgs,
    globals: &GlobalArgs,
    options: &JobStartOptions,
    service_parameters: &ServiceParameters,
) -> Result<GatewayJobUpsert> {
    validate_job_start_options(options)?;
    let task_id = TaskId::new(args.task_id.clone())?;
    let agent_alias = args
        .agent
        .as_ref()
        .map(|value| AgentAlias::new(value.clone()))
        .transpose()?;
    let timeout = globals
        .timeout
        .as_deref()
        .map(|value| parse_duration_arg("--timeout", value))
        .transpose()?
        .unwrap_or(DEFAULT_WAIT_TIMEOUT);
    let interval = args
        .interval
        .as_deref()
        .map(|value| parse_duration_arg("--interval", value))
        .transpose()?
        .unwrap_or(DEFAULT_WAIT_INTERVAL);
    let job_id = GatewayJobId::new(format!(
        "job/wait/{}-{}",
        safe_identifier_fragment(task_id.as_str(), 48),
        safe_identifier_fragment(&protocol::new_message_id(), 32)
    ))?;
    let mut job = GatewayJobUpsert::new(
        job_id,
        BACKGROUND_JOB_KIND_WAIT,
        json!({
            "schema": JOB_SCHEMA,
            "operation": BACKGROUND_JOB_KIND_WAIT,
            "agent": agent_alias.as_ref().map(AgentAlias::as_str),
            "task_id": task_id.as_str(),
            "local": args.local,
            "history_length": args.history_length,
            "interval_ms": duration_millis(interval),
            "timeout_ms": duration_millis(timeout),
            "cancel_remote_on_cancel": options.cancel_remote_on_cancel,
        }),
    );
    job.agent_alias = agent_alias;
    job.task_id = Some(task_id);
    job.max_attempts = options.max_attempts;
    job.metadata = job_metadata(BACKGROUND_JOB_KIND_WAIT, service_parameters)?;
    Ok(job)
}

fn reduce_job_upsert(
    args: &JobStartReduceArgs,
    options: &JobStartOptions,
    service_parameters: &ServiceParameters,
) -> Result<GatewayJobUpsert> {
    validate_job_start_options(options)?;
    let group_name = GroupName::new(args.group.clone())?;
    let context_id = ContextId::new(args.context.clone())?;
    let strategy = normalize_reduce_strategy(&args.strategy)?;
    let job_id = GatewayJobId::new(format!(
        "job/reduce/{}-{}",
        safe_identifier_fragment(group_name.as_str(), 48),
        safe_identifier_fragment(&protocol::new_message_id(), 32)
    ))?;
    let mut job = GatewayJobUpsert::new(
        job_id,
        BACKGROUND_JOB_KIND_REDUCE,
        json!({
            "schema": JOB_SCHEMA,
            "operation": BACKGROUND_JOB_KIND_REDUCE,
            "group": group_name.as_str(),
            "context_id": context_id.as_str(),
            "strategy": strategy,
            "cancel_remote_on_cancel": options.cancel_remote_on_cancel,
        }),
    );
    job.group_name = Some(group_name);
    job.context_id = Some(context_id);
    job.max_attempts = options.max_attempts;
    job.metadata = job_metadata(BACKGROUND_JOB_KIND_REDUCE, service_parameters)?;
    Ok(job)
}

fn job_metadata(kind: &str, service_parameters: &ServiceParameters) -> Result<Metadata> {
    let mut metadata = service_parameters.to_metadata()?;
    metadata.insert_str("gateway.job.kind", kind.to_owned())?;
    metadata.insert_str("gateway.job.schema", JOB_SCHEMA.to_owned())?;
    Ok(metadata)
}

fn validate_job_start_options(options: &JobStartOptions) -> Result<()> {
    if options.max_attempts == 0 {
        return Err(MissiveError::validation(
            "--max-attempts must be at least 1",
        ));
    }
    if let Some(value) = options.attach_timeout.as_deref() {
        parse_duration_arg("--attach-timeout", value)?;
    }
    Ok(())
}

fn send_args_from_stream_args(args: &StreamArgs) -> SendArgs {
    SendArgs {
        agent: args.agent.clone(),
        message: args.message.clone(),
        stdin: args.stdin,
        files: args.files.clone(),
        file_bytes: args.file_bytes.clone(),
        json_parts: args.json_parts.clone(),
        mime: args.mime.clone(),
        parts: args.parts.clone(),
        metadata: args.metadata.clone(),
        context: args.context.clone(),
        task: args.task.clone(),
        accepted_output_modes: args.accepted_output_modes.clone(),
    }
}

fn ensure_job_links(
    store: &Store,
    job: &GatewayJobUpsert,
    service_parameters: &ServiceParameters,
) -> Result<()> {
    if let Some(context_id) = &job.context_id
        && store.get_context(context_id)?.is_none()
    {
        let mut context = ContextUpsert::new(context_id.clone());
        context.agent_alias = job.agent_alias.clone();
        store.upsert_context(&context)?;
    }

    if let Some(task_id) = &job.task_id
        && store.get_task(task_id)?.is_none()
    {
        let Some(agent_alias) = &job.agent_alias else {
            return Err(MissiveError::validation(format!(
                "background job {} references task {} but no --agent was supplied and the task is not known locally",
                job.gateway_job_id.as_str(),
                task_id.as_str()
            ))
            .with_help("Pass --agent for a new background wait job, or create/fetch the task locally first."));
        };
        let mut task = TaskUpsert::new(task_id.clone(), agent_alias.clone(), TaskState::Submitted);
        task.source = TaskSource::Gateway;
        task.context_id = job.context_id.clone();
        task.record_a2a_protocol_version(service_parameters.protocol_version.clone())?;
        store.upsert_task(&task)?;
    }

    Ok(())
}

fn append_job_enqueued_event(
    store: &Store,
    job: &GatewayJobRecord,
    service_parameters: &ServiceParameters,
) -> Result<()> {
    let mut event = new_cli_event(
        "missive.job.enqueued",
        json!({
            "job_id": job.gateway_job_id.as_str(),
            "kind": job.kind,
            "state": job.state.as_str(),
            "agent": job.agent_alias.as_ref().map(AgentAlias::as_str),
            "context_id": job.context_id.as_ref().map(ContextId::as_str),
            "task_id": job.task_id.as_ref().map(TaskId::as_str),
            "group": job.group_name.as_ref().map(GroupName::as_str),
            "max_attempts": job.max_attempts,
        }),
    )?;
    event.agent_alias = job.agent_alias.clone();
    event.context_id = job.context_id.clone();
    event.task_id = job.task_id.clone();
    event.group_name = job.group_name.clone();
    event.gateway_job_id = Some(job.gateway_job_id.clone());
    event.metadata = service_parameters.to_metadata()?;
    event.record_a2a_protocol_version(service_parameters.protocol_version.clone())?;
    store.append_event(&event)?;
    Ok(())
}

fn append_job_cancelled_event(
    store: &Store,
    job: &GatewayJobRecord,
    remote_requested: bool,
    remote_result: Option<&Value>,
    service_parameters: &ServiceParameters,
) -> Result<()> {
    let mut event = new_cli_event(
        "missive.job.cancelled",
        json!({
            "job_id": job.gateway_job_id.as_str(),
            "kind": job.kind,
            "state": job.state.as_str(),
            "remote_requested": remote_requested,
            "remote_result": remote_result,
        }),
    )?;
    event.agent_alias = job.agent_alias.clone();
    event.context_id = job.context_id.clone();
    event.task_id = job.task_id.clone();
    event.group_name = job.group_name.clone();
    event.gateway_job_id = Some(job.gateway_job_id.clone());
    event.metadata = service_parameters.to_metadata()?;
    event.record_a2a_protocol_version(service_parameters.protocol_version.clone())?;
    store.append_event(&event)?;
    Ok(())
}

fn cancel_remote_task_for_job(
    store: &mut Store,
    job: &GatewayJobRecord,
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
    service_parameters: &ServiceParameters,
) -> Result<Value> {
    let task_id = job_task_id(job).ok_or_else(|| {
        MissiveError::validation(format!(
            "background job {} has no known task id for remote cancellation",
            job.gateway_job_id.as_str()
        ))
        .with_help("Cancel locally without --remote, or wait until the job records a task id.")
    })?;
    let agent_alias = job_agent_alias(job).ok_or_else(|| {
        MissiveError::validation(format!(
            "background job {} has no known agent for remote cancellation",
            job.gateway_job_id.as_str()
        ))
    })?;
    let agent = get_existing_agent(store, &agent_alias)?;
    let auth_headers = auth_headers_for_agent(store, &agent, globals, environment)?;
    let (agent, selected_interface) = crate::send::resolve_send_interface_with_store(
        store,
        agent,
        service_parameters,
        &auth_headers,
    )?;
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
    Ok(json!({
        "task_id": task_id.as_str(),
        "agent": agent.alias.as_str(),
        "url": outcome.url,
        "http_status": outcome.status,
        "selected_interface": {
            "binding": selected_interface.binding,
            "url": selected_interface.url,
            "protocol_version": selected_interface.protocol_version,
        },
        "task": outcome.raw_json,
    }))
}

fn job_task_id(job: &GatewayJobRecord) -> Option<TaskId> {
    job.task_id.clone().or_else(|| {
        job.result_json
            .as_ref()
            .and_then(|value| value.get("task_id"))
            .and_then(Value::as_str)
            .or_else(|| job.request_json.get("task_id").and_then(Value::as_str))
            .and_then(|value| TaskId::new(value.to_owned()).ok())
    })
}

fn job_agent_alias(job: &GatewayJobRecord) -> Option<AgentAlias> {
    job.agent_alias.clone().or_else(|| {
        job.request_json
            .get("agent")
            .and_then(Value::as_str)
            .and_then(|value| AgentAlias::new(value.to_owned()).ok())
    })
}

fn wait_for_job_terminal(
    state_paths: &StatePaths,
    job_id: &GatewayJobId,
    attach_timeout: Option<&str>,
) -> Result<GatewayJobRecord> {
    let timeout = attach_timeout
        .map(|value| parse_duration_arg("--attach-timeout", value))
        .transpose()?
        .unwrap_or(DEFAULT_ATTACH_TIMEOUT);
    let started = Instant::now();
    loop {
        let store = Store::open(state_paths.database_path())?;
        let job = get_existing_background_job(&store, job_id)?;
        if is_terminal_job_state(job.state) {
            return Ok(job);
        }
        if started.elapsed() >= timeout {
            return Ok(job);
        }
        let remaining = timeout
            .checked_sub(started.elapsed())
            .unwrap_or_else(|| Duration::from_millis(1));
        thread::sleep(DEFAULT_ATTACH_INTERVAL.min(remaining).min(MAX_ATTACH_SLEEP));
    }
}

fn get_existing_background_job(store: &Store, job_id: &GatewayJobId) -> Result<GatewayJobRecord> {
    let job = store.get_gateway_job(job_id)?.ok_or_else(|| {
        MissiveError::validation(format!(
            "background job {:?} is not known locally",
            job_id.as_str()
        ))
    })?;
    if !is_background_job_kind(&job.kind) {
        return Err(MissiveError::validation(format!(
            "gateway job {:?} has kind {:?}, which is not managed by 'missive job'",
            job_id.as_str(),
            job.kind
        )));
    }
    Ok(job)
}

#[derive(Debug, Clone)]
struct ParsedJobFilters {
    kind: Option<String>,
    state: Option<GatewayJobState>,
    agent: Option<AgentAlias>,
    context_id: Option<ContextId>,
    limit: Option<usize>,
}

impl ParsedJobFilters {
    fn from_args(args: &JobListArgs) -> Result<Self> {
        Ok(Self {
            kind: args.kind.clone(),
            state: args.state,
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
            limit: args.limit,
        })
    }

    fn matches(&self, job: &GatewayJobRecord) -> bool {
        if self.kind.as_ref().is_some_and(|kind| &job.kind != kind) {
            return false;
        }
        if self.state.is_some_and(|state| job.state != state) {
            return false;
        }
        if self
            .agent
            .as_ref()
            .is_some_and(|agent| job.agent_alias.as_ref() != Some(agent))
        {
            return false;
        }
        if self
            .context_id
            .as_ref()
            .is_some_and(|context_id| job.context_id.as_ref() != Some(context_id))
        {
            return false;
        }
        true
    }
}

impl JobFiltersView {
    fn from_filters(filters: &ParsedJobFilters) -> Self {
        Self {
            kind: filters.kind.clone(),
            state: filters.state.map(|state| state.as_str().to_owned()),
            agent: filters
                .agent
                .as_ref()
                .map(|agent| agent.as_str().to_owned()),
            context_id: filters
                .context_id
                .as_ref()
                .map(|context_id| context_id.as_str().to_owned()),
            limit: filters.limit,
        }
    }
}

impl JobView {
    fn from_record(record: &GatewayJobRecord) -> Self {
        Self {
            job_id: record.gateway_job_id.as_str().to_owned(),
            kind: record.kind.clone(),
            state: record.state.as_str().to_owned(),
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
            group: record
                .group_name
                .as_ref()
                .map(|group| group.as_str().to_owned()),
            retry_count: record.retry_count,
            max_attempts: record.max_attempts,
            next_run_at: record.next_run_at.map(MissiveTimestamp::to_rfc3339),
            locked_by: record.locked_by.clone(),
            locked_until: record.locked_until.map(MissiveTimestamp::to_rfc3339),
            created_at: record.created_at.to_rfc3339(),
            updated_at: record.updated_at.to_rfc3339(),
            completed_at: record.completed_at.map(MissiveTimestamp::to_rfc3339),
            request: summarize_job_request(&record.request_json),
            result: record.result_json.as_ref().map(redact_json),
            metadata: record.metadata.clone(),
        }
    }
}

fn summarize_job_request(request: &Value) -> Value {
    let operation = request
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut summary = json!({
        "schema": request.get("schema").and_then(Value::as_str),
        "operation": operation,
        "agent": request.get("agent").and_then(Value::as_str),
        "context_id": request
            .get("requested_context_id")
            .or_else(|| request.get("context_id"))
            .and_then(Value::as_str),
        "task_id": request
            .get("requested_task_id")
            .or_else(|| request.get("task_id"))
            .and_then(Value::as_str),
        "group": request.get("group").and_then(Value::as_str),
        "strategy": request.get("strategy").and_then(Value::as_str),
        "part_count": request.get("part_count").and_then(Value::as_u64),
        "local_input_bytes": request.get("local_input_bytes").and_then(Value::as_u64),
        "request_bytes": request.get("request_bytes").and_then(Value::as_u64),
        "cancel_remote_on_cancel": request.get("cancel_remote_on_cancel").and_then(Value::as_bool).unwrap_or(false),
    });
    if let Some(object) = summary.as_object_mut() {
        object.retain(|_, value| !value.is_null());
    }
    redact_json(&summary)
}

fn parse_background_job_kind(value: &str) -> Result<String> {
    if is_background_job_kind(value) {
        Ok(value.to_owned())
    } else {
        Err(MissiveError::validation(format!(
            "invalid background job kind {value:?}; expected send, stream, wait, or reduce"
        )))
    }
}

fn parse_gateway_job_state(value: &str) -> Result<GatewayJobState> {
    value.parse()
}

fn normalize_reduce_strategy(value: &str) -> Result<String> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "summarise" | "summarize" | "merge" | "rank" | "vote" => Ok(normalized),
        _ => Err(MissiveError::validation(format!(
            "--strategy value {value:?} is not supported for background reduce jobs"
        ))
        .with_help("Use summarise, summarize, merge, rank, or vote.")),
    }
}

fn is_terminal_job_state(state: GatewayJobState) -> bool {
    matches!(
        state,
        GatewayJobState::Succeeded | GatewayJobState::Failed | GatewayJobState::Cancelled
    )
}

fn job_to_upsert(job: &GatewayJobRecord) -> GatewayJobUpsert {
    GatewayJobUpsert {
        gateway_job_id: job.gateway_job_id.clone(),
        kind: job.kind.clone(),
        state: job.state,
        agent_alias: job.agent_alias.clone(),
        context_id: job.context_id.clone(),
        task_id: job.task_id.clone(),
        group_name: job.group_name.clone(),
        adapter_binding_id: job.adapter_binding_id.clone(),
        request_json: job.request_json.clone(),
        result_json: job.result_json.clone(),
        metadata: job.metadata.clone(),
        retry_count: job.retry_count,
        max_attempts: job.max_attempts,
        next_run_at: job.next_run_at,
        locked_by: job.locked_by.clone(),
        locked_until: job.locked_until,
        completed_at: job.completed_at,
    }
}

fn safe_identifier_fragment(value: &str, max_chars: usize) -> String {
    let fragment = value
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
        .take(max_chars)
        .collect::<String>();
    if fragment.is_empty() {
        "job".to_owned()
    } else {
        fragment
    }
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_job_kind_parser_rejects_subscription_jobs() {
        assert_eq!(
            parse_background_job_kind(BACKGROUND_JOB_KIND_SEND).expect("send"),
            BACKGROUND_JOB_KIND_SEND
        );
        assert!(parse_background_job_kind("task_subscription").is_err());
    }

    #[test]
    fn job_request_summary_omits_raw_a2a_request_body() {
        let summary = summarize_job_request(&json!({
            "schema": JOB_SCHEMA,
            "operation": "send",
            "agent": "echo",
            "request": {"message": {"parts": [{"text": "secret-ish content"}]}},
            "part_count": 1,
            "request_bytes": 42,
        }));

        assert_eq!(summary["operation"], "send");
        assert!(summary.get("request").is_none());
    }
}
