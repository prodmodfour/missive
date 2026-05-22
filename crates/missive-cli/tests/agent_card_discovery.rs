use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use missive_cli::{OUTPUT_SCHEMA_VERSION, run_from_with_environment};
use missive_core::MissiveExitCode;
use serde_json::Value;
use tempfile::tempdir;

#[derive(Debug, Clone)]
struct RecordedRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
struct MockResponse {
    status: u16,
    reason: &'static str,
    headers: Vec<(&'static str, String)>,
    body: Option<String>,
    close_without_response: bool,
}

impl MockResponse {
    fn ok_json(body: String, headers: Vec<(&'static str, String)>) -> Self {
        Self {
            status: 200,
            reason: "OK",
            headers,
            body: Some(body),
            close_without_response: false,
        }
    }

    fn raw(status: u16, reason: &'static str, body: impl Into<String>) -> Self {
        Self {
            status,
            reason,
            headers: Vec::new(),
            body: Some(body.into()),
            close_without_response: false,
        }
    }

    fn close_without_response() -> Self {
        Self {
            status: 0,
            reason: "",
            headers: Vec::new(),
            body: None,
            close_without_response: true,
        }
    }
}

#[derive(Debug)]
struct MockServer {
    base_url: String,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
}

impl MockServer {
    fn start(responses: Vec<MockResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let address = listener.local_addr().expect("local addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let thread_requests = Arc::clone(&requests);
        thread::spawn(move || {
            for response in responses {
                let Ok((mut stream, _peer)) = listener.accept() else {
                    return;
                };
                if response.close_without_response {
                    continue;
                }
                let request = read_request(&mut stream);
                thread_requests.lock().expect("requests lock").push(request);
                write_response(&mut stream, &response);
            }
        });

        Self {
            base_url: format!("http://{address}"),
            requests,
        }
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
    let mut buffer = [0_u8; 512];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                data.extend_from_slice(&buffer[..read]);
                if data.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    let text = String::from_utf8_lossy(&data);
    let mut lines = text.split("\r\n");
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

    RecordedRequest {
        method,
        path,
        headers,
    }
}

fn write_response(stream: &mut TcpStream, response: &MockResponse) {
    let body = response.body.as_deref().unwrap_or_default();
    let mut headers = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        response.reason,
        body.len()
    );
    for (name, value) in &response.headers {
        headers.push_str(name);
        headers.push_str(": ");
        headers.push_str(value);
        headers.push_str("\r\n");
    }
    headers.push_str("\r\n");
    stream.write_all(headers.as_bytes()).expect("write headers");
    stream.write_all(body.as_bytes()).expect("write body");
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

fn agent_card_json(base_url: &str, version: &str) -> String {
    serde_json::json!({
        "name": "Echo Agent",
        "description": "Replies with whatever it receives.",
        "supportedInterfaces": [
            {
                "url": format!("{base_url}/a2a"),
                "protocolBinding": "HTTP+JSON",
                "protocolVersion": "1.0"
            },
            {
                "url": format!("{base_url}/rpc"),
                "protocolBinding": "JSONRPC",
                "protocolVersion": "1.0"
            }
        ],
        "provider": {
            "url": "https://example.test/provider",
            "organization": "Example Agents"
        },
        "version": version,
        "documentationUrl": "https://example.test/agents/echo",
        "capabilities": {
            "streaming": true,
            "pushNotifications": true,
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
                "examples": ["Say hello"],
                "inputModes": ["text/plain"],
                "outputModes": ["text/plain"]
            }
        ]
    })
    .to_string()
}

#[test]
fn agent_inspect_fetches_public_card_and_caches_metadata() {
    let temp = tempdir().expect("tempdir");
    let environment = isolated_env(&temp.path().join("missive-home"));
    let server = MockServer::start(vec![MockResponse::ok_json(
        agent_card_json("http://127.0.0.1:1", "1.0.0"),
        vec![
            ("Content-Type", "application/json".to_owned()),
            ("ETag", "W/\"card-v1\"".to_owned()),
            ("Last-Modified", "Wed, 21 Oct 2015 07:28:00 GMT".to_owned()),
        ],
    )]);
    add_agent("echo", &server.base_url, &environment, temp.path());

    let (code, stdout, stderr) = run(
        &["missive", "agent", "inspect", "echo", "--json"],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_success(&stdout, "agent_inspect");
    assert_eq!(value["data"]["agent"]["alias"], "echo");
    assert_eq!(value["data"]["cache"]["status"], "fetched");
    assert_eq!(value["data"]["cache"]["etag"], "W/\"card-v1\"");
    assert_eq!(
        value["data"]["cache"]["last_modified"],
        "Wed, 21 Oct 2015 07:28:00 GMT"
    );
    assert_eq!(value["data"]["card"]["name"], "Echo Agent");
    assert_eq!(
        value["data"]["card"]["provider"]["organization"],
        "Example Agents"
    );
    assert_eq!(value["data"]["card"]["agent_version"], "1.0.0");
    assert_eq!(
        value["data"]["card"]["protocol_versions"],
        serde_json::json!(["1.0"])
    );
    assert_eq!(value["data"]["card"]["capabilities"]["streaming"], true);
    assert_eq!(value["data"]["card"]["skills"][0]["id"], "echo");
    assert_eq!(value["data"]["raw_card"]["name"], "Echo Agent");

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, "/.well-known/agent-card.json");

    let (code, stdout, stderr) = run(
        &["missive", "agent", "show", "echo", "--json"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "agent_show");
    assert_eq!(value["data"]["agent"]["agent_card_etag"], "W/\"card-v1\"");
    assert!(value["data"]["agent"]["agent_card_fetched_at"].is_string());
}

#[test]
fn agent_inspect_human_output_lists_card_details() {
    let temp = tempdir().expect("tempdir");
    let environment = isolated_env(&temp.path().join("missive-home"));
    let server = MockServer::start(vec![MockResponse::ok_json(
        agent_card_json("http://127.0.0.1:1", "1.0.0"),
        vec![("Content-Type", "application/json".to_owned())],
    )]);
    add_agent("echo", &server.base_url, &environment, temp.path());

    let (code, stdout, stderr) = run(
        &["missive", "agent", "inspect", "echo"],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    assert!(stdout.contains("Agent Card for echo (Echo Agent)"));
    assert!(stdout.contains("provider: Example Agents"));
    assert!(stdout.contains("agent_version: 1.0.0"));
    assert!(stdout.contains("protocol_versions: 1.0"));
    assert!(stdout.contains("capabilities:"));
    assert!(stdout.contains("supported_interfaces:"));
    assert!(stdout.contains("HTTP+JSON 1.0"));
    assert!(stdout.contains("skills:"));
    assert!(stdout.contains("echo (Echo)"));
}

#[test]
fn agent_inspect_uses_cache_until_refresh_bypasses_it() {
    let temp = tempdir().expect("tempdir");
    let environment = isolated_env(&temp.path().join("missive-home"));
    let server = MockServer::start(vec![
        MockResponse::ok_json(
            agent_card_json("http://127.0.0.1:1", "1.0.0"),
            vec![
                ("Content-Type", "application/json".to_owned()),
                ("ETag", "W/\"card-v1\"".to_owned()),
            ],
        ),
        MockResponse::ok_json(
            agent_card_json("http://127.0.0.1:1", "2.0.0"),
            vec![
                ("Content-Type", "application/json".to_owned()),
                ("ETag", "W/\"card-v2\"".to_owned()),
            ],
        ),
    ]);
    add_agent("echo", &server.base_url, &environment, temp.path());

    let (code, stdout, stderr) = run(
        &["missive", "agent", "inspect", "echo", "--json"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert_eq!(
        json_success(&stdout, "agent_inspect")["data"]["card"]["agent_version"],
        "1.0.0"
    );

    let (code, stdout, stderr) = run(
        &["missive", "agent", "inspect", "echo", "--json"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "agent_inspect");
    assert_eq!(value["data"]["cache"]["status"], "cached");
    assert_eq!(value["data"]["card"]["agent_version"], "1.0.0");
    assert_eq!(
        server.requests().len(),
        1,
        "cached inspect should not fetch again"
    );

    let (code, stdout, stderr) = run(
        &["missive", "agent", "inspect", "echo", "--refresh", "--json"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "agent_inspect");
    assert_eq!(value["data"]["cache"]["status"], "refreshed");
    assert_eq!(value["data"]["card"]["agent_version"], "2.0.0");
    assert_eq!(value["data"]["cache"]["etag"], "W/\"card-v2\"");

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].headers.get("if-none-match").map(String::as_str),
        Some("W/\"card-v1\"")
    );
}

#[test]
fn agent_refresh_command_fetches_and_renders_card() {
    let temp = tempdir().expect("tempdir");
    let environment = isolated_env(&temp.path().join("missive-home"));
    let server = MockServer::start(vec![MockResponse::ok_json(
        agent_card_json("http://127.0.0.1:1", "1.0.0"),
        vec![("Content-Type", "application/json".to_owned())],
    )]);
    add_agent("echo", &server.base_url, &environment, temp.path());

    let (code, stdout, stderr) = run(
        &["missive", "agent", "refresh", "echo", "--json"],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert_eq!(
        json_success(&stdout, "agent_refresh")["data"]["cache"]["status"],
        "refreshed"
    );
}

#[test]
fn agent_inspect_reports_404_as_transport_error() {
    let temp = tempdir().expect("tempdir");
    let environment = isolated_env(&temp.path().join("missive-home"));
    let server = MockServer::start(vec![MockResponse::raw(404, "Not Found", "missing")]);
    add_agent("echo", &server.base_url, &environment, temp.path());

    let (code, stdout, stderr) = run(
        &["missive", "agent", "inspect", "echo", "--json"],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Unavailable.as_i32());
    assert!(stdout.is_empty());
    let value = json_error(&stderr);
    assert_eq!(value["data"]["code"], "missive::transport");
    assert!(
        value["data"]["message"]
            .as_str()
            .expect("message")
            .contains("HTTP 404")
    );
}

#[test]
fn agent_inspect_reports_malformed_json_as_protocol_error() {
    let temp = tempdir().expect("tempdir");
    let environment = isolated_env(&temp.path().join("missive-home"));
    let server = MockServer::start(vec![MockResponse::raw(200, "OK", "{not-json")]);
    add_agent("echo", &server.base_url, &environment, temp.path());

    let (code, stdout, stderr) = run(
        &["missive", "agent", "inspect", "echo", "--json"],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Protocol.as_i32());
    assert!(stdout.is_empty());
    let value = json_error(&stderr);
    assert_eq!(value["data"]["code"], "missive::protocol");
    assert!(
        value["data"]["message"]
            .as_str()
            .expect("message")
            .contains("not valid JSON")
    );
}

#[test]
fn agent_inspect_reports_tls_or_http_transport_errors() {
    let temp = tempdir().expect("tempdir");
    let environment = isolated_env(&temp.path().join("missive-home"));
    let server = MockServer::start(vec![MockResponse::close_without_response()]);
    let https_base_url = server.base_url.replacen("http://", "https://", 1);
    add_agent("echo", &https_base_url, &environment, temp.path());

    let (code, stdout, stderr) = run(
        &["missive", "agent", "inspect", "echo", "--json"],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Unavailable.as_i32());
    assert!(stdout.is_empty());
    let value = json_error(&stderr);
    assert_eq!(value["data"]["code"], "missive::transport");
    assert!(
        value["data"]["message"]
            .as_str()
            .expect("message")
            .contains("fetching A2A Agent Card")
    );
}
