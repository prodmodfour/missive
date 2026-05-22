use std::collections::BTreeMap;
use std::path::Path;

use missive_cli::{OUTPUT_SCHEMA_VERSION, REDACTED, run_from_with_environment};
use missive_core::{AgentAlias, ContextId, EventId, MessageId, MissiveExitCode, TaskId};
use missive_store::{
    EventInsert, MessageDirection, MessageInsert, MessageRole, Store, TaskState, TaskUpsert,
};
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

fn open_store(home: &Path) -> Store {
    Store::open(
        home.join("state")
            .join("profiles")
            .join("default")
            .join("missive.sqlite3"),
    )
    .expect("open store")
}

#[test]
fn context_create_show_and_list_support_human_names() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    add_agent("echo", "http://127.0.0.1:65530", &environment, temp.path());

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "context",
            "create",
            "--id",
            "ctx-plan",
            "--name",
            "Planning Round",
            "--agent",
            "echo",
            "--summary",
            "Initial planning",
            "--metadata",
            "round=1",
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_success(&stdout, "context_create");
    assert_eq!(value["data"]["context"]["context_id"], "ctx-plan");
    assert_eq!(value["data"]["context"]["name"], "Planning Round");
    assert_eq!(value["data"]["context"]["agent"], "echo");
    assert_eq!(value["data"]["context"]["summary"], "Initial planning");
    assert_eq!(value["data"]["context"]["metadata"]["round"], 1);

    let store = open_store(&home);
    let stored = store
        .get_context(&ContextId::new("ctx-plan").expect("context id"))
        .expect("get context")
        .expect("context persisted");
    assert_eq!(stored.name.as_deref(), Some("Planning Round"));
    assert_eq!(
        stored.agent_alias.as_ref().map(ToString::to_string),
        Some("echo".to_owned())
    );

    let (code, stdout, stderr) = run(
        &["missive", "context", "show", "Planning Round", "--json"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "context_show");
    assert_eq!(value["data"]["context"]["context_id"], "ctx-plan");

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "context",
            "list",
            "--agent",
            "echo",
            "--state",
            "open",
            "--name",
            "Planning Round",
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "context_list");
    assert_eq!(value["data"]["count"], 1);
    assert_eq!(value["data"]["contexts"][0]["context_id"], "ctx-plan");
}

#[test]
fn context_fork_close_and_export_include_redacted_related_records() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    add_agent("echo", "http://127.0.0.1:65530", &environment, temp.path());

    let (code, _stdout, stderr) = run(
        &[
            "missive",
            "context",
            "create",
            "--id",
            "ctx-parent",
            "--name",
            "Parent Context",
            "--agent",
            "echo",
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");

    let store = open_store(&home);
    let agent = AgentAlias::new("echo").expect("agent");
    let context = ContextId::new("ctx-parent").expect("context id");
    let task_id = TaskId::new("task-context-export").expect("task id");
    let mut task = TaskUpsert::new(task_id.clone(), agent.clone(), TaskState::Working);
    task.context_id = Some(context.clone());
    task.remote_task_json = Some(json!({
        "id": "task-context-export",
        "contextId": "ctx-parent",
        "headers": {"Authorization": "Bearer value-hidden-in-output"},
        "token": "value-hidden-in-output"
    }));
    task.record_a2a_protocol_version("1.0")
        .expect("protocol version");
    store.upsert_task(&task).expect("task");

    let mut message = MessageInsert::new(
        MessageId::new("msg-context-export").expect("message id"),
        MessageDirection::Response,
        json!({
            "parts": [{"text": "visible"}],
            "headers": {"Authorization": "Bearer value-hidden-in-output"},
            "token": "value-hidden-in-output"
        }),
    );
    message.agent_alias = Some(agent.clone());
    message.context_id = Some(context.clone());
    message.task_id = Some(task_id.clone());
    message.role = Some(MessageRole::Agent);
    message.ordinal = 2;
    store.insert_message(&message).expect("message");

    let mut event = EventInsert::new(
        EventId::new("evt-context-export").expect("event id"),
        "test",
        "context.export.fixture",
        json!({
            "headers": {"Authorization": "Bearer value-hidden-in-output"},
            "token": "value-hidden-in-output",
            "public": "visible"
        }),
    );
    event.agent_alias = Some(agent);
    event.context_id = Some(context);
    event.task_id = Some(task_id);
    store.append_event(&event).expect("event");

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "context",
            "fork",
            "Parent Context",
            "--id",
            "ctx-child",
            "--name",
            "Child Context",
            "--metadata",
            "branch=1",
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "context_fork");
    assert_eq!(value["data"]["context"]["context_id"], "ctx-child");
    assert_eq!(value["data"]["context"]["parent_context_id"], "ctx-parent");
    assert_eq!(value["data"]["context"]["agent"], "echo");
    assert_eq!(
        value["data"]["context"]["metadata"]["missive.context.parent_id"],
        "ctx-parent"
    );
    assert_eq!(value["data"]["context"]["metadata"]["branch"], 1);

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "context",
            "close",
            "Child Context",
            "--summary",
            "Child finished",
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "context_close");
    assert_eq!(value["data"]["context"]["state"], "closed");
    assert_eq!(value["data"]["context"]["summary"], "Child finished");
    assert!(value["data"]["context"]["closed_at"].as_str().is_some());

    let (code, stdout, stderr) = run(
        &["missive", "context", "export", "Parent Context", "--json"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(!stdout.contains("value-hidden-in-output"));
    let value = json_success(&stdout, "context_export");
    assert_eq!(value["data"]["redacted"], true);
    assert_eq!(value["data"]["counts"]["task_count"], 1);
    assert_eq!(value["data"]["counts"]["message_count"], 1);
    assert_eq!(value["data"]["counts"]["event_count"], 1);
    assert_eq!(value["data"]["tasks"][0]["remote_task"]["token"], REDACTED);
    assert_eq!(value["data"]["messages"][0]["content"]["token"], REDACTED);
    assert_eq!(value["data"]["events"][0]["payload"]["token"], REDACTED);
    assert_eq!(
        value["data"]["messages"][0]["content"]["headers"]["Authorization"],
        format!("Bearer {REDACTED}")
    );
}
