use std::collections::BTreeMap;
use std::path::Path;

use missive_cli::{OUTPUT_SCHEMA_VERSION, REDACTED, run_from_with_environment};
use missive_core::MissiveExitCode;
use missive_store::{StatePathResolver, Store};
use missive_test_support::{MockA2aServer, push_config_json};
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

fn open_store(environment: &BTreeMap<String, String>) -> Store {
    let loaded = missive_core::ConfigDiscovery::new()
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
    Store::open(paths.database_path()).expect("open store")
}

#[test]
fn push_commands_cover_http_json_endpoints_and_redact_auth_info() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let mut environment = isolated_env(&home);
    environment.insert(
        "MISSIVE_PUSH_CALLBACK_SECRET".to_owned(),
        "super-secret-callback-token".to_owned(),
    );
    let server = MockA2aServer::start();

    add_agent("fixture", server.base_url(), &[], &environment, temp.path());
    let (code, stdout, stderr) = run(
        &[
            "missive",
            "push",
            "create",
            "fixture",
            "task-push",
            "http://127.0.0.1:9090/a2a/push",
            "--config-id",
            "push-1",
            "--auth-scheme",
            "Bearer",
            "--auth-credentials-env",
            "MISSIVE_PUSH_CALLBACK_SECRET",
            "--metadata",
            "purpose=fixture",
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    assert!(!stdout.contains("super-secret-callback-token"));
    let created = json_success(&stdout, "push_create");
    assert_eq!(created["data"]["push_config"]["config_id"], "push-1");
    assert_eq!(
        created["data"]["push_config"]["authentication"]["credentials"],
        REDACTED
    );
    assert_eq!(
        created["data"]["push_config"]["metadata"]["purpose"],
        "fixture"
    );

    let (code, stdout, stderr) = run(
        &["missive", "push", "list", "fixture", "task-push", "--json"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let listed = json_success(&stdout, "push_list");
    assert_eq!(listed["data"]["count"], 1);
    assert_eq!(listed["data"]["push_configs"][0]["config_id"], "push-1");
    assert_eq!(
        listed["data"]["push_configs"][0]["authentication"]["credentials"],
        REDACTED
    );

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "push",
            "get",
            "fixture",
            "task-push",
            "push-1",
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let fetched = json_success(&stdout, "push_get");
    assert_eq!(fetched["data"]["push_config"]["config_id"], "push-1");

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "push",
            "delete",
            "fixture",
            "task-push",
            "push-1",
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let deleted = json_success(&stdout, "push_delete");
    assert_eq!(deleted["data"]["deleted"], true);
    assert_eq!(deleted["data"]["local_record_deleted"], true);

    let requests = server.requests();
    assert_eq!(requests[0].path, "/.well-known/agent-card.json");
    assert_eq!(requests[1].method, "POST");
    assert_eq!(
        requests[1].path,
        "/a2a/tasks/task-push/pushNotificationConfigs"
    );
    let create_body = requests[1].json_body().expect("create JSON body");
    assert_eq!(create_body["id"], "push-1");
    assert_eq!(
        create_body["authentication"]["credentials"],
        "super-secret-callback-token"
    );
    assert_eq!(requests[2].method, "GET");
    assert_eq!(
        requests[2].path,
        "/a2a/tasks/task-push/pushNotificationConfigs"
    );
    assert_eq!(requests[3].method, "GET");
    assert_eq!(
        requests[3].path,
        "/a2a/tasks/task-push/pushNotificationConfigs/push-1"
    );
    assert_eq!(requests[4].method, "DELETE");
    assert_eq!(
        requests[4].path,
        "/a2a/tasks/task-push/pushNotificationConfigs/push-1"
    );

    let store = open_store(&environment);
    let push = store
        .get_push_config(&"push-1".parse().expect("push id"))
        .expect("get push config")
        .expect("push config persisted");
    assert_eq!(push.callback_url, "http://127.0.0.1:9090/a2a/push");
    assert_eq!(push.metadata.get_str("purpose"), Some("fixture"));
    assert!(push.deleted_at.is_some());
    let events = store.list_events().expect("events");
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "a2a.push.create")
    );
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "a2a.push.delete")
    );
    for event in events {
        assert!(
            !event
                .payload_json
                .to_string()
                .contains("super-secret-callback-token")
        );
    }
}

#[test]
fn push_commands_use_json_rpc_binding() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let server = MockA2aServer::start();
    server.handle().insert_push_config(push_config_json(
        "task-rpc-push",
        "push-rpc-1",
        "http://127.0.0.1:9090/rpc-push",
    ));

    add_agent(
        "fixture-rpc",
        server.base_url(),
        &["--binding-preference", "json-rpc"],
        &environment,
        temp.path(),
    );
    for args in [
        vec![
            "missive",
            "push",
            "create",
            "fixture-rpc",
            "task-rpc-push",
            "http://127.0.0.1:9090/rpc-push",
            "--config-id",
            "push-rpc-1",
            "--json",
        ],
        vec![
            "missive",
            "push",
            "list",
            "fixture-rpc",
            "task-rpc-push",
            "--json",
        ],
        vec![
            "missive",
            "push",
            "get",
            "fixture-rpc",
            "task-rpc-push",
            "push-rpc-1",
            "--json",
        ],
        vec![
            "missive",
            "push",
            "delete",
            "fixture-rpc",
            "task-rpc-push",
            "push-rpc-1",
            "--json",
        ],
    ] {
        let (code, _stdout, stderr) = run(&args, &environment, temp.path());
        assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    }

    let methods = server
        .requests()
        .into_iter()
        .filter(|request| request.path == "/rpc")
        .map(|request| request.json_body().expect("rpc body")["method"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        methods,
        vec![
            json!("CreateTaskPushNotificationConfig"),
            json!("ListTaskPushNotificationConfigs"),
            json!("GetTaskPushNotificationConfig"),
            json!("DeleteTaskPushNotificationConfig"),
        ]
    );
}

#[test]
fn push_create_validates_callback_url_shape() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let server = MockA2aServer::start();
    add_agent("fixture", server.base_url(), &[], &environment, temp.path());

    let (code, _stdout, stderr) = run(
        &[
            "missive",
            "push",
            "create",
            "fixture",
            "task-push",
            "not-a-url",
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Usage.as_i32());
    assert!(stderr.contains("push callback URL"));
}
