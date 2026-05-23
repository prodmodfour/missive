use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use missive_cli::run_from_with_environment;
use missive_core::{ConfigDiscovery, MissiveExitCode};
use missive_store::{StatePathResolver, Store};
use serde_json::Value;
use tempfile::tempdir;

fn isolated_env(home: &Path) -> BTreeMap<String, String> {
    BTreeMap::from([(
        "MISSIVE_HOME".to_owned(),
        home.to_string_lossy().into_owned(),
    )])
}

fn http_request(port: u16, method: &str, path: &str) -> (u16, String) {
    http_request_with_body(port, method, path, &[], "")
}

fn http_request_with_body(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect gateway");
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

fn response_body(response: &str) -> &str {
    response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .expect("HTTP body")
}

#[derive(Debug)]
struct GatewayProcess {
    child: Child,
    port: u16,
    stdout_lines: Arc<Mutex<Vec<String>>>,
    stdout_reader: Option<JoinHandle<()>>,
    line_rx: mpsc::Receiver<String>,
}

impl GatewayProcess {
    fn spawn(home: &Path, extra_env: &[(&str, &str)], args: &[&str]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_missive"));
        command.env_clear();
        add_required_child_environment(&mut command);
        command
            .env("MISSIVE_HOME", home)
            .arg("gateway")
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
            .expect("spawn gateway");

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
            self.panic_if_exited("gateway exited before startup event was emitted");
            match self.line_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(line) => {
                    let Ok(value) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    if value["kind"] != "gateway_started" {
                        continue;
                    }
                    let bind_address = value["data"]["bind_address"]
                        .as_str()
                        .expect("gateway_started bind address");
                    let addr: SocketAddr = bind_address
                        .parse()
                        .expect("gateway_started socket address");
                    return addr.port();
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.panic_if_exited("gateway stdout closed before startup event was emitted");
                    panic!("gateway stdout closed before startup event was emitted");
                }
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let stdout = self.collected_stdout();
        let stderr = self.drain_stderr();
        panic!(
            "gateway did not emit startup event before timeout; stdout: {stdout}; stderr: {stderr}"
        );
    }

    fn wait_for_health(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            self.panic_if_exited("gateway exited before health check succeeded");
            if TcpStream::connect(("127.0.0.1", self.port)).is_ok() {
                let (status, _response) = http_request(self.port, "GET", "/healthz");
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
        panic!("gateway did not become healthy on port {port}; stdout: {stdout}; stderr: {stderr}");
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
                    "gateway did not exit before timeout; stdout: {}; stderr: {}",
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

fn run_cli_json(
    args: &[&str],
    environment: &BTreeMap<String, String>,
    current_dir: &Path,
) -> (i32, Value, String) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = run_from_with_environment(args, environment, current_dir, &mut stdout, &mut stderr);
    let stdout = String::from_utf8(stdout).expect("stdout utf8");
    let stderr = String::from_utf8(stderr).expect("stderr utf8");
    let json = serde_json::from_str(&stdout).unwrap_or_else(|error| {
        panic!("stdout was not JSON: {error}; stdout={stdout:?}; stderr={stderr:?}")
    });
    (code, json, stderr)
}

fn run_cli_expect_error_json(
    args: &[&str],
    environment: &BTreeMap<String, String>,
    current_dir: &Path,
) -> (i32, String, Value) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = run_from_with_environment(args, environment, current_dir, &mut stdout, &mut stderr);
    let stdout = String::from_utf8(stdout).expect("stdout utf8");
    let stderr = String::from_utf8(stderr).expect("stderr utf8");
    let json = serde_json::from_str(&stderr).unwrap_or_else(|error| {
        panic!("stderr was not JSON: {error}; stdout={stdout:?}; stderr={stderr:?}")
    });
    (code, stdout, json)
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
fn gateway_run_serves_status_and_shuts_down_cleanly() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let gateway = GatewayProcess::spawn(&home, &[], &["--timeout", "750ms", "--ndjson"]);
    let port = gateway.port();

    let (health_status, health_response) = http_request(port, "GET", "/healthz");
    assert_eq!(health_status, 200, "response: {health_response}");
    let health: Value = serde_json::from_str(response_body(&health_response)).expect("health JSON");
    assert_eq!(health["ok"], true);
    assert_eq!(health["endpoint"], "health");

    let (status_code, status_response) = http_request(port, "GET", "/status");
    assert_eq!(status_code, 200, "response: {status_response}");
    let status: Value = serde_json::from_str(response_body(&status_response)).expect("status JSON");
    assert_eq!(status["status"], "ok");
    let components = status["components"].as_array().expect("components array");
    assert!(components.iter().any(|component| {
        component["name"] == "health_http" && component["state"] == "running"
    }));
    assert!(components.iter().any(|component| {
        component["name"] == "background_jobs"
            && matches!(component["state"].as_str(), Some("running" | "idle"))
    }));

    let (status, stdout, stderr) = gateway.wait(Duration::from_secs(10));
    assert_eq!(
        status.code(),
        Some(MissiveExitCode::Success.as_i32()),
        "stderr: {stderr}"
    );
    assert!(stderr.is_empty(), "stderr: {stderr}");

    let lines: Vec<Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("NDJSON envelope"))
        .collect();
    assert!(lines.iter().any(|line| line["kind"] == "gateway_started"));
    assert!(lines.iter().any(|line| line["kind"] == "gateway_component"));
    let stopped = lines
        .iter()
        .find(|line| line["kind"] == "gateway_stopped")
        .expect("stopped summary");
    assert_eq!(stopped["data"]["shutdown_reason"], "timeout");
    assert!(
        stopped["data"]["components"]
            .as_array()
            .expect("components")
            .iter()
            .any(|component| component["name"] == "supervisor" && component["state"] == "stopped")
    );

    let store = open_store(&environment);
    let events = store.list_events().expect("events");
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "missive.gateway.started")
    );
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "missive.gateway.stopped")
    );
}

#[test]
fn gateway_run_http_adapter_auth_validates_and_redacts_requests() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let token = "value-hidden-in-output";
    let gateway = GatewayProcess::spawn(
        &home,
        &[("MISSIVE_HTTP_ADAPTER_TOKEN", token)],
        &[
            "--http-adapter",
            "--http-adapter-auth-token-env",
            "MISSIVE_HTTP_ADAPTER_TOKEN",
            "--http-adapter-rate-limit",
            "4",
            "--timeout",
            "900ms",
            "--ndjson",
        ],
    );
    let port = gateway.port();

    let (adapter_health_status, adapter_health_response) =
        http_request(port, "GET", "/adapter/http/healthz");
    assert_eq!(
        adapter_health_status, 200,
        "response: {adapter_health_response}"
    );
    let adapter_health: Value =
        serde_json::from_str(response_body(&adapter_health_response)).expect("adapter health JSON");
    assert_eq!(adapter_health["component"], "http_adapter");
    assert_eq!(adapter_health["auth"]["token"], "[REDACTED]");

    let valid_body = r#"{
        "schema_version":"missive.http.v1",
        "id":"http-1",
        "source":{"source_id":"client-1","resume_name":"default"},
        "command":"send",
        "agent":"echo",
        "message":"hello via HTTP",
        "metadata":{"api_token":"value-hidden-in-output"}
    }"#;
    let (unauthorized_status, unauthorized_response) = http_request_with_body(
        port,
        "POST",
        "/adapter/http/v1/messages",
        &[("Content-Type", "application/json")],
        valid_body,
    );
    assert_eq!(
        unauthorized_status, 401,
        "response: {unauthorized_response}"
    );
    assert!(!unauthorized_response.contains(token));

    let (accepted_status, accepted_response) = http_request_with_body(
        port,
        "POST",
        "/adapter/http/v1/messages",
        &[
            ("Content-Type", "application/json"),
            ("Authorization", "Bearer value-hidden-in-output"),
        ],
        valid_body,
    );
    assert_eq!(accepted_status, 202, "response: {accepted_response}");
    let accepted: Value =
        serde_json::from_str(response_body(&accepted_response)).expect("accepted response JSON");
    assert_eq!(accepted["ok"], true);
    assert_eq!(accepted["id"], "http-1");
    assert_eq!(accepted["event_type"], "missive.adapter.http.accepted");

    let invalid_body = r#"{"schema_version":"missive.http.v1","id":"bad","command":"send"}"#;
    let (invalid_status, invalid_response) = http_request_with_body(
        port,
        "POST",
        "/adapter/http/v1/messages",
        &[
            ("Content-Type", "application/json"),
            ("Authorization", "Bearer value-hidden-in-output"),
        ],
        invalid_body,
    );
    assert_eq!(invalid_status, 400, "response: {invalid_response}");
    assert!(!invalid_response.contains(token));

    let (status, stdout, stderr) = gateway.wait(Duration::from_secs(10));
    assert_eq!(
        status.code(),
        Some(MissiveExitCode::Success.as_i32()),
        "stderr: {stderr}"
    );
    assert!(stderr.is_empty(), "stderr: {stderr}");
    assert!(!stdout.contains(token), "stdout leaked token: {stdout}");
    assert!(stdout.contains("gateway_adapter_event"));

    let store = open_store(&environment);
    let events = store.list_events().expect("events");
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "missive.adapter.http.accepted")
    );
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "missive.adapter.http.rejected")
    );
    let serialized_events = serde_json::to_string(&events).expect("events JSON");
    assert!(!serialized_events.contains(token));
}

#[test]
fn gateway_install_dry_run_renders_systemd_service_plan() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let missive_home = temp.path().join("missive-home");
    let environment = BTreeMap::from([
        ("HOME".to_owned(), home.to_string_lossy().into_owned()),
        (
            "MISSIVE_HOME".to_owned(),
            missive_home.to_string_lossy().into_owned(),
        ),
        ("PATH".to_owned(), "/usr/local/bin:/usr/bin:/bin".to_owned()),
    ]);

    let (code, output, stderr) = run_cli_json(
        &[
            "missive",
            "--json",
            "gateway",
            "install",
            "--dry-run",
            "--bin",
            "/usr/local/bin/missive",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    assert_eq!(output["kind"], "gateway_service_install");
    let data = &output["data"];
    assert_eq!(data["dry_run"], true);
    assert_eq!(data["manager"], "systemd");
    assert_eq!(data["scope"], "user");
    assert_eq!(data["service_name"], "missive-gateway.service");
    assert!(
        data["service_path"]
            .as_str()
            .expect("service path")
            .ends_with("/.config/systemd/user/missive-gateway.service")
    );
    let service_file = data["service_file"].as_str().expect("service file");
    assert!(service_file.contains("[Service]"));
    assert!(service_file.contains("ExecStart=\"/usr/local/bin/missive\""));
    assert!(service_file.contains("\"--profile\" \"default\" \"gateway\" \"run\""));
    assert!(service_file.contains("Environment=\"PATH=/usr/local/bin:/usr/bin:/bin\""));
    assert!(service_file.contains("Environment=\"MISSIVE_HOME="));
    assert_eq!(
        data["planned_commands"][0]["display"],
        "systemctl --user daemon-reload"
    );
}

#[test]
fn gateway_start_dry_run_renders_supervisor_command() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let temp = tempdir().expect("tempdir");
    let environment = BTreeMap::from([
        (
            "HOME".to_owned(),
            temp.path().join("home").to_string_lossy().into_owned(),
        ),
        ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
    ]);

    let (code, output, stderr) = run_cli_json(
        &["missive", "--json", "gateway", "start", "--dry-run"],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    assert_eq!(output["kind"], "gateway_service_start");
    assert_eq!(
        output["data"]["planned_commands"][0]["display"],
        "systemctl --user start missive-gateway.service"
    );
    assert_eq!(output["data"]["file_written"], false);
}

#[test]
fn gateway_service_install_rejects_sensitive_environment_names() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let temp = tempdir().expect("tempdir");
    let environment = BTreeMap::from([
        (
            "HOME".to_owned(),
            temp.path().join("home").to_string_lossy().into_owned(),
        ),
        ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
    ]);

    let (code, stdout, error) = run_cli_expect_error_json(
        &[
            "missive",
            "--json",
            "gateway",
            "install",
            "--dry-run",
            "--bin",
            "/usr/local/bin/missive",
            "--env",
            "API_TOKEN=example-value",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Usage.as_i32());
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert_eq!(error["kind"], "error");
    assert!(
        error["data"]["message"]
            .as_str()
            .expect("message")
            .contains("sensitive-looking environment variable")
    );
}

#[test]
fn gateway_system_install_requires_explicit_missive_home() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let temp = tempdir().expect("tempdir");
    let environment = BTreeMap::from([
        (
            "HOME".to_owned(),
            temp.path().join("home").to_string_lossy().into_owned(),
        ),
        ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
    ]);

    let (code, stdout, error) = run_cli_expect_error_json(
        &[
            "missive",
            "--json",
            "gateway",
            "install",
            "--dry-run",
            "--system",
            "--bin",
            "/usr/local/bin/missive",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Usage.as_i32());
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert!(
        error["data"]["message"]
            .as_str()
            .expect("message")
            .contains("MISSIVE_HOME")
    );
}
