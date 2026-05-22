use std::collections::BTreeMap;
use std::path::Path;

use missive_cli::run_from_with_environment;
use missive_core::{ConfigDiscovery, MissiveExitCode};
use missive_store::{GatewayJobId, GatewayJobState, StatePathResolver, Store};
use missive_test_support::MockA2aServer;
use serde_json::Value;
use tempfile::tempdir;

fn isolated_env(home: &Path) -> BTreeMap<String, String> {
    BTreeMap::from([(
        "MISSIVE_HOME".to_owned(),
        home.to_string_lossy().into_owned(),
    )])
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

fn add_agent(server: &MockA2aServer, environment: &BTreeMap<String, String>, current_dir: &Path) {
    let (code, json, stderr) = run_cli_json(
        &[
            "missive",
            "agent",
            "add",
            "echo",
            server.base_url(),
            "--json",
        ],
        environment,
        current_dir,
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert_eq!(json["kind"], "agent_add");
}

#[test]
fn job_start_list_show_and_cancel_are_machine_readable() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let server = MockA2aServer::start();
    add_agent(&server, &environment, temp.path());

    let (code, started, stderr) = run_cli_json(
        &[
            "missive",
            "job",
            "start",
            "send",
            "echo",
            "hello background",
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert_eq!(started["kind"], "job_start");
    assert_eq!(started["data"]["job"]["kind"], "send");
    assert_eq!(started["data"]["job"]["state"], "queued");
    assert!(
        started["data"]["job"]["request"].get("request").is_none(),
        "job view must not echo the raw A2A request body"
    );
    let job_id = started["data"]["job"]["job_id"]
        .as_str()
        .expect("job id")
        .to_owned();

    let (code, list, stderr) = run_cli_json(
        &["missive", "job", "list", "--kind", "send", "--json"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert_eq!(list["kind"], "job_list");
    assert_eq!(list["data"]["count"], 1);

    let (code, shown, stderr) = run_cli_json(
        &["missive", "job", "show", &job_id, "--json"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert_eq!(shown["kind"], "job_show");
    assert_eq!(shown["data"]["job"]["job_id"], job_id);

    let (code, cancelled, stderr) = run_cli_json(
        &["missive", "job", "cancel", &job_id, "--json"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert_eq!(cancelled["kind"], "job_cancel");
    assert_eq!(cancelled["data"]["job"]["state"], "cancelled");

    let store = open_store(&environment);
    let events = store.list_events().expect("events");
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "missive.job.enqueued")
    );
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "missive.job.cancelled")
    );
}

#[test]
fn gateway_run_executes_queued_send_job_and_persists_result() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let server = MockA2aServer::start();
    add_agent(&server, &environment, temp.path());

    let (code, started, stderr) = run_cli_json(
        &[
            "missive",
            "job",
            "start",
            "send",
            "echo",
            "hello gateway",
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let job_id = started["data"]["job"]["job_id"]
        .as_str()
        .expect("job id")
        .to_owned();

    let (code, gateway, stderr) = run_cli_json(
        &[
            "missive",
            "gateway",
            "run",
            "--bind-address",
            "127.0.0.1",
            "--port",
            "0",
            "--timeout",
            "750ms",
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert_eq!(gateway["kind"], "gateway_stopped");

    assert!(
        server
            .requests()
            .iter()
            .any(|request| { request.method == "POST" && request.path == "/a2a/message:send" })
    );

    let store = open_store(&environment);
    let job = store
        .get_gateway_job(&GatewayJobId::new(job_id).expect("job id"))
        .expect("read job")
        .expect("job row");
    assert_eq!(job.state, GatewayJobState::Succeeded);
    assert_eq!(
        job.result_json
            .as_ref()
            .and_then(|value| value.get("operation"))
            .and_then(Value::as_str),
        Some("send")
    );
    let events = store.list_events().expect("events");
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "missive.gateway.job.succeeded")
    );
}

#[test]
fn job_cancel_can_request_remote_task_cancellation() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let server = MockA2aServer::start();
    add_agent(&server, &environment, temp.path());

    let (code, started, stderr) = run_cli_json(
        &[
            "missive",
            "job",
            "start",
            "wait",
            "task-cancel",
            "--agent",
            "echo",
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let job_id = started["data"]["job"]["job_id"]
        .as_str()
        .expect("job id")
        .to_owned();

    let (code, cancelled, stderr) = run_cli_json(
        &["missive", "job", "cancel", &job_id, "--remote", "--json"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert_eq!(cancelled["data"]["remote_cancelled"], true);
    assert_eq!(cancelled["data"]["job"]["state"], "cancelled");

    assert!(server.requests().iter().any(|request| {
        request.method == "POST" && request.path == "/a2a/tasks/task-cancel:cancel"
    }));
}
