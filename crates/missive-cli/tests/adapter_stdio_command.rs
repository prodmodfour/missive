use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::Path;

use missive_cli::{run_from_with_environment, run_from_with_environment_and_input};
use missive_core::MissiveExitCode;
use missive_test_support::{MockA2aServer, status_update_event};
use serde_json::{Value, json};
use tempfile::tempdir;

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

fn parse_ndjson(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("NDJSON line should parse"))
        .collect()
}

#[test]
fn stdio_adapter_long_running_reports_invalid_frame_and_continues() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let input = concat!(
        "not-json\n",
        "{\"schema_version\":\"missive.stdio.v1\",\"id\":\"req-list\",\"command\":\"task_list\"}\n"
    );

    let (code, stdout, stderr) = run_with_input(
        &["missive", "adapter", "stdio", "--mode", "long-running"],
        input,
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let lines = parse_ndjson(&stdout);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["schema_version"], "missive.stdio.v1");
    assert_eq!(lines[0]["ok"], false);
    assert_eq!(lines[0]["kind"], "stdio_error");
    assert_eq!(lines[1]["ok"], true);
    assert_eq!(lines[1]["id"], "req-list");
    assert_eq!(lines[1]["data"]["kind"], "task_list");
    assert_eq!(lines[1]["data"]["data"]["count"], 0);
}

#[test]
fn stdio_adapter_stream_frame_emits_wrapped_streaming_ndjson() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let server = MockA2aServer::start();
    server.handle().set_stream_events(vec![status_update_event(
        "task-stdio-stream",
        "ctx-stdio-stream",
        "TASK_STATE_COMPLETED",
        Some("stdio stream completed"),
    )]);
    add_agent("echo", server.base_url(), &environment, temp.path());
    let input = format!(
        "{}\n",
        json!({
            "schema_version": "missive.stdio.v1",
            "id": "req-stream",
            "command": "stream",
            "agent": "echo",
            "message": "hello over stdio"
        })
    );

    let (code, stdout, stderr) = run_with_input(
        &["missive", "adapter", "stdio", "--mode", "long-running"],
        &input,
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let lines = parse_ndjson(&stdout);
    assert_eq!(lines.len(), 2);
    assert!(
        lines
            .iter()
            .all(|line| line["schema_version"] == "missive.stdio.v1")
    );
    assert!(lines.iter().all(|line| line["ok"] == true));
    assert!(lines.iter().all(|line| line["id"] == "req-stream"));
    assert_eq!(lines[0]["data"]["kind"], "stream_event");
    assert_eq!(lines[0]["data"]["data"]["event_type"], "status_update");
    assert_eq!(lines[0]["data"]["data"]["state"], "completed");
    assert_eq!(lines[1]["data"]["kind"], "stream_result");
    assert_eq!(lines[1]["data"]["data"]["event_count"], 1);

    let requests = server.requests();
    assert!(
        requests
            .iter()
            .any(|request| request.path == "/.well-known/agent-card.json")
    );
    assert!(
        requests
            .iter()
            .any(|request| request.path == "/a2a/message:stream")
    );
}
