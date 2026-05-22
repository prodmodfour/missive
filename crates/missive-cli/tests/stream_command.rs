use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use missive_cli::{
    OUTPUT_SCHEMA_VERSION, run_from_with_environment, run_from_with_environment_and_input,
};
use missive_core::MissiveExitCode;
use missive_store::{MessageDirection, Store, TaskState};
use serde_json::{Value, json};
use tempfile::tempdir;

#[derive(Debug, Clone)]
struct RecordedRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: String,
}

#[derive(Debug, Clone)]
struct MockResponse {
    status: u16,
    reason: &'static str,
    headers: Vec<(&'static str, String)>,
    body: String,
}

impl MockResponse {
    fn ok_a2a_json(body: Value) -> Self {
        Self {
            status: 200,
            reason: "OK",
            headers: vec![("Content-Type", "application/a2a+json".to_owned())],
            body: body.to_string(),
        }
    }

    fn ok_sse(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            reason: "OK",
            headers: vec![("Content-Type", "text/event-stream".to_owned())],
            body: body.into(),
        }
    }
}

#[derive(Debug)]
struct MockServer {
    base_url: String,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
}

impl MockServer {
    fn start(build_responses: impl FnOnce(&str) -> Vec<MockResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let address = listener.local_addr().expect("local addr");
        let base_url = format!("http://{address}");
        let responses = build_responses(&base_url);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let thread_requests = Arc::clone(&requests);
        thread::spawn(move || {
            for response in responses {
                let Ok((mut stream, _peer)) = listener.accept() else {
                    return;
                };
                let request = read_request(&mut stream);
                thread_requests.lock().expect("requests lock").push(request);
                write_response(&mut stream, &response);
            }
        });

        Self { base_url, requests }
    }

    fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().expect("requests lock").clone()
    }
}

fn read_request(stream: &mut TcpStream) -> RecordedRequest {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set read timeout");
    let mut data = Vec::new();
    let mut buffer = [0_u8; 1024];
    let header_end = loop {
        match stream.read(&mut buffer) {
            Ok(0) => break data.len(),
            Ok(read) => {
                data.extend_from_slice(&buffer[..read]);
                if let Some(position) = data.windows(4).position(|window| window == b"\r\n\r\n") {
                    break position + 4;
                }
            }
            Err(_) => break data.len(),
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

    RecordedRequest {
        method,
        path,
        headers,
        body,
    }
}

fn write_response(stream: &mut TcpStream, response: &MockResponse) {
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
    stream.write_all(headers.as_bytes()).expect("write headers");
    stream
        .write_all(response.body.as_bytes())
        .expect("write body");
}

fn isolated_env(home: &Path) -> BTreeMap<String, String> {
    BTreeMap::from([(
        "MISSIVE_HOME".to_owned(),
        home.to_string_lossy().into_owned(),
    )])
}

fn run(
    args: &[&str],
    environment: &BTreeMap<String, String>,
    current_dir: &Path,
) -> (i32, String, String) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = run_from_with_environment(args, environment, current_dir, &mut stdout, &mut stderr);

    (
        code,
        String::from_utf8(stdout).expect("stdout should be UTF-8"),
        String::from_utf8(stderr).expect("stderr should be UTF-8"),
    )
}

fn run_with_input(
    args: &[&str],
    stdin: &str,
    environment: &BTreeMap<String, String>,
    current_dir: &Path,
) -> (i32, String, String) {
    let mut input = Cursor::new(stdin.as_bytes());
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = run_from_with_environment_and_input(
        args,
        environment,
        current_dir,
        &mut input,
        &mut stdout,
        &mut stderr,
    );

    (
        code,
        String::from_utf8(stdout).expect("stdout should be UTF-8"),
        String::from_utf8(stderr).expect("stderr should be UTF-8"),
    )
}

fn add_agent(
    alias: &str,
    base_url: &str,
    environment: &BTreeMap<String, String>,
    current_dir: &Path,
) {
    let (code, _stdout, stderr) = run(
        &["missive", "agent", "add", alias, base_url, "--json"],
        environment,
        current_dir,
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
}

fn agent_card(base_url: &str, streaming: bool) -> Value {
    json!({
        "name": "Streaming Agent",
        "description": "Streams status and artifact updates.",
        "supportedInterfaces": [
            {
                "url": format!("{base_url}/a2a"),
                "protocolBinding": "HTTP+JSON",
                "protocolVersion": "1.0"
            }
        ],
        "version": "1.0.0",
        "capabilities": {
            "streaming": streaming,
            "pushNotifications": false,
            "extendedAgentCard": false
        },
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain", "application/json"],
        "skills": [
            {
                "id": "stream",
                "name": "Stream",
                "description": "Streams test events",
                "tags": ["stream", "test"],
                "inputModes": ["text/plain"],
                "outputModes": ["text/plain"]
            }
        ]
    })
}

fn sse_data(value: Value) -> String {
    format!("data: {value}\n\n")
}

fn task_event(state: &str) -> Value {
    json!({
        "task": {
            "id": "task-stream-1",
            "contextId": "ctx-stream-1",
            "status": { "state": state },
            "history": []
        }
    })
}

fn status_event(state: &str, text: Option<&str>) -> Value {
    let mut status = json!({"state": state});
    if let Some(text) = text {
        status["message"] = json!({
            "messageId": format!("msg-{state}"),
            "role": "ROLE_AGENT",
            "parts": [{"text": text}]
        });
    }
    json!({
        "statusUpdate": {
            "taskId": "task-stream-1",
            "contextId": "ctx-stream-1",
            "status": status
        }
    })
}

fn artifact_event() -> Value {
    json!({
        "artifactUpdate": {
            "taskId": "task-stream-1",
            "contextId": "ctx-stream-1",
            "artifact": {
                "artifactId": "artifact-1",
                "name": "answer",
                "parts": [{"text": "partial answer"}]
            },
            "append": true,
            "lastChunk": false
        }
    })
}

fn open_store(home: &Path) -> Store {
    Store::open(
        home.join("state")
            .join("profiles")
            .join("default")
            .join("missive.sqlite3"),
    )
    .expect("open store")
}

fn ndjson_lines(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("NDJSON line should parse"))
        .inspect(|value| {
            assert_eq!(value["schema_version"], OUTPUT_SCHEMA_VERSION);
            assert_eq!(value["ok"], true);
        })
        .collect()
}

#[test]
fn stream_ndjson_persists_status_artifact_and_completion_events() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let server = MockServer::start(|base_url| {
        let body = [
            sse_data(task_event("TASK_STATE_SUBMITTED")),
            sse_data(status_event("TASK_STATE_WORKING", Some("thinking"))),
            sse_data(artifact_event()),
            sse_data(status_event("TASK_STATE_COMPLETED", None)),
        ]
        .join("");
        vec![
            MockResponse::ok_a2a_json(agent_card(base_url, true)),
            MockResponse::ok_sse(body),
        ]
    });
    add_agent("echo", &server.base_url, &environment, temp.path());

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "stream",
            "echo",
            "hello stream",
            "--metadata",
            "purpose=test",
            "--accepted-output-mode",
            "text/plain",
            "--ndjson",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let lines = ndjson_lines(&stdout);
    assert_eq!(lines.len(), 5);
    for (index, value) in lines.iter().enumerate() {
        assert_eq!(value["sequence"], index);
    }
    assert_eq!(lines[0]["kind"], "stream_event");
    assert_eq!(lines[0]["data"]["event_type"], "task");
    assert_eq!(lines[1]["data"]["event_type"], "status_update");
    assert_eq!(lines[1]["data"]["state"], "working");
    assert_eq!(lines[1]["data"]["text"], "thinking");
    assert_eq!(lines[2]["data"]["event_type"], "artifact_update");
    assert_eq!(lines[2]["data"]["artifact_id"], "artifact-1");
    assert_eq!(lines[3]["data"]["state"], "completed");
    assert_eq!(lines[4]["kind"], "stream_result");
    assert_eq!(lines[4]["data"]["event_count"], 4);
    assert_eq!(lines[4]["data"]["status_update_count"], 2);
    assert_eq!(lines[4]["data"]["artifact_update_count"], 1);
    assert_eq!(lines[4]["data"]["final_state"], "completed");

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, "/.well-known/agent-card.json");
    assert_eq!(requests[1].method, "POST");
    assert_eq!(requests[1].path, "/a2a/message:stream");
    assert_eq!(
        requests[1].headers.get("a2a-version").map(String::as_str),
        Some("1.0")
    );
    assert_eq!(
        requests[1].headers.get("accept").map(String::as_str),
        Some("text/event-stream")
    );
    let request_body: Value = serde_json::from_str(&requests[1].body).expect("request body");
    assert_eq!(request_body["message"]["parts"][0]["text"], "hello stream");
    assert_eq!(request_body["metadata"]["purpose"], "test");
    assert_eq!(
        request_body["configuration"]["acceptedOutputModes"],
        json!(["text/plain"])
    );

    let store = open_store(&home);
    let events = store.list_events().expect("events");
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].event_type, "a2a.stream.task");
    assert_eq!(events[1].event_type, "a2a.stream.status_update");
    assert_eq!(events[2].event_type, "a2a.stream.artifact_update");
    assert_eq!(events[3].event_type, "a2a.stream.status_update");
    assert!(events.iter().all(|event| event.redacted));

    let task = store
        .get_task(&"task-stream-1".parse().expect("task id"))
        .expect("get task")
        .expect("task persisted");
    assert_eq!(task.state, TaskState::Completed);
    assert_eq!(
        task.context_id.as_ref().map(ToString::to_string),
        Some("ctx-stream-1".to_owned())
    );

    let messages = store.list_messages().expect("messages");
    assert_eq!(messages[0].direction, MessageDirection::Request);
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.direction == MessageDirection::StreamEvent)
            .count(),
        4
    );
    assert!(messages.iter().any(|message| {
        task.last_message_id.as_ref().map(ToString::to_string)
            == Some(message.message_id.to_string())
    }));
}

#[test]
fn stream_rejects_missing_capability_unless_forced() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let server =
        MockServer::start(|base_url| vec![MockResponse::ok_a2a_json(agent_card(base_url, false))]);
    add_agent("echo", &server.base_url, &environment, temp.path());

    let (code, stdout, stderr) = run(
        &["missive", "stream", "echo", "hello"],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Usage.as_i32());
    assert!(stdout.is_empty());
    assert!(stderr.contains("does not advertise capabilities.streaming=true"));
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn stream_force_allows_interop_when_card_omits_capability() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let server = MockServer::start(|base_url| {
        vec![
            MockResponse::ok_a2a_json(agent_card(base_url, false)),
            MockResponse::ok_sse(sse_data(json!({
                "message": {
                    "messageId": "msg-force-stream",
                    "contextId": "ctx-force",
                    "role": "ROLE_AGENT",
                    "parts": [{"text": "forced stream ok"}]
                }
            }))),
        ]
    });
    add_agent("echo", &server.base_url, &environment, temp.path());

    let (code, stdout, stderr) = run_with_input(
        &["missive", "stream", "echo", "--stdin", "--force", "--json"],
        "forced hello",
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value: Value = serde_json::from_str(&stdout).expect("JSON output");
    assert_eq!(value["kind"], "stream_result");
    assert_eq!(value["data"]["capability"]["advertised_streaming"], false);
    assert_eq!(value["data"]["capability"]["forced"], true);
    assert_eq!(value["data"]["event_count"], 1);
    assert_eq!(value["data"]["events"][0]["text"], "forced stream ok");
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn stream_records_cancelled_status_as_terminal_state() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let server = MockServer::start(|base_url| {
        let body = [
            sse_data(task_event("TASK_STATE_WORKING")),
            sse_data(status_event(
                "TASK_STATE_CANCELED",
                Some("cancelled remotely"),
            )),
        ]
        .join("");
        vec![
            MockResponse::ok_a2a_json(agent_card(base_url, true)),
            MockResponse::ok_sse(body),
        ]
    });
    add_agent("echo", &server.base_url, &environment, temp.path());

    let (code, stdout, stderr) = run(
        &["missive", "stream", "echo", "cancel me", "--json"],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value: Value = serde_json::from_str(&stdout).expect("JSON output");
    assert_eq!(value["data"]["final_state"], "cancelled");
    let task = open_store(&home)
        .get_task(&"task-stream-1".parse().expect("task id"))
        .expect("get task")
        .expect("task persisted");
    assert_eq!(task.state, TaskState::Cancelled);
    assert!(task.completed_at.is_some());
}

#[test]
fn stream_malformed_sse_event_fails_with_protocol_error() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let server = MockServer::start(|base_url| {
        vec![
            MockResponse::ok_a2a_json(agent_card(base_url, true)),
            MockResponse::ok_sse("data: {\"unknown\":{}}\n\n"),
        ]
    });
    add_agent("echo", &server.base_url, &environment, temp.path());

    let (code, stdout, stderr) = run(
        &["missive", "stream", "echo", "bad event", "--ndjson"],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Protocol.as_i32());
    assert!(stdout.is_empty());
    assert!(stderr.contains("malformed A2A stream event"));
    assert_eq!(server.requests().len(), 2);
}
