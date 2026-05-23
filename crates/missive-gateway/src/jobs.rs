//! Gateway-managed background communication jobs.
//!
//! Background jobs are durable rows in `gateway_jobs` with one of the public
//! operation kinds (`send`, `stream`, `wait`, or `reduce`).  The gateway daemon
//! scans due rows, claims them with a short lock, executes the communication
//! operation, stores the result back on the job row, and appends redacted events
//! to the local journal.  The implementation deliberately avoids persisting raw
//! auth material; gateway workers currently use public/no-auth A2A requests.

use std::collections::BTreeMap;
use std::thread;
use std::time::{Duration, Instant};

use missive_a2a::{
    AgentCardClient, AgentCardExt, AgentCardFetchOutcome, AuthHeaders, InterfaceNegotiationOptions,
    NegotiatedInterface, SendMessageClient, SendMessageOutcome, ServiceParameters,
    StreamMessageClient, StreamMessageEvent, TaskClient, negotiate_agent_interface, protocol,
};
use missive_core::{
    AgentAlias, ContextId, EventId, GroupName, MessageId, MissiveError, MissiveTimestamp, Result,
    TaskId,
};
use missive_store::{
    AgentRecord, AgentSource, AgentUpsert, ContextUpsert, EventInsert, GatewayJobId,
    GatewayJobRecord, GatewayJobState, GatewayJobUpsert, GroupMemberRecord, MessageRecord,
    ProcessLock, ProcessLockKind, StatePaths, Store, StoreTransaction, TaskRecord, TaskSource,
    TaskState, TaskUpsert,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::{mpsc, watch};
use tokio::time::sleep;

use crate::daemon::{
    COMPONENT_BACKGROUND_JOBS, GatewayBusEvent, GatewayComponentStatus, ShutdownReason,
};

/// Gateway job kind for background `missive send` operations.
pub const BACKGROUND_JOB_KIND_SEND: &str = "send";
/// Gateway job kind for background `missive stream` operations.
pub const BACKGROUND_JOB_KIND_STREAM: &str = "stream";
/// Gateway job kind for background `missive task wait` operations.
pub const BACKGROUND_JOB_KIND_WAIT: &str = "wait";
/// Gateway job kind for background `missive reduce` operations.
pub const BACKGROUND_JOB_KIND_REDUCE: &str = "reduce";

const JOB_SOURCE: &str = "gateway:jobs";
const JOB_SCAN_INTERVAL: Duration = Duration::from_millis(100);
const JOB_LOCK_TTL: Duration = Duration::from_secs(60);
const JOB_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const JOB_STREAM_TIMEOUT: Duration = Duration::from_secs(30);
const JOB_WAIT_MAX_SLEEP: Duration = Duration::from_secs(5);
const JOB_MIN_BACKOFF: Duration = Duration::from_secs(1);
const JOB_MAX_BACKOFF: Duration = Duration::from_secs(30);
const REDACTED: &str = "[REDACTED]";

/// Configuration passed from the daemon to the background job manager.
#[derive(Debug, Clone)]
pub(crate) struct JobManagerConfig {
    pub(crate) profile: String,
    pub(crate) state_paths: StatePaths,
    pub(crate) service_parameters: ServiceParameters,
    pub(crate) job_concurrency: u16,
}

/// Final summary returned by the job manager when the daemon stops.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct JobManagerSummary {
    pub(crate) started: u64,
    pub(crate) succeeded: u64,
    pub(crate) failed: u64,
    pub(crate) retrying: u64,
    pub(crate) cancelled: u64,
    pub(crate) queued: u64,
    pub(crate) running: u64,
    pub(crate) last_error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct JobSnapshot {
    due: usize,
    queued: usize,
    running: usize,
    retrying: usize,
    terminal: usize,
}

#[derive(Debug, Clone)]
struct JobCandidate {
    job: GatewayJobRecord,
}

#[derive(Debug, Clone)]
struct JobAttempt {
    job_id: GatewayJobId,
    kind: String,
    result: JobAttemptResult,
}

#[derive(Debug, Clone)]
enum JobAttemptResult {
    Succeeded(JobExecutionOutput),
    Cancelled(JobExecutionOutput),
    FailedPermanent {
        output: JobExecutionOutput,
        error: String,
    },
    RetriableError(String),
}

impl JobAttemptResult {
    fn status(&self) -> &'static str {
        match self {
            Self::Succeeded(_) => "succeeded",
            Self::Cancelled(_) => "cancelled",
            Self::FailedPermanent { .. } => "failed",
            Self::RetriableError(_) => "retrying",
        }
    }

    fn message(&self) -> String {
        match self {
            Self::Succeeded(output) | Self::Cancelled(output) => output.message.clone(),
            Self::FailedPermanent { error, .. } | Self::RetriableError(error) => error.clone(),
        }
    }
}

#[derive(Debug, Clone)]
struct JobExecutionOutput {
    result_json: Value,
    agent_alias: Option<AgentAlias>,
    context_id: Option<ContextId>,
    task_id: Option<TaskId>,
    group_name: Option<GroupName>,
    message: String,
}

/// Returns true when `kind` is one of the gateway-managed communication job kinds.
#[must_use]
pub fn is_background_job_kind(kind: &str) -> bool {
    matches!(
        kind,
        BACKGROUND_JOB_KIND_SEND
            | BACKGROUND_JOB_KIND_STREAM
            | BACKGROUND_JOB_KIND_WAIT
            | BACKGROUND_JOB_KIND_REDUCE
    )
}

/// Runs background job sweeps until the gateway shutdown signal is received.
pub(crate) async fn run_job_manager(
    config: JobManagerConfig,
    bus_tx: mpsc::UnboundedSender<GatewayBusEvent>,
    mut shutdown_rx: watch::Receiver<Option<ShutdownReason>>,
) -> Result<JobManagerSummary> {
    let manager_span = tracing::debug_span!(
        target: "missive_gateway",
        "gateway.job_manager",
        profile = %config.profile,
        job_concurrency = config.job_concurrency,
        protocol_version = %config.service_parameters.protocol_version,
    );
    manager_span.in_scope(|| {
        tracing::debug!(
            target: "missive_gateway",
            profile = %config.profile,
            job_concurrency = config.job_concurrency,
            "gateway background job manager started"
        );
    });
    let mut summary = JobManagerSummary::default();
    let _ = bus_tx.send(GatewayBusEvent::Component(GatewayComponentStatus::running(
        COMPONENT_BACKGROUND_JOBS,
        format!(
            "scanning gateway_jobs for send/stream/wait/reduce work with concurrency {}",
            config.job_concurrency
        ),
    )));

    loop {
        if shutdown_rx.borrow().is_some() {
            break;
        }

        let scan = scan_background_jobs(config.clone()).await?;
        summary.queued = u64::try_from(scan.snapshot.queued).unwrap_or(u64::MAX);
        summary.running = u64::try_from(scan.snapshot.running).unwrap_or(u64::MAX);
        emit_snapshot_status(&bus_tx, &scan.snapshot, &summary);

        for candidate in scan.candidates {
            if shutdown_rx.borrow().is_some() {
                break;
            }
            let attempt = run_job_attempt(config.clone(), candidate).await;
            finish_job_attempt(config.clone(), &attempt).await?;
            match &attempt.result {
                JobAttemptResult::Succeeded(_) => {
                    summary.succeeded = summary.succeeded.saturating_add(1);
                }
                JobAttemptResult::Cancelled(_) => {
                    summary.cancelled = summary.cancelled.saturating_add(1);
                }
                JobAttemptResult::FailedPermanent { error, .. } => {
                    summary.failed = summary.failed.saturating_add(1);
                    summary.last_error = Some(error.clone());
                }
                JobAttemptResult::RetriableError(error) => {
                    summary.retrying = summary.retrying.saturating_add(1);
                    summary.last_error = Some(error.clone());
                }
            }
            summary.started = summary.started.saturating_add(1);
            emit_attempt_status(&bus_tx, &attempt, &summary);
        }

        tokio::select! {
            changed = shutdown_rx.changed() => {
                if changed.is_ok() {
                    break;
                }
            }
            _ = sleep(JOB_SCAN_INTERVAL) => {}
        }
    }

    let _ = bus_tx.send(GatewayBusEvent::Component(GatewayComponentStatus::stopped(
        COMPONENT_BACKGROUND_JOBS,
        format!(
            "background job manager stopped; started={} succeeded={} failed={} retrying={} cancelled={} queued={} running={} last_error={}",
            summary.started,
            summary.succeeded,
            summary.failed,
            summary.retrying,
            summary.cancelled,
            summary.queued,
            summary.running,
            summary.last_error.as_deref().unwrap_or("-"),
        ),
    )));
    manager_span.in_scope(|| {
        tracing::debug!(
            target: "missive_gateway",
            started = summary.started,
            succeeded = summary.succeeded,
            failed = summary.failed,
            retrying = summary.retrying,
            cancelled = summary.cancelled,
            queued = summary.queued,
            running = summary.running,
            last_error = %summary.last_error.as_deref().unwrap_or("-"),
            "gateway background job manager stopped"
        );
    });

    Ok(summary)
}

#[derive(Debug)]
struct ScanResult {
    candidates: Vec<JobCandidate>,
    snapshot: JobSnapshot,
}

async fn scan_background_jobs(config: JobManagerConfig) -> Result<ScanResult> {
    tracing::debug!(
        target: "missive_gateway",
        profile = %config.profile,
        job_concurrency = config.job_concurrency,
        "gateway background job scan started"
    );
    tokio::task::spawn_blocking(move || {
        let _lock = ProcessLock::acquire(&config.state_paths, ProcessLockKind::StateMutation)?;
        let mut store = Store::open(config.state_paths.database_path())?;
        scan_background_jobs_blocking(&mut store, &config)
    })
    .await
    .map_err(|error| {
        MissiveError::orchestration("joining gateway background job scan task").with_source(error)
    })?
}

fn scan_background_jobs_blocking(
    store: &mut Store,
    config: &JobManagerConfig,
) -> Result<ScanResult> {
    let now = MissiveTimestamp::now_utc();
    let max_due = usize::from(config.job_concurrency.max(1));
    let mut snapshot = JobSnapshot::default();
    let mut candidates = Vec::new();

    for job in store.list_gateway_jobs()? {
        if !is_background_job_kind(&job.kind) {
            continue;
        }
        match job.state {
            GatewayJobState::Queued => snapshot.queued += 1,
            GatewayJobState::Running => snapshot.running += 1,
            GatewayJobState::Retrying => snapshot.retrying += 1,
            GatewayJobState::Succeeded | GatewayJobState::Failed | GatewayJobState::Cancelled => {
                snapshot.terminal += 1;
            }
        }

        if candidates.len() >= max_due || !job_is_due(&job, now) {
            continue;
        }

        let running = running_job_upsert(&job, config)?;
        let claimed = store.upsert_gateway_job(&running)?;
        append_job_lifecycle_event(
            store,
            "missive.gateway.job.started",
            &claimed,
            json!({
                "profile": config.profile,
                "job_id": claimed.gateway_job_id.as_str(),
                "kind": claimed.kind,
                "retry_count": claimed.retry_count,
                "max_attempts": claimed.max_attempts,
            }),
            config,
        )?;
        tracing::debug!(
            target: "missive_gateway",
            job_id = %claimed.gateway_job_id.as_str(),
            kind = %claimed.kind,
            state = %claimed.state.as_str(),
            retry_count = claimed.retry_count,
            "gateway background job claimed"
        );
        snapshot.due += 1;
        candidates.push(JobCandidate { job: claimed });
    }

    tracing::debug!(
        target: "missive_gateway",
        due = snapshot.due,
        queued = snapshot.queued,
        running = snapshot.running,
        retrying = snapshot.retrying,
        terminal = snapshot.terminal,
        "gateway background job scan completed"
    );
    Ok(ScanResult {
        candidates,
        snapshot,
    })
}

fn job_is_due(job: &GatewayJobRecord, now: MissiveTimestamp) -> bool {
    match job.state {
        GatewayJobState::Queued | GatewayJobState::Retrying => {
            job.next_run_at.is_none_or(|next_run_at| next_run_at <= now)
        }
        GatewayJobState::Running => job
            .locked_until
            .is_some_and(|locked_until| locked_until <= now),
        GatewayJobState::Succeeded | GatewayJobState::Failed | GatewayJobState::Cancelled => false,
    }
}

fn running_job_upsert(
    job: &GatewayJobRecord,
    config: &JobManagerConfig,
) -> Result<GatewayJobUpsert> {
    let mut upsert = job_to_upsert(job);
    upsert.state = GatewayJobState::Running;
    upsert.locked_by = Some(format!("gateway:jobs:{}", config.profile));
    upsert.locked_until = Some(timestamp_after(JOB_LOCK_TTL)?);
    upsert.next_run_at = None;
    upsert.completed_at = None;
    upsert.result_json = None;
    upsert
        .metadata
        .insert_str("gateway.job.profile", config.profile.clone())?;
    upsert
        .metadata
        .insert_str("gateway.job.kind", job.kind.clone())?;
    Ok(upsert)
}

async fn run_job_attempt(config: JobManagerConfig, candidate: JobCandidate) -> JobAttempt {
    let job_id = candidate.job.gateway_job_id.clone();
    let kind = candidate.job.kind.clone();
    let agent = candidate
        .job
        .agent_alias
        .as_ref()
        .map(AgentAlias::as_str)
        .unwrap_or("-")
        .to_owned();
    let task_id = candidate
        .job
        .task_id
        .as_ref()
        .map(TaskId::as_str)
        .unwrap_or("-")
        .to_owned();
    let span = tracing::debug_span!(
        target: "missive_gateway",
        "gateway.job",
        job_id = %job_id.as_str(),
        kind = %kind,
        agent = %agent,
        task_id = %task_id,
    );
    span.in_scope(|| {
        tracing::debug!(
            target: "missive_gateway",
            job_id = %job_id.as_str(),
            kind = %kind,
            "gateway background job attempt started"
        );
    });
    let blocking_span = span.clone();
    let joined = tokio::task::spawn_blocking(move || {
        blocking_span.in_scope(|| execute_job_blocking(&config, &candidate.job))
    })
    .await;
    let attempt = match joined {
        Ok(result) => JobAttempt {
            job_id,
            kind,
            result,
        },
        Err(error) => JobAttempt {
            job_id,
            kind,
            result: JobAttemptResult::RetriableError(
                MissiveError::orchestration("joining gateway background job worker")
                    .with_source(error)
                    .message()
                    .to_owned(),
            ),
        },
    };
    span.in_scope(|| {
        tracing::debug!(
            target: "missive_gateway",
            job_id = %attempt.job_id.as_str(),
            kind = %attempt.kind,
            status = attempt.result.status(),
            "gateway background job attempt completed"
        );
    });
    attempt
}

fn execute_job_blocking(config: &JobManagerConfig, job: &GatewayJobRecord) -> JobAttemptResult {
    match execute_job_inner(config, job) {
        Ok(result) => result,
        Err(error) => JobAttemptResult::RetriableError(error.message().to_owned()),
    }
}

fn execute_job_inner(
    config: &JobManagerConfig,
    job: &GatewayJobRecord,
) -> Result<JobAttemptResult> {
    tracing::debug!(
        target: "missive_gateway",
        job_id = %job.gateway_job_id.as_str(),
        kind = %job.kind,
        state = %job.state.as_str(),
        retry_count = job.retry_count,
        max_attempts = job.max_attempts,
        "gateway background job execution entered"
    );
    if job_cancelled(config, &job.gateway_job_id)? {
        return Ok(JobAttemptResult::Cancelled(JobExecutionOutput {
            result_json: json!({"status": "cancelled", "reason": "cancelled_before_start"}),
            agent_alias: job.agent_alias.clone(),
            context_id: job.context_id.clone(),
            task_id: job.task_id.clone(),
            group_name: job.group_name.clone(),
            message: format!(
                "Background job {} was cancelled before execution",
                job.gateway_job_id.as_str()
            ),
        }));
    }

    match job.kind.as_str() {
        BACKGROUND_JOB_KIND_SEND => execute_send_job(config, job).map(JobAttemptResult::Succeeded),
        BACKGROUND_JOB_KIND_STREAM => {
            execute_stream_job(config, job).map(JobAttemptResult::Succeeded)
        }
        BACKGROUND_JOB_KIND_WAIT => execute_wait_job(config, job),
        BACKGROUND_JOB_KIND_REDUCE => {
            execute_reduce_job(config, job).map(JobAttemptResult::Succeeded)
        }
        other => Ok(JobAttemptResult::FailedPermanent {
            output: JobExecutionOutput {
                result_json: json!({"status": "failed", "reason": "unsupported_job_kind", "kind": other}),
                agent_alias: job.agent_alias.clone(),
                context_id: job.context_id.clone(),
                task_id: job.task_id.clone(),
                group_name: job.group_name.clone(),
                message: format!("Unsupported background job kind {other}"),
            },
            error: format!("unsupported background job kind {other:?}"),
        }),
    }
}

fn execute_send_job(
    config: &JobManagerConfig,
    job: &GatewayJobRecord,
) -> Result<JobExecutionOutput> {
    let agent_alias = request_agent_alias(job)?;
    let request = send_request_from_job(job)?;
    let (agent, interface) = resolve_agent_interface(config, &agent_alias)?;
    let outcome = SendMessageClient::with_timeout(JOB_REQUEST_TIMEOUT)?.send_message(
        &interface,
        &request,
        &config.service_parameters,
        &AuthHeaders::new(),
    )?;
    let links = persist_send_outcome(config, job, &agent, &request, &outcome)?;
    let result_json = send_result_json(&outcome, &links);
    let message = format!(
        "Background send job {} completed for agent {}",
        job.gateway_job_id.as_str(),
        agent.alias.as_str()
    );
    Ok(JobExecutionOutput {
        result_json,
        agent_alias: Some(agent.alias),
        context_id: links.context_id,
        task_id: links.task_id,
        group_name: None,
        message,
    })
}

fn execute_stream_job(
    config: &JobManagerConfig,
    job: &GatewayJobRecord,
) -> Result<JobExecutionOutput> {
    let agent_alias = request_agent_alias(job)?;
    let request = send_request_from_job(job)?;
    let (agent, interface) = resolve_agent_interface(config, &agent_alias)?;
    let mut event_count = 0_u64;
    let mut last_task_id = None;
    let mut last_context_id = None;
    let outcome = StreamMessageClient::with_timeout(JOB_STREAM_TIMEOUT)?.stream_message(
        &interface,
        &request,
        &config.service_parameters,
        &AuthHeaders::new(),
        |event| {
            let details = persist_stream_job_event(config, job, &agent, &event)?;
            if let Some(task_id) = details.task_id {
                last_task_id = Some(task_id);
            }
            if let Some(context_id) = details.context_id {
                last_context_id = Some(context_id);
            }
            event_count = event_count.saturating_add(1);
            Ok(())
        },
    )?;
    let result_json = json!({
        "operation": BACKGROUND_JOB_KIND_STREAM,
        "status": "succeeded",
        "event_count": event_count,
        "transport_event_count": outcome.event_count,
        "url": outcome.url,
        "http_status": outcome.status,
        "selected_interface": interface_json(&outcome.interface),
        "task_id": last_task_id.as_ref().map(TaskId::as_str),
        "context_id": last_context_id.as_ref().map(ContextId::as_str),
    });
    let message = format!(
        "Background stream job {} completed with {} event(s)",
        job.gateway_job_id.as_str(),
        event_count
    );
    Ok(JobExecutionOutput {
        result_json,
        agent_alias: Some(agent.alias),
        context_id: last_context_id,
        task_id: last_task_id,
        group_name: None,
        message,
    })
}

fn execute_wait_job(config: &JobManagerConfig, job: &GatewayJobRecord) -> Result<JobAttemptResult> {
    let request = wait_request_from_job(job)?;
    let task_id = TaskId::new(request.task_id.clone())?;
    let timeout = Duration::from_millis(request.timeout_ms.max(1));
    let interval = Duration::from_millis(request.interval_ms.max(1)).min(JOB_WAIT_MAX_SLEEP);
    let started = Instant::now();
    let mut attempts = 0_u64;

    let mut remote = None;
    if !request.local {
        let agent_alias = match request.agent.as_deref() {
            Some(agent) => AgentAlias::new(agent.to_owned())?,
            None => agent_alias_for_task(config, &task_id)?,
        };
        let (agent, interface) = resolve_agent_interface(config, &agent_alias)?;
        remote = Some((agent, interface));
    }

    loop {
        if job_cancelled(config, &job.gateway_job_id)? {
            let output = JobExecutionOutput {
                result_json: json!({
                    "operation": BACKGROUND_JOB_KIND_WAIT,
                    "status": "cancelled",
                    "task_id": task_id.as_str(),
                    "attempts": attempts,
                }),
                agent_alias: remote.as_ref().map(|(agent, _)| agent.alias.clone()),
                context_id: None,
                task_id: Some(task_id),
                group_name: None,
                message: format!(
                    "Background wait job {} was cancelled",
                    job.gateway_job_id.as_str()
                ),
            };
            return Ok(JobAttemptResult::Cancelled(output));
        }

        attempts = attempts.saturating_add(1);
        let record = if request.local {
            read_task_record(config, &task_id)?
        } else {
            let (agent, interface) = remote.as_ref().expect("remote initialized");
            refresh_task_for_wait(
                config,
                job,
                agent,
                interface,
                &task_id,
                request.history_length,
            )?
        };

        if wait_state_is_decisive(record.state) {
            let result_json = json!({
                "operation": BACKGROUND_JOB_KIND_WAIT,
                "status": "succeeded",
                "task_id": record.task_id.as_str(),
                "agent": record.agent_alias.as_str(),
                "context_id": record.context_id.as_ref().map(ContextId::as_str),
                "task_state": record.state.as_str(),
                "attempts": attempts,
                "elapsed_ms": duration_millis(started.elapsed()),
                "timed_out": false,
            });
            let output = JobExecutionOutput {
                result_json,
                agent_alias: Some(record.agent_alias),
                context_id: record.context_id,
                task_id: Some(record.task_id),
                group_name: None,
                message: format!(
                    "Background wait job {} observed task {} in state {}",
                    job.gateway_job_id.as_str(),
                    task_id.as_str(),
                    record.state.as_str()
                ),
            };
            return Ok(JobAttemptResult::Succeeded(output));
        }

        if started.elapsed() >= timeout {
            let result_json = json!({
                "operation": BACKGROUND_JOB_KIND_WAIT,
                "status": "failed",
                "reason": "timeout",
                "task_id": record.task_id.as_str(),
                "agent": record.agent_alias.as_str(),
                "context_id": record.context_id.as_ref().map(ContextId::as_str),
                "task_state": record.state.as_str(),
                "attempts": attempts,
                "elapsed_ms": duration_millis(started.elapsed()),
                "timeout_ms": duration_millis(timeout),
                "timed_out": true,
            });
            let error = format!(
                "background wait job {} timed out before task {} reached a decisive state",
                job.gateway_job_id.as_str(),
                task_id.as_str()
            );
            let output = JobExecutionOutput {
                result_json,
                agent_alias: Some(record.agent_alias),
                context_id: record.context_id,
                task_id: Some(record.task_id),
                group_name: None,
                message: error.clone(),
            };
            return Ok(JobAttemptResult::FailedPermanent { output, error });
        }

        let remaining = timeout
            .checked_sub(started.elapsed())
            .unwrap_or_else(|| Duration::from_millis(1));
        thread::sleep(interval.min(remaining));
    }
}

fn execute_reduce_job(
    config: &JobManagerConfig,
    job: &GatewayJobRecord,
) -> Result<JobExecutionOutput> {
    let request = reduce_request_from_job(job)?;
    let group_name = GroupName::new(request.group.clone())?;
    let context_id = ContextId::new(request.context_id.clone())?;
    let (sources, output) = reduce_locally(config, &group_name, &context_id, &request.strategy)?;
    let result_json = json!({
        "operation": BACKGROUND_JOB_KIND_REDUCE,
        "status": "succeeded",
        "group": group_name.as_str(),
        "context_id": context_id.as_str(),
        "strategy": request.strategy,
        "output": output,
        "source_count": sources.len(),
        "sources": sources,
    });
    let message = format!(
        "Background reduce job {} reduced group {} in context {}",
        job.gateway_job_id.as_str(),
        group_name.as_str(),
        context_id.as_str()
    );
    Ok(JobExecutionOutput {
        result_json,
        agent_alias: None,
        context_id: Some(context_id),
        task_id: None,
        group_name: Some(group_name),
        message,
    })
}

async fn finish_job_attempt(config: JobManagerConfig, attempt: &JobAttempt) -> Result<()> {
    let attempt = attempt.clone();
    tokio::task::spawn_blocking(move || {
        let _lock = ProcessLock::acquire(&config.state_paths, ProcessLockKind::StateMutation)?;
        let store = Store::open(config.state_paths.database_path())?;
        finish_job_attempt_blocking(&store, &config, &attempt)
    })
    .await
    .map_err(|error| {
        MissiveError::orchestration("joining gateway background job finish task").with_source(error)
    })?
}

fn finish_job_attempt_blocking(
    store: &Store,
    config: &JobManagerConfig,
    attempt: &JobAttempt,
) -> Result<()> {
    let Some(job) = store.get_gateway_job(&attempt.job_id)? else {
        return Ok(());
    };

    if job.state == GatewayJobState::Cancelled {
        append_job_lifecycle_event(
            store,
            "missive.gateway.job.cancelled",
            &job,
            json!({
                "profile": config.profile,
                "job_id": job.gateway_job_id.as_str(),
                "kind": job.kind,
                "reason": "cancelled_while_running",
            }),
            config,
        )?;
        return Ok(());
    }

    match &attempt.result {
        JobAttemptResult::Succeeded(output) => finish_terminal_job(
            store,
            config,
            &job,
            GatewayJobState::Succeeded,
            output,
            "missive.gateway.job.succeeded",
        ),
        JobAttemptResult::Cancelled(output) => finish_terminal_job(
            store,
            config,
            &job,
            GatewayJobState::Cancelled,
            output,
            "missive.gateway.job.cancelled",
        ),
        JobAttemptResult::FailedPermanent { output, .. } => finish_terminal_job(
            store,
            config,
            &job,
            GatewayJobState::Failed,
            output,
            "missive.gateway.job.failed",
        ),
        JobAttemptResult::RetriableError(error) => finish_retry_or_fail(store, config, &job, error),
    }
}

fn finish_terminal_job(
    store: &Store,
    config: &JobManagerConfig,
    job: &GatewayJobRecord,
    state: GatewayJobState,
    output: &JobExecutionOutput,
    event_type: &str,
) -> Result<()> {
    tracing::debug!(
        target: "missive_gateway",
        job_id = %job.gateway_job_id.as_str(),
        kind = %job.kind,
        previous_state = %job.state.as_str(),
        next_state = %state.as_str(),
        event_type,
        "gateway background job terminal transition"
    );
    let mut upsert = job_to_upsert(job);
    upsert.state = state;
    upsert.agent_alias = output
        .agent_alias
        .clone()
        .or_else(|| job.agent_alias.clone());
    upsert.context_id = output.context_id.clone().or_else(|| job.context_id.clone());
    upsert.task_id = output.task_id.clone().or_else(|| job.task_id.clone());
    upsert.group_name = output.group_name.clone().or_else(|| job.group_name.clone());
    upsert.result_json = Some(output.result_json.clone());
    upsert.next_run_at = None;
    upsert.locked_by = None;
    upsert.locked_until = None;
    upsert.completed_at = Some(MissiveTimestamp::now_utc());
    upsert
        .metadata
        .insert_str("gateway.job.status", state.as_str().to_owned())?;
    let updated = store.upsert_gateway_job(&upsert)?;
    append_job_lifecycle_event(
        store,
        event_type,
        &updated,
        json!({
            "profile": config.profile,
            "job_id": updated.gateway_job_id.as_str(),
            "kind": updated.kind,
            "state": updated.state.as_str(),
            "agent": updated.agent_alias.as_ref().map(AgentAlias::as_str),
            "context_id": updated.context_id.as_ref().map(ContextId::as_str),
            "task_id": updated.task_id.as_ref().map(TaskId::as_str),
            "group": updated.group_name.as_ref().map(GroupName::as_str),
            "result": output.result_json,
            "message": output.message,
        }),
        config,
    )
}

fn finish_retry_or_fail(
    store: &Store,
    config: &JobManagerConfig,
    job: &GatewayJobRecord,
    error: &str,
) -> Result<()> {
    let next_retry_count = job.retry_count.saturating_add(1);
    if next_retry_count < job.max_attempts {
        let backoff = bounded_backoff(next_retry_count);
        let next_run_at = timestamp_after(backoff)?;
        let mut retry = job_to_upsert(job);
        retry.state = GatewayJobState::Retrying;
        retry.retry_count = next_retry_count;
        retry.next_run_at = Some(next_run_at);
        retry.locked_by = None;
        retry.locked_until = None;
        retry.completed_at = None;
        retry.result_json = Some(json!({
            "status": "retrying",
            "error": error,
            "retry_count": next_retry_count,
            "max_attempts": job.max_attempts,
            "backoff_ms": duration_millis(backoff),
            "next_run_at": next_run_at,
        }));
        retry
            .metadata
            .insert_str("gateway.job.retry_reason", error.to_owned())?;
        retry
            .metadata
            .insert("gateway.job.backoff_ms", json!(duration_millis(backoff)))?;
        tracing::debug!(
            target: "missive_gateway",
            job_id = %job.gateway_job_id.as_str(),
            kind = %job.kind,
            previous_state = %job.state.as_str(),
            next_state = %GatewayJobState::Retrying.as_str(),
            retry_count = next_retry_count,
            backoff_ms = duration_millis(backoff),
            "gateway background job retry transition"
        );
        let updated = store.upsert_gateway_job(&retry)?;
        append_job_lifecycle_event(
            store,
            "missive.gateway.job.retrying",
            &updated,
            json!({
                "profile": config.profile,
                "job_id": updated.gateway_job_id.as_str(),
                "kind": updated.kind,
                "retry_count": updated.retry_count,
                "max_attempts": updated.max_attempts,
                "backoff_ms": duration_millis(backoff),
                "next_run_at": next_run_at,
                "error": error,
            }),
            config,
        )?;
        return Ok(());
    }

    let output = JobExecutionOutput {
        result_json: json!({
            "status": "failed",
            "error": error,
            "retry_count": next_retry_count.min(job.max_attempts),
            "max_attempts": job.max_attempts,
        }),
        agent_alias: job.agent_alias.clone(),
        context_id: job.context_id.clone(),
        task_id: job.task_id.clone(),
        group_name: job.group_name.clone(),
        message: error.to_owned(),
    };
    finish_terminal_job(
        store,
        config,
        job,
        GatewayJobState::Failed,
        &output,
        "missive.gateway.job.failed",
    )
}

fn request_agent_alias(job: &GatewayJobRecord) -> Result<AgentAlias> {
    if let Some(alias) = &job.agent_alias {
        return Ok(alias.clone());
    }
    let agent = job
        .request_json
        .get("agent")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            MissiveError::validation(format!(
                "background job {} does not include an agent",
                job.gateway_job_id.as_str()
            ))
        })?;
    AgentAlias::new(agent.to_owned())
}

fn send_request_from_job(job: &GatewayJobRecord) -> Result<protocol::SendMessageRequest> {
    let value = job.request_json.get("request").cloned().ok_or_else(|| {
        MissiveError::validation(format!(
            "background job {} does not include a SendMessage request",
            job.gateway_job_id.as_str()
        ))
    })?;
    serde_json::from_value(value).map_err(|error| {
        MissiveError::protocol("decoding background SendMessage request").with_source(error)
    })
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "snake_case")]
struct WaitJobRequest {
    agent: Option<String>,
    task_id: String,
    local: bool,
    history_length: Option<i32>,
    interval_ms: u64,
    timeout_ms: u64,
}

impl Default for WaitJobRequest {
    fn default() -> Self {
        Self {
            agent: None,
            task_id: String::new(),
            local: false,
            history_length: None,
            interval_ms: 1_000,
            timeout_ms: 30_000,
        }
    }
}

fn wait_request_from_job(job: &GatewayJobRecord) -> Result<WaitJobRequest> {
    let request: WaitJobRequest =
        serde_json::from_value(job.request_json.clone()).map_err(|error| {
            MissiveError::protocol("decoding background wait job request").with_source(error)
        })?;
    if request.task_id.trim().is_empty() {
        return Err(MissiveError::validation(
            "background wait job requires task_id",
        ));
    }
    Ok(request)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "snake_case")]
struct ReduceJobRequest {
    group: String,
    context_id: String,
    strategy: String,
}

impl Default for ReduceJobRequest {
    fn default() -> Self {
        Self {
            group: String::new(),
            context_id: String::new(),
            strategy: "summarise".to_owned(),
        }
    }
}

fn reduce_request_from_job(job: &GatewayJobRecord) -> Result<ReduceJobRequest> {
    let request: ReduceJobRequest =
        serde_json::from_value(job.request_json.clone()).map_err(|error| {
            MissiveError::protocol("decoding background reduce job request").with_source(error)
        })?;
    if request.group.trim().is_empty() || request.context_id.trim().is_empty() {
        return Err(MissiveError::validation(
            "background reduce job requires group and context_id",
        ));
    }
    let strategy = normalize_reduce_strategy(&request.strategy)?;
    Ok(ReduceJobRequest {
        strategy,
        ..request
    })
}

fn normalize_reduce_strategy(value: &str) -> Result<String> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "summarise" | "summarize" | "merge" | "rank" | "vote" => Ok(normalized),
        other => Err(MissiveError::validation(format!(
            "background reduce strategy {other:?} is not supported"
        ))
        .with_help(
            "Use summarise, summarize, merge, rank, or vote for gateway-managed local reduce jobs.",
        )),
    }
}

#[derive(Debug, Clone)]
struct JobLinks {
    context_id: Option<ContextId>,
    task_id: Option<TaskId>,
}

fn persist_send_outcome(
    config: &JobManagerConfig,
    job: &GatewayJobRecord,
    agent: &AgentRecord,
    request: &protocol::SendMessageRequest,
    outcome: &SendMessageOutcome,
) -> Result<JobLinks> {
    let _lock = ProcessLock::acquire(&config.state_paths, ProcessLockKind::StateMutation)?;
    let mut store = Store::open(config.state_paths.database_path())?;
    store.transaction(|transaction| {
        let links = links_from_send_response(request, outcome)?;
        if let Some(context_id) = &links.context_id
            && transaction.get_context(context_id)?.is_none()
        {
            let mut context = ContextUpsert::new(context_id.clone());
            context.agent_alias = Some(agent.alias.clone());
            transaction.upsert_context(&context)?;
        }
        if let protocol::SendMessageResponse::Task(task) = &outcome.response {
            upsert_task_from_protocol(transaction, agent, task, &config.service_parameters)?;
        }
        let mut event = EventInsert::new(
            new_job_event_id("send-response")?,
            JOB_SOURCE,
            "a2a.job.send.response",
            redact_json(&json!({
                "profile": config.profile,
                "job_id": job.gateway_job_id.as_str(),
                "agent": agent.alias.as_str(),
                "url": outcome.url,
                "http_status": outcome.status,
                "response": outcome.raw_json,
            })),
        );
        event.agent_alias = Some(agent.alias.clone());
        event.context_id = links.context_id.clone();
        event.task_id = links.task_id.clone();
        event.gateway_job_id = Some(job.gateway_job_id.clone());
        event.metadata = config.service_parameters.to_metadata()?;
        event.record_a2a_protocol_version(config.service_parameters.protocol_version.clone())?;
        transaction.append_event(&event)?;
        Ok(links)
    })
}

fn persist_stream_job_event(
    config: &JobManagerConfig,
    job: &GatewayJobRecord,
    agent: &AgentRecord,
    event: &StreamMessageEvent,
) -> Result<StreamEventDetails> {
    let details = StreamEventDetails::from_event(&event.event)?;
    let _lock = ProcessLock::acquire(&config.state_paths, ProcessLockKind::StateMutation)?;
    let mut store = Store::open(config.state_paths.database_path())?;
    store.transaction(|transaction| {
        if let Some(context_id) = &details.context_id
            && transaction.get_context(context_id)?.is_none()
        {
            let mut context = ContextUpsert::new(context_id.clone());
            context.agent_alias = Some(agent.alias.clone());
            transaction.upsert_context(&context)?;
        }
        match &event.event {
            protocol::StreamResponse::Task(task) => {
                upsert_task_from_protocol(transaction, agent, task, &config.service_parameters)?;
            }
            protocol::StreamResponse::StatusUpdate(update) => {
                upsert_task_from_status_update(
                    transaction,
                    agent,
                    update,
                    &config.service_parameters,
                )?;
            }
            protocol::StreamResponse::Message(_) | protocol::StreamResponse::ArtifactUpdate(_) => {}
        }

        let event_type = format!("a2a.job.stream.{}", details.event_type);
        let mut journal = EventInsert::new(
            new_job_event_id(details.event_type)?,
            JOB_SOURCE,
            event_type,
            redact_json(&event.raw_json),
        );
        journal.agent_alias = Some(agent.alias.clone());
        journal.context_id = details.context_id.clone();
        journal.task_id = details.task_id.clone();
        journal.gateway_job_id = Some(job.gateway_job_id.clone());
        journal.metadata = config.service_parameters.to_metadata()?;
        journal
            .metadata
            .insert("gateway.job.stream_sequence", json!(event.sequence))?;
        journal.record_a2a_protocol_version(config.service_parameters.protocol_version.clone())?;
        transaction.append_event(&journal)?;
        Ok(details)
    })
}

fn refresh_task_for_wait(
    config: &JobManagerConfig,
    job: &GatewayJobRecord,
    agent: &AgentRecord,
    interface: &NegotiatedInterface,
    task_id: &TaskId,
    history_length: Option<i32>,
) -> Result<TaskRecord> {
    let request = protocol::GetTaskRequest {
        id: task_id.as_str().to_owned(),
        history_length,
        tenant: interface.tenant.clone(),
    };
    let outcome = TaskClient::with_timeout(JOB_REQUEST_TIMEOUT)?.get_task(
        interface,
        &request,
        &config.service_parameters,
        &AuthHeaders::new(),
    )?;
    let _lock = ProcessLock::acquire(&config.state_paths, ProcessLockKind::StateMutation)?;
    let mut store = Store::open(config.state_paths.database_path())?;
    store.transaction(|transaction| {
        let record = upsert_task_from_protocol(
            transaction,
            agent,
            &outcome.task,
            &config.service_parameters,
        )?;
        let mut event = EventInsert::new(
            new_job_event_id("wait-refresh")?,
            JOB_SOURCE,
            "a2a.job.wait.task_updated",
            redact_json(&json!({
                "profile": config.profile,
                "job_id": job.gateway_job_id.as_str(),
                "agent": agent.alias.as_str(),
                "task_id": record.task_id.as_str(),
                "state": record.state.as_str(),
                "url": outcome.url,
                "http_status": outcome.status,
                "task": outcome.raw_json,
            })),
        );
        event.agent_alias = Some(agent.alias.clone());
        event.context_id = record.context_id.clone();
        event.task_id = Some(record.task_id.clone());
        event.gateway_job_id = Some(job.gateway_job_id.clone());
        event.metadata = config.service_parameters.to_metadata()?;
        event.record_a2a_protocol_version(config.service_parameters.protocol_version.clone())?;
        transaction.append_event(&event)?;
        Ok(record)
    })
}

fn read_task_record(config: &JobManagerConfig, task_id: &TaskId) -> Result<TaskRecord> {
    let store = Store::open(config.state_paths.database_path())?;
    store.get_task(task_id)?.ok_or_else(|| {
        MissiveError::validation(format!("task {:?} is not known locally", task_id.as_str()))
    })
}

fn agent_alias_for_task(config: &JobManagerConfig, task_id: &TaskId) -> Result<AgentAlias> {
    Ok(read_task_record(config, task_id)?.agent_alias)
}

fn resolve_agent_interface(
    config: &JobManagerConfig,
    agent_alias: &AgentAlias,
) -> Result<(AgentRecord, NegotiatedInterface)> {
    let mut agent = {
        let store = Store::open(config.state_paths.database_path())?;
        store.get_agent(agent_alias)?.ok_or_else(|| {
            MissiveError::validation(format!(
                "agent {:?} is not registered for background job execution",
                agent_alias.as_str()
            ))
        })?
    };

    let raw_card = if let Some(raw_card) = agent.agent_card_json.clone() {
        raw_card
    } else {
        let client = AgentCardClient::with_timeout(JOB_REQUEST_TIMEOUT)?;
        match client.fetch_public_agent_card_with_service_parameters_and_auth(
            &agent.base_url,
            None,
            &config.service_parameters,
            &AuthHeaders::new(),
        )? {
            AgentCardFetchOutcome::Fetched(fetch) => {
                let fetched_at = MissiveTimestamp::now_utc();
                let upsert = agent_upsert_with_card(&agent, fetch.raw_json.clone(), fetched_at);
                let _lock =
                    ProcessLock::acquire(&config.state_paths, ProcessLockKind::StateMutation)?;
                let store = Store::open(config.state_paths.database_path())?;
                agent = store.upsert_agent(&upsert)?;
                fetch.raw_json
            }
            AgentCardFetchOutcome::NotModified(_) => {
                return Err(MissiveError::protocol(format!(
                    "agent {:?} returned 304 Not Modified without a cached Agent Card",
                    agent.alias.as_str()
                )));
            }
        }
    };

    let card = <protocol::AgentCard as AgentCardExt>::from_json(raw_card)?;
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
    let interface = negotiate_agent_interface(&card, &options)?;
    Ok((agent, interface))
}

fn agent_upsert_with_card(
    record: &AgentRecord,
    raw_card: Value,
    fetched_at: MissiveTimestamp,
) -> AgentUpsert {
    AgentUpsert {
        alias: record.alias.clone(),
        source: match record.source {
            AgentSource::Local => AgentSource::Local,
            AgentSource::ConfigSeed => AgentSource::ConfigSeed,
            AgentSource::Discovered => AgentSource::Discovered,
        },
        base_url: record.base_url.clone(),
        interface_urls: record.interface_urls.clone(),
        binding_preference: record.binding_preference.clone(),
        auth_ref_name: record.auth_ref_name.clone(),
        tags: record.tags.clone(),
        notes: record.notes.clone(),
        metadata: record.metadata.clone(),
        agent_card_json: Some(raw_card),
        agent_card_etag: record.agent_card_etag.clone(),
        agent_card_last_modified: record.agent_card_last_modified.clone(),
        agent_card_fetched_at: Some(fetched_at),
        read_only: record.read_only,
    }
}

fn links_from_send_response(
    request: &protocol::SendMessageRequest,
    outcome: &SendMessageOutcome,
) -> Result<JobLinks> {
    match &outcome.response {
        protocol::SendMessageResponse::Message(message) => Ok(JobLinks {
            context_id: message
                .context_id
                .as_ref()
                .or(request.message.context_id.as_ref())
                .map(|value| ContextId::new(value.clone()))
                .transpose()?,
            task_id: message
                .task_id
                .as_ref()
                .or(request.message.task_id.as_ref())
                .map(|value| TaskId::new(value.clone()))
                .transpose()?,
        }),
        protocol::SendMessageResponse::Task(task) => Ok(JobLinks {
            context_id: Some(ContextId::new(task.context_id.clone())?),
            task_id: Some(TaskId::new(task.id.clone())?),
        }),
    }
}

fn send_result_json(outcome: &SendMessageOutcome, links: &JobLinks) -> Value {
    let (shape, message_id, text) = match &outcome.response {
        protocol::SendMessageResponse::Message(message) => (
            "message",
            Some(message.message_id.clone()),
            message.text().map(ToOwned::to_owned),
        ),
        protocol::SendMessageResponse::Task(task) => (
            "task",
            task.status
                .message
                .as_ref()
                .map(|message| message.message_id.clone()),
            task.status
                .message
                .as_ref()
                .and_then(protocol::Message::text)
                .map(ToOwned::to_owned),
        ),
    };
    json!({
        "operation": BACKGROUND_JOB_KIND_SEND,
        "status": "succeeded",
        "shape": shape,
        "message_id": message_id,
        "task_id": links.task_id.as_ref().map(TaskId::as_str),
        "context_id": links.context_id.as_ref().map(ContextId::as_str),
        "text": text,
        "url": outcome.url,
        "http_status": outcome.status,
        "selected_interface": interface_json(&outcome.interface),
        "raw": outcome.raw_json,
    })
}

fn interface_json(interface: &NegotiatedInterface) -> Value {
    json!({
        "binding": interface.binding,
        "protocol_binding": interface.protocol_binding,
        "url": interface.url,
        "tenant": interface.tenant,
        "protocol_version": interface.protocol_version,
        "source": interface.source.as_str(),
    })
}

#[derive(Debug, Clone)]
struct StreamEventDetails {
    event_type: &'static str,
    task_id: Option<TaskId>,
    context_id: Option<ContextId>,
}

impl StreamEventDetails {
    fn from_event(event: &protocol::StreamResponse) -> Result<Self> {
        match event {
            protocol::StreamResponse::Task(task) => Ok(Self {
                event_type: "task",
                task_id: Some(TaskId::new(task.id.clone())?),
                context_id: Some(ContextId::new(task.context_id.clone())?),
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
            }),
            protocol::StreamResponse::StatusUpdate(update) => Ok(Self {
                event_type: "status_update",
                task_id: Some(TaskId::new(update.task_id.clone())?),
                context_id: Some(ContextId::new(update.context_id.clone())?),
            }),
            protocol::StreamResponse::ArtifactUpdate(update) => Ok(Self {
                event_type: "artifact_update",
                task_id: Some(TaskId::new(update.task_id.clone())?),
                context_id: Some(ContextId::new(update.context_id.clone())?),
            }),
        }
    }
}

fn upsert_task_from_protocol(
    transaction: &StoreTransaction<'_>,
    agent: &AgentRecord,
    task: &protocol::Task,
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
        task_id.clone(),
        agent.alias.clone(),
        map_protocol_task_state(&task.status.state),
    );
    upsert.source = TaskSource::Remote;
    upsert.context_id = Some(context_id);
    upsert.remote_task_json = Some(serde_json::to_value(task).map_err(|error| {
        MissiveError::protocol("encoding A2A task for background job persistence")
            .with_source(error)
    })?);
    upsert.last_message_id = task
        .status
        .message
        .as_ref()
        .map(|message| MessageId::new(message.message_id.clone()))
        .transpose()?;
    upsert.record_a2a_protocol_version(service_parameters.protocol_version.clone())?;
    if is_terminal_task_state(upsert.state) {
        upsert.completed_at = Some(MissiveTimestamp::now_utc());
    }
    transaction.upsert_task(&upsert)
}

fn upsert_task_from_status_update(
    transaction: &StoreTransaction<'_>,
    agent: &AgentRecord,
    update: &protocol::TaskStatusUpdateEvent,
    service_parameters: &ServiceParameters,
) -> Result<TaskRecord> {
    let task_id = TaskId::new(update.task_id.clone())?;
    let context_id = ContextId::new(update.context_id.clone())?;
    if transaction.get_context(&context_id)?.is_none() {
        let mut context = ContextUpsert::new(context_id.clone());
        context.agent_alias = Some(agent.alias.clone());
        transaction.upsert_context(&context)?;
    }
    let existing = transaction.get_task(&task_id)?;
    let mut upsert = existing.as_ref().map(task_to_upsert).unwrap_or_else(|| {
        TaskUpsert::new(task_id.clone(), agent.alias.clone(), TaskState::Unknown)
    });
    upsert.agent_alias = agent.alias.clone();
    upsert.context_id = Some(context_id);
    upsert.state = map_protocol_task_state(&update.status.state);
    upsert.last_message_id = update
        .status
        .message
        .as_ref()
        .map(|message| MessageId::new(message.message_id.clone()))
        .transpose()?;
    upsert.record_a2a_protocol_version(service_parameters.protocol_version.clone())?;
    if is_terminal_task_state(upsert.state) {
        upsert.completed_at = Some(MissiveTimestamp::now_utc());
    }
    transaction.upsert_task(&upsert)
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

fn is_terminal_task_state(state: TaskState) -> bool {
    matches!(
        state,
        TaskState::Completed | TaskState::Failed | TaskState::Cancelled
    )
}

fn wait_state_is_decisive(state: TaskState) -> bool {
    matches!(
        state,
        TaskState::Completed | TaskState::Failed | TaskState::Cancelled | TaskState::InputRequired
    )
}

fn reduce_locally(
    config: &JobManagerConfig,
    group_name: &GroupName,
    context_id: &ContextId,
    strategy: &str,
) -> Result<(Vec<Value>, String)> {
    let store = Store::open(config.state_paths.database_path())?;
    if store.get_group(group_name)?.is_none() {
        return Err(MissiveError::validation(format!(
            "group {:?} is not known locally",
            group_name.as_str()
        )));
    }
    let members = store.list_group_members(group_name)?;
    let tasks = store.list_tasks()?;
    let messages = store.list_messages()?;
    let mut sources = Vec::new();
    for member in members {
        sources.push(reduce_source_for_member(
            &member, context_id, &tasks, &messages,
        ));
    }
    let output = match strategy {
        "vote" => reduce_vote_output(&sources),
        "rank" => reduce_rank_output(&sources),
        "merge" => reduce_merge_output(&sources),
        "summarise" | "summarize" => reduce_summary_output(&sources),
        _ => reduce_summary_output(&sources),
    };
    Ok((sources, output))
}

fn reduce_source_for_member(
    member: &GroupMemberRecord,
    context_id: &ContextId,
    tasks: &[TaskRecord],
    messages: &[MessageRecord],
) -> Value {
    let latest_task = tasks
        .iter()
        .filter(|task| task.agent_alias == member.agent_alias)
        .filter(|task| task.context_id.as_ref() == Some(context_id))
        .max_by_key(|task| task.updated_at);
    let text = latest_task
        .and_then(text_from_task_record)
        .or_else(|| latest_task.and_then(|task| text_from_latest_message(task, messages)));
    json!({
        "rank": member.rank_name.as_str(),
        "agent": member.agent_alias.as_str(),
        "task_id": latest_task.map(|task| task.task_id.as_str()),
        "state": latest_task.map(|task| task.state.as_str()),
        "text": text,
        "status": if latest_task.is_some() { "ok" } else { "missing_task" },
    })
}

fn text_from_task_record(task: &TaskRecord) -> Option<String> {
    let raw = task.remote_task_json.as_ref()?;
    let task = serde_json::from_value::<protocol::Task>(raw.clone()).ok()?;
    task.status
        .message
        .as_ref()
        .and_then(protocol::Message::text)
        .map(ToOwned::to_owned)
}

fn text_from_latest_message(task: &TaskRecord, messages: &[MessageRecord]) -> Option<String> {
    let message = messages
        .iter()
        .filter(|message| message.task_id.as_ref() == Some(&task.task_id))
        .filter(|message| !matches!(message.direction.as_str(), "request"))
        .max_by_key(|message| message.created_at)?;
    serde_json::from_value::<protocol::Message>(message.content_json.clone())
        .ok()
        .and_then(|message| message.text().map(ToOwned::to_owned))
}

fn reduce_summary_output(sources: &[Value]) -> String {
    let mut lines = vec!["# Background reduce summary".to_owned()];
    for source in sources {
        let rank = source.get("rank").and_then(Value::as_str).unwrap_or("rank");
        let agent = source
            .get("agent")
            .and_then(Value::as_str)
            .unwrap_or("agent");
        let text = source
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("No local output available.");
        lines.push(format!("- {rank} ({agent}): {text}"));
    }
    lines.join("\n")
}

fn reduce_merge_output(sources: &[Value]) -> String {
    sources
        .iter()
        .filter_map(|source| source.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn reduce_rank_output(sources: &[Value]) -> String {
    sources
        .iter()
        .enumerate()
        .map(|(index, source)| {
            let rank = source.get("rank").and_then(Value::as_str).unwrap_or("rank");
            let agent = source
                .get("agent")
                .and_then(Value::as_str)
                .unwrap_or("agent");
            format!("{}. {rank} ({agent})", index + 1)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn reduce_vote_output(sources: &[Value]) -> String {
    let available = sources
        .iter()
        .filter(|source| source.get("text").and_then(Value::as_str).is_some())
        .count();
    format!(
        "{available}/{} source(s) produced local output",
        sources.len()
    )
}

fn append_job_lifecycle_event(
    store: &Store,
    event_type: &str,
    job: &GatewayJobRecord,
    payload: Value,
    config: &JobManagerConfig,
) -> Result<()> {
    let mut event = EventInsert::new(
        new_job_event_id(event_type.rsplit('.').next().unwrap_or("lifecycle"))?,
        JOB_SOURCE,
        event_type,
        redact_json(&payload),
    );
    event.agent_alias = job.agent_alias.clone();
    event.context_id = job.context_id.clone();
    event.task_id = job.task_id.clone();
    event.group_name = job.group_name.clone();
    event.gateway_job_id = Some(job.gateway_job_id.clone());
    event.metadata = config.service_parameters.to_metadata()?;
    event
        .metadata
        .insert_str("gateway.job.profile", config.profile.clone())?;
    event
        .metadata
        .insert_str("gateway.job.kind", job.kind.clone())?;
    event
        .metadata
        .insert_str("gateway.job.id", job.gateway_job_id.as_str().to_owned())?;
    event.record_a2a_protocol_version(config.service_parameters.protocol_version.clone())?;
    store.append_event(&event)?;
    Ok(())
}

fn job_cancelled(config: &JobManagerConfig, job_id: &GatewayJobId) -> Result<bool> {
    let store = Store::open(config.state_paths.database_path())?;
    Ok(store
        .get_gateway_job(job_id)?
        .is_some_and(|job| job.state == GatewayJobState::Cancelled))
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

fn emit_snapshot_status(
    bus_tx: &mpsc::UnboundedSender<GatewayBusEvent>,
    snapshot: &JobSnapshot,
    summary: &JobManagerSummary,
) {
    let state = if snapshot.due > 0 {
        "running"
    } else if snapshot.retrying > 0 {
        "retrying"
    } else {
        "idle"
    };
    let _ = bus_tx.send(GatewayBusEvent::Component(GatewayComponentStatus::new(
        COMPONENT_BACKGROUND_JOBS,
        state,
        format!(
            "due={} queued={} running={} retrying={} terminal={} succeeded={} failed={} cancelled={} last_error={}",
            snapshot.due,
            snapshot.queued,
            snapshot.running,
            snapshot.retrying,
            snapshot.terminal,
            summary.succeeded,
            summary.failed,
            summary.cancelled,
            summary.last_error.as_deref().unwrap_or("-"),
        ),
    )));
}

fn emit_attempt_status(
    bus_tx: &mpsc::UnboundedSender<GatewayBusEvent>,
    attempt: &JobAttempt,
    summary: &JobManagerSummary,
) {
    let _ = bus_tx.send(GatewayBusEvent::Component(GatewayComponentStatus::new(
        COMPONENT_BACKGROUND_JOBS,
        attempt.result.status(),
        format!(
            "job={} kind={} result={} started={} succeeded={} failed={} retrying={} cancelled={}",
            attempt.job_id.as_str(),
            attempt.kind,
            attempt.result.message(),
            summary.started,
            summary.succeeded,
            summary.failed,
            summary.retrying,
            summary.cancelled,
        ),
    )));
}

fn bounded_backoff(attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(5);
    let multiplier = 1_u64 << exponent;
    let millis = duration_millis(JOB_MIN_BACKOFF).saturating_mul(multiplier);
    Duration::from_millis(millis.min(duration_millis(JOB_MAX_BACKOFF)))
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn timestamp_after(duration: Duration) -> Result<MissiveTimestamp> {
    let seconds = i64::try_from(duration.as_secs().max(1)).map_err(|error| {
        MissiveError::validation("gateway job duration is too large").with_source(error)
    })?;
    MissiveTimestamp::from_unix_timestamp(
        MissiveTimestamp::now_utc()
            .unix_timestamp()
            .saturating_add(seconds),
    )
}

fn new_job_event_id(prefix: &str) -> Result<EventId> {
    EventId::new(format!(
        "evt/job/{}/{}",
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
    use super::*;

    #[test]
    fn recognizes_public_background_job_kinds() {
        assert!(is_background_job_kind(BACKGROUND_JOB_KIND_SEND));
        assert!(is_background_job_kind(BACKGROUND_JOB_KIND_STREAM));
        assert!(is_background_job_kind(BACKGROUND_JOB_KIND_WAIT));
        assert!(is_background_job_kind(BACKGROUND_JOB_KIND_REDUCE));
        assert!(!is_background_job_kind("task_subscription"));
    }

    #[test]
    fn job_due_logic_resumes_expired_running_jobs() {
        let job_id = GatewayJobId::new("job-1").expect("job id");
        let now = MissiveTimestamp::now_utc();
        let mut job = GatewayJobUpsert::new(job_id, BACKGROUND_JOB_KIND_SEND, json!({}));
        job.state = GatewayJobState::Running;
        job.locked_until = Some(
            MissiveTimestamp::from_unix_timestamp(now.unix_timestamp().saturating_sub(1))
                .expect("timestamp"),
        );
        let record = GatewayJobRecord {
            gateway_job_id: job.gateway_job_id,
            kind: job.kind,
            state: job.state,
            agent_alias: job.agent_alias,
            context_id: job.context_id,
            task_id: job.task_id,
            group_name: job.group_name,
            adapter_binding_id: job.adapter_binding_id,
            request_json: job.request_json,
            result_json: job.result_json,
            metadata: job.metadata,
            retry_count: job.retry_count,
            max_attempts: job.max_attempts,
            next_run_at: job.next_run_at,
            locked_by: job.locked_by,
            locked_until: job.locked_until,
            created_at: now,
            updated_at: now,
            completed_at: job.completed_at,
        };

        assert!(job_is_due(&record, now));
    }
}
