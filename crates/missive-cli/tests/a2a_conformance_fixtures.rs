use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use missive_cli::{OUTPUT_SCHEMA_VERSION, run_from_with_environment};
use missive_core::MissiveExitCode;
use serde_json::Value;
use tempfile::tempdir;

const AGENT_CARD_FIXTURE: &str = include_str!("../../../tests/fixtures/a2a/1.0/agent_card.json");
const SEND_RESPONSE_TASK_FIXTURE: &str =
    include_str!("../../../tests/fixtures/a2a/1.0/send_message_response_task.json");
const GOLDEN_UPDATE_ENV: &str = "MISSIVE_UPDATE_GOLDENS";

#[derive(Debug, Clone)]
struct RecordedRequest {
    method: String,
    path: String,
}

#[derive(Debug, Clone)]
struct FixtureResponse {
    status: u16,
    reason: &'static str,
    content_type: &'static str,
    body: String,
}

impl FixtureResponse {
    fn a2a_json(body: Value) -> Self {
        Self {
            status: 200,
            reason: "OK",
            content_type: "application/a2a+json",
            body: body.to_string(),
        }
    }
}

#[derive(Debug)]
struct FixtureServer {
    base_url: String,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
}

impl FixtureServer {
    fn start(build_responses: impl FnOnce(&str) -> Vec<FixtureResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture server");
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

    let header_text = String::from_utf8_lossy(&data);
    let first = header_text.lines().next().unwrap_or_default();
    let mut first_parts = first.split_whitespace();
    RecordedRequest {
        method: first_parts.next().unwrap_or_default().to_owned(),
        path: first_parts.next().unwrap_or_default().to_owned(),
    }
}

fn write_response(stream: &mut TcpStream, response: &FixtureResponse) {
    let headers = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nContent-Type: {}\r\nConnection: close\r\n\r\n",
        response.status,
        response.reason,
        response.body.len(),
        response.content_type
    );
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

fn add_agent(base_url: &str, environment: &BTreeMap<String, String>, current_dir: &Path) {
    let (code, _stdout, stderr) = run(
        &["missive", "agent", "add", "fixture", base_url, "--json"],
        environment,
        current_dir,
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
}

fn fixture_agent_card_for_base_url(base_url: &str) -> Value {
    let mut card: Value = serde_json::from_str(AGENT_CARD_FIXTURE).expect("Agent Card fixture");
    let interfaces = card["supportedInterfaces"]
        .as_array_mut()
        .expect("supportedInterfaces array");
    interfaces[0]["url"] = Value::String(format!("{base_url}/a2a"));
    interfaces[1]["url"] = Value::String(format!("{base_url}/rpc"));
    card
}

#[test]
fn agent_inspect_json_matches_a2a_fixture_golden() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let server = FixtureServer::start(|base_url| {
        vec![FixtureResponse::a2a_json(fixture_agent_card_for_base_url(
            base_url,
        ))]
    });
    add_agent(&server.base_url, &environment, temp.path());

    let (code, stdout, stderr) = run(
        &["missive", "agent", "inspect", "fixture", "--json"],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let normalized = normalized_cli_output(&stdout, &server.base_url);
    assert_eq!(normalized["schema_version"], OUTPUT_SCHEMA_VERSION);
    assert_eq!(normalized["kind"], "agent_inspect");
    assert_json_matches_golden("agent_inspect.json", &normalized);

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, "/.well-known/agent-card.json");
}

#[test]
fn send_json_matches_a2a_fixture_golden() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let send_response = serde_json::from_str(SEND_RESPONSE_TASK_FIXTURE).expect("send fixture");
    let server = FixtureServer::start(|base_url| {
        vec![
            FixtureResponse::a2a_json(fixture_agent_card_for_base_url(base_url)),
            FixtureResponse::a2a_json(send_response),
        ]
    });
    add_agent(&server.base_url, &environment, temp.path());

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "send",
            "fixture",
            "start a fixture task",
            "--accepted-output-mode",
            "text/plain",
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let normalized = normalized_cli_output(&stdout, &server.base_url);
    assert_eq!(normalized["schema_version"], OUTPUT_SCHEMA_VERSION);
    assert_eq!(normalized["kind"], "send_result");
    assert_json_matches_golden("send_result_task.json", &normalized);

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, "/.well-known/agent-card.json");
    assert_eq!(requests[1].method, "POST");
    assert_eq!(requests[1].path, "/a2a/message:send");
}

fn normalized_cli_output(stdout: &str, base_url: &str) -> Value {
    let mut value: Value = serde_json::from_str(stdout).expect("stdout should be JSON");
    normalize_value(&mut value, None, base_url);
    value
}

fn normalize_value(value: &mut Value, key: Option<&str>, base_url: &str) {
    match value {
        Value::Object(object) => {
            for (child_key, child_value) in object {
                normalize_value(child_value, Some(child_key), base_url);
            }
        }
        Value::Array(items) => {
            for item in items {
                normalize_value(item, key, base_url);
            }
        }
        Value::String(text) => {
            if key.is_some_and(is_dynamic_timestamp_key) {
                *text = "<timestamp>".to_owned();
            } else {
                *text = text.replace(base_url, "http://fixture.invalid");
                if is_uuid_like(text) {
                    *text = "<generated-id>".to_owned();
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn is_dynamic_timestamp_key(key: &str) -> bool {
    matches!(
        key,
        "created_at" | "updated_at" | "agent_card_fetched_at" | "fetched_at"
    )
}

fn is_uuid_like(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && [8, 13, 18, 23]
            .into_iter()
            .all(|index| bytes.get(index) == Some(&b'-'))
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| [8, 13, 18, 23].contains(&index) || byte.is_ascii_hexdigit())
}

fn assert_json_matches_golden(name: &str, actual: &Value) {
    let path = golden_path(name);
    let pretty = serde_json::to_string_pretty(actual).expect("serialize normalized CLI output");
    if std::env::var_os(GOLDEN_UPDATE_ENV).is_some() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create golden output directory");
        }
        fs::write(&path, format!("{pretty}\n")).expect("write golden output");
    }

    let expected_text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read golden {}: {error}", path.display()));
    let expected: Value = serde_json::from_str(&expected_text)
        .unwrap_or_else(|error| panic!("golden {} should be JSON: {error}", path.display()));
    assert_eq!(
        &expected,
        actual,
        "normalized CLI output differed from {}\nactual:\n{pretty}",
        path.display()
    );
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/a2a/1.0/cli")
        .join(name)
}
