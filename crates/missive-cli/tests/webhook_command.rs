use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
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

#[derive(Debug)]
struct WebhookProcess {
    child: Child,
    port: u16,
    stdout_lines: Arc<Mutex<Vec<String>>>,
    stdout_reader: Option<JoinHandle<()>>,
    line_rx: mpsc::Receiver<String>,
}

impl WebhookProcess {
    fn spawn(home: &Path, extra_env: &[(&str, &str)], args: &[&str]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_missive"));
        command.env_clear();
        add_required_child_environment(&mut command);
        command
            .env("MISSIVE_HOME", home)
            .arg("webhook")
            .arg("run")
            .arg("--bind-address")
            .arg("127.0.0.1")
            .arg("--port")
            .arg("0");
        for (name, value) in extra_env {
            command.env(name, value);
        }
        for arg in args {
            command.arg(arg);
        }
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn receiver");

        let stdout = child.stdout.take().expect("stdout pipe");
        let stdout_lines = Arc::new(Mutex::new(Vec::new()));
        let reader_lines = Arc::clone(&stdout_lines);
        let (line_tx, line_rx) = mpsc::channel();
        let stdout_reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                reader_lines
                    .lock()
                    .expect("stdout line buffer lock")
                    .push(line.clone());
                if line_tx.send(line).is_err() {
                    break;
                }
            }
        });

        let mut process = Self {
            child,
            port: 0,
            stdout_lines,
            stdout_reader: Some(stdout_reader),
            line_rx,
        };
        process.port = process.wait_for_started();
        process.wait_for_health();
        process
    }

    fn port(&self) -> u16 {
        self.port
    }

    fn wait_for_started(&mut self) -> u16 {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            self.panic_if_exited("receiver exited before startup event was emitted");
            match self.line_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(line) => {
                    let Ok(value) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    if value["kind"] != "webhook_started" {
                        continue;
                    }
                    let bind_address = value["data"]["bind_address"]
                        .as_str()
                        .expect("webhook_started bind address");
                    let addr: SocketAddr = bind_address
                        .parse()
                        .expect("webhook_started socket address");
                    return addr.port();
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.panic_if_exited("receiver stdout closed before startup event was emitted");
                    panic!("receiver stdout closed before startup event was emitted");
                }
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let stdout = self.collected_stdout();
        let stderr = self.drain_stderr();
        panic!(
            "receiver did not emit startup event before timeout; stdout: {stdout}; stderr: {stderr}"
        );
    }

    fn wait_for_health(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            self.panic_if_exited("receiver exited before health check succeeded");
            if TcpStream::connect(("127.0.0.1", self.port)).is_ok() {
                let (status, _response) = http_request(self.port, "GET", "/healthz", &[], "");
                if status == 200 {
                    return;
                }
            }
            thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let stdout = self.collected_stdout();
        let stderr = self.drain_stderr();
        let port = self.port;
        panic!(
            "receiver did not become healthy on port {port}; stdout: {stdout}; stderr: {stderr}"
        );
    }

    fn wait(mut self, timeout: Duration) -> (ExitStatus, String, String) {
        let deadline = Instant::now() + timeout;
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("poll child") {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!(
                    "receiver did not exit before timeout; stdout: {}; stderr: {}",
                    self.collected_stdout(),
                    self.drain_stderr()
                );
            }
            thread::sleep(Duration::from_millis(50));
        };

        let stderr = self.drain_stderr();
        if let Some(reader) = self.stdout_reader.take() {
            reader.join().expect("stdout reader thread");
        }
        (status, self.collected_stdout(), stderr)
    }

    fn panic_if_exited(&mut self, message: &str) {
        if let Some(status) = self.child.try_wait().expect("poll child") {
            panic!(
                "{message}: {status}; stdout: {}; stderr: {}",
                self.collected_stdout(),
                self.drain_stderr()
            );
        }
    }

    fn collected_stdout(&self) -> String {
        self.stdout_lines
            .lock()
            .expect("stdout line buffer lock")
            .join("\n")
    }

    fn drain_stderr(&mut self) -> String {
        let mut stderr = String::new();
        if let Some(mut pipe) = self.child.stderr.take() {
            pipe.read_to_string(&mut stderr).expect("read stderr");
        }
        stderr
    }
}

fn add_required_child_environment(command: &mut Command) {
    #[cfg(not(windows))]
    let _ = command;

    #[cfg(windows)]
    {
        for key in ["SystemRoot", "WINDIR", "TEMP", "TMP", "PATH", "PATHEXT"] {
            if let Ok(value) = std::env::var(key) {
                command.env(key, value);
            }
        }
    }
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

    let child = WebhookProcess::spawn(
        &home,
        &[("MISSIVE_WEBHOOK_TOKEN", "local-test-callback-token")],
        &[
            "--auth-token-env",
            "MISSIVE_WEBHOOK_TOKEN",
            "--max-events",
            "1",
            "--ndjson",
        ],
    );
    let port = child.port();

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

    let (status, stdout, stderr) = child.wait(Duration::from_secs(10));
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
