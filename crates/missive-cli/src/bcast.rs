//! Broadcast collective command implementation.
//!
//! `missive bcast` is the first MPI-inspired collective operation. It reads a
//! local group definition, creates or reuses one shared context id, sends the
//! same A2A message content to every member, records per-member send artifacts
//! through the existing send persistence path, and appends group-operation
//! events for later inspection/replay.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use clap::{ArgAction, Args, ValueEnum};
use missive_a2a::{
    AuthHeaders, NegotiatedInterface, SendMessageClient, SendMessageOutcome, ServiceParameters,
    protocol::SendMessageResponse,
};
use missive_core::{
    AgentAlias, ContextId, ErrorReport, GroupName, LoadedConfig, Metadata, MissiveError,
    MissiveExitCode, Result,
};
use missive_store::{AgentRecord, ContextUpsert, GroupMemberRecord, GroupRecord, Store};
use serde::Serialize;
use serde_json::json;

use crate::agent::{get_existing_agent, open_agent_registry};
use crate::auth::auth_headers_for_agent;
use crate::events::new_cli_event;
use crate::output::{OutputMode, render_success};
use crate::send::{
    MessagePartSummary, PreparedSend, clone_prepared_with_new_message_id, message_part_limit_bytes,
    persist_send, prepare_send_request, resolve_send_interface_with_store_timeout,
    store_task_state,
};
use crate::{GlobalArgs, service_parameters_from_config_and_globals};

const DEFAULT_MEMBER_SEND_TIMEOUT: Duration = Duration::from_secs(30);

/// Arguments for `missive bcast`.
#[derive(Debug, Clone, Args)]
pub struct BcastArgs {
    /// Local group name whose members should receive the broadcast.
    pub group: String,

    /// Text message to broadcast. Omit when using --stdin, --file, --file-bytes, --json-part, or --part.
    pub message: Option<String>,

    /// Read one UTF-8 text message part from standard input and reuse it for every member.
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

    /// Reuse this A2A context id instead of generating a broadcast context.
    #[arg(long = "context", value_name = "CONTEXT_ID")]
    pub context: Option<String>,

    /// Accepted response MIME/output mode; repeatable.
    #[arg(long = "accepted-output-mode", value_name = "MIME")]
    pub accepted_output_modes: Vec<String>,

    /// Execution strategy for member sends.
    #[arg(long = "execution", value_name = "MODE", default_value_t = BcastExecution::Sequential, value_enum)]
    pub execution: BcastExecution,

    /// Failure policy for member errors.
    #[arg(long = "failure-policy", value_name = "POLICY", default_value_t = BcastFailurePolicy::Stop, value_enum)]
    pub failure_policy: BcastFailurePolicy,
}

/// Member execution strategy for `missive bcast`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BcastExecution {
    /// Send to one member at a time in rank order.
    Sequential,
    /// Resolve members first, then perform outbound A2A sends in parallel threads.
    Concurrent,
}

impl BcastExecution {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Sequential => "sequential",
            Self::Concurrent => "concurrent",
        }
    }
}

impl std::fmt::Display for BcastExecution {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Failure handling for `missive bcast`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BcastFailurePolicy {
    /// Stop after the first member failure in sequential mode and return a non-zero exit.
    Stop,
    /// Continue sending to later members and report partial failure in the summary.
    Continue,
}

impl BcastFailurePolicy {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Continue => "continue",
        }
    }
}

impl std::fmt::Display for BcastFailurePolicy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct BcastInterfaceView {
    binding: String,
    protocol_binding: String,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tenant: Option<String>,
    protocol_version: String,
    source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct BcastRequestView {
    context_id: String,
    context_created: bool,
    part_count: usize,
    parts: Vec<MessagePartSummary>,
    local_input_bytes: u64,
    request_bytes: u64,
    accepted_output_modes: Vec<String>,
    metadata: Metadata,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct BcastMemberView {
    agent: String,
    rank: String,
    status: String,
    duration_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    selected_interface: Option<BcastInterfaceView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_shape: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct BcastOutput {
    profile: String,
    operation_id: String,
    group: String,
    execution: String,
    failure_policy: String,
    status: String,
    member_count: usize,
    success_count: usize,
    failure_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout_ms: Option<u128>,
    elapsed_ms: u128,
    request: BcastRequestView,
    members: Vec<BcastMemberView>,
    message: String,
}

#[derive(Debug)]
struct BcastOperation {
    operation_id: String,
    group: GroupRecord,
    members: Vec<GroupMemberRecord>,
    context_id: ContextId,
    context_created: bool,
    template: PreparedSend,
    service_parameters: ServiceParameters,
    deadline: Option<Instant>,
    timeout: Option<Duration>,
    started_at: Instant,
}

#[derive(Debug, Clone)]
struct PlannedMember {
    member: GroupMemberRecord,
    agent: AgentRecord,
    auth_headers: AuthHeaders,
    selected_interface: NegotiatedInterface,
    prepared: PreparedSend,
    service_parameters: ServiceParameters,
}

#[derive(Debug)]
struct MemberSendOutcome {
    index: usize,
    plan: PlannedMember,
    duration: Duration,
    outcome: Result<SendMessageOutcome>,
}

/// Executes `missive bcast`.
pub(crate) fn execute_bcast_command<R, W>(
    args: &BcastArgs,
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
    let span = tracing::debug_span!(
        target: "missive_cli",
        "collective.operation",
        collective = "bcast",
        group = %args.group,
        execution = %args.execution.as_str(),
        failure_policy = %args.failure_policy.as_str(),
    );
    let _span_guard = span.enter();
    tracing::debug!(
        target: "missive_cli",
        collective = "bcast",
        group = %args.group,
        execution = %args.execution.as_str(),
        failure_policy = %args.failure_policy.as_str(),
        "collective operation started"
    );
    let mut operation = prepare_bcast_operation(args, globals, loaded_config, input)?;
    tracing::debug!(
        target: "missive_cli",
        collective = "bcast",
        operation_id = %operation.operation_id,
        context_id = %operation.context_id.as_str(),
        protocol_version = %operation.service_parameters.protocol_version,
        "broadcast operation prepared"
    );
    let mut registry = open_agent_registry(loaded_config, environment)?;
    load_bcast_group(&mut operation, &mut registry.store)?;
    append_bcast_start_event(&registry.store, args, &operation)?;

    let mut members = match args.execution {
        BcastExecution::Sequential => run_bcast_sequential(
            args,
            globals,
            environment,
            &mut registry.store,
            &mut operation,
        )?,
        BcastExecution::Concurrent => {
            run_bcast_concurrent(globals, environment, &mut registry.store, &mut operation)?
        }
    };

    if operation
        .deadline
        .is_some_and(|deadline| Instant::now() >= deadline)
        && !members.iter().any(|member| member.status == "timeout")
        && members.iter().any(|member| member.status != "succeeded")
    {
        mark_last_failure_as_timeout(&mut members);
    }

    let output = finalize_bcast_output(
        registry.profile,
        args.execution,
        args.failure_policy,
        &operation,
        members,
    );
    append_bcast_completed_event(&registry.store, &output)?;
    tracing::debug!(
        target: "missive_cli",
        collective = "bcast",
        operation_id = %output.operation_id,
        status = %output.status,
        member_count = output.member_count,
        success_count = output.success_count,
        failure_count = output.failure_count,
        elapsed_ms = output.elapsed_ms,
        "collective operation completed"
    );
    render_success(writer, mode, "bcast_result", &output, &output.message)?;

    if output.status == "succeeded"
        || (output.status == "partial_failure"
            && args.failure_policy == BcastFailurePolicy::Continue)
    {
        Ok(())
    } else if output.status == "timeout" {
        Err(MissiveError::orchestration(format!(
            "broadcast to group {:?} timed out after {} ms",
            output.group,
            output.timeout_ms.unwrap_or_default()
        ))
        .with_help("Increase --timeout, use --failure-policy continue for best-effort sends, or retry failed members individually.")
        .with_exit_code(MissiveExitCode::TaskTimeout))
    } else {
        Err(MissiveError::orchestration(format!(
            "broadcast to group {:?} finished with {} failed member(s)",
            output.group, output.failure_count
        ))
        .with_help("Inspect the bcast_result summary, fix failed member agents, or use --failure-policy continue for best-effort broadcasts."))
    }
}

fn prepare_bcast_operation<R>(
    args: &BcastArgs,
    globals: &GlobalArgs,
    loaded_config: &LoadedConfig,
    input: &mut R,
) -> Result<BcastOperation>
where
    R: Read,
{
    let group = GroupRecord {
        group_name: GroupName::new(args.group.clone())?,
        routing_policy: String::new(),
        notes: None,
        metadata: Metadata::new(),
        created_at: missive_core::MissiveTimestamp::now_utc(),
        updated_at: missive_core::MissiveTimestamp::now_utc(),
    };
    let context_id = match args.context.as_deref() {
        Some(value) => ContextId::new(value.to_owned())?,
        None => ContextId::new(missive_a2a::protocol::new_context_id())?,
    };
    let service_parameters = service_parameters_from_config_and_globals(loaded_config, globals)?;
    let max_request_bytes = message_part_limit_bytes(loaded_config)?;
    let send_args = args.to_send_args(&context_id);
    let template = prepare_send_request(&send_args, &service_parameters, max_request_bytes, input)?;
    let timeout = globals
        .timeout
        .as_deref()
        .map(|value| parse_duration_arg("--timeout", value))
        .transpose()?;
    let started_at = Instant::now();
    let deadline = timeout.map(|timeout| started_at + timeout);

    Ok(BcastOperation {
        operation_id: format!("bcast/{}", missive_a2a::protocol::new_message_id()),
        group,
        members: Vec::new(),
        context_id,
        context_created: false,
        template,
        service_parameters,
        deadline,
        timeout,
        started_at,
    })
}

impl BcastArgs {
    fn to_send_args(&self, context_id: &ContextId) -> crate::send::SendArgs {
        crate::send::SendArgs {
            agent: "bcast-template".to_owned(),
            message: self.message.clone(),
            stdin: self.stdin,
            files: self.files.clone(),
            file_bytes: self.file_bytes.clone(),
            json_parts: self.json_parts.clone(),
            mime: self.mime.clone(),
            parts: self.parts.clone(),
            metadata: self.metadata.clone(),
            context: Some(context_id.as_str().to_owned()),
            task: None,
            accepted_output_modes: self.accepted_output_modes.clone(),
        }
    }
}

fn load_bcast_group(operation: &mut BcastOperation, store: &mut Store) -> Result<()> {
    let group = store
        .get_group(&operation.group.group_name)?
        .ok_or_else(|| {
            MissiveError::validation(format!(
                "group {:?} does not exist",
                operation.group.group_name.as_str()
            ))
            .with_help("Run 'missive group list' to see locally known groups.")
        })?;
    let members = store.list_group_members(&group.group_name)?;
    if members.is_empty() {
        return Err(MissiveError::validation(format!(
            "group {:?} has no members to broadcast to",
            group.group_name.as_str()
        ))
        .with_help("Add members with 'missive group add <group> <agent> --rank <rank>' before running bcast."));
    }

    operation.context_created = ensure_bcast_context(store, &operation.context_id, &group)?;
    operation.group = group;
    operation.members = members;
    Ok(())
}

fn ensure_bcast_context(
    store: &Store,
    context_id: &ContextId,
    group: &GroupRecord,
) -> Result<bool> {
    if store.get_context(context_id)?.is_some() {
        return Ok(false);
    }
    let mut context = ContextUpsert::new(context_id.clone());
    context.summary = Some(format!(
        "Broadcast collective for group {}",
        group.group_name.as_str()
    ));
    context.metadata.insert_str("missive.collective", "bcast")?;
    context
        .metadata
        .insert_str("missive.group", group.group_name.as_str())?;
    store.upsert_context(&context)?;
    Ok(true)
}

fn run_bcast_sequential(
    args: &BcastArgs,
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
    store: &mut Store,
    operation: &mut BcastOperation,
) -> Result<Vec<BcastMemberView>> {
    let mut results = Vec::with_capacity(operation.members.len());
    for (index, member) in operation.members.clone().into_iter().enumerate() {
        if let Some(timeout) = timeout_member_before_start(index, &member, operation.deadline) {
            append_bcast_member_event(
                store,
                &operation.group.group_name,
                &operation.context_id,
                &timeout,
            )?;
            results.push(timeout);
            if args.failure_policy == BcastFailurePolicy::Stop {
                break;
            }
            continue;
        }

        tracing::debug!(
            target: "missive_cli",
            collective = "bcast",
            operation_id = %operation.operation_id,
            member_index = index,
            agent = %member.agent_alias.as_str(),
            rank = %member.rank_name.as_str(),
            "broadcast member planning started"
        );
        let plan = match plan_member_send(globals, environment, store, operation, member.clone()) {
            Ok(plan) => plan,
            Err(error) => {
                let result = failed_member_view(index, &member, Duration::ZERO, error, false);
                append_bcast_member_event(
                    store,
                    &operation.group.group_name,
                    &operation.context_id,
                    &result,
                )?;
                results.push(result);
                if args.failure_policy == BcastFailurePolicy::Stop {
                    break;
                }
                continue;
            }
        };

        let started = Instant::now();
        tracing::debug!(
            target: "missive_cli",
            collective = "bcast",
            operation_id = %operation.operation_id,
            member_index = index,
            agent = %plan.agent.alias.as_str(),
            binding = %plan.selected_interface.binding,
            request_message_id = %plan.prepared.request_message_id.as_str(),
            "broadcast member send started"
        );
        let outcome = send_planned_member(&plan, remaining_or_default(operation.deadline));
        let duration = started.elapsed();
        let result = persist_member_result(
            store,
            operation,
            index,
            plan,
            duration,
            outcome,
            operation
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline),
        )?;
        tracing::debug!(
            target: "missive_cli",
            collective = "bcast",
            operation_id = %operation.operation_id,
            member_index = index,
            agent = %result.agent,
            status = %result.status,
            task_id = %result.task_id.as_deref().unwrap_or("-"),
            state = %result.state.as_deref().unwrap_or("-"),
            duration_ms = result.duration_ms,
            "broadcast member completed"
        );
        let succeeded = result.status == "succeeded";
        append_bcast_member_event(
            store,
            &operation.group.group_name,
            &operation.context_id,
            &result,
        )?;
        results.push(result);
        if !succeeded && args.failure_policy == BcastFailurePolicy::Stop {
            break;
        }
    }

    Ok(results)
}

fn run_bcast_concurrent(
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
    store: &mut Store,
    operation: &mut BcastOperation,
) -> Result<Vec<BcastMemberView>> {
    let mut planned = Vec::new();
    let mut early_results = Vec::new();
    for (index, member) in operation.members.clone().into_iter().enumerate() {
        if let Some(timeout) = timeout_member_before_start(index, &member, operation.deadline) {
            append_bcast_member_event(
                store,
                &operation.group.group_name,
                &operation.context_id,
                &timeout,
            )?;
            early_results.push((index, timeout));
            continue;
        }
        match plan_member_send(globals, environment, store, operation, member.clone()) {
            Ok(plan) => planned.push((index, plan)),
            Err(error) => {
                let result = failed_member_view(index, &member, Duration::ZERO, error, false);
                append_bcast_member_event(
                    store,
                    &operation.group.group_name,
                    &operation.context_id,
                    &result,
                )?;
                early_results.push((index, result));
            }
        }
    }

    let timeout = remaining_or_default(operation.deadline);
    let handles = planned
        .into_iter()
        .map(|(index, plan)| {
            thread::spawn(move || {
                let started = Instant::now();
                let outcome = send_planned_member(&plan, timeout);
                MemberSendOutcome {
                    index,
                    plan,
                    duration: started.elapsed(),
                    outcome,
                }
            })
        })
        .collect::<Vec<_>>();

    let mut results = early_results;
    for handle in handles {
        let member = handle.join().map_err(|_| {
            MissiveError::orchestration("broadcast worker thread panicked")
                .with_help("Retry the broadcast; if this repeats, file a bug with the command arguments and logs.")
        })?;
        let timed_out = operation
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline);
        let result = persist_member_result(
            store,
            operation,
            member.index,
            member.plan,
            member.duration,
            member.outcome,
            timed_out,
        )?;
        append_bcast_member_event(
            store,
            &operation.group.group_name,
            &operation.context_id,
            &result,
        )?;
        results.push((member.index, result));
    }

    results.sort_by_key(|(index, _)| *index);
    Ok(results.into_iter().map(|(_, result)| result).collect())
}

fn plan_member_send(
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
    store: &Store,
    operation: &BcastOperation,
    member: GroupMemberRecord,
) -> Result<PlannedMember> {
    let agent = get_existing_agent(store, &member.agent_alias)?;
    let auth_headers = auth_headers_for_agent(store, &agent, globals, environment)?;
    let (agent, selected_interface) = resolve_send_interface_with_store_timeout(
        store,
        agent,
        &operation.service_parameters,
        &auth_headers,
        operation.deadline.map(remaining_or_timeout),
    )?;
    let prepared = clone_prepared_with_new_message_id(&operation.template)?;
    Ok(PlannedMember {
        member,
        agent,
        auth_headers,
        selected_interface,
        prepared,
        service_parameters: operation.service_parameters.clone(),
    })
}

fn send_planned_member(plan: &PlannedMember, timeout: Duration) -> Result<SendMessageOutcome> {
    let client = SendMessageClient::with_timeout(timeout)?;
    client.send_message(
        &plan.selected_interface,
        &plan.prepared.request,
        &plan.service_parameters,
        &plan.auth_headers,
    )
}

fn persist_member_result(
    store: &mut Store,
    operation: &BcastOperation,
    index: usize,
    plan: PlannedMember,
    duration: Duration,
    outcome: Result<SendMessageOutcome>,
    timed_out: bool,
) -> Result<BcastMemberView> {
    let result = match outcome {
        Ok(outcome) => {
            let persisted = persist_send(
                store,
                &plan.agent,
                &plan.prepared,
                &outcome,
                &operation.service_parameters,
            )?;
            success_member_view(index, &plan, &outcome, &persisted, duration)
        }
        Err(error) => failed_member_view(index, &plan.member, duration, error, timed_out),
    };
    tracing::debug!(
        target: "missive_cli",
        collective = "bcast",
        operation_id = %operation.operation_id,
        member_index = index,
        agent = %result.agent,
        status = %result.status,
        task_id = %result.task_id.as_deref().unwrap_or("-"),
        state = %result.state.as_deref().unwrap_or("-"),
        duration_ms = result.duration_ms,
        "broadcast member persisted"
    );
    Ok(result)
}

fn timeout_member_before_start(
    index: usize,
    member: &GroupMemberRecord,
    deadline: Option<Instant>,
) -> Option<BcastMemberView> {
    deadline.and_then(|deadline| {
        (Instant::now() >= deadline).then(|| {
            failed_member_view(
                index,
                member,
                Duration::ZERO,
                MissiveError::orchestration("broadcast timeout reached before member send started")
                    .with_help("Increase --timeout or reduce group size."),
                true,
            )
        })
    })
}

fn remaining_or_default(deadline: Option<Instant>) -> Duration {
    match deadline {
        Some(deadline) => remaining_or_timeout(deadline),
        None => DEFAULT_MEMBER_SEND_TIMEOUT,
    }
}

fn remaining_or_timeout(deadline: Instant) -> Duration {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .unwrap_or_else(|| Duration::from_millis(1))
}

fn success_member_view(
    _index: usize,
    plan: &PlannedMember,
    outcome: &SendMessageOutcome,
    persisted: &crate::send::PersistedSend,
    duration: Duration,
) -> BcastMemberView {
    let response = response_summary(outcome);
    BcastMemberView {
        agent: plan.member.agent_alias.as_str().to_owned(),
        rank: plan.member.rank_name.as_str().to_owned(),
        status: "succeeded".to_owned(),
        duration_ms: duration.as_millis(),
        request_message_id: Some(plan.prepared.request_message_id.as_str().to_owned()),
        selected_interface: Some(BcastInterfaceView::from(&outcome.interface)),
        response_shape: Some(response.shape),
        response_message_id: response.message_id,
        task_id: persisted
            .task_id
            .as_ref()
            .map(|task_id| task_id.as_str().to_owned())
            .or(response.task_id),
        context_id: persisted
            .context_id
            .as_ref()
            .map(|context_id| context_id.as_str().to_owned())
            .or(response.context_id),
        state: response.state,
        error: None,
    }
}

fn failed_member_view(
    _index: usize,
    member: &GroupMemberRecord,
    duration: Duration,
    error: MissiveError,
    timed_out: bool,
) -> BcastMemberView {
    BcastMemberView {
        agent: member.agent_alias.as_str().to_owned(),
        rank: member.rank_name.as_str().to_owned(),
        status: if timed_out { "timeout" } else { "failed" }.to_owned(),
        duration_ms: duration.as_millis(),
        request_message_id: None,
        selected_interface: None,
        response_shape: None,
        response_message_id: None,
        task_id: None,
        context_id: None,
        state: None,
        error: Some(error.to_report()),
    }
}

fn mark_last_failure_as_timeout(members: &mut [BcastMemberView]) {
    if let Some(member) = members
        .iter_mut()
        .rev()
        .find(|member| member.status == "failed")
    {
        member.status = "timeout".to_owned();
    }
}

#[derive(Debug)]
struct ResponseSummary {
    shape: String,
    message_id: Option<String>,
    task_id: Option<String>,
    context_id: Option<String>,
    state: Option<String>,
}

fn response_summary(outcome: &SendMessageOutcome) -> ResponseSummary {
    match &outcome.response {
        SendMessageResponse::Message(message) => ResponseSummary {
            shape: "message".to_owned(),
            message_id: Some(message.message_id.clone()),
            task_id: message.task_id.clone(),
            context_id: message.context_id.clone(),
            state: None,
        },
        SendMessageResponse::Task(task) => ResponseSummary {
            shape: "task".to_owned(),
            message_id: task
                .status
                .message
                .as_ref()
                .map(|message| message.message_id.clone()),
            task_id: Some(task.id.clone()),
            context_id: Some(task.context_id.clone()),
            state: Some(store_task_state(&task.status.state).as_str().to_owned()),
        },
    }
}

fn finalize_bcast_output(
    profile: String,
    execution: BcastExecution,
    failure_policy: BcastFailurePolicy,
    operation: &BcastOperation,
    members: Vec<BcastMemberView>,
) -> BcastOutput {
    let success_count = members
        .iter()
        .filter(|member| member.status == "succeeded")
        .count();
    let failure_count = members.len().saturating_sub(success_count);
    let timed_out = members.iter().any(|member| member.status == "timeout");
    let status = if timed_out {
        "timeout"
    } else if failure_count == 0 && members.len() == operation.members.len() {
        "succeeded"
    } else if success_count > 0 {
        "partial_failure"
    } else {
        "failed"
    }
    .to_owned();
    let message = match status.as_str() {
        "succeeded" => format!(
            "Broadcast to group '{}' succeeded for {} member(s)",
            operation.group.group_name.as_str(),
            success_count
        ),
        "partial_failure" => format!(
            "Broadcast to group '{}' reached {} member(s) with {} failure(s)",
            operation.group.group_name.as_str(),
            success_count,
            failure_count
        ),
        "timeout" => format!(
            "Broadcast to group '{}' timed out after reaching {} member(s)",
            operation.group.group_name.as_str(),
            success_count
        ),
        _ => format!(
            "Broadcast to group '{}' failed for {} member(s)",
            operation.group.group_name.as_str(),
            failure_count
        ),
    };

    BcastOutput {
        profile,
        operation_id: operation.operation_id.clone(),
        group: operation.group.group_name.as_str().to_owned(),
        execution: execution.as_str().to_owned(),
        failure_policy: failure_policy.as_str().to_owned(),
        status,
        member_count: operation.members.len(),
        success_count,
        failure_count,
        timeout_ms: operation.timeout.map(|timeout| timeout.as_millis()),
        elapsed_ms: operation.started_at.elapsed().as_millis(),
        request: BcastRequestView {
            context_id: operation.context_id.as_str().to_owned(),
            context_created: operation.context_created,
            part_count: operation.template.request.message.parts.len(),
            parts: operation.template.part_summaries.clone(),
            local_input_bytes: operation.template.local_input_bytes,
            request_bytes: operation.template.request_bytes,
            accepted_output_modes: operation.template.accepted_output_modes.clone(),
            metadata: operation.template.local_metadata.clone(),
        },
        members,
        message,
    }
}

fn append_bcast_start_event(
    store: &Store,
    args: &BcastArgs,
    operation: &BcastOperation,
) -> Result<()> {
    let mut event = new_cli_event(
        "missive.bcast.started",
        json!({
            "operation_id": operation.operation_id.as_str(),
            "group": operation.group.group_name.as_str(),
            "context_id": operation.context_id.as_str(),
            "execution": args.execution.as_str(),
            "failure_policy": args.failure_policy.as_str(),
            "member_count": operation.members.len(),
            "request": {
                "part_count": operation.template.request.message.parts.len(),
                "parts": operation.template.part_summaries.clone(),
                "local_input_bytes": operation.template.local_input_bytes,
                "request_bytes": operation.template.request_bytes,
                "accepted_output_modes": operation.template.accepted_output_modes.clone(),
                "metadata": operation.template.local_metadata.clone(),
            }
        }),
    )?;
    event.group_name = Some(operation.group.group_name.clone());
    event.context_id = Some(operation.context_id.clone());
    event.metadata = operation.template.local_metadata.clone();
    event.record_a2a_protocol_version(operation.service_parameters.protocol_version.clone())?;
    store.append_event(&event)?;
    Ok(())
}

fn append_bcast_member_event(
    store: &Store,
    group_name: &GroupName,
    context_id: &ContextId,
    result: &BcastMemberView,
) -> Result<()> {
    let event_type = match result.status.as_str() {
        "succeeded" => "missive.bcast.member.succeeded",
        "timeout" => "missive.bcast.member.timeout",
        _ => "missive.bcast.member.failed",
    };
    let mut event = new_cli_event(event_type, json!(result))?;
    event.group_name = Some(group_name.clone());
    event.context_id = Some(context_id.clone());
    event.agent_alias = Some(AgentAlias::new(result.agent.clone())?);
    event.task_id = result
        .task_id
        .as_ref()
        .map(|task_id| missive_core::TaskId::new(task_id.clone()))
        .transpose()?;
    store.append_event(&event)?;
    Ok(())
}

fn append_bcast_completed_event(store: &Store, output: &BcastOutput) -> Result<()> {
    let mut event = new_cli_event(
        "missive.bcast.completed",
        json!({
            "operation_id": output.operation_id.as_str(),
            "group": output.group.as_str(),
            "context_id": output.request.context_id.as_str(),
            "execution": output.execution.as_str(),
            "failure_policy": output.failure_policy.as_str(),
            "status": output.status.as_str(),
            "member_count": output.member_count,
            "success_count": output.success_count,
            "failure_count": output.failure_count,
            "timeout_ms": output.timeout_ms,
            "elapsed_ms": output.elapsed_ms,
            "members": output.members.clone(),
        }),
    )?;
    event.group_name = Some(GroupName::new(output.group.clone())?);
    event.context_id = Some(ContextId::new(output.request.context_id.clone())?);
    store.append_event(&event)?;
    Ok(())
}

impl From<&NegotiatedInterface> for BcastInterfaceView {
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
    fn bcast_duration_parser_requires_suffix_and_positive_value() {
        assert_eq!(
            parse_duration_arg("--timeout", "250ms").expect("duration"),
            Duration::from_millis(250)
        );
        assert_eq!(
            parse_duration_arg("--timeout", "2s").expect("duration"),
            Duration::from_secs(2)
        );
        assert!(parse_duration_arg("--timeout", "0s").is_err());
        assert!(parse_duration_arg("--timeout", "2").is_err());
    }
}
