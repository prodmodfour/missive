//! Local gateway daemon skeleton.
//!
//! This module owns the first long-running `missive gateway run` runtime.  The
//! daemon intentionally supervises only inert component placeholders in this
//! ticket; later gateway tickets can replace those placeholders with real
//! subscription, webhook, job, and adapter workers without changing the CLI
//! lifecycle contract.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use missive_a2a::{ServiceParameters, protocol};
use missive_core::{EventId, Metadata, MissiveError, Result};
use missive_store::{EventInsert, EventRecord, ProcessLock, ProcessLockKind, StatePaths, Store};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio::time::sleep;

use crate::subscription::{SubscriptionManagerConfig, run_subscription_manager};

/// Default health endpoint for the local gateway daemon.
pub const DEFAULT_GATEWAY_HEALTH_PATH: &str = "/healthz";

/// Default readiness endpoint for the local gateway daemon.
pub const DEFAULT_GATEWAY_READY_PATH: &str = "/readyz";

/// Default status endpoint for the local gateway daemon.
pub const DEFAULT_GATEWAY_STATUS_PATH: &str = "/status";

const GATEWAY_SOURCE: &str = "gateway:daemon";
const COMPONENT_SUPERVISOR: &str = "supervisor";
const COMPONENT_EVENT_BUS: &str = "event_bus";
const COMPONENT_STORE: &str = "store";
const COMPONENT_SESSIONS: &str = "sessions";
const COMPONENT_HEALTH_HTTP: &str = "health_http";
pub(crate) const COMPONENT_SUBSCRIPTIONS: &str = "subscriptions";
const COMPONENT_WEBHOOK_RECEIVER: &str = "webhook_receiver";
const COMPONENT_BACKGROUND_JOBS: &str = "background_jobs";
const COMPONENT_ADAPTERS: &str = "adapters";

/// Runtime configuration for `missive gateway run`.
#[derive(Debug, Clone)]
pub struct GatewayDaemonConfig {
    /// Selected missive profile name.
    pub profile: String,
    /// Socket address to bind for local health/readiness/status endpoints.
    pub bind_addr: SocketAddr,
    /// Profile state paths used for locks, migrations, and lifecycle events.
    pub state_paths: StatePaths,
    /// Stop after this duration even if no signal is received.
    pub shutdown_after: Option<Duration>,
    /// HTTP path for liveness checks.
    pub health_path: String,
    /// HTTP path for readiness checks.
    pub ready_path: String,
    /// HTTP path for detailed status checks.
    pub status_path: String,
    /// Maximum number of concurrently running gateway jobs reserved by config.
    pub job_concurrency: u16,
    /// A2A service parameters used by gateway-managed outbound protocol calls.
    pub service_parameters: ServiceParameters,
}

impl GatewayDaemonConfig {
    /// Validates the daemon configuration before a socket is bound.
    pub fn validate(&self) -> Result<()> {
        if self.profile.trim().is_empty() {
            return Err(MissiveError::validation(
                "gateway profile name cannot be empty",
            ));
        }
        validate_endpoint_path("--health-path", &self.health_path)?;
        validate_endpoint_path("--ready-path", &self.ready_path)?;
        validate_endpoint_path("--status-path", &self.status_path)?;
        validate_distinct_paths(&[
            ("--health-path", &self.health_path),
            ("--ready-path", &self.ready_path),
            ("--status-path", &self.status_path),
        ])?;
        if self.job_concurrency == 0 {
            return Err(MissiveError::validation(
                "gateway job concurrency must be greater than zero",
            ));
        }
        self.service_parameters.validate()?;
        Ok(())
    }
}

/// Event emitted by the gateway daemon to callers such as the CLI renderer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "runtime_event", rename_all = "snake_case")]
pub enum GatewayRuntimeEvent {
    /// The socket is bound and the daemon is ready to answer health requests.
    Started(GatewayStarted),
    /// A supervised component changed or reported status.
    Component(GatewayComponentStatus),
}

/// Startup details for a gateway daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GatewayStarted {
    /// Selected profile.
    pub profile: String,
    /// Bound socket address.
    pub bind_address: String,
    /// Liveness endpoint URL.
    pub health_url: String,
    /// Readiness endpoint URL.
    pub ready_url: String,
    /// Detailed status endpoint URL.
    pub status_url: String,
    /// Effective configured gateway job concurrency.
    pub job_concurrency: u16,
    /// Components supervised by this skeleton runtime.
    pub components: Vec<GatewayComponentStatus>,
    /// Human-readable message.
    pub message: String,
}

/// Status for one supervised gateway component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GatewayComponentStatus {
    /// Stable component name.
    pub name: String,
    /// Stable component state, for example `running`, `ready`, `idle`, or `stopped`.
    pub state: String,
    /// Human-readable detail without secret material.
    pub detail: String,
    /// Human-readable status line.
    pub message: String,
}

impl GatewayComponentStatus {
    pub(crate) fn new(name: &str, state: &str, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        Self {
            name: name.to_owned(),
            state: state.to_owned(),
            message: format!("Gateway component {name} is {state}: {detail}"),
            detail,
        }
    }

    pub(crate) fn running(name: &str, detail: impl Into<String>) -> Self {
        Self::new(name, "running", detail)
    }

    pub(crate) fn ready(name: &str, detail: impl Into<String>) -> Self {
        Self::new(name, "ready", detail)
    }

    pub(crate) fn idle(name: &str, detail: impl Into<String>) -> Self {
        Self::new(name, "idle", detail)
    }

    pub(crate) fn stopped(name: &str, detail: impl Into<String>) -> Self {
        Self::new(name, "stopped", detail)
    }
}

/// Detailed health/status response returned by the daemon endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GatewayStatusResponse {
    /// Whether the daemon considers itself healthy.
    pub ok: bool,
    /// Stable top-level status keyword.
    pub status: String,
    /// Endpoint kind that produced this response.
    pub endpoint: String,
    /// Selected profile.
    pub profile: String,
    /// Bound socket address.
    pub bind_address: String,
    /// Gateway uptime in milliseconds.
    pub uptime_ms: u128,
    /// Effective configured gateway job concurrency.
    pub job_concurrency: u16,
    /// Number of lifecycle/component events emitted through the local event bus.
    pub event_bus_events: u64,
    /// Supervised component statuses.
    pub components: Vec<GatewayComponentStatus>,
}

/// Final daemon summary after graceful shutdown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GatewayDaemonSummary {
    /// Selected profile.
    pub profile: String,
    /// Bound socket address.
    pub bind_address: String,
    /// Liveness endpoint URL.
    pub health_url: String,
    /// Readiness endpoint URL.
    pub ready_url: String,
    /// Detailed status endpoint URL.
    pub status_url: String,
    /// Reason the daemon stopped.
    pub shutdown_reason: String,
    /// Uptime in milliseconds.
    pub uptime_ms: u128,
    /// Effective configured gateway job concurrency.
    pub job_concurrency: u16,
    /// Number of lifecycle/component events emitted through the local event bus.
    pub event_bus_events: u64,
    /// Final component statuses.
    pub components: Vec<GatewayComponentStatus>,
    /// Human-readable message.
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShutdownReason {
    Timeout,
    Signal,
    ServerStopped,
}

impl ShutdownReason {
    const fn as_str(&self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Signal => "signal",
            Self::ServerStopped => "server_stopped",
        }
    }
}

#[derive(Debug, Clone)]
struct ShutdownController {
    sender: watch::Sender<Option<ShutdownReason>>,
    reason: Arc<Mutex<Option<ShutdownReason>>>,
}

impl ShutdownController {
    fn new(sender: watch::Sender<Option<ShutdownReason>>) -> Self {
        Self {
            sender,
            reason: Arc::new(Mutex::new(None)),
        }
    }

    fn request(&self, reason: ShutdownReason) {
        let should_send = {
            let mut guard = self.reason.lock().expect("shutdown reason mutex poisoned");
            if guard.is_none() {
                *guard = Some(reason.clone());
                true
            } else {
                false
            }
        };
        if should_send {
            let _ = self.sender.send(Some(reason));
        }
    }

    fn reason(&self) -> ShutdownReason {
        self.reason
            .lock()
            .expect("shutdown reason mutex poisoned")
            .clone()
            .unwrap_or(ShutdownReason::ServerStopped)
    }
}

#[derive(Debug)]
struct GatewayAppState {
    profile: String,
    bind_address: Mutex<Option<SocketAddr>>,
    job_concurrency: u16,
    started_at: Instant,
    components: Mutex<BTreeMap<String, GatewayComponentStatus>>,
    event_bus_events: AtomicU64,
}

impl GatewayAppState {
    fn new(profile: String, job_concurrency: u16) -> Self {
        let state = Self {
            profile,
            bind_address: Mutex::new(None),
            job_concurrency,
            started_at: Instant::now(),
            components: Mutex::new(BTreeMap::new()),
            event_bus_events: AtomicU64::new(0),
        };
        for component in initial_components(job_concurrency) {
            state.set_component(component);
        }
        state
    }

    fn set_bind_address(&self, bind_address: SocketAddr) {
        *self
            .bind_address
            .lock()
            .expect("gateway bind address mutex poisoned") = Some(bind_address);
    }

    fn bind_address(&self) -> Option<SocketAddr> {
        *self
            .bind_address
            .lock()
            .expect("gateway bind address mutex poisoned")
    }

    fn set_component(&self, status: GatewayComponentStatus) {
        self.components
            .lock()
            .expect("gateway component mutex poisoned")
            .insert(status.name.clone(), status);
    }

    fn component_snapshot(&self) -> Vec<GatewayComponentStatus> {
        self.components
            .lock()
            .expect("gateway component mutex poisoned")
            .values()
            .cloned()
            .collect()
    }

    fn note_bus_event(&self) -> u64 {
        self.event_bus_events.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn event_bus_events(&self) -> u64 {
        self.event_bus_events.load(Ordering::SeqCst)
    }

    fn uptime_ms(&self) -> u128 {
        self.started_at.elapsed().as_millis()
    }

    fn status_response(&self, endpoint: &str) -> GatewayStatusResponse {
        GatewayStatusResponse {
            ok: true,
            status: "ok".to_owned(),
            endpoint: endpoint.to_owned(),
            profile: self.profile.clone(),
            bind_address: self
                .bind_address()
                .map(|addr| addr.to_string())
                .unwrap_or_else(|| "unbound".to_owned()),
            uptime_ms: self.uptime_ms(),
            job_concurrency: self.job_concurrency,
            event_bus_events: self.event_bus_events(),
            components: self.component_snapshot(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum GatewayBusEvent {
    Component(GatewayComponentStatus),
}

/// Runs the local gateway daemon skeleton until it receives a shutdown signal or
/// the optional timeout elapses.
pub async fn run_gateway_daemon(
    config: GatewayDaemonConfig,
    event_tx: mpsc::UnboundedSender<GatewayRuntimeEvent>,
) -> Result<GatewayDaemonSummary> {
    config.validate()?;
    config.state_paths.ensure_directories()?;

    let gateway_lock = ProcessLock::try_acquire(&config.state_paths, ProcessLockKind::Gateway)?;
    initialize_store(&config.state_paths).await?;

    let state = Arc::new(GatewayAppState::new(
        config.profile.clone(),
        config.job_concurrency,
    ));
    let (bus_tx, bus_rx) = mpsc::unbounded_channel();
    let supervisor_handle = tokio::spawn(supervisor_loop(state.clone(), bus_rx, event_tx.clone()));

    let (shutdown_tx, shutdown_rx) = watch::channel(None);
    let shutdown = ShutdownController::new(shutdown_tx);

    let listener = TcpListener::bind(config.bind_addr)
        .await
        .map_err(|error| MissiveError::io("binding gateway daemon socket", error))?;
    let local_addr = listener
        .local_addr()
        .map_err(|error| MissiveError::io("reading gateway daemon local address", error))?;
    state.set_bind_address(local_addr);

    let health_status = GatewayComponentStatus::running(
        COMPONENT_HEALTH_HTTP,
        format!(
            "serving {}, {}, and {}",
            config.health_path, config.ready_path, config.status_path
        ),
    );
    state.set_component(health_status);

    persist_lifecycle_event(&config, local_addr, "missive.gateway.started", None).await?;

    let started = started_event(&config, &state, local_addr);
    let _ = event_tx.send(GatewayRuntimeEvent::Started(started));
    emit_component_snapshot(&state, &bus_tx);
    let subscription_config = SubscriptionManagerConfig {
        profile: config.profile.clone(),
        state_paths: config.state_paths.clone(),
        service_parameters: config.service_parameters.clone(),
    };
    let subscription_handle = tokio::spawn(run_subscription_manager(
        subscription_config,
        bus_tx.clone(),
        shutdown_rx.clone(),
    ));

    let app = Router::new()
        .route(&config.health_path, get(health))
        .route(&config.ready_path, get(ready))
        .route(&config.status_path, get(status))
        .with_state(state.clone());

    let serve = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal(
        shutdown_rx,
        shutdown.clone(),
        config.shutdown_after,
    ));

    serve
        .await
        .map_err(|error| MissiveError::io("serving gateway daemon", error))?;

    shutdown.request(ShutdownReason::ServerStopped);
    drop(gateway_lock);

    let reason = shutdown.reason();
    let _ = bus_tx.send(GatewayBusEvent::Component(GatewayComponentStatus::stopped(
        COMPONENT_HEALTH_HTTP,
        "HTTP health/status listener stopped",
    )));
    subscription_handle.await.map_err(|error| {
        MissiveError::orchestration("joining gateway subscription manager task").with_source(error)
    })??;
    drop(bus_tx);
    supervisor_handle.await.map_err(|error| {
        MissiveError::orchestration("joining gateway supervisor task").with_source(error)
    })?;
    state.set_component(GatewayComponentStatus::stopped(
        COMPONENT_EVENT_BUS,
        "event bus drained",
    ));
    state.set_component(GatewayComponentStatus::stopped(
        COMPONENT_SUPERVISOR,
        "supervisor task stopped",
    ));

    persist_lifecycle_event(
        &config,
        local_addr,
        "missive.gateway.stopped",
        Some(&reason),
    )
    .await?;

    Ok(summary_event(&config, &state, local_addr, &reason))
}

async fn initialize_store(paths: &StatePaths) -> Result<()> {
    let paths = paths.clone();
    tokio::task::spawn_blocking(move || {
        let _lock = ProcessLock::acquire(&paths, ProcessLockKind::StateMutation)?;
        Store::open(paths.database_path()).map(|_| ())
    })
    .await
    .map_err(|error| {
        MissiveError::orchestration("joining gateway store initialization task").with_source(error)
    })?
}

async fn supervisor_loop(
    state: Arc<GatewayAppState>,
    mut bus_rx: mpsc::UnboundedReceiver<GatewayBusEvent>,
    runtime_tx: mpsc::UnboundedSender<GatewayRuntimeEvent>,
) {
    while let Some(event) = bus_rx.recv().await {
        match event {
            GatewayBusEvent::Component(component) => {
                state.set_component(component.clone());
                state.note_bus_event();
                let _ = runtime_tx.send(GatewayRuntimeEvent::Component(component));
            }
        }
    }
}

async fn shutdown_signal(
    mut receiver: watch::Receiver<Option<ShutdownReason>>,
    shutdown: ShutdownController,
    shutdown_after: Option<Duration>,
) {
    match shutdown_after {
        Some(duration) => {
            tokio::select! {
                _ = receiver.changed() => {},
                _ = tokio::signal::ctrl_c() => shutdown.request(ShutdownReason::Signal),
                _ = sleep(duration) => shutdown.request(ShutdownReason::Timeout),
            }
        }
        None => {
            tokio::select! {
                _ = receiver.changed() => {},
                _ = tokio::signal::ctrl_c() => shutdown.request(ShutdownReason::Signal),
            }
        }
    }
}

async fn health(State(state): State<Arc<GatewayAppState>>) -> Json<GatewayStatusResponse> {
    Json(state.status_response("health"))
}

async fn ready(State(state): State<Arc<GatewayAppState>>) -> Json<GatewayStatusResponse> {
    Json(state.status_response("ready"))
}

async fn status(State(state): State<Arc<GatewayAppState>>) -> Json<GatewayStatusResponse> {
    Json(state.status_response("status"))
}

fn started_event(
    config: &GatewayDaemonConfig,
    state: &GatewayAppState,
    local_addr: SocketAddr,
) -> GatewayStarted {
    let health_url = local_url(local_addr, &config.health_path);
    let ready_url = local_url(local_addr, &config.ready_path);
    let status_url = local_url(local_addr, &config.status_path);
    GatewayStarted {
        profile: config.profile.clone(),
        bind_address: local_addr.to_string(),
        health_url: health_url.clone(),
        ready_url,
        status_url: status_url.clone(),
        job_concurrency: config.job_concurrency,
        components: state.component_snapshot(),
        message: format!("Gateway daemon listening on {status_url} (health: {health_url})"),
    }
}

fn summary_event(
    config: &GatewayDaemonConfig,
    state: &GatewayAppState,
    local_addr: SocketAddr,
    reason: &ShutdownReason,
) -> GatewayDaemonSummary {
    let uptime_ms = state.uptime_ms();
    let shutdown_reason = reason.as_str().to_owned();
    GatewayDaemonSummary {
        profile: config.profile.clone(),
        bind_address: local_addr.to_string(),
        health_url: local_url(local_addr, &config.health_path),
        ready_url: local_url(local_addr, &config.ready_path),
        status_url: local_url(local_addr, &config.status_path),
        shutdown_reason: shutdown_reason.clone(),
        uptime_ms,
        job_concurrency: config.job_concurrency,
        event_bus_events: state.event_bus_events(),
        components: state.component_snapshot(),
        message: format!(
            "Gateway daemon stopped after {uptime_ms}ms with reason {shutdown_reason}"
        ),
    }
}

fn emit_component_snapshot(
    state: &GatewayAppState,
    bus_tx: &mpsc::UnboundedSender<GatewayBusEvent>,
) {
    for component in state.component_snapshot() {
        let _ = bus_tx.send(GatewayBusEvent::Component(component));
    }
}

fn initial_components(job_concurrency: u16) -> Vec<GatewayComponentStatus> {
    vec![
        GatewayComponentStatus::running(COMPONENT_SUPERVISOR, "supervising gateway tasks"),
        GatewayComponentStatus::running(
            COMPONENT_EVENT_BUS,
            "dispatching local gateway lifecycle events",
        ),
        GatewayComponentStatus::ready(
            COMPONENT_STORE,
            "SQLite store opened and migrations applied",
        ),
        GatewayComponentStatus::ready(
            COMPONENT_SESSIONS,
            "persistent source/agent session store and reset policies available",
        ),
        GatewayComponentStatus::new(
            COMPONENT_HEALTH_HTTP,
            "starting",
            "binding local health/status listener",
        ),
        GatewayComponentStatus::idle(
            COMPONENT_SUBSCRIPTIONS,
            "remote task subscription worker will scan in-flight tasks after startup",
        ),
        GatewayComponentStatus::idle(
            COMPONENT_WEBHOOK_RECEIVER,
            "embedded webhook receiver is reserved for a later ticket; use missive webhook run today",
        ),
        GatewayComponentStatus::idle(
            COMPONENT_BACKGROUND_JOBS,
            format!(
                "background job workers are reserved for a later ticket; configured concurrency is {job_concurrency}"
            ),
        ),
        GatewayComponentStatus::idle(
            COMPONENT_ADAPTERS,
            "adapter tasks are reserved for later adapter tickets",
        ),
    ]
}

async fn persist_lifecycle_event(
    config: &GatewayDaemonConfig,
    local_addr: SocketAddr,
    event_type: &str,
    reason: Option<&ShutdownReason>,
) -> Result<EventRecord> {
    let payload = lifecycle_payload(config, local_addr, reason);
    let mut event = EventInsert::new(
        new_gateway_event_id(event_type)?,
        GATEWAY_SOURCE,
        event_type,
        payload,
    );
    event.metadata = lifecycle_metadata(config, local_addr, reason)?;
    append_event_blocking(config.state_paths.clone(), event).await
}

async fn append_event_blocking(paths: StatePaths, event: EventInsert) -> Result<EventRecord> {
    tokio::task::spawn_blocking(move || {
        let _lock = ProcessLock::acquire(&paths, ProcessLockKind::StateMutation)?;
        let store = Store::open(paths.database_path())?;
        store.append_event(&event)
    })
    .await
    .map_err(|error| {
        MissiveError::orchestration("joining gateway lifecycle event persistence task")
            .with_source(error)
    })?
}

fn lifecycle_payload(
    config: &GatewayDaemonConfig,
    local_addr: SocketAddr,
    reason: Option<&ShutdownReason>,
) -> Value {
    let mut payload = json!({
        "profile": config.profile,
        "bind_address": local_addr.to_string(),
        "health_url": local_url(local_addr, &config.health_path),
        "ready_url": local_url(local_addr, &config.ready_path),
        "status_url": local_url(local_addr, &config.status_path),
        "job_concurrency": config.job_concurrency,
    });
    if let Some(reason) = reason
        && let Some(object) = payload.as_object_mut()
    {
        object.insert(
            "shutdown_reason".to_owned(),
            Value::String(reason.as_str().to_owned()),
        );
    }
    payload
}

fn lifecycle_metadata(
    config: &GatewayDaemonConfig,
    local_addr: SocketAddr,
    reason: Option<&ShutdownReason>,
) -> Result<Metadata> {
    let mut metadata = Metadata::new();
    metadata.insert_str("gateway.profile", config.profile.clone())?;
    metadata.insert_str("gateway.bind_address", local_addr.to_string())?;
    metadata.insert_str("gateway.health_path", config.health_path.clone())?;
    metadata.insert_str("gateway.ready_path", config.ready_path.clone())?;
    metadata.insert_str("gateway.status_path", config.status_path.clone())?;
    metadata.insert("gateway.job_concurrency", json!(config.job_concurrency))?;
    if let Some(reason) = reason {
        metadata.insert_str("gateway.shutdown_reason", reason.as_str().to_owned())?;
    }
    Ok(metadata)
}

fn new_gateway_event_id(event_type: &str) -> Result<EventId> {
    let suffix = event_type
        .rsplit('.')
        .next()
        .unwrap_or("event")
        .replace('_', "-");
    EventId::new(format!(
        "evt/gateway/{suffix}/{}",
        protocol::new_message_id()
    ))
}

fn validate_endpoint_path(flag: &str, path: &str) -> Result<()> {
    if !path.starts_with('/') || path == "/" {
        return Err(MissiveError::validation(format!(
            "{flag} must start with '/' and include a non-root path segment"
        )));
    }
    if path.contains('?') || path.contains('#') || path.chars().any(char::is_whitespace) {
        return Err(MissiveError::validation(format!(
            "{flag} must not contain whitespace, query strings, or fragments"
        )));
    }
    Ok(())
}

fn validate_distinct_paths(paths: &[(&str, &str)]) -> Result<()> {
    for (left_index, (left_name, left_path)) in paths.iter().enumerate() {
        for (right_name, right_path) in paths.iter().skip(left_index + 1) {
            if left_path == right_path {
                return Err(MissiveError::validation(format!(
                    "{left_name} and {right_name} must use distinct HTTP paths"
                )));
            }
        }
    }
    Ok(())
}

fn local_url(addr: SocketAddr, path: &str) -> String {
    let host = if addr.ip().is_ipv6() {
        format!("[{}]", addr.ip())
    } else {
        addr.ip().to_string()
    };
    format!("http://{host}:{}{}", addr.port(), path)
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use missive_core::{AgentAlias, ConfigDiscovery, TaskId};
    use missive_store::{
        AgentUpsert, GatewayJobState, GatewayJobUpsert, StatePathResolver, Store, TaskState,
        TaskUpsert,
    };
    use missive_test_support::{MockA2aServer, status_update_event};
    use serde_json::{Value, json};
    use tempfile::{TempDir, tempdir};

    use super::*;
    use crate::subscription::{TASK_SUBSCRIPTION_JOB_KIND, subscription_job_id};

    fn test_config(shutdown_after: Option<Duration>) -> (TempDir, GatewayDaemonConfig) {
        let temp = tempdir().expect("tempdir");
        let env = BTreeMap::from([(
            "MISSIVE_HOME".to_owned(),
            temp.path().join("home").to_string_lossy().into_owned(),
        )]);
        let loaded = ConfigDiscovery::new()
            .with_env(env.iter().map(|(key, value)| (key.clone(), value.clone())))
            .load()
            .expect("config");
        let paths = StatePathResolver::new()
            .with_env(env.iter().map(|(key, value)| (key.clone(), value.clone())))
            .resolve_loaded(&loaded)
            .expect("paths");
        let config = GatewayDaemonConfig {
            profile: loaded.selected_profile,
            bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            state_paths: paths,
            shutdown_after,
            health_path: DEFAULT_GATEWAY_HEALTH_PATH.to_owned(),
            ready_path: DEFAULT_GATEWAY_READY_PATH.to_owned(),
            status_path: DEFAULT_GATEWAY_STATUS_PATH.to_owned(),
            job_concurrency: 2,
            service_parameters: ServiceParameters::default(),
        };
        (temp, config)
    }

    #[test]
    fn daemon_config_rejects_invalid_or_duplicate_paths() {
        let (_temp, mut config) = test_config(Some(Duration::from_millis(1)));
        config.status_path = config.health_path.clone();
        assert!(config.validate().is_err());

        config.status_path = "status".to_owned();
        assert!(config.validate().is_err());
    }

    #[test]
    fn initial_component_snapshot_includes_future_idle_workers() {
        let components = initial_components(3);
        let by_name: BTreeMap<_, _> = components
            .iter()
            .map(|component| (component.name.as_str(), component.state.as_str()))
            .collect();

        assert_eq!(by_name[COMPONENT_SUPERVISOR], "running");
        assert_eq!(by_name[COMPONENT_EVENT_BUS], "running");
        assert_eq!(by_name[COMPONENT_STORE], "ready");
        assert_eq!(by_name[COMPONENT_SESSIONS], "ready");
        assert_eq!(by_name[COMPONENT_SUBSCRIPTIONS], "idle");
        assert_eq!(by_name[COMPONENT_BACKGROUND_JOBS], "idle");
        assert_eq!(by_name[COMPONENT_ADAPTERS], "idle");
    }

    fn streaming_agent_card(server: &MockA2aServer) -> Value {
        json!({
            "name": "mock subscription agent",
            "description": "Gateway subscription fixture",
            "version": "0.1.0",
            "capabilities": {"streaming": true, "pushNotifications": true},
            "defaultInputModes": ["text/plain"],
            "defaultOutputModes": ["text/plain"],
            "skills": [
                {
                    "id": "echo",
                    "name": "Echo",
                    "description": "Returns deterministic fixture responses.",
                    "tags": ["mock", "test"],
                    "inputModes": ["text/plain"],
                    "outputModes": ["text/plain"]
                }
            ],
            "supportedInterfaces": [
                {
                    "url": server.http_json_interface_url(),
                    "protocolBinding": "HTTP+JSON",
                    "protocolVersion": "1.0"
                }
            ]
        })
    }

    fn seed_streaming_task(
        config: &GatewayDaemonConfig,
        server: &MockA2aServer,
        task_id: &str,
        state: TaskState,
        seed_job: bool,
    ) {
        config
            .state_paths
            .ensure_directories()
            .expect("state directories");
        let alias = AgentAlias::new("echo".to_owned()).expect("agent alias");
        let task_id = TaskId::new(task_id.to_owned()).expect("task id");
        let store = Store::open(config.state_paths.database_path()).expect("store");
        let mut agent = AgentUpsert::new(alias.clone(), server.base_url());
        agent.agent_card_json = Some(streaming_agent_card(server));
        store.upsert_agent(&agent).expect("agent");
        let mut task = TaskUpsert::new(task_id.clone(), alias.clone(), state);
        task.record_a2a_protocol_version("1.0").expect("version");
        store.upsert_task(&task).expect("task");
        if seed_job {
            let job_id = subscription_job_id(&alias, &task_id).expect("subscription job id");
            let mut job = GatewayJobUpsert::new(
                job_id,
                TASK_SUBSCRIPTION_JOB_KIND,
                json!({"seeded_by": "restart_test"}),
            );
            job.state = GatewayJobState::Retrying;
            job.agent_alias = Some(alias);
            job.task_id = Some(task_id);
            job.max_attempts = u32::MAX;
            store.upsert_gateway_job(&job).expect("gateway job");
        }
    }

    #[tokio::test]
    async fn gateway_daemon_starts_times_out_and_persists_lifecycle_events() {
        let (_temp, config) = test_config(Some(Duration::from_millis(25)));
        let database_path = config.state_paths.database_path().to_path_buf();
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        let summary = run_gateway_daemon(config, event_tx)
            .await
            .expect("gateway daemon");

        assert_eq!(summary.shutdown_reason, "timeout");
        assert!(summary.components.iter().any(
            |component| component.name == COMPONENT_HEALTH_HTTP && component.state == "stopped"
        ));

        let events: Vec<_> = std::iter::from_fn(|| event_rx.try_recv().ok()).collect();
        assert!(matches!(
            events.first(),
            Some(GatewayRuntimeEvent::Started(_))
        ));
        assert!(events
            .iter()
            .any(|event| matches!(event, GatewayRuntimeEvent::Component(component) if component.name == COMPONENT_HEALTH_HTTP)));

        let store = Store::open(database_path).expect("store");
        let journal = store.list_events().expect("journal");
        assert!(
            journal
                .iter()
                .any(|event| event.event_type == "missive.gateway.started")
        );
        assert!(
            journal
                .iter()
                .any(|event| event.event_type == "missive.gateway.stopped")
        );
    }

    #[tokio::test]
    async fn gateway_subscriptions_resume_in_flight_task_and_clean_terminal_job() {
        let server = MockA2aServer::start();
        server.handle().set_stream_events(vec![status_update_event(
            "task-resume",
            "ctx-resume",
            "TASK_STATE_COMPLETED",
            Some("done"),
        )]);
        let (_temp, config) = test_config(Some(Duration::from_millis(250)));
        seed_streaming_task(&config, &server, "task-resume", TaskState::Working, true);
        let database_path = config.state_paths.database_path().to_path_buf();
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        let summary = run_gateway_daemon(config, event_tx)
            .await
            .expect("gateway daemon");

        assert_eq!(summary.shutdown_reason, "timeout");
        assert!(server.requests().iter().any(|request| {
            request.method == "POST" && request.path == "/a2a/tasks/task-resume:subscribe"
        }));

        let runtime_events: Vec<_> = std::iter::from_fn(|| event_rx.try_recv().ok()).collect();
        assert!(runtime_events.iter().any(|event| {
            matches!(event, GatewayRuntimeEvent::Component(component) if component.name == COMPONENT_SUBSCRIPTIONS && component.detail.contains("task=task-resume"))
        }));

        let store = Store::open(database_path).expect("store");
        let task = store
            .get_task(&TaskId::new("task-resume".to_owned()).expect("task id"))
            .expect("get task")
            .expect("task row");
        assert_eq!(task.state, TaskState::Completed);
        assert!(task.completed_at.is_some());
        assert!(
            store
                .list_gateway_jobs()
                .expect("gateway jobs")
                .iter()
                .all(|job| job.kind != TASK_SUBSCRIPTION_JOB_KIND)
        );
        let journal = store.list_events().expect("journal");
        assert!(
            journal
                .iter()
                .any(|event| event.event_type == "a2a.subscription.status_update")
        );
        assert!(
            journal
                .iter()
                .any(|event| event.event_type == "missive.gateway.subscription.cleaned")
        );
    }

    #[tokio::test]
    async fn gateway_subscription_failures_persist_bounded_backoff() {
        let server = MockA2aServer::builder().malformed_stream_event().start();
        let (_temp, config) = test_config(Some(Duration::from_millis(250)));
        seed_streaming_task(&config, &server, "task-backoff", TaskState::Working, false);
        let database_path = config.state_paths.database_path().to_path_buf();
        let (event_tx, _event_rx) = mpsc::unbounded_channel();

        run_gateway_daemon(config, event_tx)
            .await
            .expect("gateway daemon");

        assert!(server.requests().iter().any(|request| {
            request.method == "POST" && request.path == "/a2a/tasks/task-backoff:subscribe"
        }));
        let store = Store::open(database_path).expect("store");
        let jobs = store.list_gateway_jobs().expect("gateway jobs");
        let job = jobs
            .iter()
            .find(|job| job.kind == TASK_SUBSCRIPTION_JOB_KIND)
            .expect("retrying subscription job");
        assert_eq!(job.state, GatewayJobState::Retrying);
        assert_eq!(job.retry_count, 1);
        assert!(job.next_run_at.is_some());
        let backoff_ms = job
            .metadata
            .get("gateway.subscription.backoff_ms")
            .and_then(Value::as_u64)
            .expect("backoff metadata");
        assert!((1_000..=30_000).contains(&backoff_ms));
        let journal = store.list_events().expect("journal");
        assert!(
            journal
                .iter()
                .any(|event| event.event_type == "missive.gateway.subscription.retrying")
        );
    }
}
