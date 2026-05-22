use std::collections::BTreeMap;
use std::fs;
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

fn json_success(stdout: &str, expected_kind: &str) -> Value {
    let value: Value = serde_json::from_str(stdout).expect("stdout should be JSON");
    assert_eq!(value["schema_version"], OUTPUT_SCHEMA_VERSION);
    assert_eq!(value["ok"], true);
    assert_eq!(value["kind"], expected_kind);
    value
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

fn agent_card(base_url: &str) -> Value {
    json!({
        "name": "Echo Agent",
        "description": "Replies with whatever it receives.",
        "supportedInterfaces": [
            {
                "url": format!("{base_url}/a2a"),
                "protocolBinding": "HTTP+JSON",
                "protocolVersion": "1.0"
            }
        ],
        "provider": {
            "url": "https://example.test/provider",
            "organization": "Example Agents"
        },
        "version": "1.0.0",
        "capabilities": {
            "streaming": true,
            "pushNotifications": false,
            "extendedAgentCard": false
        },
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain", "application/json"],
        "skills": [
            {
                "id": "echo",
                "name": "Echo",
                "description": "Echoes text input",
                "tags": ["echo", "test"],
                "inputModes": ["text/plain"],
                "outputModes": ["text/plain"]
            }
        ]
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

#[test]
fn send_positional_message_persists_direct_message_response() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let server = MockServer::start(|base_url| {
        vec![
            MockResponse::ok_a2a_json(agent_card(base_url)),
            MockResponse::ok_a2a_json(json!({
                "message": {
                    "messageId": "msg-response-1",
                    "contextId": "ctx-direct-1",
                    "role": "ROLE_AGENT",
                    "parts": [{"text": "hello back"}]
                }
            })),
        ]
    });
    add_agent("echo", &server.base_url, &environment, temp.path());

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "send",
            "echo",
            "hello from cli",
            "--metadata",
            "purpose=test",
            "--accepted-output-mode",
            "text/plain",
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_success(&stdout, "send_result");
    assert_eq!(value["data"]["agent"], "echo");
    assert_eq!(value["data"]["selected_interface"]["binding"], "http+json");
    assert_eq!(value["data"]["request"]["part_count"], 1);
    assert_eq!(value["data"]["response"]["shape"], "message");
    assert_eq!(value["data"]["response"]["message_id"], "msg-response-1");
    assert_eq!(value["data"]["response"]["context_id"], "ctx-direct-1");
    assert_eq!(value["data"]["response"]["text"], "hello back");
    assert_eq!(value["data"]["persistence"]["context_id"], "ctx-direct-1");

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, "/.well-known/agent-card.json");
    assert_eq!(requests[1].method, "POST");
    assert_eq!(requests[1].path, "/a2a/message:send");
    assert!(
        requests[1]
            .headers
            .get("content-type")
            .expect("content type")
            .starts_with("application/a2a+json")
    );
    assert_eq!(
        requests[1].headers.get("a2a-version").map(String::as_str),
        Some("1.0")
    );
    let body: Value = serde_json::from_str(&requests[1].body).expect("request body");
    assert_eq!(body["message"]["role"], "ROLE_USER");
    assert_eq!(body["message"]["parts"][0]["text"], "hello from cli");
    assert_eq!(body["metadata"]["purpose"], "test");
    assert_eq!(
        body["configuration"]["acceptedOutputModes"],
        json!(["text/plain"])
    );

    let store = open_store(&home);
    let messages = store.list_messages().expect("messages");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].direction, MessageDirection::Request);
    assert_eq!(
        messages[0].context_id.as_ref().map(ToString::to_string),
        Some("ctx-direct-1".to_owned())
    );
    assert_eq!(messages[1].direction, MessageDirection::Response);
    assert_eq!(
        messages[1].protocol_message_id.as_deref(),
        Some("msg-response-1")
    );
}

#[test]
fn send_file_reference_persists_task_response_and_linkage() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let server = MockServer::start(|base_url| {
        vec![
            MockResponse::ok_a2a_json(agent_card(base_url)),
            MockResponse::ok_a2a_json(json!({
                "task": {
                    "id": "task-file-1",
                    "contextId": "ctx-task-1",
                    "status": {
                        "state": "TASK_STATE_SUBMITTED",
                        "message": {
                            "messageId": "msg-status-1",
                            "role": "ROLE_AGENT",
                            "parts": [{"text": "working"}]
                        }
                    },
                    "history": []
                }
            })),
        ]
    });
    add_agent("echo", &server.base_url, &environment, temp.path());
    let input_path = temp.path().join("message.txt");
    fs::write(&input_path, "file based hello").expect("write input file");

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "send",
            "echo",
            "--file",
            input_path.to_str().expect("input path"),
            "--mime",
            "text/plain",
            "--context",
            "ctx-requested",
            "--accepted-output-mode",
            "application/json",
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "send_result");
    assert_eq!(value["data"]["response"]["shape"], "task");
    assert_eq!(value["data"]["response"]["task_id"], "task-file-1");
    assert_eq!(value["data"]["response"]["context_id"], "ctx-task-1");
    assert_eq!(value["data"]["response"]["state"], "submitted");
    assert_eq!(value["data"]["persistence"]["task_id"], "task-file-1");
    assert_eq!(value["data"]["persistence"]["context_id"], "ctx-task-1");

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let body: Value = serde_json::from_str(&requests[1].body).expect("request body");
    assert_eq!(body["message"]["contextId"], "ctx-requested");
    let file_url = url::Url::from_file_path(fs::canonicalize(&input_path).expect("canonical path"))
        .expect("file url")
        .to_string();
    assert_eq!(body["message"]["parts"][0]["url"], file_url);
    assert_eq!(body["message"]["parts"][0]["filename"], "message.txt");
    assert_eq!(body["message"]["parts"][0]["mediaType"], "text/plain");
    assert_eq!(
        value["data"]["request"]["parts"][0]["kind"],
        "file_reference"
    );
    assert_eq!(
        body["configuration"]["acceptedOutputModes"],
        json!(["application/json"])
    );

    let store = open_store(&home);
    let task = store
        .get_task(&"task-file-1".parse().expect("task id"))
        .expect("get task")
        .expect("task persisted");
    assert_eq!(task.state, TaskState::Submitted);
    assert_eq!(
        task.context_id.as_ref().map(ToString::to_string),
        Some("ctx-task-1".to_owned())
    );
    assert_eq!(
        task.last_message_id.as_ref().map(ToString::to_string),
        Some("msg-status-1".to_owned())
    );

    let messages = store.list_messages().expect("messages");
    assert_eq!(messages.len(), 2);
    assert!(messages.iter().all(|message| {
        message.task_id.as_ref().map(ToString::to_string) == Some("task-file-1".to_owned())
            && message.context_id.as_ref().map(ToString::to_string) == Some("ctx-task-1".to_owned())
    }));
}

#[test]
fn send_stdin_input_reaches_remote_agent() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let server = MockServer::start(|base_url| {
        vec![
            MockResponse::ok_a2a_json(agent_card(base_url)),
            MockResponse::ok_a2a_json(json!({
                "message": {
                    "messageId": "msg-stdin-response",
                    "contextId": "ctx-stdin",
                    "role": "ROLE_AGENT",
                    "parts": [{"text": "stdin ack"}]
                }
            })),
        ]
    });
    add_agent("echo", &server.base_url, &environment, temp.path());

    let (code, stdout, stderr) = run_with_input(
        &["missive", "send", "echo", "--stdin", "--json"],
        "hello from stdin",
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert_eq!(
        json_success(&stdout, "send_result")["data"]["response"]["text"],
        "stdin ack"
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let body: Value = serde_json::from_str(&requests[1].body).expect("request body");
    assert_eq!(body["message"]["parts"][0]["text"], "hello from stdin");
}

#[test]
fn send_json_and_file_bytes_parts_reach_remote_agent() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let server = MockServer::start(|base_url| {
        vec![
            MockResponse::ok_a2a_json(agent_card(base_url)),
            MockResponse::ok_a2a_json(json!({
                "message": {
                    "messageId": "msg-rich-response",
                    "contextId": "ctx-rich",
                    "role": "ROLE_AGENT",
                    "parts": [{"text": "rich ack"}]
                }
            })),
        ]
    });
    add_agent("echo", &server.base_url, &environment, temp.path());
    let bytes_path = temp.path().join("payload.bin");
    fs::write(&bytes_path, [1_u8, 2, 3, 4]).expect("write bytes");

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "send",
            "echo",
            "text hello",
            "--file-bytes",
            bytes_path.to_str().expect("bytes path"),
            "--json-part",
            r#"{"kind":"sample","n":2}"#,
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "send_result");
    assert_eq!(value["data"]["request"]["part_count"], 3);
    assert_eq!(value["data"]["request"]["parts"][0]["kind"], "text");
    assert_eq!(value["data"]["request"]["parts"][1]["kind"], "file_bytes");
    assert_eq!(value["data"]["request"]["parts"][2]["kind"], "data");

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let body: Value = serde_json::from_str(&requests[1].body).expect("request body");
    assert_eq!(body["message"]["parts"][0]["text"], "text hello");
    assert_eq!(body["message"]["parts"][1]["raw"], "AQIDBA==");
    assert_eq!(body["message"]["parts"][1]["filename"], "payload.bin");
    assert_eq!(
        body["message"]["parts"][2]["data"],
        json!({"kind": "sample", "n": 2})
    );
    assert_eq!(body["message"]["parts"][2]["mediaType"], "application/json");
}

#[test]
fn send_large_file_bytes_respects_profile_size_limit() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let mut environment = isolated_env(&home);
    environment.insert("MISSIVE_REPO_CONFIG".to_owned(), "0".to_owned());
    let config_path = temp.path().join("small-limit.toml");
    fs::write(
        &config_path,
        r#"
schema_version = "missive.config.v1"
default_profile = "default"

[profiles.default]

[profiles.default.qos]
max_request_bytes = 4
"#,
    )
    .expect("write config");
    let bytes_path = temp.path().join("too-large.bin");
    fs::write(&bytes_path, [1_u8, 2, 3, 4, 5]).expect("write bytes");

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "send",
            "echo",
            "--file-bytes",
            bytes_path.to_str().expect("bytes path"),
            "--config",
            config_path.to_str().expect("config path"),
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Usage.as_i32(), "stderr: {stderr}");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert!(stderr.contains("qos.max_request_bytes"), "stderr: {stderr}");
    assert!(stderr.contains("too-large.bin"), "stderr: {stderr}");
}
