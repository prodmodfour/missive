use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use missive_cli::{OUTPUT_SCHEMA_VERSION, run_from_with_environment};
use missive_core::{AgentAlias, ContextId, MissiveExitCode, TaskId};
use missive_store::{ContextUpsert, Store, TaskSource, TaskState, TaskUpsert};
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

fn json_success(stdout: &str, expected_kind: &str) -> Value {
    let value: Value = serde_json::from_str(stdout).expect("stdout should be JSON");
    assert_eq!(value["schema_version"], OUTPUT_SCHEMA_VERSION);
    assert_eq!(value["ok"], true);
    assert_eq!(value["kind"], expected_kind);
    value
}

fn json_error(stderr: &str) -> Value {
    let value: Value = serde_json::from_str(stderr).expect("stderr should be JSON");
    assert_eq!(value["schema_version"], OUTPUT_SCHEMA_VERSION);
    assert_eq!(value["ok"], false);
    assert_eq!(value["kind"], "error");
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
        "name": "Task Agent",
        "description": "Manages task state for tests.",
        "supportedInterfaces": [
            {
                "url": format!("{base_url}/a2a"),
                "protocolBinding": "HTTP+JSON",
                "protocolVersion": "1.0"
            }
        ],
        "version": "1.0.0",
        "capabilities": {
            "streaming": true,
            "pushNotifications": false,
            "extendedAgentCard": false
        },
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"],
        "skills": [
            {
                "id": "tasks",
                "name": "Tasks",
                "description": "Reports task state",
                "tags": ["test"],
                "inputModes": ["text/plain"],
                "outputModes": ["text/plain"]
            }
        ]
    })
}

fn task_json(id: &str, context_id: &str, state: &str, text: &str) -> Value {
    json!({
        "id": id,
        "contextId": context_id,
        "status": {
            "state": state,
            "message": {
                "messageId": format!("msg-{id}-{state}"),
                "role": "ROLE_AGENT",
                "parts": [{"text": text}]
            },
            "timestamp": "2026-05-22T00:00:00Z"
        },
        "history": [],
        "artifacts": [
            {
                "artifactId": format!("artifact-{id}"),
                "parts": [{"text": format!("artifact for {id}")}]
            }
        ]
    })
}

fn task_json_with_multiple_artifacts() -> Value {
    json!({
        "id": "task-artifacts",
        "contextId": "ctx-artifacts",
        "status": {
            "state": "TASK_STATE_COMPLETED",
            "message": {
                "messageId": "msg-task-artifacts",
                "role": "ROLE_AGENT",
                "parts": [{"text": "done with artifacts"}]
            },
            "timestamp": "2026-05-22T00:00:00Z"
        },
        "history": [],
        "artifacts": [
            {
                "artifactId": "artifact-text",
                "name": "../unsafe.txt",
                "description": "Text answer",
                "parts": [{"text": "hello artifact", "mediaType": "text/plain"}],
                "metadata": {"quality": "ok"}
            },
            {
                "artifactId": "artifact-json",
                "name": "data",
                "parts": [{"data": {"answer": 42}, "mediaType": "application/json"}]
            },
            {
                "artifactId": "artifact-file",
                "name": "remote-file",
                "parts": [{
                    "url": "file:///tmp/remote.txt",
                    "filename": "../../remote.txt",
                    "mediaType": "text/plain"
                }]
            }
        ]
    })
}

fn list_response(tasks: Vec<Value>) -> Value {
    json!({
        "tasks": tasks,
        "nextPageToken": "",
        "pageSize": 50,
        "totalSize": 1
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
fn task_list_filters_local_store() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    add_agent("echo", "http://127.0.0.1:65530", &environment, temp.path());

    let store = open_store(&home);
    let agent = AgentAlias::new("echo").expect("agent");
    let context = ContextId::new("ctx-filtered").expect("context");
    store
        .upsert_context(&ContextUpsert::new(context.clone()))
        .expect("context");
    let mut included = TaskUpsert::new(
        TaskId::new("task-included").expect("task id"),
        agent.clone(),
        TaskState::Working,
    );
    included.context_id = Some(context.clone());
    included.source = TaskSource::Remote;
    included.remote_task_json = Some(task_json(
        "task-included",
        "ctx-filtered",
        "TASK_STATE_WORKING",
        "working",
    ));
    included
        .record_a2a_protocol_version("1.0")
        .expect("protocol version");
    store.upsert_task(&included).expect("included task");

    let mut excluded = TaskUpsert::new(
        TaskId::new("task-excluded").expect("task id"),
        agent,
        TaskState::Submitted,
    );
    excluded.context_id = Some(context);
    excluded.source = TaskSource::Local;
    store.upsert_task(&excluded).expect("excluded task");

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "task",
            "list",
            "--agent",
            "echo",
            "--context",
            "ctx-filtered",
            "--state",
            "working",
            "--source",
            "remote",
            "--updated-after",
            "1970-01-01T00:00:00Z",
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_success(&stdout, "task_list");
    assert_eq!(value["data"]["count"], 1);
    assert_eq!(value["data"]["tasks"][0]["task_id"], "task-included");
    assert_eq!(value["data"]["tasks"][0]["state"], "working");
    assert_eq!(value["data"]["tasks"][0]["source"], "remote");
}

#[test]
fn task_list_remote_applies_a2a_filters_and_persists_tasks() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let server = MockServer::start(|base_url| {
        vec![
            MockResponse::ok_a2a_json(agent_card(base_url)),
            MockResponse::ok_a2a_json(list_response(vec![task_json(
                "task-list-1",
                "ctx-list",
                "TASK_STATE_WORKING",
                "remote list working",
            )])),
        ]
    });
    add_agent("echo", &server.base_url, &environment, temp.path());

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "task",
            "list",
            "--agent",
            "echo",
            "--remote",
            "--context",
            "ctx-list",
            "--state",
            "working",
            "--updated-after",
            "2026-05-21T00:00:00Z",
            "--include-artifacts",
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "task_list");
    assert_eq!(value["data"]["source"], "remote");
    assert_eq!(value["data"]["tasks"][0]["task_id"], "task-list-1");
    assert_eq!(value["data"]["tasks"][0]["text"], "remote list working");
    assert_eq!(value["data"]["tasks"][0]["artifact_count"], 1);

    let requests = server.requests();
    assert_eq!(requests[0].path, "/.well-known/agent-card.json");
    assert_eq!(requests[1].method, "GET");
    assert!(requests[1].path.starts_with("/a2a/tasks?"));
    assert!(requests[1].path.contains("contextId=ctx-list"));
    assert!(requests[1].path.contains("status=TASK_STATE_WORKING"));
    assert!(
        requests[1]
            .path
            .contains("statusTimestampAfter=2026-05-21T00%3A00%3A00%2B00%3A00")
    );
    assert!(requests[1].path.contains("includeArtifacts=true"));
    assert_eq!(
        requests[1].headers.get("a2a-version").map(String::as_str),
        Some("1.0")
    );

    let store = open_store(&home);
    let task = store
        .get_task(&TaskId::new("task-list-1").expect("task id"))
        .expect("get task")
        .expect("task persisted");
    assert_eq!(task.state, TaskState::Working);
    let artifacts = store
        .list_artifacts_for_task(&TaskId::new("task-list-1").expect("task id"))
        .expect("task artifacts");
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].artifact_id.as_str(), "artifact-task-list-1");
}

#[test]
fn task_artifact_commands_list_show_save_and_export_safely() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let server = MockServer::start(|base_url| {
        vec![
            MockResponse::ok_a2a_json(agent_card(base_url)),
            MockResponse::ok_a2a_json(task_json_with_multiple_artifacts()),
        ]
    });
    add_agent("echo", &server.base_url, &environment, temp.path());

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "task",
            "get",
            "task-artifacts",
            "--remote",
            "--agent",
            "echo",
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "task_get");
    assert_eq!(value["data"]["task"]["artifact_count"], 3);
    let artifact_text = value["data"]["task"]["artifacts"]
        .as_array()
        .expect("artifact summaries")
        .iter()
        .find(|artifact| artifact["artifact_id"] == "artifact-text")
        .expect("artifact-text summary");
    assert_eq!(artifact_text["metadata"]["a2a.description"], "Text answer");
    assert_eq!(artifact_text["metadata"]["a2a.metadata"]["quality"], "ok");

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "task",
            "artifact",
            "list",
            "task-artifacts",
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "task_artifact_list");
    assert_eq!(value["data"]["count"], 3);

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "task",
            "artifact",
            "show",
            "task-artifacts",
            "artifact-json",
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "task_artifact_show");
    assert_eq!(value["data"]["artifact"]["kind"], "json");
    assert_eq!(
        value["data"]["artifact"]["content"]["parts"][0]["data"]["answer"],
        42
    );

    let save_dir = temp.path().join("saved");
    std::fs::create_dir(&save_dir).expect("save dir");
    let (code, stdout, stderr) = run(
        &[
            "missive",
            "task",
            "artifact",
            "save",
            "task-artifacts",
            "artifact-text",
            "--output",
            save_dir.to_str().expect("save dir"),
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "task_artifact_save");
    let saved_path = save_dir.join("unsafe.txt");
    assert_eq!(
        value["data"]["artifact"]["path"],
        saved_path.display().to_string()
    );
    assert_eq!(
        std::fs::read_to_string(&saved_path).expect("artifact file"),
        "hello artifact"
    );
    assert!(!temp.path().join("unsafe.txt").exists());

    let export_dir = temp.path().join("exported");
    let (code, stdout, stderr) = run(
        &[
            "missive",
            "task",
            "artifact",
            "export",
            "task-artifacts",
            "--output-dir",
            export_dir.to_str().expect("export dir"),
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "task_artifact_export");
    assert_eq!(value["data"]["count"], 3);
    assert_eq!(
        std::fs::read_to_string(export_dir.join("unsafe.txt")).expect("text"),
        "hello artifact"
    );
    let json_export: Value = serde_json::from_str(
        &std::fs::read_to_string(export_dir.join("data.json")).expect("json export"),
    )
    .expect("json export parses");
    assert_eq!(json_export["answer"], 42);
    let file_export: Value = serde_json::from_str(
        &std::fs::read_to_string(export_dir.join("remote-file.json")).expect("file export"),
    )
    .expect("file export parses");
    assert_eq!(file_export["files"][0]["url"], "file:///tmp/remote.txt");
    assert_eq!(file_export["files"][0]["filename"], "../../remote.txt");
}

#[test]
fn task_wait_polls_remote_until_completed() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let server = MockServer::start(|base_url| {
        vec![
            MockResponse::ok_a2a_json(agent_card(base_url)),
            MockResponse::ok_a2a_json(task_json(
                "task-wait",
                "ctx-wait",
                "TASK_STATE_WORKING",
                "working",
            )),
            MockResponse::ok_a2a_json(task_json(
                "task-wait",
                "ctx-wait",
                "TASK_STATE_COMPLETED",
                "done",
            )),
        ]
    });
    add_agent("echo", &server.base_url, &environment, temp.path());

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "task",
            "wait",
            "task-wait",
            "--agent",
            "echo",
            "--timeout",
            "2s",
            "--interval",
            "10ms",
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_success(&stdout, "task_wait");
    assert_eq!(value["data"]["status"], "completed");
    assert_eq!(value["data"]["attempts"], 2);
    assert_eq!(value["data"]["task"]["text"], "done");

    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].path, "/a2a/tasks/task-wait");
    assert_eq!(requests[2].path, "/a2a/tasks/task-wait");

    let store = open_store(&home);
    let task = store
        .get_task(&TaskId::new("task-wait").expect("task id"))
        .expect("get task")
        .expect("task persisted");
    assert_eq!(task.state, TaskState::Completed);
}

#[test]
fn task_wait_uses_deterministic_non_success_codes() {
    let cases = [
        ("TASK_STATE_FAILED", "failed", MissiveExitCode::TaskFailed),
        (
            "TASK_STATE_CANCELED",
            "cancelled",
            MissiveExitCode::TaskCancelled,
        ),
        (
            "TASK_STATE_INPUT_REQUIRED",
            "input_required",
            MissiveExitCode::TaskInputRequired,
        ),
    ];

    for (wire_state, local_state, expected_code) in cases {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join(format!("missive-home-{local_state}"));
        let environment = isolated_env(&home);
        let server = MockServer::start(|base_url| {
            vec![
                MockResponse::ok_a2a_json(agent_card(base_url)),
                MockResponse::ok_a2a_json(task_json(
                    "task-terminal",
                    "ctx-terminal",
                    wire_state,
                    local_state,
                )),
            ]
        });
        add_agent("echo", &server.base_url, &environment, temp.path());

        let (code, stdout, stderr) = run(
            &[
                "missive",
                "task",
                "wait",
                "task-terminal",
                "--agent",
                "echo",
                "--timeout",
                "1s",
                "--interval",
                "10ms",
                "--json",
            ],
            &environment,
            temp.path(),
        );

        assert_eq!(code, expected_code.as_i32(), "stderr: {stderr}");
        assert_eq!(
            json_success(&stdout, "task_wait")["data"]["status"],
            local_state
        );
        assert_eq!(
            json_error(&stderr)["data"]["exit_code"],
            expected_code.as_u8()
        );
    }
}

#[test]
fn task_wait_times_out_with_deterministic_code() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let server = MockServer::start(|base_url| {
        let mut responses = vec![MockResponse::ok_a2a_json(agent_card(base_url))];
        for _ in 0..8 {
            responses.push(MockResponse::ok_a2a_json(task_json(
                "task-timeout",
                "ctx-timeout",
                "TASK_STATE_WORKING",
                "still working",
            )));
        }
        responses
    });
    add_agent("echo", &server.base_url, &environment, temp.path());

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "task",
            "wait",
            "task-timeout",
            "--agent",
            "echo",
            "--timeout",
            "25ms",
            "--interval",
            "10ms",
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(
        code,
        MissiveExitCode::TaskTimeout.as_i32(),
        "stderr: {stderr}"
    );
    let value = json_success(&stdout, "task_wait");
    assert_eq!(value["data"]["status"], "timeout");
    assert_eq!(value["data"]["timed_out"], true);
    assert_eq!(
        json_error(&stderr)["data"]["exit_code"],
        MissiveExitCode::TaskTimeout.as_u8()
    );
}

#[test]
fn task_cancel_requests_remote_cancellation_and_updates_store() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let server = MockServer::start(|base_url| {
        vec![
            MockResponse::ok_a2a_json(agent_card(base_url)),
            MockResponse::ok_a2a_json(task_json(
                "task-cancel",
                "ctx-cancel",
                "TASK_STATE_CANCELED",
                "cancelled",
            )),
        ]
    });
    add_agent("echo", &server.base_url, &environment, temp.path());

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "task",
            "cancel",
            "task-cancel",
            "--agent",
            "echo",
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_success(&stdout, "task_cancel");
    assert_eq!(value["data"]["task"]["task_id"], "task-cancel");
    assert_eq!(value["data"]["task"]["state"], "cancelled");

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].method, "POST");
    assert_eq!(requests[1].path, "/a2a/tasks/task-cancel:cancel");
    assert!(requests[1].body.is_empty());

    let store = open_store(&home);
    let task = store
        .get_task(&TaskId::new("task-cancel").expect("task id"))
        .expect("get task")
        .expect("task persisted");
    assert_eq!(task.state, TaskState::Cancelled);
}
