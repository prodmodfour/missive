#![doc = "Reusable local A2A test fixtures for missive integration tests."]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use missive_a2a::protocol;
use serde_json::{Value, json};

const DEFAULT_PROTOCOL_VERSION: &str = "1.0";
const HTTP_JSON_INTERFACE_PATH: &str = "/a2a";
const JSON_RPC_INTERFACE_PATH: &str = "/rpc";
const AGENT_CARD_PATH: &str = "/.well-known/agent-card.json";

/// One HTTP request captured by [`MockA2aServer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRequest {
    /// HTTP request method, for example `GET` or `POST`.
    pub method: String,
    /// Request target path as sent by the client, including any query string.
    pub path: String,
    /// Lowercase HTTP headers.
    pub headers: BTreeMap<String, String>,
    /// UTF-8 decoded request body. A2A fixture requests are expected to be JSON.
    pub body: String,
}

impl RecordedRequest {
    /// Returns a header value by case-insensitive name.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    /// Parses the request body as JSON for assertions.
    pub fn json_body(&self) -> serde_json::Result<Value> {
        serde_json::from_str(&self.body)
    }

    fn path_without_query(&self) -> &str {
        self.path
            .split_once('?')
            .map_or(&self.path, |(path, _)| path)
    }
}

/// Required auth header for the mock server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockAuthRequirement {
    header: String,
    value: String,
}

impl MockAuthRequirement {
    /// Creates an auth requirement from one exact header/value pair.
    #[must_use]
    pub fn new(header: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            header: header.into().to_ascii_lowercase(),
            value: value.into(),
        }
    }
}

#[derive(Debug, Clone)]
struct MockA2aConfig {
    supported_protocol_versions: BTreeSet<String>,
    streaming: bool,
    push_notifications: bool,
    require_auth: Option<MockAuthRequirement>,
    malformed_agent_card: bool,
    malformed_send_response: bool,
    malformed_task_response: bool,
    malformed_stream_event: bool,
    malformed_json_rpc_envelope: bool,
    send_response_delay: Option<Duration>,
}

impl Default for MockA2aConfig {
    fn default() -> Self {
        Self {
            supported_protocol_versions: BTreeSet::from([DEFAULT_PROTOCOL_VERSION.to_owned()]),
            streaming: true,
            push_notifications: true,
            require_auth: None,
            malformed_agent_card: false,
            malformed_send_response: false,
            malformed_task_response: false,
            malformed_stream_event: false,
            malformed_json_rpc_envelope: false,
            send_response_delay: None,
        }
    }
}

/// Builder for a reusable local A2A fixture server.
#[derive(Debug, Clone, Default)]
pub struct MockA2aServerBuilder {
    config: MockA2aConfig,
}

impl MockA2aServerBuilder {
    /// Enables or disables `capabilities.streaming` in the served Agent Card.
    #[must_use]
    pub fn streaming(mut self, enabled: bool) -> Self {
        self.config.streaming = enabled;
        self
    }

    /// Enables or disables `capabilities.pushNotifications` in the served Agent Card.
    #[must_use]
    pub fn push_notifications(mut self, enabled: bool) -> Self {
        self.config.push_notifications = enabled;
        self
    }

    /// Requires a specific header value on every mock A2A endpoint.
    #[must_use]
    pub fn require_auth_header(
        mut self,
        header: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.config.require_auth = Some(MockAuthRequirement::new(header, value));
        self
    }

    /// Restricts accepted `A2A-Version` request header values.
    ///
    /// Requests without `A2A-Version` are accepted so low-level fixture tests can
    /// call endpoints directly. Requests with a header outside this set receive a
    /// `VERSION_NOT_SUPPORTED` A2A error body.
    #[must_use]
    pub fn supported_protocol_versions<I, S>(mut self, versions: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let versions = versions
            .into_iter()
            .map(Into::into)
            .filter(|version: &String| !version.trim().is_empty())
            .collect::<BTreeSet<_>>();
        self.config.supported_protocol_versions = if versions.is_empty() {
            BTreeSet::from([DEFAULT_PROTOCOL_VERSION.to_owned()])
        } else {
            versions
        };
        self
    }

    /// Serves invalid JSON for public Agent Card discovery.
    #[must_use]
    pub fn malformed_agent_card(mut self) -> Self {
        self.config.malformed_agent_card = true;
        self
    }

    /// Serves a syntactically valid but protocol-invalid SendMessage response.
    #[must_use]
    pub fn malformed_send_response(mut self) -> Self {
        self.config.malformed_send_response = true;
        self
    }

    /// Serves syntactically valid but protocol-invalid task responses.
    #[must_use]
    pub fn malformed_task_response(mut self) -> Self {
        self.config.malformed_task_response = true;
        self
    }

    /// Serves a malformed SSE event for streaming requests.
    #[must_use]
    pub fn malformed_stream_event(mut self) -> Self {
        self.config.malformed_stream_event = true;
        self
    }

    /// Serves malformed JSON-RPC envelopes for non-streaming JSON-RPC methods.
    #[must_use]
    pub fn malformed_json_rpc_envelope(mut self) -> Self {
        self.config.malformed_json_rpc_envelope = true;
        self
    }

    /// Delays SendMessage responses by the requested duration.
    #[must_use]
    pub fn send_response_delay(mut self, delay: Duration) -> Self {
        self.config.send_response_delay = Some(delay);
        self
    }

    /// Starts the server on `127.0.0.1` with an ephemeral port.
    #[must_use]
    pub fn start(self) -> MockA2aServer {
        MockA2aServer::start_with_config(self.config)
    }
}

/// Handle for mutating fixture state after the server has started.
#[derive(Debug, Clone)]
pub struct MockA2aHandle {
    state: Arc<SharedState>,
}

impl MockA2aHandle {
    /// Replaces the SendMessage response returned by both HTTP+JSON and JSON-RPC.
    pub fn set_send_response(&self, response: Value) {
        self.state.with_inner(|inner| {
            inner.send_response = response;
        });
    }

    /// Replaces the SSE stream event sequence returned by both streaming bindings.
    pub fn set_stream_events(&self, events: Vec<Value>) {
        self.state.with_inner(|inner| {
            inner.stream_events = events;
        });
    }

    /// Sets a single stable task response for a task id.
    pub fn set_task(&self, task_id: impl Into<String>, task: Value) {
        self.state.with_inner(|inner| {
            inner.tasks.insert(task_id.into(), VecDeque::from([task]));
        });
    }

    /// Enqueues task responses for successive GetTask calls.
    ///
    /// Once the queue reaches its final value, the final task is returned for all
    /// later reads. ListTasks observes the current front of each queue without
    /// advancing it.
    pub fn enqueue_task_sequence<I>(&self, task_id: impl Into<String>, tasks: I)
    where
        I: IntoIterator<Item = Value>,
    {
        let mut tasks = tasks.into_iter().collect::<VecDeque<_>>();
        if tasks.is_empty() {
            let task_id = task_id.into();
            tasks.push_back(task_json(
                &task_id,
                &format!("ctx-{task_id}"),
                "TASK_STATE_SUBMITTED",
                "queued",
            ));
            self.state.with_inner(|inner| {
                inner.tasks.insert(task_id, tasks);
            });
        } else {
            self.state.with_inner(|inner| {
                inner.tasks.insert(task_id.into(), tasks);
            });
        }
    }

    /// Enqueues task responses from state names such as `TASK_STATE_WORKING`.
    pub fn enqueue_task_states<I, S>(
        &self,
        task_id: impl Into<String>,
        context_id: impl Into<String>,
        states: I,
    ) where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let task_id = task_id.into();
        let context_id = context_id.into();
        let tasks = states
            .into_iter()
            .map(|state| {
                let state = state.as_ref();
                task_json(
                    &task_id,
                    &context_id,
                    state,
                    &format!("mock task {task_id} is {state}"),
                )
            })
            .collect::<Vec<_>>();
        self.enqueue_task_sequence(task_id, tasks);
    }

    /// Inserts or replaces one push notification config fixture.
    pub fn insert_push_config(&self, config: Value) {
        let task_id = config
            .get("taskId")
            .and_then(Value::as_str)
            .unwrap_or("task-fixture-1")
            .to_owned();
        let id = config
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("push-fixture-1")
            .to_owned();
        self.state.with_inner(|inner| {
            inner.push_configs.insert((task_id, id), config);
        });
    }

    /// Returns all push configs currently held by the fixture.
    #[must_use]
    pub fn push_configs(&self) -> Vec<Value> {
        self.state
            .with_inner(|inner| inner.push_configs.values().cloned().collect::<Vec<Value>>())
    }
}

/// A local mock A2A server for integration tests.
#[derive(Debug)]
pub struct MockA2aServer {
    base_url: String,
    address: SocketAddr,
    shutdown: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    state: Arc<SharedState>,
}

impl MockA2aServer {
    /// Starts a default mock server with HTTP+JSON, JSON-RPC, streaming, tasks,
    /// push config endpoints, default auth-free access, and A2A `1.0` support.
    #[must_use]
    pub fn start() -> Self {
        Self::builder().start()
    }

    /// Returns a configurable server builder.
    #[must_use]
    pub fn builder() -> MockA2aServerBuilder {
        MockA2aServerBuilder::default()
    }

    fn start_with_config(config: MockA2aConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock A2A server");
        let address = listener.local_addr().expect("mock A2A local addr");
        let base_url = format!("http://{address}");
        let state = Arc::new(SharedState::new(base_url.clone(), config));
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_state = Arc::clone(&state);
        let thread_shutdown = Arc::clone(&shutdown);
        let join = thread::spawn(move || serve(listener, thread_state, thread_shutdown));

        Self {
            base_url,
            address,
            shutdown,
            join: Some(join),
            state,
        }
    }

    /// Base URL used for public Agent Card discovery.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// HTTP+JSON interface URL advertised in the Agent Card.
    #[must_use]
    pub fn http_json_interface_url(&self) -> String {
        format!("{}{}", self.base_url, HTTP_JSON_INTERFACE_PATH)
    }

    /// JSON-RPC interface URL advertised in the Agent Card.
    #[must_use]
    pub fn json_rpc_interface_url(&self) -> String {
        format!("{}{}", self.base_url, JSON_RPC_INTERFACE_PATH)
    }

    /// Returns a cloneable handle for mutating fixture state.
    #[must_use]
    pub fn handle(&self) -> MockA2aHandle {
        MockA2aHandle {
            state: Arc::clone(&self.state),
        }
    }

    /// Returns all requests captured so far in arrival order.
    #[must_use]
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.state.with_inner(|inner| inner.requests.clone())
    }
}

impl Drop for MockA2aServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[derive(Debug)]
struct SharedState {
    inner: Mutex<MockStateInner>,
}

impl SharedState {
    fn new(base_url: String, config: MockA2aConfig) -> Self {
        Self {
            inner: Mutex::new(MockStateInner::new(base_url, config)),
        }
    }

    fn with_inner<T>(&self, run: impl FnOnce(&mut MockStateInner) -> T) -> T {
        let mut inner = self.inner.lock().expect("mock A2A state lock");
        run(&mut inner)
    }
}

#[derive(Debug)]
struct MockStateInner {
    base_url: String,
    config: MockA2aConfig,
    requests: Vec<RecordedRequest>,
    send_response: Value,
    tasks: BTreeMap<String, VecDeque<Value>>,
    stream_events: Vec<Value>,
    push_configs: BTreeMap<(String, String), Value>,
}

impl MockStateInner {
    fn new(base_url: String, config: MockA2aConfig) -> Self {
        Self {
            base_url,
            config,
            requests: Vec::new(),
            send_response: send_message_response_message(
                "msg-mock-response",
                "ctx-mock-response",
                "mock response",
            ),
            tasks: BTreeMap::new(),
            stream_events: vec![status_update_event(
                "task-stream-fixture",
                "ctx-stream-fixture",
                "TASK_STATE_COMPLETED",
                Some("mock stream completed"),
            )],
            push_configs: BTreeMap::new(),
        }
    }

    fn current_tasks(&self) -> Vec<Value> {
        self.tasks
            .values()
            .filter_map(|queue| queue.front().cloned())
            .collect()
    }

    fn next_task(&mut self, task_id: &str) -> Value {
        let queue = self.tasks.entry(task_id.to_owned()).or_insert_with(|| {
            VecDeque::from([task_json(
                task_id,
                &format!("ctx-{task_id}"),
                "TASK_STATE_SUBMITTED",
                "mock task",
            )])
        });
        if queue.len() > 1 {
            queue.pop_front().expect("non-empty task queue")
        } else {
            queue.front().cloned().expect("non-empty task queue")
        }
    }

    fn set_cancelled_task(&mut self, task_id: &str) -> Value {
        let context_id = self
            .tasks
            .get(task_id)
            .and_then(|queue| queue.front())
            .and_then(|task| task.get("contextId"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("ctx-{task_id}"));
        let task = task_json(task_id, &context_id, "TASK_STATE_CANCELED", "cancelled");
        self.tasks
            .insert(task_id.to_owned(), VecDeque::from([task.clone()]));
        task
    }
}

#[derive(Debug, Clone)]
struct HttpResponse {
    status: u16,
    reason: &'static str,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
}

impl HttpResponse {
    fn json(status: u16, reason: &'static str, content_type: &'static str, body: Value) -> Self {
        Self {
            status,
            reason,
            headers: vec![("Content-Type", content_type.to_owned())],
            body: body.to_string().into_bytes(),
        }
    }

    fn raw_json(status: u16, reason: &'static str, content_type: &'static str, body: &str) -> Self {
        Self {
            status,
            reason,
            headers: vec![("Content-Type", content_type.to_owned())],
            body: body.as_bytes().to_vec(),
        }
    }

    fn sse(body: String) -> Self {
        Self {
            status: 200,
            reason: "OK",
            headers: vec![("Content-Type", "text/event-stream".to_owned())],
            body: body.into_bytes(),
        }
    }
}

fn serve(listener: TcpListener, state: Arc<SharedState>, shutdown: Arc<AtomicBool>) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            if shutdown.load(Ordering::SeqCst) {
                break;
            }
            continue;
        };
        if shutdown.load(Ordering::SeqCst) {
            break;
        }

        if let Some(request) = read_request(&mut stream) {
            let response = state.with_inner(|inner| {
                inner.requests.push(request.clone());
                response_for_request(inner, &request)
            });
            let _ = write_response(&mut stream, &response);
        }
    }
}

fn response_for_request(inner: &mut MockStateInner, request: &RecordedRequest) -> HttpResponse {
    let is_rpc = request.path_without_query() == JSON_RPC_INTERFACE_PATH;
    if !auth_matches(&inner.config, request) {
        return unauthorized_response(is_rpc, request);
    }
    if !version_matches(&inner.config, request) {
        return version_not_supported_response(is_rpc, request);
    }

    let path = request.path_without_query();
    match (request.method.as_str(), path) {
        ("GET", AGENT_CARD_PATH) => {
            if inner.config.malformed_agent_card {
                HttpResponse::raw_json(200, "OK", "application/a2a+json", "{not valid json")
            } else {
                HttpResponse::json(
                    200,
                    "OK",
                    "application/a2a+json",
                    agent_card_json(
                        &inner.base_url,
                        &inner.config.supported_protocol_versions,
                        inner.config.streaming,
                        inner.config.push_notifications,
                    ),
                )
            }
        }
        ("POST", "/a2a/message:send") => rest_send_response(inner),
        ("POST", "/a2a/message:stream") => stream_response(inner, None),
        ("GET", "/a2a/tasks") => rest_list_tasks_response(inner),
        ("POST", JSON_RPC_INTERFACE_PATH) => json_rpc_response(inner, request),
        _ => route_dynamic_rest(inner, request),
    }
}

fn route_dynamic_rest(inner: &mut MockStateInner, request: &RecordedRequest) -> HttpResponse {
    let path = request.path_without_query();
    let segments = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
    if segments.len() >= 3 && segments[0] == "a2a" && segments[1] == "tasks" {
        let task_segment = segments[2];
        if (request.method == "POST" || request.method == "GET")
            && task_segment.ends_with(":subscribe")
            && segments.len() == 3
        {
            return stream_response(inner, None);
        }
        if request.method == "POST" && task_segment.ends_with(":cancel") && segments.len() == 3 {
            if inner.config.malformed_task_response {
                return malformed_protocol_response("application/a2a+json");
            }
            let task_id = task_segment.trim_end_matches(":cancel");
            return HttpResponse::json(
                200,
                "OK",
                "application/a2a+json",
                inner.set_cancelled_task(task_id),
            );
        }
        if request.method == "GET" && segments.len() == 3 {
            if inner.config.malformed_task_response {
                return malformed_protocol_response("application/a2a+json");
            }
            return HttpResponse::json(
                200,
                "OK",
                "application/a2a+json",
                inner.next_task(task_segment),
            );
        }
        if let Some(response) = route_rest_push_config(inner, request, &segments) {
            return response;
        }
    }

    HttpResponse::json(
        404,
        "Not Found",
        "application/json",
        json!({"error": "not found", "path": request.path}),
    )
}

fn route_rest_push_config(
    inner: &mut MockStateInner,
    request: &RecordedRequest,
    segments: &[&str],
) -> Option<HttpResponse> {
    if segments.len() < 4 || !is_push_config_collection_segment(segments[3]) {
        return None;
    }
    let task_id = segments[2];
    match (request.method.as_str(), segments.len()) {
        ("POST", 4) => {
            let config = normalize_push_config_json(task_id, None, request_json_or_null(request));
            let id = config_id_from_value(&config);
            inner
                .push_configs
                .insert((task_id.to_owned(), id), config.clone());
            Some(HttpResponse::json(
                200,
                "OK",
                "application/a2a+json",
                config,
            ))
        }
        ("GET", 4) => Some(list_push_configs_response(
            inner,
            task_id,
            "application/a2a+json",
        )),
        ("GET", 5) => Some(get_push_config_response(
            inner,
            task_id,
            segments[4],
            "application/a2a+json",
        )),
        ("DELETE", 5) => {
            inner
                .push_configs
                .remove(&(task_id.to_owned(), segments[4].to_owned()));
            Some(HttpResponse::json(
                200,
                "OK",
                "application/a2a+json",
                json!({"deleted": true, "taskId": task_id, "id": segments[4]}),
            ))
        }
        _ => None,
    }
}

fn json_rpc_response(inner: &mut MockStateInner, request: &RecordedRequest) -> HttpResponse {
    let rpc = request_json_or_null(request);
    let id = rpc.get("id").cloned().unwrap_or(Value::Null);
    let method = rpc
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = rpc.get("params").cloned().unwrap_or(Value::Null);

    if method == protocol::jsonrpc_methods::SEND_STREAMING_MESSAGE
        || method == protocol::jsonrpc_methods::SUBSCRIBE_TO_TASK
    {
        return stream_response(inner, Some(id));
    }

    if inner.config.malformed_json_rpc_envelope {
        return HttpResponse::json(
            200,
            "OK",
            "application/json",
            json!({"jsonrpc": "2.0", "result": {"malformed": true}}),
        );
    }

    let result = match method {
        protocol::jsonrpc_methods::SEND_MESSAGE => {
            apply_send_delay(&inner.config);
            if inner.config.malformed_send_response {
                json!({"unexpected": true})
            } else {
                inner.send_response.clone()
            }
        }
        protocol::jsonrpc_methods::GET_TASK => {
            if inner.config.malformed_task_response {
                json!({"unexpected": true})
            } else {
                let task_id = params
                    .get("id")
                    .or_else(|| params.get("taskId"))
                    .and_then(Value::as_str)
                    .unwrap_or("task-fixture-1");
                inner.next_task(task_id)
            }
        }
        protocol::jsonrpc_methods::LIST_TASKS => {
            if inner.config.malformed_task_response {
                json!({"unexpected": true})
            } else {
                json!({"tasks": inner.current_tasks()})
            }
        }
        protocol::jsonrpc_methods::CANCEL_TASK => {
            if inner.config.malformed_task_response {
                json!({"unexpected": true})
            } else {
                let task_id = params
                    .get("id")
                    .or_else(|| params.get("taskId"))
                    .and_then(Value::as_str)
                    .unwrap_or("task-fixture-1");
                inner.set_cancelled_task(task_id)
            }
        }
        protocol::jsonrpc_methods::CREATE_PUSH_CONFIG => {
            let task_id = params
                .get("taskId")
                .and_then(Value::as_str)
                .unwrap_or("task-fixture-1")
                .to_owned();
            let config = normalize_push_config_json(&task_id, None, params);
            let id = config_id_from_value(&config);
            inner.push_configs.insert((task_id, id), config.clone());
            config
        }
        protocol::jsonrpc_methods::GET_PUSH_CONFIG => {
            let task_id = params
                .get("taskId")
                .and_then(Value::as_str)
                .unwrap_or("task-fixture-1");
            let id = params
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("push-fixture-1");
            inner
                .push_configs
                .get(&(task_id.to_owned(), id.to_owned()))
                .cloned()
                .unwrap_or_else(|| push_config_json(task_id, id, "http://127.0.0.1/push"))
        }
        protocol::jsonrpc_methods::LIST_PUSH_CONFIGS => {
            let task_id = params
                .get("taskId")
                .and_then(Value::as_str)
                .unwrap_or("task-fixture-1");
            let configs = inner
                .push_configs
                .iter()
                .filter(|((stored_task_id, _), _)| *stored_task_id == task_id)
                .map(|(_, config)| config.clone())
                .collect::<Vec<_>>();
            json!({"configs": configs})
        }
        protocol::jsonrpc_methods::DELETE_PUSH_CONFIG => {
            let task_id = params
                .get("taskId")
                .and_then(Value::as_str)
                .unwrap_or("task-fixture-1");
            let id = params
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("push-fixture-1");
            inner
                .push_configs
                .remove(&(task_id.to_owned(), id.to_owned()));
            json!({"deleted": true, "taskId": task_id, "id": id})
        }
        _ => {
            let error = json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("unknown JSON-RPC method {method}"),
                }
            });
            return HttpResponse::json(200, "OK", "application/json", error);
        }
    };

    HttpResponse::json(
        200,
        "OK",
        "application/json",
        json!({"jsonrpc": "2.0", "id": id, "result": result}),
    )
}

fn rest_send_response(inner: &MockStateInner) -> HttpResponse {
    apply_send_delay(&inner.config);
    if inner.config.malformed_send_response {
        malformed_protocol_response("application/a2a+json")
    } else {
        HttpResponse::json(
            200,
            "OK",
            "application/a2a+json",
            inner.send_response.clone(),
        )
    }
}

fn apply_send_delay(config: &MockA2aConfig) {
    if let Some(delay) = config.send_response_delay {
        thread::sleep(delay);
    }
}

fn rest_list_tasks_response(inner: &MockStateInner) -> HttpResponse {
    if inner.config.malformed_task_response {
        malformed_protocol_response("application/a2a+json")
    } else {
        HttpResponse::json(
            200,
            "OK",
            "application/a2a+json",
            json!({"tasks": inner.current_tasks()}),
        )
    }
}

fn stream_response(inner: &MockStateInner, json_rpc_id: Option<Value>) -> HttpResponse {
    let mut body = String::new();
    if inner.config.malformed_stream_event {
        body.push_str("event: malformed\n");
        body.push_str("data: {not valid stream json\n\n");
        return HttpResponse::sse(body);
    }

    for event in &inner.stream_events {
        body.push_str("event: message\n");
        let data = if let Some(id) = &json_rpc_id {
            json!({"jsonrpc": "2.0", "id": id, "result": event}).to_string()
        } else {
            event.to_string()
        };
        body.push_str("data: ");
        body.push_str(&data);
        body.push_str("\n\n");
    }
    HttpResponse::sse(body)
}

fn malformed_protocol_response(content_type: &'static str) -> HttpResponse {
    HttpResponse::json(
        200,
        "OK",
        content_type,
        json!({"unexpected": true, "reason": "malformed fixture response"}),
    )
}

fn list_push_configs_response(
    inner: &MockStateInner,
    task_id: &str,
    content_type: &'static str,
) -> HttpResponse {
    let configs = inner
        .push_configs
        .iter()
        .filter(|((stored_task_id, _), _)| *stored_task_id == task_id)
        .map(|(_, config)| config.clone())
        .collect::<Vec<_>>();
    HttpResponse::json(200, "OK", content_type, json!({"configs": configs}))
}

fn get_push_config_response(
    inner: &MockStateInner,
    task_id: &str,
    id: &str,
    content_type: &'static str,
) -> HttpResponse {
    if let Some(config) = inner
        .push_configs
        .get(&(task_id.to_owned(), id.to_owned()))
        .cloned()
    {
        HttpResponse::json(200, "OK", content_type, config)
    } else {
        HttpResponse::json(
            404,
            "Not Found",
            "application/json",
            json!({"error": "push config not found", "taskId": task_id, "id": id}),
        )
    }
}

fn auth_matches(config: &MockA2aConfig, request: &RecordedRequest) -> bool {
    config
        .require_auth
        .as_ref()
        .is_none_or(|required| request.header(&required.header) == Some(required.value.as_str()))
}

fn version_matches(config: &MockA2aConfig, request: &RecordedRequest) -> bool {
    request
        .header(protocol::SVC_PARAM_VERSION)
        .is_none_or(|version| config.supported_protocol_versions.contains(version))
}

fn unauthorized_response(is_rpc: bool, request: &RecordedRequest) -> HttpResponse {
    if is_rpc {
        let id = request_json_or_null(request)
            .get("id")
            .cloned()
            .unwrap_or(Value::Null);
        HttpResponse::json(
            200,
            "OK",
            "application/json",
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": 401, "message": "unauthorized"}
            }),
        )
    } else {
        HttpResponse::json(
            401,
            "Unauthorized",
            "application/json",
            json!({"error": "unauthorized"}),
        )
    }
}

fn version_not_supported_response(is_rpc: bool, request: &RecordedRequest) -> HttpResponse {
    let code = protocol::error_code::VERSION_NOT_SUPPORTED;
    if is_rpc {
        let id = request_json_or_null(request)
            .get("id")
            .cloned()
            .unwrap_or(Value::Null);
        HttpResponse::json(
            200,
            "OK",
            "application/json",
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": code,
                    "message": "VERSION_NOT_SUPPORTED",
                    "data": {"reason": "VERSION_NOT_SUPPORTED"}
                }
            }),
        )
    } else {
        HttpResponse::json(
            400,
            "Bad Request",
            "application/a2a+json",
            json!({
                "error": {
                    "code": code,
                    "message": "VERSION_NOT_SUPPORTED",
                    "data": {"reason": "VERSION_NOT_SUPPORTED"}
                }
            }),
        )
    }
}

fn agent_card_json(
    base_url: &str,
    supported_protocol_versions: &BTreeSet<String>,
    streaming: bool,
    push_notifications: bool,
) -> Value {
    let protocol_version = supported_protocol_versions
        .iter()
        .next()
        .map(String::as_str)
        .unwrap_or(DEFAULT_PROTOCOL_VERSION);
    json!({
        "name": "missive mock A2A agent",
        "description": "Local fixture server for missive integration tests.",
        "supportedInterfaces": [
            {
                "url": format!("{base_url}{HTTP_JSON_INTERFACE_PATH}"),
                "protocolBinding": "HTTP+JSON",
                "protocolVersion": protocol_version,
            },
            {
                "url": format!("{base_url}{JSON_RPC_INTERFACE_PATH}"),
                "protocolBinding": "JSONRPC",
                "protocolVersion": protocol_version,
            },
        ],
        "provider": {
            "url": "https://example.test/missive/mock-a2a",
            "organization": "missive test fixtures",
        },
        "version": "0.1.0-fixture",
        "capabilities": {
            "streaming": streaming,
            "pushNotifications": push_notifications,
            "extendedAgentCard": false,
        },
        "defaultInputModes": ["text/plain", "application/json"],
        "defaultOutputModes": ["text/plain", "application/json"],
        "skills": [
            {
                "id": "echo",
                "name": "Echo",
                "description": "Returns deterministic fixture responses.",
                "tags": ["mock", "test"],
                "inputModes": ["text/plain", "application/json"],
                "outputModes": ["text/plain", "application/json"],
            },
        ],
    })
}

/// Builds a simple A2A agent message JSON value.
#[must_use]
pub fn message_json(message_id: &str, context_id: &str, text: &str) -> Value {
    json!({
        "messageId": message_id,
        "contextId": context_id,
        "role": "ROLE_AGENT",
        "parts": [{"text": text, "mediaType": "text/plain"}],
    })
}

/// Builds a simple A2A task JSON value with one status message and text artifact.
#[must_use]
pub fn task_json(task_id: &str, context_id: &str, state: &str, text: &str) -> Value {
    json!({
        "id": task_id,
        "contextId": context_id,
        "status": {
            "state": state,
            "message": message_json(&format!("msg-{task_id}-{state}"), context_id, text),
            "timestamp": "2026-05-22T00:00:00Z",
        },
        "history": [],
        "artifacts": [
            {
                "artifactId": format!("artifact-{task_id}"),
                "name": "answer",
                "parts": [{"text": format!("artifact for {task_id}"), "mediaType": "text/plain"}],
            },
        ],
    })
}

/// Wraps a message as a SendMessageResponse fixture.
#[must_use]
pub fn send_message_response_message(message_id: &str, context_id: &str, text: &str) -> Value {
    json!({"message": message_json(message_id, context_id, text)})
}

/// Wraps a task as a SendMessageResponse fixture.
#[must_use]
pub fn send_message_response_task(task: Value) -> Value {
    json!({"task": task})
}

/// Builds a TaskStatusUpdateEvent stream response fixture.
#[must_use]
pub fn status_update_event(
    task_id: &str,
    context_id: &str,
    state: &str,
    text: Option<&str>,
) -> Value {
    let mut status = json!({
        "state": state,
        "timestamp": "2026-05-22T00:00:00Z",
    });
    if let Some(text) = text {
        status["message"] = message_json(&format!("msg-{task_id}-{state}"), context_id, text);
    }
    json!({
        "statusUpdate": {
            "taskId": task_id,
            "contextId": context_id,
            "status": status,
        }
    })
}

/// Builds a TaskArtifactUpdateEvent stream response fixture.
#[must_use]
pub fn artifact_update_event(
    task_id: &str,
    context_id: &str,
    artifact_id: &str,
    text: &str,
    append: bool,
    last_chunk: bool,
) -> Value {
    json!({
        "artifactUpdate": {
            "taskId": task_id,
            "contextId": context_id,
            "artifact": {
                "artifactId": artifact_id,
                "name": "answer",
                "parts": [{"text": text, "mediaType": "text/plain"}],
            },
            "append": append,
            "lastChunk": last_chunk,
        }
    })
}

/// Builds a TaskPushNotificationConfig fixture.
#[must_use]
pub fn push_config_json(task_id: &str, config_id: &str, url: &str) -> Value {
    json!({
        "id": config_id,
        "taskId": task_id,
        "url": url,
        "authentication": {
            "scheme": "Bearer",
            "credentials": "fixture-validation-value",
        },
    })
}

fn request_json_or_null(request: &RecordedRequest) -> Value {
    serde_json::from_str(&request.body).unwrap_or(Value::Null)
}

fn normalize_push_config_json(task_id: &str, id: Option<&str>, mut value: Value) -> Value {
    if !value.is_object() {
        value = push_config_json(
            task_id,
            id.unwrap_or("push-fixture-1"),
            "http://127.0.0.1/push",
        );
    }
    if value.get("taskId").and_then(Value::as_str).is_none() {
        value["taskId"] = Value::String(task_id.to_owned());
    }
    if value.get("id").and_then(Value::as_str).is_none() {
        value["id"] = Value::String(id.unwrap_or("push-fixture-1").to_owned());
    }
    if value.get("url").and_then(Value::as_str).is_none() {
        value["url"] = Value::String("http://127.0.0.1/push".to_owned());
    }
    value
}

fn config_id_from_value(value: &Value) -> String {
    value
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("push-fixture-1")
        .to_owned()
}

fn is_push_config_collection_segment(value: &str) -> bool {
    matches!(
        value,
        "push-configs" | "pushNotificationConfigs" | "push-notification-configs"
    )
}

fn read_request(stream: &mut TcpStream) -> Option<RecordedRequest> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut data = Vec::new();
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        match stream.read(&mut buffer) {
            Ok(0) => return None,
            Ok(read) => {
                data.extend_from_slice(&buffer[..read]);
                if let Some(position) = data.windows(4).position(|window| window == b"\r\n\r\n") {
                    break position + 4;
                }
            }
            Err(_) => return None,
        }
    };

    let header_text = String::from_utf8_lossy(&data[..header_end]);
    let mut lines = header_text.split("\r\n");
    let first = lines.next().unwrap_or_default();
    let mut first_parts = first.split_whitespace();
    let method = first_parts.next().unwrap_or_default().to_owned();
    let path = first_parts.next().unwrap_or_default().to_owned();
    let mut headers = BTreeMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }

    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    while data.len().saturating_sub(header_end) < content_length {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => data.extend_from_slice(&buffer[..read]),
            Err(_) => break,
        }
    }
    let body_end = header_end.saturating_add(content_length).min(data.len());
    let body = String::from_utf8_lossy(&data[header_end..body_end]).into_owned();

    Some(RecordedRequest {
        method,
        path,
        headers,
        body,
    })
}

fn write_response(stream: &mut TcpStream, response: &HttpResponse) -> std::io::Result<()> {
    let mut headers = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        response.reason,
        response.body.len()
    );
    for (name, value) in &response.headers {
        headers.push_str(name);
        headers.push_str(": ");
        headers.push_str(value);
        headers.push_str("\r\n");
    }
    headers.push_str("\r\n");
    stream.write_all(headers.as_bytes())?;
    stream.write_all(&response.body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_json_routes_support_tasks_push_and_recording() {
        let server = MockA2aServer::start();
        let handle = server.handle();
        handle.enqueue_task_states(
            "task-1",
            "ctx-1",
            ["TASK_STATE_WORKING", "TASK_STATE_COMPLETED"],
        );

        let client = reqwest::blocking::Client::new();
        let card: Value = client
            .get(format!("{}/.well-known/agent-card.json", server.base_url()))
            .send()
            .expect("agent card response")
            .json()
            .expect("agent card json");
        assert_eq!(card["capabilities"]["streaming"], true);
        assert_eq!(
            card["supportedInterfaces"]
                .as_array()
                .expect("interfaces")
                .len(),
            2
        );

        let first_task: Value = client
            .get(format!("{}/a2a/tasks/task-1", server.base_url()))
            .send()
            .expect("first task response")
            .json()
            .expect("first task json");
        let second_task: Value = client
            .get(format!("{}/a2a/tasks/task-1", server.base_url()))
            .send()
            .expect("second task response")
            .json()
            .expect("second task json");
        assert_eq!(first_task["status"]["state"], "TASK_STATE_WORKING");
        assert_eq!(second_task["status"]["state"], "TASK_STATE_COMPLETED");

        let subscription = client
            .post(format!("{}/a2a/tasks/task-1:subscribe", server.base_url()))
            .send()
            .expect("subscribe response")
            .text()
            .expect("subscribe SSE body");
        assert!(subscription.contains("data:"));

        let config = push_config_json("task-1", "push-1", "http://127.0.0.1/callback");
        let created: Value = client
            .post(format!(
                "{}/a2a/tasks/task-1/push-configs",
                server.base_url()
            ))
            .json(&config)
            .send()
            .expect("create push config response")
            .json()
            .expect("create push config json");
        assert_eq!(created["id"], "push-1");
        let listed: Value = client
            .get(format!(
                "{}/a2a/tasks/task-1/push-configs",
                server.base_url()
            ))
            .send()
            .expect("list push configs response")
            .json()
            .expect("list push configs json");
        assert_eq!(listed["configs"].as_array().expect("configs").len(), 1);
        let deleted: Value = client
            .delete(format!(
                "{}/a2a/tasks/task-1/push-configs/push-1",
                server.base_url()
            ))
            .send()
            .expect("delete push config response")
            .json()
            .expect("delete push config json");
        assert_eq!(deleted["deleted"], true);

        let requests = server.requests();
        assert_eq!(requests.len(), 7);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].path, AGENT_CARD_PATH);
        assert_eq!(requests[3].method, "POST");
        assert_eq!(requests[3].path, "/a2a/tasks/task-1:subscribe");
        assert_eq!(requests[4].method, "POST");
        assert_eq!(requests[4].json_body().expect("json body")["id"], "push-1");
    }

    #[test]
    fn json_rpc_routes_wrap_results_and_stream_events() {
        let server = MockA2aServer::start();
        let handle = server.handle();
        handle.set_send_response(send_message_response_task(task_json(
            "task-rpc-1",
            "ctx-rpc-1",
            "TASK_STATE_SUBMITTED",
            "submitted",
        )));
        handle.set_stream_events(vec![status_update_event(
            "task-rpc-1",
            "ctx-rpc-1",
            "TASK_STATE_COMPLETED",
            Some("done"),
        )]);

        let client = reqwest::blocking::Client::new();
        let send_response: Value = client
            .post(server.json_rpc_interface_url())
            .json(&json!({
                "jsonrpc": "2.0",
                "id": "rpc-1",
                "method": protocol::jsonrpc_methods::SEND_MESSAGE,
                "params": {"message": {"messageId": "msg-1", "role": "ROLE_USER", "parts": [{"text": "hello"}]}}
            }))
            .send()
            .expect("json-rpc send response")
            .json()
            .expect("json-rpc send json");
        assert_eq!(send_response["id"], "rpc-1");
        assert_eq!(send_response["result"]["task"]["id"], "task-rpc-1");

        let stream_response = client
            .post(server.json_rpc_interface_url())
            .json(&json!({
                "jsonrpc": "2.0",
                "id": "rpc-stream-1",
                "method": protocol::jsonrpc_methods::SEND_STREAMING_MESSAGE,
                "params": {"message": {"messageId": "msg-stream", "role": "ROLE_USER", "parts": [{"text": "stream"}]}}
            }))
            .send()
            .expect("json-rpc stream response")
            .text()
            .expect("json-rpc stream body");
        assert!(stream_response.contains("text/event-stream") || stream_response.contains("data:"));
        assert!(stream_response.contains("rpc-stream-1"));
        assert!(stream_response.contains("statusUpdate"));
    }

    #[test]
    fn auth_version_and_malformed_modes_are_controllable() {
        let server = MockA2aServer::builder()
            .require_auth_header("Authorization", "Bearer fixture-value")
            .supported_protocol_versions(["2.0"])
            .start();
        let client = reqwest::blocking::Client::new();

        let unauthorized = client
            .get(format!("{}/.well-known/agent-card.json", server.base_url()))
            .send()
            .expect("unauthorized response");
        assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);

        let version_error: Value = client
            .get(format!("{}/.well-known/agent-card.json", server.base_url()))
            .header("Authorization", "Bearer fixture-value")
            .header(protocol::SVC_PARAM_VERSION, DEFAULT_PROTOCOL_VERSION)
            .send()
            .expect("version error response")
            .json()
            .expect("version error json");
        assert_eq!(
            version_error["error"]["data"]["reason"],
            "VERSION_NOT_SUPPORTED"
        );

        let ok = client
            .get(format!("{}/.well-known/agent-card.json", server.base_url()))
            .header("Authorization", "Bearer fixture-value")
            .header(protocol::SVC_PARAM_VERSION, "2.0")
            .send()
            .expect("authorized response");
        assert_eq!(ok.status(), reqwest::StatusCode::OK);

        let malformed = MockA2aServer::builder().malformed_agent_card().start();
        let malformed_body = client
            .get(format!(
                "{}/.well-known/agent-card.json",
                malformed.base_url()
            ))
            .send()
            .expect("malformed card response")
            .text()
            .expect("malformed card body");
        assert_eq!(malformed_body, "{not valid json");
    }
}
