use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

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

fn unused_local_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral port");
    listener.local_addr().expect("local addr").port()
}

fn http_request(port: u16, method: &str, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect gateway");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("read timeout");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
    );
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

fn wait_for_health(child: &mut Child, port: u16) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("poll child") {
            panic!("gateway exited before health check succeeded: {status}");
        }
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            let (status, _response) = http_request(port, "GET", "/healthz");
            if status == 200 {
                return;
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("gateway did not become healthy on port {port}");
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
            panic!("gateway did not exit before timeout");
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
fn gateway_run_serves_status_and_shuts_down_cleanly() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let port = unused_local_port();

    let mut child = Command::new(env!("CARGO_BIN_EXE_missive"))
        .env_clear()
        .env("MISSIVE_HOME", &home)
        .arg("gateway")
        .arg("run")
        .arg("--bind-address")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .arg("--timeout")
        .arg("750ms")
        .arg("--ndjson")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn gateway");

    wait_for_health(&mut child, port);

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
        component["name"] == "background_jobs" && component["state"] == "idle"
    }));

    let (status, stdout, stderr) = wait_for_child(child, Duration::from_secs(10));
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
