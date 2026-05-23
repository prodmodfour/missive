use std::collections::BTreeMap;
use std::path::Path;
use std::thread;
use std::time::Duration;

use missive_cli::{OUTPUT_SCHEMA_VERSION, REDACTED, run_from_with_environment};
use missive_core::{AgentAlias, ContextId, EventId, MissiveExitCode, TaskId};
use missive_store::{ContextUpsert, EventInsert, Store, TaskState, TaskUpsert};
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

fn run_owned(
    args: Vec<String>,
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

fn ndjson_lines(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("NDJSON line should parse"))
        .inspect(|value| {
            assert_eq!(value["schema_version"], OUTPUT_SCHEMA_VERSION);
            assert_eq!(value["ok"], true);
        })
        .collect()
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

fn seed_task(store: &Store, agent: &AgentAlias, context: &ContextId, task: &TaskId) {
    let mut context_upsert = ContextUpsert::new(context.clone());
    context_upsert.agent_alias = Some(agent.clone());
    store.upsert_context(&context_upsert).expect("context");

    let mut task_upsert = TaskUpsert::new(task.clone(), agent.clone(), TaskState::Working);
    task_upsert.context_id = Some(context.clone());
    task_upsert
        .record_a2a_protocol_version("1.0")
        .expect("protocol version");
    store.upsert_task(&task_upsert).expect("task");
}

fn append_task_event(
    store: &Store,
    event_id: &str,
    agent: &AgentAlias,
    context: &ContextId,
    task: &TaskId,
    state: &str,
) -> i64 {
    let mut event = EventInsert::new(
        EventId::new(event_id).expect("event id"),
        "cli",
        "a2a.task.updated",
        json!({
            "task_id": task.as_str(),
            "context_id": context.as_str(),
            "agent": agent.as_str(),
            "state": state,
            "token": "value-hidden-in-output",
        }),
    );
    event.agent_alias = Some(agent.clone());
    event.context_id = Some(context.clone());
    event.task_id = Some(task.clone());
    event
        .record_a2a_protocol_version("1.0")
        .expect("event protocol version");
    store.append_event(&event).expect("event").sequence
}

fn append_diagnostic_event(store: &Store, event_id: &str, message: &str) -> i64 {
    let event = EventInsert::new(
        EventId::new(event_id).expect("event id"),
        "cli",
        "diagnostic.log",
        json!({
            "message": message,
        }),
    );
    store.append_event(&event).expect("event").sequence
}

#[test]
fn events_list_and_export_render_redacted_records() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    add_agent("echo", "http://127.0.0.1:65530", &environment, temp.path());

    let store = open_store(&home);
    let agent = AgentAlias::new("echo").expect("agent");
    let context = ContextId::new("ctx-events").expect("context");
    let task = TaskId::new("task-events").expect("task");
    seed_task(&store, &agent, &context, &task);
    let sequence = append_task_event(
        &store,
        "evt-events-list",
        &agent,
        &context,
        &task,
        "working",
    );

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "events",
            "list",
            "--task",
            "task-events",
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "events_list");
    assert_eq!(value["data"]["count"], 1);
    let event = &value["data"]["events"][0];
    assert_eq!(event["sequence"], sequence);
    assert!(event["timestamp"].as_str().is_some());
    assert_eq!(event["source"], "cli");
    assert_eq!(event["event_type"], "a2a.task.updated");
    assert_eq!(event["agent"], "echo");
    assert_eq!(event["context_id"], "ctx-events");
    assert_eq!(event["task_id"], "task-events");
    assert_eq!(event["payload"]["token"], REDACTED);
    assert_eq!(event["redacted"], true);

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "events",
            "export",
            "--task",
            "task-events",
            "--ndjson",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let lines = ndjson_lines(&stdout);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["kind"], "event_record");
    assert_eq!(lines[0]["sequence"], sequence);
    assert_eq!(lines[0]["data"]["payload"]["token"], REDACTED);

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "events",
            "list",
            "--agent",
            "echo",
            "--type",
            "missive.agent.add",
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "events_list");
    assert_eq!(value["data"]["count"], 1);
    assert_eq!(
        value["data"]["events"][0]["event_type"],
        "missive.agent.add"
    );
}

#[test]
fn events_replay_reconstructs_task_and_context_summaries() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    add_agent("echo", "http://127.0.0.1:65530", &environment, temp.path());

    let store = open_store(&home);
    let agent = AgentAlias::new("echo").expect("agent");
    let context = ContextId::new("ctx-replay").expect("context");
    let task = TaskId::new("task-replay").expect("task");
    seed_task(&store, &agent, &context, &task);
    append_task_event(
        &store,
        "evt-replay-working",
        &agent,
        &context,
        &task,
        "working",
    );
    append_task_event(
        &store,
        "evt-replay-completed",
        &agent,
        &context,
        &task,
        "completed",
    );

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "events",
            "replay",
            "--context",
            "ctx-replay",
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "events_replay");
    assert_eq!(value["data"]["event_count"], 2);
    assert_eq!(value["data"]["context_count"], 1);
    assert_eq!(value["data"]["task_count"], 1);
    assert_eq!(value["data"]["event_types"]["a2a.task.updated"], 2);
    assert_eq!(value["data"]["contexts"][0]["context_id"], "ctx-replay");
    assert_eq!(
        value["data"]["contexts"][0]["task_ids"],
        json!(["task-replay"])
    );
    assert_eq!(value["data"]["tasks"][0]["task_id"], "task-replay");
    assert_eq!(value["data"]["tasks"][0]["context_id"], "ctx-replay");
    assert_eq!(value["data"]["tasks"][0]["state"], "completed");
}

#[test]
fn events_tail_follows_new_events() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    add_agent("echo", "http://127.0.0.1:65530", &environment, temp.path());

    let store = open_store(&home);
    let agent = AgentAlias::new("echo").expect("agent");
    let context = ContextId::new("ctx-tail").expect("context");
    let task = TaskId::new("task-tail").expect("task");
    seed_task(&store, &agent, &context, &task);
    let from_sequence = store
        .list_events()
        .expect("events")
        .last()
        .map_or(0, |event| event.sequence);

    let tail_environment = environment.clone();
    let tail_dir = temp.path().to_path_buf();
    let tail_args = vec![
        "missive".to_owned(),
        "--timeout".to_owned(),
        "2s".to_owned(),
        "events".to_owned(),
        "tail".to_owned(),
        "--task".to_owned(),
        "task-tail".to_owned(),
        "--from-sequence".to_owned(),
        from_sequence.to_string(),
        "--limit".to_owned(),
        "1".to_owned(),
        "--poll-interval".to_owned(),
        "10ms".to_owned(),
        "--ndjson".to_owned(),
    ];
    let handle = thread::spawn(move || run_owned(tail_args, &tail_environment, &tail_dir));

    thread::sleep(Duration::from_millis(50));
    let sequence = append_task_event(
        &store,
        "evt-tail-follow",
        &agent,
        &context,
        &task,
        "completed",
    );

    let (code, stdout, stderr) = handle.join().expect("tail thread");
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let lines = ndjson_lines(&stdout);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["kind"], "event_record");
    assert_eq!(lines[0]["sequence"], sequence);
    assert_eq!(lines[0]["data"]["event_type"], "a2a.task.updated");
    assert_eq!(lines[0]["data"]["task_id"], "task-tail");
}

#[test]
fn events_tail_json_timeout_is_bounded_and_machine_readable() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    add_agent("echo", "http://127.0.0.1:65530", &environment, temp.path());

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "--timeout",
            "100ms",
            "events",
            "tail",
            "--type",
            "diagnostic.log",
            "--limit",
            "1",
            "--poll-interval",
            "10ms",
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "events_tail");
    assert_eq!(value["data"]["emitted"], 0);
    assert_eq!(value["data"]["timed_out"], true);
    assert!(
        value["data"]["events"]
            .as_array()
            .expect("events")
            .is_empty()
    );
}

#[test]
fn events_tail_ndjson_redacts_secret_like_text_values() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    add_agent("echo", "http://127.0.0.1:65530", &environment, temp.path());

    let store = open_store(&home);
    let sequence = append_diagnostic_event(
        &store,
        "evt-tail-diagnostic",
        "token=value-hidden-in-output Authorization: Bearer value-hidden-in-output",
    );

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "events",
            "tail",
            "--type",
            "diagnostic.log",
            "--from-sequence",
            "0",
            "--limit",
            "1",
            "--ndjson",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let lines = ndjson_lines(&stdout);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["kind"], "event_record");
    assert_eq!(lines[0]["sequence"], sequence);
    assert_eq!(
        lines[0]["data"]["payload"]["message"],
        format!("token={REDACTED} Authorization: {REDACTED}")
    );
    assert!(!stdout.contains("value-hidden-in-output"));
}
