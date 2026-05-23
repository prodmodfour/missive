//! Local A2A push notification webhook receiver.
//!
//! This module owns the small standalone receiver used by `missive webhook run`.
//! It intentionally does not start the broader gateway daemon; later gateway
//! work can embed the same receiver behind a supervisor.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::header::{CONTENT_TYPE, HeaderName};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use missive_a2a::METADATA_A2A_PROTOCOL_VERSION;
use missive_a2a::protocol::{self, StreamResponse};
use missive_core::{ContextId, EventId, Metadata, MissiveError, Result, TaskId};
use missive_store::{
    ContextUpsert, EventInsert, EventRecord, ProcessLock, ProcessLockKind, StatePaths, Store,
};
use serde::Serialize;
use serde_json::{Map, Value, json};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio::time::sleep;

/// Default path for local A2A push notification callbacks.
pub const DEFAULT_WEBHOOK_PATH: &str = "/a2a/push";

/// Default maximum accepted request body size for webhook callbacks.
pub const DEFAULT_MAX_BODY_BYTES: usize = 1024 * 1024;

const WEBHOOK_SOURCE: &str = "gateway:webhook";
const REDACTED: &str = "[REDACTED]";

/// Runtime configuration for a local A2A push notification receiver.
#[derive(Debug, Clone)]
pub struct WebhookReceiverConfig {
    /// Selected missive profile name.
    pub profile: String,
    /// Socket address to bind.
    pub bind_addr: SocketAddr,
    /// HTTP path that receives A2A `StreamResponse` POST payloads.
    pub path: String,
    /// Profile state paths used to persist event journal records.
    pub state_paths: StatePaths,
    /// Optional inbound auth validation.
    pub auth: WebhookAuth,
    /// Stop after this many accepted callbacks.
    pub max_events: Option<u64>,
    /// Stop after this duration even if no callbacks arrive.
    pub shutdown_after: Option<Duration>,
    /// Maximum accepted request body size in bytes.
    pub max_body_bytes: usize,
    /// Protocol version recorded when a callback omits `A2A-Version`.
    pub protocol_version: String,
}

impl WebhookReceiverConfig {
    /// Validates the receiver configuration before binding a socket.
    pub fn validate(&self) -> Result<()> {
        validate_webhook_path(&self.path)?;
        self.auth.validate()?;
        if self.max_events.is_some_and(|value| value == 0) {
            return Err(MissiveError::validation(
                "--max-events must be greater than zero",
            ));
        }
        if self.max_body_bytes == 0 {
            return Err(MissiveError::validation(
                "webhook maximum body size must be greater than zero",
            ));
        }
        if self.protocol_version.trim().is_empty() {
            return Err(MissiveError::validation(
                "webhook protocol version fallback cannot be empty",
            ));
        }
        Ok(())
    }
}

/// Inbound authentication check for webhook callbacks.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum WebhookAuth {
    /// Do not require an auth header.
    #[default]
    Disabled,
    /// Require one header to match the configured token and optional scheme.
    Header {
        /// HTTP header name to inspect.
        name: String,
        /// Secret token value resolved outside this crate.
        token: String,
        /// Optional auth scheme prefix. `None` means the raw header value must
        /// equal the token.
        scheme: Option<String>,
    },
}

impl WebhookAuth {
    fn validate(&self) -> Result<()> {
        match self {
            Self::Disabled => Ok(()),
            Self::Header {
                name,
                token,
                scheme,
            } => {
                validate_header_name(name)?;
                if token.is_empty() {
                    return Err(MissiveError::auth(
                        "webhook auth token cannot be empty when auth validation is enabled",
                    ));
                }
                if let Some(scheme) = scheme {
                    validate_auth_scheme(scheme)?;
                }
                Ok(())
            }
        }
    }

    /// Returns a secret-free representation suitable for output and tests.
    #[must_use]
    pub fn redacted_view(&self) -> WebhookAuthView {
        match self {
            Self::Disabled => WebhookAuthView {
                required: false,
                header: None,
                scheme: None,
                token: None,
            },
            Self::Header { name, scheme, .. } => WebhookAuthView {
                required: true,
                header: Some(name.clone()),
                scheme: scheme.clone(),
                token: Some(REDACTED.to_owned()),
            },
        }
    }

    fn validate_headers(&self, headers: &HeaderMap) -> std::result::Result<bool, String> {
        match self {
            Self::Disabled => Ok(false),
            Self::Header {
                name,
                token,
                scheme,
            } => {
                let header_name = HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| format!("configured auth header {name:?} is invalid"))?;
                let Some(actual) = headers.get(&header_name) else {
                    return Err(format!("missing required auth header {name}"));
                };
                let actual = actual
                    .to_str()
                    .map_err(|_| format!("auth header {name} contains non-UTF-8 data"))?;
                let expected = expected_auth_header_value(scheme.as_deref(), token);
                if constant_time_eq(actual.as_bytes(), expected.as_bytes()) {
                    Ok(true)
                } else {
                    Err(format!(
                        "auth header {name} did not match the configured token"
                    ))
                }
            }
        }
    }
}

/// Secret-free auth description for structured output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WebhookAuthView {
    /// Whether callbacks must pass auth validation.
    pub required: bool,
    /// Header inspected when auth is enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// Scheme prefix expected when auth is enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheme: Option<String>,
    /// Redacted token marker when auth is enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

/// Event emitted by the receiver to callers such as the CLI renderer.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "runtime_event", rename_all = "snake_case")]
pub enum WebhookRuntimeEvent {
    /// The socket is bound and the receiver is ready.
    Started(WebhookStarted),
    /// A valid A2A push callback was accepted and persisted.
    Accepted(WebhookAccepted),
    /// A callback was rejected and a redacted rejection event was persisted.
    Rejected(WebhookRejected),
}

/// Startup details for a webhook receiver.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WebhookStarted {
    /// Selected profile.
    pub profile: String,
    /// Bound socket address.
    pub bind_address: String,
    /// Callback URL for local HTTP use.
    pub callback_url: String,
    /// Health endpoint URL.
    pub health_url: String,
    /// Callback path.
    pub path: String,
    /// Inbound auth configuration without secret material.
    pub auth: WebhookAuthView,
    /// TLS note for humans and automation.
    pub tls: WebhookTlsNote,
    /// Human-readable message.
    pub message: String,
}

/// TLS limitation note for the local HTTP receiver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WebhookTlsNote {
    /// TLS mode implemented by this receiver.
    pub mode: String,
    /// Operational note.
    pub note: String,
}

impl Default for WebhookTlsNote {
    fn default() -> Self {
        Self {
            mode: "external_termination".to_owned(),
            note: "missive webhook run serves local HTTP only; terminate HTTPS in a trusted tunnel or reverse proxy before forwarding callbacks to this listener".to_owned(),
        }
    }
}

/// Details for an accepted A2A push callback.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WebhookAccepted {
    /// Selected profile.
    pub profile: String,
    /// Monotonic event journal sequence.
    pub event_sequence: i64,
    /// Stored event id.
    pub event_id: String,
    /// Stored event type.
    pub event_type: String,
    /// A2A push payload variant.
    pub payload_kind: String,
    /// Linked task id when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Linked context id when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    /// Task state for task/status payloads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Artifact id for artifact updates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    /// Remote peer socket address when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_addr: Option<String>,
    /// Human-readable message.
    pub message: String,
}

/// Details for a rejected webhook callback.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WebhookRejected {
    /// Selected profile.
    pub profile: String,
    /// HTTP status returned to the caller.
    pub status: u16,
    /// Stable rejection reason.
    pub reason: String,
    /// Monotonic event journal sequence when the rejection was persisted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_sequence: Option<i64>,
    /// Stored event id when the rejection was persisted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    /// Remote peer socket address when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_addr: Option<String>,
    /// Human-readable message.
    pub message: String,
}

/// Final receiver summary after graceful shutdown.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WebhookReceiverSummary {
    /// Selected profile.
    pub profile: String,
    /// Bound socket address.
    pub bind_address: String,
    /// Callback URL for local HTTP use.
    pub callback_url: String,
    /// Health endpoint URL.
    pub health_url: String,
    /// Accepted callback count.
    pub accepted: u64,
    /// Rejected callback count.
    pub rejected: u64,
    /// Reason the receiver stopped.
    pub shutdown_reason: String,
    /// TLS note for humans and automation.
    pub tls: WebhookTlsNote,
    /// Human-readable message.
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ShutdownReason {
    MaxEvents,
    Timeout,
    Signal,
    ServerStopped,
}

impl ShutdownReason {
    const fn as_str(&self) -> &'static str {
        match self {
            Self::MaxEvents => "max_events",
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
struct WebhookAppState {
    profile: String,
    path: String,
    state_paths: StatePaths,
    auth: WebhookAuth,
    max_events: Option<u64>,
    accepted: AtomicU64,
    rejected: AtomicU64,
    protocol_version: String,
    event_tx: mpsc::UnboundedSender<WebhookRuntimeEvent>,
    shutdown: ShutdownController,
}

impl WebhookAppState {
    fn accepted_count(&self) -> u64 {
        self.accepted.load(Ordering::SeqCst)
    }

    fn rejected_count(&self) -> u64 {
        self.rejected.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    ok: bool,
    status: &'static str,
    component: &'static str,
    profile: String,
    path: String,
    accepted: u64,
    rejected: u64,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    ok: bool,
    error: String,
    reason: String,
}

#[derive(Debug, Serialize)]
struct AcceptedResponse {
    ok: bool,
    event_type: String,
    event_id: String,
    sequence: i64,
}

#[derive(Debug, Clone)]
struct Rejection {
    status: StatusCode,
    reason: &'static str,
    message: String,
    remote_addr: Option<String>,
    content_type: Option<String>,
    payload: Option<Value>,
    auth_validated: bool,
}

#[derive(Debug, Clone)]
struct PayloadDetails {
    kind: &'static str,
    event_type: &'static str,
    task_id: Option<TaskId>,
    context_id: Option<ContextId>,
    state: Option<String>,
    artifact_id: Option<String>,
}

/// Runs a local A2A push notification receiver until it receives a shutdown
/// signal, reaches `max_events`, or the optional timeout elapses.
pub async fn run_webhook_receiver(
    config: WebhookReceiverConfig,
    event_tx: mpsc::UnboundedSender<WebhookRuntimeEvent>,
) -> Result<WebhookReceiverSummary> {
    config.validate()?;
    config.state_paths.ensure_directories()?;

    let gateway_lock = ProcessLock::try_acquire(&config.state_paths, ProcessLockKind::Gateway)?;
    initialize_store(&config.state_paths).await?;

    let (shutdown_tx, shutdown_rx) = watch::channel(None);
    let shutdown = ShutdownController::new(shutdown_tx);
    let state = Arc::new(WebhookAppState {
        profile: config.profile.clone(),
        path: config.path.clone(),
        state_paths: config.state_paths.clone(),
        auth: config.auth.clone(),
        max_events: config.max_events,
        accepted: AtomicU64::new(0),
        rejected: AtomicU64::new(0),
        protocol_version: config.protocol_version.clone(),
        event_tx: event_tx.clone(),
        shutdown: shutdown.clone(),
    });

    let app = Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(health))
        .route(&config.path, post(push_callback))
        .layer(DefaultBodyLimit::max(config.max_body_bytes))
        .with_state(state.clone());

    let listener = TcpListener::bind(config.bind_addr)
        .await
        .map_err(|error| MissiveError::io("binding webhook receiver socket", error))?;
    let local_addr = listener
        .local_addr()
        .map_err(|error| MissiveError::io("reading webhook receiver local address", error))?;
    let started = started_event(&config, local_addr);
    let _ = event_tx.send(WebhookRuntimeEvent::Started(started));

    let serve = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal(
        shutdown_rx,
        shutdown.clone(),
        config.shutdown_after,
    ));

    serve
        .await
        .map_err(|error| MissiveError::io("serving webhook receiver", error))?;

    drop(gateway_lock);

    let reason = shutdown.reason();
    let accepted = state.accepted_count();
    let rejected = state.rejected_count();
    let message = format!(
        "Webhook receiver stopped after accepting {accepted} callback(s) and rejecting {rejected} callback(s)"
    );
    Ok(WebhookReceiverSummary {
        profile: config.profile,
        bind_address: local_addr.to_string(),
        callback_url: local_url(local_addr, &config.path),
        health_url: local_url(local_addr, "/healthz"),
        accepted,
        rejected,
        shutdown_reason: reason.as_str().to_owned(),
        tls: WebhookTlsNote::default(),
        message,
    })
}

async fn initialize_store(paths: &StatePaths) -> Result<()> {
    let paths = paths.clone();
    tokio::task::spawn_blocking(move || {
        let _lock = ProcessLock::acquire(&paths, ProcessLockKind::StateMutation)?;
        Store::open(paths.database_path()).map(|_| ())
    })
    .await
    .map_err(|error| {
        MissiveError::orchestration("joining webhook store initialization task").with_source(error)
    })?
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

fn started_event(config: &WebhookReceiverConfig, local_addr: SocketAddr) -> WebhookStarted {
    let callback_url = local_url(local_addr, &config.path);
    let health_url = local_url(local_addr, "/healthz");
    WebhookStarted {
        profile: config.profile.clone(),
        bind_address: local_addr.to_string(),
        callback_url: callback_url.clone(),
        health_url: health_url.clone(),
        path: config.path.clone(),
        auth: config.auth.redacted_view(),
        tls: WebhookTlsNote::default(),
        message: format!("Webhook receiver listening on {callback_url} (health: {health_url})"),
    }
}

async fn health(State(state): State<Arc<WebhookAppState>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        status: "ok",
        component: "webhook",
        profile: state.profile.clone(),
        path: state.path.clone(),
        accepted: state.accepted_count(),
        rejected: state.rejected_count(),
    })
}

async fn push_callback(
    State(state): State<Arc<WebhookAppState>>,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let remote = Some(remote_addr.to_string());

    match state.auth.validate_headers(&headers) {
        Ok(auth_validated) => {
            handle_authorized_callback(state, headers, body, remote, auth_validated).await
        }
        Err(reason) => {
            reject_callback(
                state,
                Rejection {
                    status: StatusCode::UNAUTHORIZED,
                    reason: "auth_failed",
                    message: reason,
                    remote_addr: remote,
                    content_type: None,
                    payload: None,
                    auth_validated: false,
                },
            )
            .await
        }
    }
}

async fn handle_authorized_callback(
    state: Arc<WebhookAppState>,
    headers: HeaderMap,
    body: Bytes,
    remote_addr: Option<String>,
    auth_validated: bool,
) -> Response {
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let protocol_version = headers
        .get(protocol::SVC_PARAM_VERSION)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| state.protocol_version.clone());

    let raw_json = match serde_json::from_slice::<Value>(&body) {
        Ok(value) => value,
        Err(error) => {
            return reject_callback(
                state,
                Rejection {
                    status: StatusCode::BAD_REQUEST,
                    reason: "invalid_json",
                    message: format!("request body is not valid JSON: {error}"),
                    remote_addr,
                    content_type,
                    payload: None,
                    auth_validated,
                },
            )
            .await;
        }
    };

    let stream_response = match serde_json::from_value::<StreamResponse>(raw_json.clone()) {
        Ok(value) => value,
        Err(error) => {
            return reject_callback(
                state,
                Rejection {
                    status: StatusCode::BAD_REQUEST,
                    reason: "invalid_a2a_stream_response",
                    message: format!("request body is not an A2A StreamResponse payload: {error}"),
                    remote_addr,
                    content_type,
                    payload: Some(redact_json(&raw_json)),
                    auth_validated,
                },
            )
            .await;
        }
    };

    let details = match PayloadDetails::from_stream_response(&stream_response) {
        Ok(details) => details,
        Err(error) => {
            return reject_callback(
                state,
                Rejection {
                    status: StatusCode::BAD_REQUEST,
                    reason: "invalid_a2a_identifiers",
                    message: error.message().to_owned(),
                    remote_addr,
                    content_type,
                    payload: Some(redact_json(&raw_json)),
                    auth_validated,
                },
            )
            .await;
        }
    };

    match persist_accepted_event(
        &state,
        &details,
        &raw_json,
        &protocol_version,
        content_type.as_deref(),
        remote_addr.as_deref(),
        auth_validated,
    )
    .await
    {
        Ok(record) => {
            let accepted = accepted_event(&state, &details, &record, remote_addr);
            state.accepted.fetch_add(1, Ordering::SeqCst);
            maybe_shutdown_after_max_events(&state);
            let _ = state
                .event_tx
                .send(WebhookRuntimeEvent::Accepted(accepted.clone()));
            (
                StatusCode::ACCEPTED,
                Json(AcceptedResponse {
                    ok: true,
                    event_type: accepted.event_type,
                    event_id: accepted.event_id,
                    sequence: accepted.event_sequence,
                }),
            )
                .into_response()
        }
        Err(error) => internal_error_response(error),
    }
}

fn maybe_shutdown_after_max_events(state: &WebhookAppState) {
    if let Some(max_events) = state.max_events {
        if state.accepted_count() >= max_events {
            state.shutdown.request(ShutdownReason::MaxEvents);
        }
    }
}

async fn reject_callback(state: Arc<WebhookAppState>, rejection: Rejection) -> Response {
    match persist_rejected_event(&state, &rejection).await {
        Ok(record) => {
            state.rejected.fetch_add(1, Ordering::SeqCst);
            let rejected = WebhookRejected {
                profile: state.profile.clone(),
                status: rejection.status.as_u16(),
                reason: rejection.reason.to_owned(),
                event_sequence: Some(record.sequence),
                event_id: Some(record.event_id.as_str().to_owned()),
                remote_addr: rejection.remote_addr.clone(),
                message: format!("Rejected webhook callback: {}", rejection.message),
            };
            let _ = state
                .event_tx
                .send(WebhookRuntimeEvent::Rejected(rejected.clone()));
            (
                rejection.status,
                Json(ErrorResponse {
                    ok: false,
                    error: rejection.message,
                    reason: rejection.reason.to_owned(),
                }),
            )
                .into_response()
        }
        Err(error) => internal_error_response(error),
    }
}

fn internal_error_response(error: MissiveError) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse {
            ok: false,
            error: error.message().to_owned(),
            reason: "persistence_failed".to_owned(),
        }),
    )
        .into_response()
}

async fn persist_accepted_event(
    state: &WebhookAppState,
    details: &PayloadDetails,
    raw_json: &Value,
    protocol_version: &str,
    content_type: Option<&str>,
    remote_addr: Option<&str>,
    auth_validated: bool,
) -> Result<EventRecord> {
    let mut event = EventInsert::new(
        new_event_id("push")?,
        WEBHOOK_SOURCE,
        format!("a2a.push.{}", details.event_type),
        redact_json(raw_json),
    );
    event.context_id = details.context_id.clone();
    event.task_id = details.task_id.clone();
    event.metadata = webhook_metadata(
        &state.path,
        details.kind,
        protocol_version,
        content_type,
        remote_addr,
        auth_validated,
        true,
    )?;
    append_event_blocking(state.state_paths.clone(), event).await
}

async fn persist_rejected_event(
    state: &WebhookAppState,
    rejection: &Rejection,
) -> Result<EventRecord> {
    let mut event = EventInsert::new(
        new_event_id("push-rejected")?,
        WEBHOOK_SOURCE,
        "a2a.push.rejected",
        json!({
            "status": rejection.status.as_u16(),
            "reason": rejection.reason,
            "error": rejection.message.clone(),
            "payload": rejection.payload.clone(),
        }),
    );
    event.metadata = webhook_metadata(
        &state.path,
        "rejected",
        &state.protocol_version,
        rejection.content_type.as_deref(),
        rejection.remote_addr.as_deref(),
        rejection.auth_validated,
        false,
    )?;
    append_event_blocking(state.state_paths.clone(), event).await
}

async fn append_event_blocking(paths: StatePaths, event: EventInsert) -> Result<EventRecord> {
    tokio::task::spawn_blocking(move || {
        let _lock = ProcessLock::acquire(&paths, ProcessLockKind::StateMutation)?;
        let store = Store::open(paths.database_path())?;
        let mut event = event;
        normalize_event_links(&store, &mut event)?;
        store.append_event(&event)
    })
    .await
    .map_err(|error| {
        MissiveError::orchestration("joining webhook event persistence task").with_source(error)
    })?
}

fn normalize_event_links(store: &Store, event: &mut EventInsert) -> Result<()> {
    if let Some(context_id) = event.context_id.clone()
        && store.get_context(&context_id)?.is_none()
    {
        store.upsert_context(&ContextUpsert::new(context_id))?;
    }

    if let Some(task_id) = event.task_id.clone()
        && store.get_task(&task_id)?.is_none()
    {
        event.task_id = None;
    }

    Ok(())
}

fn webhook_metadata(
    path: &str,
    payload_kind: &str,
    protocol_version: &str,
    content_type: Option<&str>,
    remote_addr: Option<&str>,
    auth_validated: bool,
    valid_payload: bool,
) -> Result<Metadata> {
    let mut metadata = Metadata::new();
    metadata.insert_str("webhook.path", path.to_owned())?;
    metadata.insert_str("webhook.payload_kind", payload_kind.to_owned())?;
    metadata.insert("webhook.auth_validated", json!(auth_validated))?;
    metadata.insert("webhook.valid_payload", json!(valid_payload))?;
    metadata.insert_str(METADATA_A2A_PROTOCOL_VERSION, protocol_version.to_owned())?;
    if let Some(content_type) = content_type {
        metadata.insert_str("webhook.content_type", content_type.to_owned())?;
    }
    if let Some(remote_addr) = remote_addr {
        metadata.insert_str("webhook.remote_addr", remote_addr.to_owned())?;
    }
    Ok(metadata)
}

fn accepted_event(
    state: &WebhookAppState,
    details: &PayloadDetails,
    record: &EventRecord,
    remote_addr: Option<String>,
) -> WebhookAccepted {
    let task_id = details
        .task_id
        .as_ref()
        .map(|task_id| task_id.as_str().to_owned());
    let context_id = details
        .context_id
        .as_ref()
        .map(|context_id| context_id.as_str().to_owned());
    let message = match &task_id {
        Some(task_id) => format!(
            "Accepted A2A push {} callback for task {task_id}",
            details.event_type
        ),
        None => format!("Accepted A2A push {} callback", details.event_type),
    };
    WebhookAccepted {
        profile: state.profile.clone(),
        event_sequence: record.sequence,
        event_id: record.event_id.as_str().to_owned(),
        event_type: record.event_type.clone(),
        payload_kind: details.kind.to_owned(),
        task_id,
        context_id,
        state: details.state.clone(),
        artifact_id: details.artifact_id.clone(),
        remote_addr,
        message,
    }
}

impl PayloadDetails {
    fn from_stream_response(response: &StreamResponse) -> Result<Self> {
        match response {
            StreamResponse::Task(task) => Ok(Self {
                kind: "task",
                event_type: "task",
                task_id: Some(TaskId::new(task.id.clone())?),
                context_id: Some(ContextId::new(task.context_id.clone())?),
                state: Some(task_state_label(&task.status.state).to_owned()),
                artifact_id: None,
            }),
            StreamResponse::Message(message) => Ok(Self {
                kind: "message",
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
                artifact_id: None,
            }),
            StreamResponse::StatusUpdate(update) => Ok(Self {
                kind: "status_update",
                event_type: "status_update",
                task_id: Some(TaskId::new(update.task_id.clone())?),
                context_id: Some(ContextId::new(update.context_id.clone())?),
                state: Some(task_state_label(&update.status.state).to_owned()),
                artifact_id: None,
            }),
            StreamResponse::ArtifactUpdate(update) => Ok(Self {
                kind: "artifact_update",
                event_type: "artifact_update",
                task_id: Some(TaskId::new(update.task_id.clone())?),
                context_id: Some(ContextId::new(update.context_id.clone())?),
                state: None,
                artifact_id: Some(update.artifact.artifact_id.clone()),
            }),
        }
    }
}

fn new_event_id(prefix: &str) -> Result<EventId> {
    EventId::new(format!(
        "evt/webhook/{prefix}/{}",
        protocol::new_message_id()
    ))
}

fn validate_webhook_path(path: &str) -> Result<()> {
    if !path.starts_with('/') || path == "/" {
        return Err(MissiveError::validation(
            "webhook path must start with '/' and include a non-root path segment",
        )
        .with_help("Use a callback path such as /a2a/push."));
    }
    if path.contains('?') || path.contains('#') || path.chars().any(char::is_whitespace) {
        return Err(MissiveError::validation(
            "webhook path must not contain whitespace, query strings, or fragments",
        ));
    }
    Ok(())
}

fn validate_header_name(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        return Err(MissiveError::validation(
            "webhook auth header name cannot be empty",
        ));
    }
    HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
        MissiveError::validation("webhook auth header name is not a valid HTTP header name")
            .with_source(error)
            .with_help("Use an ASCII HTTP header name such as Authorization or X-Webhook-Token.")
    })?;
    Ok(())
}

fn validate_auth_scheme(scheme: &str) -> Result<()> {
    if scheme.trim().is_empty() {
        return Err(MissiveError::validation(
            "webhook auth scheme cannot be empty; use 'none' for a raw token comparison",
        ));
    }
    if scheme.chars().any(char::is_whitespace) || scheme.chars().any(char::is_control) {
        return Err(MissiveError::validation(
            "webhook auth scheme must not contain whitespace or control characters",
        ));
    }
    Ok(())
}

fn expected_auth_header_value(scheme: Option<&str>, token: &str) -> String {
    match scheme {
        Some(scheme) => format!("{scheme} {token}"),
        None => token.to_owned(),
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let max_len = left.len().max(right.len());
    let mut diff = left.len() ^ right.len();
    for index in 0..max_len {
        let left_byte = left.get(index).copied().unwrap_or(0);
        let right_byte = right.get(index).copied().unwrap_or(0);
        diff |= usize::from(left_byte ^ right_byte);
    }
    diff == 0
}

fn task_state_label(state: &protocol::TaskState) -> &'static str {
    match state {
        protocol::TaskState::Submitted => "submitted",
        protocol::TaskState::Working => "working",
        protocol::TaskState::Completed => "completed",
        protocol::TaskState::Failed | protocol::TaskState::Rejected => "failed",
        protocol::TaskState::Canceled => "cancelled",
        protocol::TaskState::InputRequired | protocol::TaskState::AuthRequired => "input_required",
        protocol::TaskState::Unspecified => "unknown",
    }
}

fn local_url(addr: SocketAddr, path: &str) -> String {
    let host = if addr.ip().is_ipv6() {
        format!("[{}]", addr.ip())
    } else {
        addr.ip().to_string()
    };
    format!("http://{host}:{}{}", addr.port(), path)
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
            | "apiKey"
            | "password"
            | "secret"
            | "credentials"
            | "cookie"
            | "setcookie"
    ) || normalized.contains("token")
        || normalized.contains("secret")
        || normalized.contains("password")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::AUTHORIZATION;

    #[test]
    fn webhook_path_validation_rejects_root_and_queries() {
        assert!(validate_webhook_path("/a2a/push").is_ok());
        assert!(validate_webhook_path("/").is_err());
        assert!(validate_webhook_path("a2a/push").is_err());
        assert!(validate_webhook_path("/a2a/push?token=value").is_err());
    }

    #[test]
    fn webhook_auth_compares_scheme_or_raw_token() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, "Bearer expected".parse().expect("header"));
        let auth = WebhookAuth::Header {
            name: "Authorization".to_owned(),
            token: "expected".to_owned(),
            scheme: Some("Bearer".to_owned()),
        };
        assert_eq!(auth.validate_headers(&headers), Ok(true));
        let auth = WebhookAuth::Header {
            name: "Authorization".to_owned(),
            token: "wrong".to_owned(),
            scheme: Some("Bearer".to_owned()),
        };
        assert!(auth.validate_headers(&headers).is_err());
    }

    #[test]
    fn payload_details_extracts_status_update_ids() {
        let raw = json!({
            "statusUpdate": {
                "taskId": "task-1",
                "contextId": "ctx-1",
                "status": { "state": "TASK_STATE_WORKING" }
            }
        });
        let response: StreamResponse = serde_json::from_value(raw).expect("stream response");
        let details = PayloadDetails::from_stream_response(&response).expect("details");
        assert_eq!(details.kind, "status_update");
        assert_eq!(details.event_type, "status_update");
        assert_eq!(details.task_id.expect("task").as_str(), "task-1");
        assert_eq!(details.context_id.expect("context").as_str(), "ctx-1");
        assert_eq!(details.state.as_deref(), Some("working"));
    }

    #[test]
    fn redaction_covers_token_like_payload_keys() {
        let value = json!({
            "token": "secret",
            "nested": { "credentials": "secret", "safe": "ok" }
        });
        let redacted = redact_json(&value);
        assert_eq!(redacted["token"], REDACTED);
        assert_eq!(redacted["nested"]["credentials"], REDACTED);
        assert_eq!(redacted["nested"]["safe"], "ok");
    }
}
