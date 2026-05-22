//! Barrier collective command implementation.
//!
//! `missive barrier` waits for the tasks associated with every member of a
//! local group and one shared context to reach terminal states, or explicitly
//! requested states. It can discover task ids from the local store or consume a
//! prior `missive bcast --json` result so the broadcast/barrier pair is usable
//! from shell automation.

use std::cmp;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::thread;
use std::time::{Duration, Instant};

use clap::{ArgAction, Args, ValueEnum};
use missive_a2a::{
    NegotiatedInterface, ServiceParameters, TaskClient,
    protocol::{GetTaskRequest, Task},
};
use missive_core::{
    AgentAlias, ContextId, ErrorReport, GroupName, LoadedConfig, MessageId, MissiveError,
    MissiveExitCode, MissiveTimestamp, Result, TaskId,
};
use missive_store::{
    AgentRecord, ContextUpsert, GroupMemberRecord, GroupRecord, Store, StoreTransaction,
    TaskRecord, TaskSource, TaskState, TaskUpsert,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::agent::{AgentRegistry, get_existing_agent, open_agent_registry};
use crate::artifact::persist_task_artifacts;
use crate::auth::auth_headers_for_agent;
use crate::events::new_cli_event;
use crate::output::{OutputMode, redact_text, render_success};
use crate::send::{resolve_send_interface, store_task_state};
use crate::{GlobalArgs, service_parameters_from_config_and_globals};

const DEFAULT_BARRIER_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_BARRIER_INTERVAL: Duration = Duration::from_secs(1);
const MAX_BARRIER_INTERVAL: Duration = Duration::from_secs(5);

/// Arguments for `missive barrier`.
#[derive(Debug, Clone, Args)]
pub struct BarrierArgs {
    /// Local group name whose member tasks should be synchronized.
    pub group: String,

    /// A2A context id shared by the member tasks.
    #[arg(
        long = "context",
        value_name = "CONTEXT_ID",
        required_unless_present = "from_bcast"
    )]
    pub context: Option<String>,

    /// Read a previous `missive bcast --json` result from this path, or '-' for stdin.
    #[arg(long = "from-bcast", value_name = "PATH_OR_-")]
    pub from_bcast: Option<String>,

    /// State that counts as satisfying the barrier; repeatable.
    ///
    /// When omitted, the barrier waits for terminal states and succeeds only for
    /// completed tasks. Failed or cancelled member tasks produce deterministic
    /// non-zero exits unless a smaller quorum has already succeeded.
    #[arg(long = "state", value_name = "STATE", action = ArgAction::Append, value_parser = parse_barrier_state_arg)]
    pub states: Vec<TaskState>,

    /// Number of member tasks that must satisfy the target states; defaults to every member.
    #[arg(long = "required", value_name = "N")]
    pub required: Option<usize>,

    /// Failure handling for member task failures or cancellations.
    #[arg(long = "failure-policy", value_name = "POLICY", default_value_t = BarrierFailurePolicy::Stop, value_enum)]
    pub failure_policy: BarrierFailurePolicy,

    /// Poll only local SQLite task rows instead of refreshing remote A2A tasks.
    #[arg(long = "local", action = ArgAction::SetTrue)]
    pub local: bool,

    /// Poll interval such as 500ms, 2s, or 1m.
    #[arg(long = "interval", value_name = "DURATION")]
    pub interval: Option<String>,

    /// Request this many history messages from remote A2A GetTask calls.
    #[arg(long = "history-length", value_name = "N")]
    pub history_length: Option<i32>,
}

/// Failure handling for `missive barrier`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BarrierFailurePolicy {
    /// Stop as soon as a non-requested failed/cancelled terminal state is observed.
    Stop,
    /// Continue polling until the requested quorum is reached, impossible, or timed out.
    Continue,
}

impl BarrierFailurePolicy {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Continue => "continue",
        }
    }
}

impl std::fmt::Display for BarrierFailurePolicy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BcastReference {
    group: Option<String>,
    context_id: Option<String>,
    task_ids_by_agent: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
struct BarrierTargets {
    explicit: bool,
    target_states: Vec<TaskState>,
    success_states: Vec<TaskState>,
}

impl BarrierTargets {
    fn from_args(states: &[TaskState]) -> Self {
        let mut unique = BTreeSet::new();
        let explicit_states = states
            .iter()
            .copied()
            .filter(|state| unique.insert(state.as_str().to_owned()))
            .collect::<Vec<_>>();
        if explicit_states.is_empty() {
            Self {
                explicit: false,
                target_states: vec![
                    TaskState::Completed,
                    TaskState::Failed,
                    TaskState::Cancelled,
                ],
                success_states: vec![TaskState::Completed],
            }
        } else {
            Self {
                explicit: true,
                target_states: explicit_states.clone(),
                success_states: explicit_states,
            }
        }
    }

    fn target_names(&self) -> Vec<String> {
        self.target_states
            .iter()
            .map(|state| state.as_str().to_owned())
            .collect()
    }

    fn success_names(&self) -> Vec<String> {
        self.success_states
            .iter()
            .map(|state| state.as_str().to_owned())
            .collect()
    }

    fn is_success(&self, state: TaskState) -> bool {
        self.success_states.contains(&state)
    }

    fn is_requested(&self, state: TaskState) -> bool {
        self.target_states.contains(&state)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct BarrierInterfaceView {
    binding: String,
    protocol_binding: String,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tenant: Option<String>,
    protocol_version: String,
    source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct BarrierMemberView {
    agent: String,
    rank: String,
    status: String,
    attempts: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    selected_interface: Option<BarrierInterfaceView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct BarrierOutput {
    profile: String,
    operation_id: String,
    group: String,
    context_id: String,
    status: String,
    failure_policy: String,
    local: bool,
    from_bcast: bool,
    attempts: u64,
    member_count: usize,
    required: usize,
    reached_count: usize,
    failure_count: usize,
    cancellation_count: usize,
    pending_count: usize,
    target_states: Vec<String>,
    success_states: Vec<String>,
    timeout_ms: u128,
    interval_ms: u128,
    elapsed_ms: u128,
    members: Vec<BarrierMemberView>,
    message: String,
}

#[derive(Debug)]
struct BarrierOperation {
    operation_id: String,
    group: GroupRecord,
    members: Vec<GroupMemberRecord>,
    context_id: ContextId,
    targets: BarrierTargets,
    required: usize,
    timeout: Duration,
    interval: Duration,
    failure_policy: BarrierFailurePolicy,
    local: bool,
    from_bcast: bool,
    history_length: Option<i32>,
    service_parameters: ServiceParameters,
    started_at: Instant,
}

impl BarrierOperation {
    fn deadline(&self) -> Instant {
        self.started_at + self.timeout
    }
}

#[derive(Debug, Clone)]
struct BarrierMemberState {
    member: GroupMemberRecord,
    task_id: Option<TaskId>,
    context_id: Option<ContextId>,
    state: Option<TaskState>,
    source: Option<TaskSource>,
    updated_at: Option<MissiveTimestamp>,
    status: MemberBarrierStatus,
    attempts: u64,
    selected_interface: Option<NegotiatedInterface>,
    error: Option<ErrorReport>,
}

impl BarrierMemberState {
    fn new(member: GroupMemberRecord, task_id: Option<TaskId>) -> Self {
        Self {
            member,
            task_id,
            context_id: None,
            state: None,
            source: None,
            updated_at: None,
            status: MemberBarrierStatus::Pending,
            attempts: 0,
            selected_interface: None,
            error: None,
        }
    }

    fn apply_record(&mut self, record: TaskRecord, targets: &BarrierTargets) {
        self.task_id = Some(record.task_id.clone());
        self.context_id = record.context_id.clone();
        self.state = Some(record.state);
        self.source = Some(record.source);
        self.updated_at = Some(record.updated_at);
        self.error = None;
        self.status = classify_record_state(record.state, targets);
    }

    fn mark_missing(&mut self) {
        self.state = None;
        self.source = None;
        self.updated_at = None;
        self.status = MemberBarrierStatus::Missing;
        self.error = None;
    }

    fn mark_error(&mut self, error: MissiveError) {
        self.status = MemberBarrierStatus::Error;
        self.error = Some(error.to_report());
    }

    fn view(&self) -> BarrierMemberView {
        BarrierMemberView {
            agent: self.member.agent_alias.as_str().to_owned(),
            rank: self.member.rank_name.as_str().to_owned(),
            status: self.status.as_str().to_owned(),
            attempts: self.attempts,
            task_id: self
                .task_id
                .as_ref()
                .map(|task_id| task_id.as_str().to_owned()),
            context_id: self
                .context_id
                .as_ref()
                .map(|context_id| context_id.as_str().to_owned()),
            state: self.state.map(|state| state.as_str().to_owned()),
            source: self.source.map(|source| source.as_str().to_owned()),
            updated_at: self.updated_at.map(MissiveTimestamp::to_rfc3339),
            selected_interface: self
                .selected_interface
                .as_ref()
                .map(BarrierInterfaceView::from),
            error: self.error.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemberBarrierStatus {
    Satisfied,
    Pending,
    Failed,
    Cancelled,
    Missing,
    Error,
    TerminalUnmatched,
}

impl MemberBarrierStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Satisfied => "satisfied",
            Self::Pending => "pending",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Missing => "missing",
            Self::Error => "error",
            Self::TerminalUnmatched => "terminal_unmatched",
        }
    }

    const fn is_success(self) -> bool {
        matches!(self, Self::Satisfied)
    }

    const fn is_failure(self) -> bool {
        matches!(self, Self::Failed | Self::Error | Self::TerminalUnmatched)
    }

    const fn is_cancellation(self) -> bool {
        matches!(self, Self::Cancelled)
    }

    const fn is_pending(self) -> bool {
        matches!(self, Self::Pending | Self::Missing)
    }

    const fn is_disqualified(self) -> bool {
        matches!(
            self,
            Self::Failed | Self::Cancelled | Self::Error | Self::TerminalUnmatched
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BarrierDecision {
    Continue,
    Succeeded,
    Failed,
    Cancelled,
    Timeout,
}

/// Executes `missive barrier`.
pub(crate) fn execute_barrier_command<R, W>(
    args: &BarrierArgs,
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
    let reference = read_bcast_reference(args, input)?;
    let service_parameters = service_parameters_from_config_and_globals(loaded_config, globals)?;
    let mut registry = open_agent_registry(loaded_config, environment)?;
    let operation = prepare_barrier_operation(
        args,
        globals,
        &mut registry.store,
        reference.as_ref(),
        service_parameters,
    )?;
    append_barrier_started_event(&registry.store, &operation)?;

    let mut members = build_member_states(&registry.store, &operation, reference.as_ref())?;
    let mut attempts = 0_u64;

    loop {
        attempts += 1;
        for member in &mut members {
            refresh_barrier_member(member, &operation, &mut registry, globals, environment);
        }

        let decision = evaluate_barrier(&operation, &members);
        if !matches!(decision, BarrierDecision::Continue) {
            let output = finalize_barrier_output(
                registry.profile.clone(),
                &operation,
                &members,
                attempts,
                decision,
            );
            append_barrier_member_events(&registry.store, &operation, &output)?;
            append_barrier_completed_event(&registry.store, &output)?;
            render_barrier_success(writer, mode, &output)?;
            return barrier_exit(&output);
        }

        let elapsed = operation.started_at.elapsed();
        let remaining = operation
            .timeout
            .checked_sub(elapsed)
            .unwrap_or_else(|| Duration::from_millis(1));
        thread::sleep(cmp::min(operation.interval, remaining));
    }
}

fn prepare_barrier_operation(
    args: &BarrierArgs,
    globals: &GlobalArgs,
    store: &mut Store,
    reference: Option<&BcastReference>,
    service_parameters: ServiceParameters,
) -> Result<BarrierOperation> {
    validate_non_negative_i32("--history-length", args.history_length)?;
    if let Some(reference_group) = reference.and_then(|reference| reference.group.as_deref())
        && reference_group != args.group
    {
        return Err(MissiveError::validation(format!(
            "--from-bcast result is for group {reference_group:?}, not {:?}",
            args.group
        )));
    }

    let context_value = match (
        &args.context,
        reference.and_then(|reference| reference.context_id.as_ref()),
    ) {
        (Some(explicit), Some(from_bcast)) if explicit != from_bcast => {
            return Err(MissiveError::validation(format!(
                "--context {explicit:?} does not match --from-bcast context {from_bcast:?}"
            ))
            .with_help("Use the same context id as the broadcast result, or omit --context when using --from-bcast."));
        }
        (Some(explicit), _) => explicit.clone(),
        (None, Some(from_bcast)) => from_bcast.clone(),
        (None, None) => {
            return Err(MissiveError::validation(
                "missive barrier requires --context <id> unless --from-bcast provides one",
            ));
        }
    };
    let context_id = ContextId::new(context_value)?;
    let group_name = GroupName::new(args.group.clone())?;
    let group = store.get_group(&group_name)?.ok_or_else(|| {
        MissiveError::validation(format!("group {:?} does not exist", group_name.as_str()))
            .with_help("Run 'missive group list' to see locally known groups.")
    })?;
    let members = store.list_group_members(&group.group_name)?;
    if members.is_empty() {
        return Err(MissiveError::validation(format!(
            "group {:?} has no members for barrier synchronization",
            group.group_name.as_str()
        ))
        .with_help("Add members with 'missive group add <group> <agent> --rank <rank>' before running barrier."));
    }
    let required = args.required.unwrap_or(members.len());
    if required == 0 || required > members.len() {
        return Err(MissiveError::validation(format!(
            "--required must be between 1 and the group member count ({})",
            members.len()
        )));
    }
    let timeout = match globals.timeout.as_deref() {
        Some(value) => parse_duration_arg("--timeout", value)?,
        None => DEFAULT_BARRIER_TIMEOUT,
    };
    let interval = match args.interval.as_deref() {
        Some(value) => parse_duration_arg("--interval", value)?,
        None => DEFAULT_BARRIER_INTERVAL,
    };

    Ok(BarrierOperation {
        operation_id: format!("barrier/{}", missive_a2a::protocol::new_message_id()),
        group,
        members,
        context_id,
        targets: BarrierTargets::from_args(&args.states),
        required,
        timeout,
        interval: cmp::min(interval, MAX_BARRIER_INTERVAL),
        failure_policy: args.failure_policy,
        local: args.local,
        from_bcast: reference.is_some(),
        history_length: args.history_length,
        service_parameters,
        started_at: Instant::now(),
    })
}

fn build_member_states(
    store: &Store,
    operation: &BarrierOperation,
    reference: Option<&BcastReference>,
) -> Result<Vec<BarrierMemberState>> {
    operation
        .members
        .iter()
        .cloned()
        .map(|member| {
            let referenced_task_id = reference
                .and_then(|reference| {
                    reference
                        .task_ids_by_agent
                        .get(member.agent_alias.as_str())
                        .cloned()
                })
                .map(TaskId::new)
                .transpose()?;
            let task_id = match referenced_task_id {
                Some(task_id) => Some(task_id),
                None => latest_member_task(store, &member.agent_alias, &operation.context_id)?
                    .map(|record| record.task_id),
            };
            Ok(BarrierMemberState::new(member, task_id))
        })
        .collect()
}

fn refresh_barrier_member(
    member: &mut BarrierMemberState,
    operation: &BarrierOperation,
    registry: &mut AgentRegistry,
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
) {
    member.attempts = member.attempts.saturating_add(1);
    if member.status.is_success() || member.status.is_disqualified() {
        return;
    }

    let result = if operation.local {
        refresh_local_member(
            member,
            &registry.store,
            &operation.context_id,
            &operation.targets,
        )
    } else {
        refresh_remote_member(member, operation, registry, globals, environment)
    };

    if let Err(error) = result {
        member.mark_error(error);
    }
}

fn refresh_local_member(
    member: &mut BarrierMemberState,
    store: &Store,
    context_id: &ContextId,
    targets: &BarrierTargets,
) -> Result<()> {
    let task_id = match &member.task_id {
        Some(task_id) => task_id.clone(),
        None => match latest_member_task(store, &member.member.agent_alias, context_id)? {
            Some(record) => {
                member.apply_record(record, targets);
                return Ok(());
            }
            None => {
                member.mark_missing();
                return Ok(());
            }
        },
    };
    match store.get_task(&task_id)? {
        Some(record) => {
            member.apply_record(record, targets);
            Ok(())
        }
        None => {
            member.mark_missing();
            Ok(())
        }
    }
}

fn refresh_remote_member(
    member: &mut BarrierMemberState,
    operation: &BarrierOperation,
    registry: &mut AgentRegistry,
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
) -> Result<()> {
    if let Some(task_id) = &member.task_id {
        if let Some(record) = registry.store.get_task(task_id)? {
            member.apply_record(record, &operation.targets);
            if member.status.is_success() || member.status.is_disqualified() {
                return Ok(());
            }
        }
    } else if let Some(record) = latest_member_task(
        &registry.store,
        &member.member.agent_alias,
        &operation.context_id,
    )? {
        member.task_id = Some(record.task_id.clone());
        member.apply_record(record, &operation.targets);
        if member.status.is_success() || member.status.is_disqualified() {
            return Ok(());
        }
    }
    let Some(task_id) = member.task_id.clone() else {
        member.mark_missing();
        return Ok(());
    };

    let agent = get_existing_agent(&registry.store, &member.member.agent_alias)?;
    let auth_headers = auth_headers_for_agent(&registry.store, &agent, globals, environment)?;
    let (agent, selected_interface) = resolve_send_interface(
        registry,
        agent,
        &operation.service_parameters,
        &auth_headers,
    )?;
    let request = GetTaskRequest {
        id: task_id.as_str().to_owned(),
        history_length: operation.history_length,
        tenant: selected_interface.tenant.clone(),
    };
    let client = TaskClient::with_timeout(remaining_or_timeout(operation.deadline()))?;
    let outcome = client.get_task(
        &selected_interface,
        &request,
        &operation.service_parameters,
        &auth_headers,
    )?;
    let record = persist_barrier_remote_task(
        &mut registry.store,
        &agent,
        &outcome.task,
        &operation.service_parameters,
    )?;
    member.selected_interface = Some(selected_interface);
    member.apply_record(record, &operation.targets);
    Ok(())
}

fn latest_member_task(
    store: &Store,
    agent_alias: &AgentAlias,
    context_id: &ContextId,
) -> Result<Option<TaskRecord>> {
    Ok(store
        .list_tasks()?
        .into_iter()
        .filter(|record| &record.agent_alias == agent_alias)
        .filter(|record| record.context_id.as_ref() == Some(context_id))
        .max_by(|left, right| {
            left.updated_at
                .cmp(&right.updated_at)
                .then_with(|| left.task_id.as_str().cmp(right.task_id.as_str()))
        }))
}

fn persist_barrier_remote_task(
    store: &mut Store,
    agent: &AgentRecord,
    task: &Task,
    service_parameters: &ServiceParameters,
) -> Result<TaskRecord> {
    store.transaction(|transaction| {
        let task_id = TaskId::new(task.id.clone())?;
        let context_id = ContextId::new(task.context_id.clone())?;
        if transaction.get_context(&context_id)?.is_none() {
            let mut context = ContextUpsert::new(context_id.clone());
            context.agent_alias = Some(agent.alias.clone());
            transaction.upsert_context(&context)?;
        }

        let raw_task_json = serde_json::to_value(task).map_err(|error| {
            MissiveError::protocol("encoding A2A task for barrier persistence").with_source(error)
        })?;
        let existing = transaction.get_task(&task_id)?;
        let state = store_task_state(&task.status.state);
        let changed = existing.as_ref().is_none_or(|record| {
            record.state != state || record.remote_task_json.as_ref() != Some(&raw_task_json)
        });

        let mut upsert = TaskUpsert::new(task_id.clone(), agent.alias.clone(), state);
        upsert.source = TaskSource::Remote;
        upsert.context_id = Some(context_id.clone());
        upsert.remote_task_json = Some(raw_task_json.clone());
        upsert.last_message_id = task
            .status
            .message
            .as_ref()
            .map(|message| MessageId::new(message.message_id.clone()))
            .transpose()?;
        upsert.record_a2a_protocol_version(service_parameters.protocol_version.clone())?;
        if is_terminal_task_state(state) {
            upsert.completed_at = Some(MissiveTimestamp::now_utc());
        }
        let record = transaction.upsert_task(&upsert)?;
        persist_task_artifacts(transaction, task)?;
        if changed {
            append_barrier_task_updated_event(
                transaction,
                agent,
                &record,
                raw_task_json,
                service_parameters,
            )?;
        }
        Ok(record)
    })
}

fn append_barrier_task_updated_event(
    transaction: &StoreTransaction<'_>,
    agent: &AgentRecord,
    record: &TaskRecord,
    raw_task_json: Value,
    service_parameters: &ServiceParameters,
) -> Result<()> {
    let mut event = new_cli_event(
        "a2a.task.updated",
        json!({
            "task_id": record.task_id.as_str(),
            "context_id": record.context_id.as_ref().map(ContextId::as_str),
            "agent": agent.alias.as_str(),
            "state": record.state.as_str(),
            "source": record.source.as_str(),
            "observed_by": "barrier",
            "task": raw_task_json,
        }),
    )?;
    event.agent_alias = Some(agent.alias.clone());
    event.context_id = record.context_id.clone();
    event.task_id = Some(record.task_id.clone());
    event.metadata = record.metadata.clone();
    event.record_a2a_protocol_version(service_parameters.protocol_version.clone())?;
    transaction.append_event(&event)?;
    Ok(())
}

fn classify_record_state(state: TaskState, targets: &BarrierTargets) -> MemberBarrierStatus {
    if targets.is_success(state) {
        return MemberBarrierStatus::Satisfied;
    }
    if !targets.explicit {
        return match state {
            TaskState::Failed => MemberBarrierStatus::Failed,
            TaskState::Cancelled => MemberBarrierStatus::Cancelled,
            _ => MemberBarrierStatus::Pending,
        };
    }
    if is_terminal_task_state(state) && !targets.is_requested(state) {
        return MemberBarrierStatus::TerminalUnmatched;
    }
    match state {
        TaskState::Failed if !targets.is_requested(TaskState::Failed) => {
            MemberBarrierStatus::Failed
        }
        TaskState::Cancelled if !targets.is_requested(TaskState::Cancelled) => {
            MemberBarrierStatus::Cancelled
        }
        _ => MemberBarrierStatus::Pending,
    }
}

fn evaluate_barrier(
    operation: &BarrierOperation,
    members: &[BarrierMemberState],
) -> BarrierDecision {
    let reached = members
        .iter()
        .filter(|member| member.status.is_success())
        .count();
    if reached >= operation.required {
        return BarrierDecision::Succeeded;
    }

    let failure_count = members
        .iter()
        .filter(|member| member.status.is_failure())
        .count();
    let cancellation_count = members
        .iter()
        .filter(|member| member.status.is_cancellation())
        .count();
    if operation.failure_policy == BarrierFailurePolicy::Stop {
        if failure_count > 0 {
            return BarrierDecision::Failed;
        }
        if cancellation_count > 0 {
            return BarrierDecision::Cancelled;
        }
    }

    let disqualified = members
        .iter()
        .filter(|member| member.status.is_disqualified())
        .count();
    if members.len().saturating_sub(disqualified) < operation.required {
        if cancellation_count > 0 && failure_count == 0 {
            return BarrierDecision::Cancelled;
        }
        return BarrierDecision::Failed;
    }

    if operation.started_at.elapsed() >= operation.timeout {
        return BarrierDecision::Timeout;
    }

    BarrierDecision::Continue
}

fn finalize_barrier_output(
    profile: String,
    operation: &BarrierOperation,
    members: &[BarrierMemberState],
    attempts: u64,
    decision: BarrierDecision,
) -> BarrierOutput {
    let member_views = members
        .iter()
        .map(BarrierMemberState::view)
        .collect::<Vec<_>>();
    let reached_count = members
        .iter()
        .filter(|member| member.status.is_success())
        .count();
    let failure_count = members
        .iter()
        .filter(|member| member.status.is_failure())
        .count();
    let cancellation_count = members
        .iter()
        .filter(|member| member.status.is_cancellation())
        .count();
    let pending_count = members
        .iter()
        .filter(|member| member.status.is_pending())
        .count();
    let status = match decision {
        BarrierDecision::Succeeded => "succeeded",
        BarrierDecision::Failed => "failed",
        BarrierDecision::Cancelled => "cancelled",
        BarrierDecision::Timeout => "timeout",
        BarrierDecision::Continue => unreachable!("barrier output is finalized only for decisions"),
    }
    .to_owned();
    let message = match status.as_str() {
        "succeeded" => format!(
            "Barrier for group '{}' reached quorum {}/{} in context '{}'",
            operation.group.group_name.as_str(),
            reached_count,
            operation.required,
            operation.context_id.as_str()
        ),
        "cancelled" => format!(
            "Barrier for group '{}' observed {} cancelled member task(s)",
            operation.group.group_name.as_str(),
            cancellation_count
        ),
        "timeout" => format!(
            "Barrier for group '{}' timed out after {} attempt(s)",
            operation.group.group_name.as_str(),
            attempts
        ),
        _ => format!(
            "Barrier for group '{}' failed with {} failed member task(s)",
            operation.group.group_name.as_str(),
            failure_count
        ),
    };

    BarrierOutput {
        profile,
        operation_id: operation.operation_id.clone(),
        group: operation.group.group_name.as_str().to_owned(),
        context_id: operation.context_id.as_str().to_owned(),
        status,
        failure_policy: operation.failure_policy.as_str().to_owned(),
        local: operation.local,
        from_bcast: operation.from_bcast,
        attempts,
        member_count: members.len(),
        required: operation.required,
        reached_count,
        failure_count,
        cancellation_count,
        pending_count,
        target_states: operation.targets.target_names(),
        success_states: operation.targets.success_names(),
        timeout_ms: operation.timeout.as_millis(),
        interval_ms: operation.interval.as_millis(),
        elapsed_ms: operation.started_at.elapsed().as_millis(),
        members: member_views,
        message,
    }
}

fn barrier_exit(output: &BarrierOutput) -> Result<()> {
    match output.status.as_str() {
        "succeeded" => Ok(()),
        "timeout" => Err(MissiveError::orchestration(format!(
            "barrier for group {:?} timed out after {} ms",
            output.group, output.timeout_ms
        ))
        .with_help("Increase --timeout, lower --interval, or inspect member task states.")
        .with_exit_code(MissiveExitCode::TaskTimeout)),
        "cancelled" => Err(MissiveError::orchestration(format!(
            "barrier for group {:?} observed cancelled member task(s)",
            output.group
        ))
        .with_help("Inspect the barrier_result member list for cancelled tasks.")
        .with_exit_code(MissiveExitCode::TaskCancelled)),
        _ => Err(MissiveError::orchestration(format!(
            "barrier for group {:?} failed before reaching quorum",
            output.group
        ))
        .with_help(
            "Inspect the barrier_result member list for failed, unmatched, or errored tasks.",
        )
        .with_exit_code(MissiveExitCode::TaskFailed)),
    }
}

fn append_barrier_started_event(store: &Store, operation: &BarrierOperation) -> Result<()> {
    let mut event = new_cli_event(
        "missive.barrier.started",
        json!({
            "operation_id": operation.operation_id.as_str(),
            "group": operation.group.group_name.as_str(),
            "context_id": operation.context_id.as_str(),
            "required": operation.required,
            "member_count": operation.members.len(),
            "target_states": operation.targets.target_names(),
            "success_states": operation.targets.success_names(),
            "failure_policy": operation.failure_policy.as_str(),
            "local": operation.local,
            "from_bcast": operation.from_bcast,
            "timeout_ms": operation.timeout.as_millis(),
            "interval_ms": operation.interval.as_millis(),
        }),
    )?;
    event.group_name = Some(operation.group.group_name.clone());
    event.context_id = Some(operation.context_id.clone());
    event.record_a2a_protocol_version(operation.service_parameters.protocol_version.clone())?;
    store.append_event(&event)?;
    Ok(())
}

fn append_barrier_member_events(
    store: &Store,
    operation: &BarrierOperation,
    output: &BarrierOutput,
) -> Result<()> {
    for member in &output.members {
        let event_type = match output.status.as_str() {
            "timeout" if matches!(member.status.as_str(), "pending" | "missing") => {
                "missive.barrier.member.timeout"
            }
            _ => match member.status.as_str() {
                "satisfied" => "missive.barrier.member.satisfied",
                "cancelled" => "missive.barrier.member.cancelled",
                "failed" | "error" | "terminal_unmatched" => "missive.barrier.member.failed",
                "missing" => "missive.barrier.member.missing",
                _ => "missive.barrier.member.pending",
            },
        };
        let mut event = new_cli_event(event_type, json!(member))?;
        event.group_name = Some(operation.group.group_name.clone());
        event.context_id = Some(operation.context_id.clone());
        event.agent_alias = Some(AgentAlias::new(member.agent.clone())?);
        event.task_id = member
            .task_id
            .as_ref()
            .map(|task_id| TaskId::new(task_id.clone()))
            .transpose()?;
        store.append_event(&event)?;
    }
    Ok(())
}

fn append_barrier_completed_event(store: &Store, output: &BarrierOutput) -> Result<()> {
    let mut event = new_cli_event(
        "missive.barrier.completed",
        json!({
            "operation_id": output.operation_id.as_str(),
            "group": output.group.as_str(),
            "context_id": output.context_id.as_str(),
            "status": output.status.as_str(),
            "required": output.required,
            "member_count": output.member_count,
            "reached_count": output.reached_count,
            "failure_count": output.failure_count,
            "cancellation_count": output.cancellation_count,
            "pending_count": output.pending_count,
            "target_states": output.target_states.clone(),
            "success_states": output.success_states.clone(),
            "failure_policy": output.failure_policy.as_str(),
            "local": output.local,
            "from_bcast": output.from_bcast,
            "timeout_ms": output.timeout_ms,
            "interval_ms": output.interval_ms,
            "elapsed_ms": output.elapsed_ms,
            "members": output.members.clone(),
        }),
    )?;
    event.group_name = Some(GroupName::new(output.group.clone())?);
    event.context_id = Some(ContextId::new(output.context_id.clone())?);
    store.append_event(&event)?;
    Ok(())
}

fn read_bcast_reference<R>(args: &BarrierArgs, input: &mut R) -> Result<Option<BcastReference>>
where
    R: Read,
{
    let Some(source) = args.from_bcast.as_deref() else {
        return Ok(None);
    };
    let mut content = String::new();
    if source == "-" {
        input
            .read_to_string(&mut content)
            .map_err(|error| MissiveError::io("reading --from-bcast stdin", error))?;
    } else {
        content = fs::read_to_string(source).map_err(|error| {
            MissiveError::io(format!("reading --from-bcast file {source:?}"), error)
        })?;
    }
    parse_bcast_reference(&content).map(Some)
}

fn parse_bcast_reference(content: &str) -> Result<BcastReference> {
    if content.trim().is_empty() {
        return Err(MissiveError::validation("--from-bcast input is empty"));
    }
    let value = serde_json::from_str::<Value>(content)
        .or_else(|whole_error| {
            let Some(line) = content.lines().find(|line| !line.trim().is_empty()) else {
                return Err(whole_error);
            };
            serde_json::from_str::<Value>(line)
        })
        .map_err(|error| {
            MissiveError::validation("--from-bcast input is not valid JSON")
                .with_source(error)
                .with_help("Pass the JSON output produced by 'missive bcast --json'.")
        })?;
    let data = value.get("data").unwrap_or(&value);
    let group = data
        .get("group")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let context_id = data
        .pointer("/request/context_id")
        .or_else(|| data.get("context_id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let mut task_ids_by_agent = BTreeMap::new();
    if let Some(members) = data.get("members").and_then(Value::as_array) {
        for member in members {
            let Some(agent) = member.get("agent").and_then(Value::as_str) else {
                continue;
            };
            let Some(task_id) = member.get("task_id").and_then(Value::as_str) else {
                continue;
            };
            task_ids_by_agent.insert(agent.to_owned(), task_id.to_owned());
        }
    }
    if context_id.is_none() && task_ids_by_agent.is_empty() {
        return Err(MissiveError::validation(
            "--from-bcast JSON did not contain data.request.context_id or member task ids",
        )
        .with_help("Pass a complete 'missive bcast --json' output document."));
    }
    Ok(BcastReference {
        group,
        context_id,
        task_ids_by_agent,
    })
}

fn is_terminal_task_state(state: TaskState) -> bool {
    matches!(
        state,
        TaskState::Completed | TaskState::Failed | TaskState::Cancelled
    )
}

fn remaining_or_timeout(deadline: Instant) -> Duration {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .unwrap_or_else(|| Duration::from_millis(1))
}

fn parse_barrier_state_arg(value: &str) -> std::result::Result<TaskState, String> {
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

fn validate_non_negative_i32(flag: &str, value: Option<i32>) -> Result<()> {
    if value.is_some_and(|value| value < 0) {
        return Err(MissiveError::validation(format!(
            "{flag} must be greater than or equal to zero"
        )));
    }
    Ok(())
}

impl From<&NegotiatedInterface> for BarrierInterfaceView {
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

fn write_barrier_human<W>(writer: &mut W, output: &BarrierOutput) -> Result<()>
where
    W: Write,
{
    writeln!(writer, "{}", redact_text(&output.message))
        .map_err(|error| MissiveError::io("writing barrier output", error))?;
    for member in &output.members {
        writeln!(
            writer,
            "  {}  rank={}  status={}  state={}  task={}",
            redact_text(&member.agent),
            redact_text(&member.rank),
            redact_text(&member.status),
            member
                .state
                .as_deref()
                .map(redact_text)
                .unwrap_or_else(|| "-".to_owned()),
            member
                .task_id
                .as_deref()
                .map(redact_text)
                .unwrap_or_else(|| "-".to_owned())
        )
        .map_err(|error| MissiveError::io("writing barrier output", error))?;
    }
    Ok(())
}

fn render_barrier_success<W>(writer: &mut W, mode: OutputMode, output: &BarrierOutput) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => write_barrier_human(writer, output),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, "barrier_result", output, &output.message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_parser_requires_suffix_and_positive_value() {
        assert_eq!(
            parse_duration_arg("--timeout", "250ms").expect("duration"),
            Duration::from_millis(250)
        );
        assert_eq!(
            parse_duration_arg("--interval", "2s").expect("duration"),
            Duration::from_secs(2)
        );
        assert!(parse_duration_arg("--timeout", "0s").is_err());
        assert!(parse_duration_arg("--timeout", "2").is_err());
    }

    #[test]
    fn bcast_reference_parser_accepts_output_envelope() {
        let reference = parse_bcast_reference(
            r#"{"kind":"bcast_result","data":{"group":"team","request":{"context_id":"ctx-1"},"members":[{"agent":"alpha","task_id":"task-a"},{"agent":"beta"}]}}"#,
        )
        .expect("reference");
        assert_eq!(reference.group.as_deref(), Some("team"));
        assert_eq!(reference.context_id.as_deref(), Some("ctx-1"));
        assert_eq!(
            reference.task_ids_by_agent.get("alpha").map(String::as_str),
            Some("task-a")
        );
        assert!(!reference.task_ids_by_agent.contains_key("beta"));
    }
}
