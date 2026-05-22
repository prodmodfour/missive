//! Gateway task subscription workers.
//!
//! The first subscription implementation deliberately stays inside the local
//! gateway daemon: it resumes persisted/in-flight task monitoring from the
//! SQLite store, opens A2A `SubscribeToTask` SSE streams for agents whose cached
//! Agent Card advertises streaming, records task/event updates, and persists a
//! bounded retry backoff in `gateway_jobs` when a stream cannot complete.

use std::collections::BTreeMap;
use std::time::Duration;

use missive_a2a::{
    AgentCardExt, AuthHeaders, InterfaceNegotiationOptions, NegotiatedInterface, ServiceParameters,
    StreamMessageEvent, TaskClient, negotiate_agent_interface, protocol,
};
use missive_core::{ContextId, EventId, Metadata, MissiveError, MissiveTimestamp, Result, TaskId};
use missive_store::{
    AgentRecord, ContextUpsert, EventInsert, GatewayJobId, GatewayJobRecord, GatewayJobState,
    GatewayJobUpsert, ProcessLock, ProcessLockKind, StatePaths, Store, StoreTransaction,
    TaskRecord, TaskState, TaskUpsert,
};
use serde::Serialize;
use serde_json::{Map, Value, json};
use tokio::sync::{mpsc, watch};
use tokio::time::sleep;

use crate::daemon::{
    COMPONENT_SUBSCRIPTIONS, GatewayBusEvent, GatewayComponentStatus, ShutdownReason,
};

/// Gateway job kind used for durable A2A task subscriptions.
pub(crate) const TASK_SUBSCRIPTION_JOB_KIND: &str = "task_subscription";

const SUBSCRIPTION_SOURCE: &str = "gateway:subscriptions";
const SUBSCRIPTION_SCAN_INTERVAL: Duration = Duration::from_millis(100);
const SUBSCRIPTION_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const SUBSCRIPTION_LOCK_TTL: Duration = Duration::from_secs(30);
const SUBSCRIPTION_MIN_BACKOFF: Duration = Duration::from_secs(1);
const SUBSCRIPTION_MAX_BACKOFF: Duration = Duration::from_secs(30);
const SUBSCRIPTION_MAX_ATTEMPTS: u32 = u32::MAX;
const REDACTED: &str = "[REDACTED]";

/// Configuration passed from the daemon to the subscription manager.
#[derive(Debug, Clone)]
pub(crate) struct SubscriptionManagerConfig {
    pub(crate) profile: String,
    pub(crate) state_paths: StatePaths,
    pub(crate) service_parameters: ServiceParameters,
}

/// Final summary returned by the subscription manager when the daemon stops.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct SubscriptionManagerSummary {
    pub(crate) subscribed: u64,
    pub(crate) events: u64,
    pub(crate) retrying: u64,
    pub(crate) cleaned_up: u64,
    pub(crate) skipped_unsupported: u64,
    pub(crate) last_backoff_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SubscriptionSnapshot {
    due: usize,
    active_jobs: usize,
    retrying_jobs: usize,
    cleaned_up: usize,
    skipped_unsupported: usize,
}

#[derive(Debug, Clone)]
struct SubscriptionCandidate {
    agent: AgentRecord,
    task: TaskRecord,
    job: GatewayJobRecord,
    interface: NegotiatedInterface,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SubscriptionAttempt {
    job_id: GatewayJobId,
    task_id: TaskId,
    events: u64,
    terminal_seen: bool,
    result: SubscriptionAttemptResult,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SubscriptionAttemptResult {
    Ok,
    Err(String),
}

impl SubscriptionAttemptResult {
    const fn is_ok(&self) -> bool {
        matches!(self, Self::Ok)
    }

    fn message(&self) -> String {
        match self {
            Self::Ok => "ok".to_owned(),
            Self::Err(message) => message.clone(),
        }
    }
}

/// Runs subscription sweeps until the gateway shutdown signal is received.
pub(crate) async fn run_subscription_manager(
    config: SubscriptionManagerConfig,
    bus_tx: mpsc::UnboundedSender<GatewayBusEvent>,
    mut shutdown_rx: watch::Receiver<Option<ShutdownReason>>,
) -> Result<SubscriptionManagerSummary> {
    let mut summary = SubscriptionManagerSummary::default();
    let _ = bus_tx.send(GatewayBusEvent::Component(GatewayComponentStatus::running(
        COMPONENT_SUBSCRIPTIONS,
        "scanning local in-flight tasks for resumable A2A subscriptions",
    )));

    loop {
        if shutdown_rx.borrow().is_some() {
            break;
        }

        let scan = scan_subscription_jobs(config.clone()).await?;
        summary.cleaned_up = summary
            .cleaned_up
            .saturating_add(u64::try_from(scan.snapshot.cleaned_up).unwrap_or(u64::MAX));
        summary.skipped_unsupported = summary
            .skipped_unsupported
            .saturating_add(u64::try_from(scan.snapshot.skipped_unsupported).unwrap_or(u64::MAX));
        emit_snapshot_status(&bus_tx, &scan.snapshot, &summary);

        for candidate in scan.candidates {
            if shutdown_rx.borrow().is_some() {
                break;
            }
            let attempt = run_subscription_attempt(config.clone(), candidate).await;
            match finish_subscription_attempt(config.clone(), &attempt).await? {
                AttemptOutcome::CleanedUp => {
                    summary.cleaned_up = summary.cleaned_up.saturating_add(1);
                }
                AttemptOutcome::Retrying { backoff_ms } => {
                    summary.retrying = summary.retrying.saturating_add(1);
                    summary.last_backoff_ms = Some(backoff_ms);
                }
            }
            if attempt.result.is_ok() {
                summary.subscribed = summary.subscribed.saturating_add(1);
                summary.events = summary.events.saturating_add(attempt.events);
            }
            emit_attempt_status(&bus_tx, &attempt, &summary);
        }

        tokio::select! {
            changed = shutdown_rx.changed() => {
                if changed.is_ok() {
                    break;
                }
            }
            _ = sleep(SUBSCRIPTION_SCAN_INTERVAL) => {}
        }
    }

    let _ = bus_tx.send(GatewayBusEvent::Component(GatewayComponentStatus::stopped(
        COMPONENT_SUBSCRIPTIONS,
        format!(
            "subscription manager stopped; subscribed={} events={} retrying={} cleaned_up={} skipped_unsupported={} last_backoff_ms={}",
            summary.subscribed,
            summary.events,
            summary.retrying,
            summary.cleaned_up,
            summary.skipped_unsupported,
            summary
                .last_backoff_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_owned())
        ),
    )));

    Ok(summary)
}

#[derive(Debug)]
struct ScanResult {
    candidates: Vec<SubscriptionCandidate>,
    snapshot: SubscriptionSnapshot,
}

async fn scan_subscription_jobs(config: SubscriptionManagerConfig) -> Result<ScanResult> {
    tokio::task::spawn_blocking(move || {
        let _lock = ProcessLock::acquire(&config.state_paths, ProcessLockKind::StateMutation)?;
        let mut store = Store::open(config.state_paths.database_path())?;
        scan_subscription_jobs_blocking(&mut store, &config)
    })
    .await
    .map_err(|error| {
        MissiveError::orchestration("joining gateway subscription scan task").with_source(error)
    })?
}

fn scan_subscription_jobs_blocking(
    store: &mut Store,
    config: &SubscriptionManagerConfig,
) -> Result<ScanResult> {
    let now = MissiveTimestamp::now_utc();
    let mut snapshot = SubscriptionSnapshot::default();
    let mut candidates = Vec::new();

    cleanup_stale_or_terminal_jobs(store, config, &mut snapshot)?;

    for task in store.list_tasks()? {
        if !is_monitorable_task_state(task.state) {
            continue;
        }
        let Some(agent) = store.get_agent(&task.agent_alias)? else {
            continue;
        };
        let Some(interface) = streaming_interface_for_agent(&agent)? else {
            snapshot.skipped_unsupported += 1;
            continue;
        };

        let job_id = subscription_job_id(&agent.alias, &task.task_id)?;
        let existing = store.get_gateway_job(&job_id)?;
        let job = match existing {
            Some(job) => job,
            None => {
                let upsert = new_subscription_job(&job_id, &agent, &task, &interface, config)?;
                store.upsert_gateway_job(&upsert)?
            }
        };

        if matches!(job.state, GatewayJobState::Retrying) {
            snapshot.retrying_jobs += 1;
        } else {
            snapshot.active_jobs += 1;
        }

        if !job_is_due(&job, now) {
            continue;
        }

        let running = running_job_upsert(&job, &agent, &task, &interface, config)?;
        let job = store.upsert_gateway_job(&running)?;
        append_subscription_lifecycle_event(
            store,
            "missive.gateway.subscription.started",
            &job,
            Some(&agent.alias),
            Some(&task.task_id),
            json!({
                "profile": config.profile,
                "agent": agent.alias.as_str(),
                "task_id": task.task_id.as_str(),
                "job_id": job.gateway_job_id.as_str(),
                "interface": {
                    "binding": interface.binding,
                    "url": interface.url,
                    "protocol_version": interface.protocol_version,
                }
            }),
            config,
        )?;
        snapshot.due += 1;
        candidates.push(SubscriptionCandidate {
            agent,
            task,
            job,
            interface,
        });
    }

    Ok(ScanResult {
        candidates,
        snapshot,
    })
}

async fn run_subscription_attempt(
    config: SubscriptionManagerConfig,
    candidate: SubscriptionCandidate,
) -> SubscriptionAttempt {
    let job_id = candidate.job.gateway_job_id.clone();
    let task_id = candidate.task.task_id.clone();
    match tokio::task::spawn_blocking(move || subscribe_task_blocking(config, candidate)).await {
        Ok(attempt) => attempt,
        Err(error) => SubscriptionAttempt {
            job_id,
            task_id,
            events: 0,
            terminal_seen: false,
            result: SubscriptionAttemptResult::Err(
                MissiveError::orchestration("joining gateway task subscription worker")
                    .with_source(error)
                    .message()
                    .to_owned(),
            ),
        },
    }
}

fn subscribe_task_blocking(
    config: SubscriptionManagerConfig,
    candidate: SubscriptionCandidate,
) -> SubscriptionAttempt {
    let job_id = candidate.job.gateway_job_id.clone();
    let task_id = candidate.task.task_id.clone();
    let request = protocol::SubscribeToTaskRequest {
        id: task_id.as_str().to_owned(),
        tenant: None,
    };
    let client = match TaskClient::with_timeout(SUBSCRIPTION_REQUEST_TIMEOUT) {
        Ok(client) => client,
        Err(error) => {
            return SubscriptionAttempt {
                job_id,
                task_id,
                events: 0,
                terminal_seen: false,
                result: SubscriptionAttemptResult::Err(error.message().to_owned()),
            };
        }
    };

    let mut events = 0_u64;
    let mut terminal_seen = false;
    let result = client.subscribe_task(
        &candidate.interface,
        &request,
        &config.service_parameters,
        &AuthHeaders::new(),
        |event| {
            let terminal = persist_subscription_stream_event(&config, &candidate, &event)?;
            terminal_seen |= terminal;
            events = events.saturating_add(1);
            Ok(())
        },
    );

    SubscriptionAttempt {
        job_id,
        task_id,
        events,
        terminal_seen,
        result: result
            .map(|_| SubscriptionAttemptResult::Ok)
            .unwrap_or_else(|error| SubscriptionAttemptResult::Err(error.message().to_owned())),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttemptOutcome {
    CleanedUp,
    Retrying { backoff_ms: u64 },
}

async fn finish_subscription_attempt(
    config: SubscriptionManagerConfig,
    attempt: &SubscriptionAttempt,
) -> Result<AttemptOutcome> {
    let attempt = attempt.clone();
    tokio::task::spawn_blocking(move || {
        let _lock = ProcessLock::acquire(&config.state_paths, ProcessLockKind::StateMutation)?;
        let mut store = Store::open(config.state_paths.database_path())?;
        finish_subscription_attempt_blocking(&mut store, &config, &attempt)
    })
    .await
    .map_err(|error| {
        MissiveError::orchestration("joining gateway subscription finish task").with_source(error)
    })?
}

fn finish_subscription_attempt_blocking(
    store: &mut Store,
    config: &SubscriptionManagerConfig,
    attempt: &SubscriptionAttempt,
) -> Result<AttemptOutcome> {
    let Some(job) = store.get_gateway_job(&attempt.job_id)? else {
        return Ok(AttemptOutcome::CleanedUp);
    };
    let task = store.get_task(&attempt.task_id)?;
    let task_is_terminal = task
        .as_ref()
        .is_none_or(|task| is_terminal_task_state(task.state));
    if attempt.terminal_seen || task_is_terminal {
        append_subscription_lifecycle_event(
            store,
            "missive.gateway.subscription.cleaned",
            &job,
            task.as_ref().map(|task| &task.agent_alias),
            Some(&attempt.task_id),
            json!({
                "profile": config.profile,
                "job_id": job.gateway_job_id.as_str(),
                "task_id": attempt.task_id.as_str(),
                "reason": "terminal_task",
                "events": attempt.events,
            }),
            config,
        )?;
        store.delete_gateway_job(&job.gateway_job_id)?;
        return Ok(AttemptOutcome::CleanedUp);
    }

    let retry_reason = match &attempt.result {
        SubscriptionAttemptResult::Ok => "stream_closed_non_terminal".to_owned(),
        SubscriptionAttemptResult::Err(message) => message.clone(),
    };
    let backoff = bounded_backoff(job.retry_count.saturating_add(1));
    let backoff_ms = duration_millis(backoff);
    let next_run_at = timestamp_after(backoff)?;
    let mut retry = job_to_upsert(&job);
    retry.state = GatewayJobState::Retrying;
    retry.retry_count = retry.retry_count.saturating_add(1).min(retry.max_attempts);
    retry.next_run_at = Some(next_run_at);
    retry.locked_by = None;
    retry.locked_until = None;
    retry.completed_at = None;
    retry.result_json = Some(json!({
        "reason": retry_reason,
        "events": attempt.events,
        "terminal_seen": attempt.terminal_seen,
        "backoff_ms": backoff_ms,
        "next_run_at": next_run_at,
    }));
    retry
        .metadata
        .insert("gateway.subscription.backoff_ms", json!(backoff_ms))?;
    retry
        .metadata
        .insert_str("gateway.subscription.retry_reason", retry_reason.clone())?;
    let job = store.upsert_gateway_job(&retry)?;
    append_subscription_lifecycle_event(
        store,
        "missive.gateway.subscription.retrying",
        &job,
        task.as_ref().map(|task| &task.agent_alias),
        Some(&attempt.task_id),
        json!({
            "profile": config.profile,
            "job_id": job.gateway_job_id.as_str(),
            "task_id": attempt.task_id.as_str(),
            "retry_count": job.retry_count,
            "max_attempts": job.max_attempts,
            "backoff_ms": backoff_ms,
            "next_run_at": next_run_at,
            "reason": retry_reason,
            "bounded": {
                "min_backoff_ms": duration_millis(SUBSCRIPTION_MIN_BACKOFF),
                "max_backoff_ms": duration_millis(SUBSCRIPTION_MAX_BACKOFF),
            }
        }),
        config,
    )?;
    Ok(AttemptOutcome::Retrying { backoff_ms })
}

fn persist_subscription_stream_event(
    config: &SubscriptionManagerConfig,
    candidate: &SubscriptionCandidate,
    event: &StreamMessageEvent,
) -> Result<bool> {
    let _lock = ProcessLock::acquire(&config.state_paths, ProcessLockKind::StateMutation)?;
    let mut store = Store::open(config.state_paths.database_path())?;
    store.transaction(|transaction| {
        persist_subscription_stream_event_in_transaction(transaction, config, candidate, event)
    })
}

fn persist_subscription_stream_event_in_transaction(
    transaction: &mut StoreTransaction<'_>,
    config: &SubscriptionManagerConfig,
    candidate: &SubscriptionCandidate,
    event: &StreamMessageEvent,
) -> Result<bool> {
    let details = StreamEventDetails::from_event(&event.event)?;
    if let Some(context_id) = &details.context_id {
        if transaction.get_context(context_id)?.is_none() {
            let mut context = ContextUpsert::new(context_id.clone());
            context.agent_alias = Some(candidate.agent.alias.clone());
            transaction.upsert_context(&context)?;
        }
    }

    let mut terminal = false;
    if let Some(task_id) = &details.task_id {
        let record = transaction.get_task(task_id)?;
        let mut upsert = if let Some(record) = record {
            task_to_upsert(&record)
        } else {
            TaskUpsert::new(
                task_id.clone(),
                candidate.agent.alias.clone(),
                TaskState::Unknown,
            )
        };
        upsert.agent_alias = candidate.agent.alias.clone();
        upsert.context_id.clone_from(&details.context_id);
        if let Some(state) = details.state {
            upsert.state = state;
            if is_terminal_task_state(state) {
                upsert.completed_at = Some(MissiveTimestamp::now_utc());
                terminal = true;
            }
        }
        if let protocol::StreamResponse::Task(task) = &event.event {
            upsert.remote_task_json = Some(serde_json::to_value(task).map_err(|error| {
                MissiveError::protocol("encoding subscribed task update for persistence")
                    .with_source(error)
            })?);
        }
        upsert.record_a2a_protocol_version(config.service_parameters.protocol_version.clone())?;
        transaction.upsert_task(&upsert)?;
    }

    let event_type = format!("a2a.subscription.{}", details.event_type);
    let mut journal = EventInsert::new(
        new_subscription_event_id(details.event_type)?,
        SUBSCRIPTION_SOURCE,
        event_type,
        redact_json(&event.raw_json),
    );
    journal.agent_alias = Some(candidate.agent.alias.clone());
    journal.context_id = details.context_id;
    journal.task_id = details.task_id;
    journal.gateway_job_id = Some(candidate.job.gateway_job_id.clone());
    journal.metadata = subscription_event_metadata(config, event, details.event_type)?;
    journal.record_a2a_protocol_version(config.service_parameters.protocol_version.clone())?;
    transaction.append_event(&journal)?;
    Ok(terminal)
}

#[derive(Debug, Clone)]
struct StreamEventDetails {
    event_type: &'static str,
    task_id: Option<TaskId>,
    context_id: Option<ContextId>,
    state: Option<TaskState>,
}

impl StreamEventDetails {
    fn from_event(event: &protocol::StreamResponse) -> Result<Self> {
        match event {
            protocol::StreamResponse::Task(task) => Ok(Self {
                event_type: "task",
                task_id: Some(TaskId::new(task.id.clone())?),
                context_id: Some(ContextId::new(task.context_id.clone())?),
                state: Some(map_protocol_task_state(&task.status.state)),
            }),
            protocol::StreamResponse::Message(message) => Ok(Self {
                event_type: "message",
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
                state: None,
            }),
            protocol::StreamResponse::StatusUpdate(update) => Ok(Self {
                event_type: "status_update",
                task_id: Some(TaskId::new(update.task_id.clone())?),
                context_id: Some(ContextId::new(update.context_id.clone())?),
                state: Some(map_protocol_task_state(&update.status.state)),
            }),
            protocol::StreamResponse::ArtifactUpdate(update) => Ok(Self {
                event_type: "artifact_update",
                task_id: Some(TaskId::new(update.task_id.clone())?),
                context_id: Some(ContextId::new(update.context_id.clone())?),
                state: None,
            }),
        }
    }
}

fn cleanup_stale_or_terminal_jobs(
    store: &mut Store,
    config: &SubscriptionManagerConfig,
    snapshot: &mut SubscriptionSnapshot,
) -> Result<()> {
    for job in store.list_gateway_jobs()? {
        if job.kind != TASK_SUBSCRIPTION_JOB_KIND {
            continue;
        }
        let should_delete = match &job.task_id {
            Some(task_id) => store
                .get_task(task_id)?
                .is_none_or(|task| !is_monitorable_task_state(task.state)),
            None => true,
        };
        if should_delete {
            append_subscription_lifecycle_event(
                store,
                "missive.gateway.subscription.cleaned",
                &job,
                job.agent_alias.as_ref(),
                job.task_id.as_ref(),
                json!({
                    "profile": config.profile,
                    "job_id": job.gateway_job_id.as_str(),
                    "task_id": job.task_id.as_ref().map(TaskId::as_str),
                    "reason": "stale_or_terminal_task",
                }),
                config,
            )?;
            store.delete_gateway_job(&job.gateway_job_id)?;
            snapshot.cleaned_up += 1;
        }
    }
    Ok(())
}

fn streaming_interface_for_agent(agent: &AgentRecord) -> Result<Option<NegotiatedInterface>> {
    let Some(card_json) = agent.agent_card_json.clone() else {
        return Ok(None);
    };
    let card = <protocol::AgentCard as AgentCardExt>::from_json(card_json)?;
    if !card.capabilities.streaming.unwrap_or(false) {
        return Ok(None);
    }
    let options = InterfaceNegotiationOptions {
        preferred_bindings: agent
            .binding_preference
            .iter()
            .map(ToString::to_string)
            .collect(),
        binding_override: None,
        fallback_interface_urls: agent
            .interface_urls
            .iter()
            .map(|(binding, url)| (binding.to_string(), url.clone()))
            .collect::<BTreeMap<_, _>>(),
        fallback_base_url: Some(agent.base_url.clone()),
    };
    negotiate_agent_interface(&card, &options).map(Some)
}

fn new_subscription_job(
    job_id: &GatewayJobId,
    agent: &AgentRecord,
    task: &TaskRecord,
    interface: &NegotiatedInterface,
    config: &SubscriptionManagerConfig,
) -> Result<GatewayJobUpsert> {
    let mut job = GatewayJobUpsert::new(
        job_id.clone(),
        TASK_SUBSCRIPTION_JOB_KIND,
        subscription_request_json(agent, task, interface, config),
    );
    job.agent_alias = Some(agent.alias.clone());
    job.context_id.clone_from(&task.context_id);
    job.task_id = Some(task.task_id.clone());
    job.state = GatewayJobState::Queued;
    job.max_attempts = SUBSCRIPTION_MAX_ATTEMPTS;
    job.metadata = subscription_job_metadata(agent, task, interface, config)?;
    Ok(job)
}

fn running_job_upsert(
    job: &GatewayJobRecord,
    agent: &AgentRecord,
    task: &TaskRecord,
    interface: &NegotiatedInterface,
    config: &SubscriptionManagerConfig,
) -> Result<GatewayJobUpsert> {
    let mut upsert = job_to_upsert(job);
    upsert.state = GatewayJobState::Running;
    upsert.agent_alias = Some(agent.alias.clone());
    upsert.context_id.clone_from(&task.context_id);
    upsert.task_id = Some(task.task_id.clone());
    upsert.request_json = subscription_request_json(agent, task, interface, config);
    upsert.result_json = None;
    upsert.next_run_at = None;
    upsert.locked_by = Some(format!("gateway:{}", config.profile));
    upsert.locked_until = Some(timestamp_after(SUBSCRIPTION_LOCK_TTL)?);
    upsert.completed_at = None;
    upsert.metadata = subscription_job_metadata(agent, task, interface, config)?;
    Ok(upsert)
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

fn task_to_upsert(task: &TaskRecord) -> TaskUpsert {
    TaskUpsert {
        task_id: task.task_id.clone(),
        agent_alias: task.agent_alias.clone(),
        context_id: task.context_id.clone(),
        state: task.state,
        source: task.source,
        protocol_version: task.protocol_version.clone(),
        remote_task_json: task.remote_task_json.clone(),
        last_message_id: task.last_message_id.clone(),
        metadata: task.metadata.clone(),
        completed_at: task.completed_at,
    }
}

fn subscription_request_json(
    agent: &AgentRecord,
    task: &TaskRecord,
    interface: &NegotiatedInterface,
    config: &SubscriptionManagerConfig,
) -> Value {
    json!({
        "profile": config.profile,
        "agent": agent.alias.as_str(),
        "task_id": task.task_id.as_str(),
        "context_id": task.context_id.as_ref().map(ContextId::as_str),
        "protocol_version": config.service_parameters.protocol_version,
        "interface": {
            "binding": interface.binding,
            "url": interface.url,
            "protocol_version": interface.protocol_version,
        },
    })
}

fn subscription_job_metadata(
    agent: &AgentRecord,
    task: &TaskRecord,
    interface: &NegotiatedInterface,
    config: &SubscriptionManagerConfig,
) -> Result<Metadata> {
    let mut metadata = config.service_parameters.to_metadata()?;
    metadata.insert_str("gateway.subscription.profile", config.profile.clone())?;
    metadata.insert_str(
        "gateway.subscription.agent",
        agent.alias.as_str().to_owned(),
    )?;
    metadata.insert_str(
        "gateway.subscription.task_id",
        task.task_id.as_str().to_owned(),
    )?;
    metadata.insert_str("gateway.subscription.binding", interface.binding.clone())?;
    metadata.insert_str("gateway.subscription.interface_url", interface.url.clone())?;
    metadata.insert_str(
        "gateway.subscription.interface_protocol_version",
        interface.protocol_version.clone(),
    )?;
    metadata.insert(
        "gateway.subscription.max_backoff_ms",
        json!(duration_millis(SUBSCRIPTION_MAX_BACKOFF)),
    )?;
    Ok(metadata)
}

fn subscription_event_metadata(
    config: &SubscriptionManagerConfig,
    event: &StreamMessageEvent,
    event_type: &str,
) -> Result<Metadata> {
    let mut metadata = config.service_parameters.to_metadata()?;
    metadata.insert_str("gateway.subscription.profile", config.profile.clone())?;
    metadata.insert_str("gateway.subscription.event_type", event_type.to_owned())?;
    metadata.insert("gateway.subscription.sequence", json!(event.sequence))?;
    if let Some(sse_event_type) = &event.sse_event_type {
        metadata.insert_str("gateway.subscription.sse_event", sse_event_type.clone())?;
    }
    Ok(metadata)
}

fn append_subscription_lifecycle_event(
    store: &Store,
    event_type: &str,
    job: &GatewayJobRecord,
    agent: Option<&missive_core::AgentAlias>,
    task_id: Option<&TaskId>,
    payload: Value,
    config: &SubscriptionManagerConfig,
) -> Result<()> {
    let mut event = EventInsert::new(
        new_subscription_event_id(event_type.rsplit('.').next().unwrap_or("lifecycle"))?,
        SUBSCRIPTION_SOURCE,
        event_type,
        redact_json(&payload),
    );
    event.agent_alias = agent.cloned();
    event.context_id.clone_from(&job.context_id);
    event.task_id = task_id.cloned();
    event.gateway_job_id = Some(job.gateway_job_id.clone());
    event.metadata = config.service_parameters.to_metadata()?;
    event
        .metadata
        .insert_str("gateway.subscription.profile", config.profile.clone())?;
    event.metadata.insert_str(
        "gateway.subscription.job_id",
        job.gateway_job_id.as_str().to_owned(),
    )?;
    event.record_a2a_protocol_version(config.service_parameters.protocol_version.clone())?;
    store.append_event(&event)?;
    Ok(())
}

fn emit_snapshot_status(
    bus_tx: &mpsc::UnboundedSender<GatewayBusEvent>,
    snapshot: &SubscriptionSnapshot,
    summary: &SubscriptionManagerSummary,
) {
    let state = if snapshot.due > 0 {
        "running"
    } else if snapshot.retrying_jobs > 0 {
        "retrying"
    } else {
        "idle"
    };
    let _ = bus_tx.send(GatewayBusEvent::Component(GatewayComponentStatus::new(
        COMPONENT_SUBSCRIPTIONS,
        state,
        format!(
            "due={} active_jobs={} retrying_jobs={} cleaned_up={} skipped_unsupported={} subscribed={} events={} last_backoff_ms={}",
            snapshot.due,
            snapshot.active_jobs,
            snapshot.retrying_jobs,
            snapshot.cleaned_up,
            snapshot.skipped_unsupported,
            summary.subscribed,
            summary.events,
            summary
                .last_backoff_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_owned())
        ),
    )));
}

fn emit_attempt_status(
    bus_tx: &mpsc::UnboundedSender<GatewayBusEvent>,
    attempt: &SubscriptionAttempt,
    summary: &SubscriptionManagerSummary,
) {
    let state = if attempt.result.is_ok() && attempt.terminal_seen {
        "ready"
    } else {
        "retrying"
    };
    let result = attempt.result.message();
    let _ = bus_tx.send(GatewayBusEvent::Component(GatewayComponentStatus::new(
        COMPONENT_SUBSCRIPTIONS,
        state,
        format!(
            "task={} events={} terminal_seen={} result={} subscribed={} retrying={} cleaned_up={} last_backoff_ms={}",
            attempt.task_id.as_str(),
            attempt.events,
            attempt.terminal_seen,
            result,
            summary.subscribed,
            summary.retrying,
            summary.cleaned_up,
            summary
                .last_backoff_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_owned())
        ),
    )));
}

fn job_is_due(job: &GatewayJobRecord, now: MissiveTimestamp) -> bool {
    job.next_run_at.is_none_or(|next_run_at| next_run_at <= now)
}

fn is_monitorable_task_state(state: TaskState) -> bool {
    matches!(
        state,
        TaskState::Submitted | TaskState::Working | TaskState::Unknown
    )
}

fn is_terminal_task_state(state: TaskState) -> bool {
    matches!(
        state,
        TaskState::Completed | TaskState::Failed | TaskState::Cancelled
    )
}

fn map_protocol_task_state(state: &protocol::TaskState) -> TaskState {
    match state {
        protocol::TaskState::Submitted => TaskState::Submitted,
        protocol::TaskState::Working => TaskState::Working,
        protocol::TaskState::Completed => TaskState::Completed,
        protocol::TaskState::Failed | protocol::TaskState::Rejected => TaskState::Failed,
        protocol::TaskState::Canceled => TaskState::Cancelled,
        protocol::TaskState::InputRequired | protocol::TaskState::AuthRequired => {
            TaskState::InputRequired
        }
        protocol::TaskState::Unspecified => TaskState::Unknown,
    }
}

fn bounded_backoff(attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(5);
    let multiplier = 1_u64 << exponent;
    let millis = duration_millis(SUBSCRIPTION_MIN_BACKOFF).saturating_mul(multiplier);
    Duration::from_millis(millis.min(duration_millis(SUBSCRIPTION_MAX_BACKOFF)))
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn timestamp_after(duration: Duration) -> Result<MissiveTimestamp> {
    let seconds = i64::try_from(duration.as_secs().max(1)).map_err(|error| {
        MissiveError::validation("gateway subscription backoff duration is too large")
            .with_source(error)
    })?;
    MissiveTimestamp::from_unix_timestamp(
        MissiveTimestamp::now_utc()
            .unix_timestamp()
            .saturating_add(seconds),
    )
}

pub(crate) fn subscription_job_id(
    agent: &missive_core::AgentAlias,
    task_id: &TaskId,
) -> Result<GatewayJobId> {
    let task_fragment = safe_identifier_fragment(task_id.as_str(), 32);
    GatewayJobId::new(format!(
        "task-subscription/{}/{}-{}",
        agent.as_str(),
        task_fragment,
        stable_hex_hash(&[agent.as_str(), task_id.as_str()])
    ))
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
        "task".to_owned()
    } else {
        fragment
    }
}

fn stable_hex_hash(parts: &[&str]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for part in parts {
        for byte in part.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn new_subscription_event_id(prefix: &str) -> Result<EventId> {
    EventId::new(format!(
        "evt/subscription/{}/{}",
        prefix.replace('_', "-"),
        protocol::new_message_id()
    ))
}

fn redact_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut redacted = Map::new();
            for (key, value) in map {
                if is_secret_key(key) {
                    redacted.insert(key.clone(), Value::String(REDACTED.to_owned()));
                } else {
                    redacted.insert(key.clone(), redact_json(value));
                }
            }
            Value::Object(redacted)
        }
        Value::Array(values) => Value::Array(values.iter().map(redact_json).collect()),
        other => other.clone(),
    }
}

fn is_secret_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    matches!(
        normalized.as_str(),
        "authorization"
            | "proxyauthorization"
            | "token"
            | "accesstoken"
            | "refreshtoken"
            | "apikey"
            | "password"
            | "secret"
            | "credentials"
            | "cookie"
            | "setcookie"
    )
}

#[cfg(test)]
mod tests {
    use missive_core::AgentAlias;

    use super::*;

    #[test]
    fn bounded_backoff_doubles_until_cap() {
        assert_eq!(bounded_backoff(1), Duration::from_secs(1));
        assert_eq!(bounded_backoff(2), Duration::from_secs(2));
        assert_eq!(bounded_backoff(3), Duration::from_secs(4));
        assert_eq!(bounded_backoff(99), SUBSCRIPTION_MAX_BACKOFF);
    }

    #[test]
    fn subscription_job_ids_are_stable_and_bounded() {
        let agent = AgentAlias::new("echo".to_owned()).expect("agent");
        let task =
            TaskId::new("task-with-a-very-long-identifier-0123456789".to_owned()).expect("task");

        let first = subscription_job_id(&agent, &task).expect("first");
        let second = subscription_job_id(&agent, &task).expect("second");

        assert_eq!(first, second);
        assert!(first.as_str().len() <= 256);
        assert!(!first.as_str().contains(' '));
    }
}
