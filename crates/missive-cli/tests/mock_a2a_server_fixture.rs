use std::collections::BTreeMap;
use std::path::Path;

use missive_cli::{OUTPUT_SCHEMA_VERSION, run_from_with_environment};
use missive_core::MissiveExitCode;
use missive_test_support::{MockA2aServer, send_message_response_message};
use serde_json::Value;
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
    extra_args: &[&str],
    environment: &BTreeMap<String, String>,
    current_dir: &Path,
) {
    let mut args = vec!["missive", "agent", "add", alias, base_url];
    args.extend_from_slice(extra_args);
    args.push("--json");
    let (code, _stdout, stderr) = run(&args, environment, current_dir);
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
}

#[test]
fn reusable_mock_a2a_server_drives_http_json_and_json_rpc_cli_sends() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let server = MockA2aServer::start();
    server
        .handle()
        .set_send_response(send_message_response_message(
            "msg-fixture-response",
            "ctx-fixture-response",
            "fixture response",
        ));

    add_agent("fixture", server.base_url(), &[], &environment, temp.path());
    let (code, stdout, stderr) = run(
        &["missive", "send", "fixture", "hello fixture", "--json"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let http = json_success(&stdout, "send_result");
    assert_eq!(http["data"]["selected_interface"]["binding"], "http+json");
    assert_eq!(
        http["data"]["response"]["message_id"],
        "msg-fixture-response"
    );
    assert_eq!(http["data"]["response"]["text"], "fixture response");

    server
        .handle()
        .set_send_response(send_message_response_message(
            "msg-fixture-rpc-response",
            "ctx-fixture-rpc-response",
            "fixture rpc response",
        ));
    add_agent(
        "fixture-rpc",
        server.base_url(),
        &["--binding-preference", "json-rpc"],
        &environment,
        temp.path(),
    );
    let (code, stdout, stderr) = run(
        &["missive", "send", "fixture-rpc", "hello rpc", "--json"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let rpc = json_success(&stdout, "send_result");
    assert_eq!(rpc["data"]["selected_interface"]["binding"], "json-rpc");
    assert_eq!(
        rpc["data"]["response"]["message_id"],
        "msg-fixture-rpc-response"
    );

    let requests = server.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].path, "/.well-known/agent-card.json");
    assert_eq!(requests[1].path, "/a2a/message:send");
    assert_eq!(requests[1].header("a2a-version"), Some("1.0"));
    assert_eq!(requests[2].path, "/.well-known/agent-card.json");
    assert_eq!(requests[3].path, "/rpc");
    let rpc_body = requests[3].json_body().expect("JSON-RPC body");
    assert_eq!(rpc_body["method"], "SendMessage");
}
