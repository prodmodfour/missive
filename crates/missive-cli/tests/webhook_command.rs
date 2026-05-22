use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use missive_core::{AgentAlias, ConfigDiscovery, ContextId, MissiveExitCode, TaskId};
use missive_store::{AgentUpsert, ContextUpsert, StatePathResolver, Store, TaskState, TaskUpsert};
use serde_json::Value;
use tempfile::tempdir;

fn isolated_env(home: &Path) -> BTreeMap<String, String> {
    BTreeMap::from([(
        "MISSIVE_HOME".to_owned(),
        home.to_string_lossy().into_owned(),
    )])
}

fn unused_local_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral port");
    listener.local_addr().expect("local addr").port()
}

fn http_request(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect receiver");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("read timeout");
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream.write_all(request.as_bytes()).expect("write request");

    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    let status = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok())
        .expect("HTTP status");
    (status, response)
}

fn wait_for_health(child: &mut Child, port: u16) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("poll child") {
            panic!("receiver exited before health check succeeded: {status}");
        }
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            let (status, _response) = http_request(port, "GET", "/healthz", &[], "");
            if status == 200 {
                return;
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("receiver did not become healthy on port {port}");
}

fn wait_for_child(mut child: Child, timeout: Duration) -> (ExitStatus, String, String) {
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll child") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("receiver did not exit before timeout");
        }
        thread::sleep(Duration::from_millis(50));
    };

    let mut stdout = String::new();
    let mut stderr = String::new();
    child
        .stdout
        .take()
        .expect("stdout pipe")
        .read_to_string(&mut stdout)
        .expect("read stdout");
    child
        .stderr
        .take()
        .expect("stderr pipe")
        .read_to_string(&mut stderr)
        .expect("read stderr");
    (status, stdout, stderr)
}

fn open_store(environment: &BTreeMap<String, String>) -> Store {
    let loaded = ConfigDiscovery::new()
        .with_env(
            environment
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        )
        .load()
        .expect("load default config");
    let resolver = StatePathResolver::new().with_env(
        environment
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    let paths = resolver.resolve_loaded(&loaded).expect("resolve paths");
    paths.ensure_directories().expect("state directories");
    Store::open(paths.database_path()).expect("open store")
}

#[test]
fn webhook_run_accepts_valid_push_payload_rejects_invalid_and_shuts_down() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let mut environment = isolated_env(&home);
    environment.insert(
        "MISSIVE_WEBHOOK_TOKEN".to_owned(),
        "local-test-callback-token".to_owned(),
    );
    let port = unused_local_port();

    let store = open_store(&environment);
    let alias = AgentAlias::new("fixture").expect("alias");
    store
        .upsert_agent(&AgentUpsert::new(alias.clone(), "http://127.0.0.1:1"))
        .expect("agent");
    let context_id = ContextId::new("ctx-webhook").expect("context");
    store
        .upsert_context(&ContextUpsert::new(context_id.clone()))
        .expect("context row");
    let task_id = TaskId::new("task-webhook").expect("task");
    let mut task = TaskUpsert::new(task_id, alias, TaskState::Working);
    task.context_id = Some(context_id);
    store.upsert_task(&task).expect("task row");

    let mut child = Command::new(env!("CARGO_BIN_EXE_missive"))
        .env_clear()
        .env("MISSIVE_HOME", &home)
        .env("MISSIVE_WEBHOOK_TOKEN", "local-test-callback-token")
        .arg("webhook")
        .arg("run")
        .arg("--bind-address")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .arg("--auth-token-env")
        .arg("MISSIVE_WEBHOOK_TOKEN")
        .arg("--max-events")
        .arg("1")
        .arg("--ndjson")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn receiver");

    wait_for_health(&mut child, port);

    let (status, _response) = http_request(
        port,
        "POST",
        "/a2a/push",
        &[("Content-Type", "application/json")],
        r#"{"statusUpdate":{"taskId":"task-webhook","contextId":"ctx-webhook","status":{"state":"TASK_STATE_WORKING"}}}"#,
    );
    assert_eq!(status, 401);

    let (status, _response) = http_request(
        port,
        "POST",
        "/a2a/push",
        &[
            ("Content-Type", "application/json"),
            ("Authorization", "Bearer local-test-callback-token"),
        ],
        r#"{"notAStreamResponse":true,"token":"must-redact"}"#,
    );
    assert_eq!(status, 400);

    let (status, response) = http_request(
        port,
        "POST",
        "/a2a/push",
        &[
            ("Content-Type", "application/json"),
            ("Authorization", "Bearer local-test-callback-token"),
            ("A2A-Version", "1.0"),
        ],
        r#"{"statusUpdate":{"taskId":"task-webhook","contextId":"ctx-webhook","status":{"state":"TASK_STATE_WORKING"}}}"#,
    );
    assert_eq!(status, 202, "response: {response}");

    let (status, stdout, stderr) = wait_for_child(child, Duration::from_secs(10));
    assert_eq!(
        status.code(),
        Some(MissiveExitCode::Success.as_i32()),
        "stderr: {stderr}"
    );
    assert!(stderr.is_empty(), "stderr: {stderr}");
    assert!(!stdout.contains("local-test-callback-token"));
    assert!(!stdout.contains("must-redact"));

    let lines: Vec<Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("NDJSON envelope"))
        .collect();
    assert!(lines.iter().any(|line| line["kind"] == "webhook_started"));
    assert!(lines.iter().any(|line| line["kind"] == "webhook_rejected"));
    assert!(lines.iter().any(|line| line["kind"] == "webhook_event"));
    assert!(lines.iter().any(|line| line["kind"] == "webhook_stopped"));
    let event = lines
        .iter()
        .find(|line| line["kind"] == "webhook_event")
        .expect("event line");
    assert_eq!(event["data"]["payload_kind"], "status_update");
    assert_eq!(event["data"]["task_id"], "task-webhook");
    assert_eq!(event["data"]["context_id"], "ctx-webhook");
    assert_eq!(event["data"]["state"], "working");

    let store = open_store(&environment);
    let events = store.list_events().expect("events");
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "a2a.push.status_update")
    );
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "a2a.push.rejected")
    );
    let accepted = events
        .iter()
        .find(|event| event.event_type == "a2a.push.status_update")
        .expect("accepted event");
    assert_eq!(
        accepted.task_id.as_ref().expect("task id").as_str(),
        "task-webhook"
    );
    assert_eq!(
        accepted.context_id.as_ref().expect("context id").as_str(),
        "ctx-webhook"
    );
    let rejected_payloads = events
        .iter()
        .filter(|event| event.event_type == "a2a.push.rejected")
        .map(|event| event.payload_json.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!rejected_payloads.contains("must-redact"));
    assert!(rejected_payloads.contains("[REDACTED]"));
}
