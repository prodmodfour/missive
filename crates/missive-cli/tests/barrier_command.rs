use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::Path;

use missive_cli::{
    OUTPUT_SCHEMA_VERSION, run_from_with_environment, run_from_with_environment_and_input,
};
use missive_core::{AgentAlias, ContextId, MissiveExitCode, TaskId};
use missive_store::{ContextUpsert, Store, TaskState, TaskUpsert};
use missive_test_support::{MockA2aServer, send_message_response_task, task_json};
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

fn run_with_input(
    args: &[&str],
    input: &mut Cursor<Vec<u8>>,
    environment: &BTreeMap<String, String>,
    current_dir: &Path,
) -> (i32, String, String) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = run_from_with_environment_and_input(
        args,
        environment,
        current_dir,
        input,
        &mut stdout,
        &mut stderr,
    );

    (
        code,
        String::from_utf8(stdout).expect("stdout should be UTF-8"),
        String::from_utf8(stderr).expect("stderr should be UTF-8"),
    )
}

fn json_envelope(stdout: &str, expected_kind: &str) -> Value {
    let value: Value = serde_json::from_str(stdout).expect("stdout should be JSON");
    assert_eq!(value["schema_version"], OUTPUT_SCHEMA_VERSION);
    assert_eq!(value["kind"], expected_kind);
    value
}

fn json_error(stderr: &str) -> Value {
    serde_json::from_str(stderr).expect("stderr should be JSON")
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

fn create_group(environment: &BTreeMap<String, String>, current_dir: &Path) {
    let (code, _stdout, stderr) = run(
        &[
            "missive",
            "group",
            "create",
            "team",
            "--routing-policy",
            "broadcast",
            "--json",
        ],
        environment,
        current_dir,
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
}

fn add_member(
    group: &str,
    agent: &str,
    rank: &str,
    environment: &BTreeMap<String, String>,
    current_dir: &Path,
) {
    let (code, _stdout, stderr) = run(
        &[
            "missive", "group", "add", group, agent, "--rank", rank, "--json",
        ],
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

fn upsert_local_task(home: &Path, agent: &str, task_id: &str, context: &str, state: TaskState) {
    let store = open_store(home);
    let context_id = ContextId::new(context.to_owned()).expect("context id");
    store
        .upsert_context(&ContextUpsert::new(context_id.clone()))
        .expect("context");
    let mut task = TaskUpsert::new(
        TaskId::new(task_id.to_owned()).expect("task id"),
        AgentAlias::new(agent.to_owned()).expect("agent alias"),
        state,
    );
    task.context_id = Some(context_id);
    task.source = missive_store::TaskSource::Local;
    store.upsert_task(&task).expect("task upsert");
}

#[test]
fn barrier_consumes_bcast_output_and_polls_until_all_members_complete() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let context_id = "ctx-barrier-success";

    let alpha = MockA2aServer::start();
    alpha
        .handle()
        .set_send_response(send_message_response_task(task_json(
            "task-alpha-barrier",
            context_id,
            "TASK_STATE_SUBMITTED",
            "alpha accepted",
        )));
    alpha.handle().enqueue_task_states(
        "task-alpha-barrier",
        context_id,
        ["TASK_STATE_WORKING", "TASK_STATE_COMPLETED"],
    );
    let beta = MockA2aServer::start();
    beta.handle()
        .set_send_response(send_message_response_task(task_json(
            "task-beta-barrier",
            context_id,
            "TASK_STATE_SUBMITTED",
            "beta accepted",
        )));
    beta.handle()
        .enqueue_task_states("task-beta-barrier", context_id, ["TASK_STATE_COMPLETED"]);

    add_agent("alpha", alpha.base_url(), &environment, temp.path());
    add_agent("beta", beta.base_url(), &environment, temp.path());
    create_group(&environment, temp.path());
    add_member("team", "alpha", "rank-0", &environment, temp.path());
    add_member("team", "beta", "rank-1", &environment, temp.path());

    let (code, bcast_stdout, stderr) = run(
        &[
            "missive",
            "bcast",
            "team",
            "hello barrier",
            "--context",
            context_id,
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");

    let mut input = Cursor::new(bcast_stdout.into_bytes());
    let (code, stdout, stderr) = run_with_input(
        &[
            "missive",
            "barrier",
            "team",
            "--from-bcast",
            "-",
            "--timeout",
            "2s",
            "--interval",
            "10ms",
            "--json",
        ],
        &mut input,
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_envelope(&stdout, "barrier_result");
    let data = &value["data"];
    assert_eq!(data["status"], "succeeded");
    assert_eq!(data["group"], "team");
    assert_eq!(data["context_id"], context_id);
    assert_eq!(data["from_bcast"], true);
    assert_eq!(data["required"], 2);
    assert_eq!(data["reached_count"], 2);
    assert_eq!(data["failure_count"], 0);
    assert_eq!(data["success_states"], serde_json::json!(["completed"]));
    assert_eq!(
        data["target_states"],
        serde_json::json!(["completed", "failed", "cancelled"])
    );
    assert_eq!(data["members"][0]["agent"], "alpha");
    assert_eq!(data["members"][0]["task_id"], "task-alpha-barrier");
    assert_eq!(data["members"][0]["state"], "completed");
    assert_eq!(data["members"][0]["status"], "satisfied");
    assert_eq!(data["members"][1]["agent"], "beta");
    assert_eq!(data["members"][1]["state"], "completed");

    let alpha_requests = alpha.requests();
    assert!(
        alpha_requests
            .iter()
            .any(|request| request.path == "/a2a/tasks/task-alpha-barrier"),
        "alpha should be polled with GetTask: {alpha_requests:?}"
    );
    let beta_requests = beta.requests();
    assert!(
        beta_requests
            .iter()
            .any(|request| request.path == "/a2a/tasks/task-beta-barrier"),
        "beta should be polled with GetTask: {beta_requests:?}"
    );

    let store = open_store(&home);
    assert_eq!(
        store
            .get_task(&TaskId::new("task-alpha-barrier").expect("task id"))
            .expect("task")
            .expect("task")
            .state,
        TaskState::Completed
    );
    let events = store.list_events().expect("events");
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "missive.barrier.started")
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "missive.barrier.member.satisfied")
            .count(),
        2
    );
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "missive.barrier.completed")
    );
}

#[test]
fn barrier_local_terminal_failures_use_deterministic_exit_codes() {
    let cases = [
        (TaskState::Failed, "failed", MissiveExitCode::TaskFailed),
        (
            TaskState::Cancelled,
            "cancelled",
            MissiveExitCode::TaskCancelled,
        ),
    ];

    for (state, expected_status, expected_code) in cases {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join(format!("missive-home-{expected_status}"));
        let environment = isolated_env(&home);
        let server = MockA2aServer::start();
        add_agent("alpha", server.base_url(), &environment, temp.path());
        create_group(&environment, temp.path());
        add_member("team", "alpha", "rank-0", &environment, temp.path());
        upsert_local_task(&home, "alpha", "task-terminal", "ctx-terminal", state);

        let (code, stdout, stderr) = run(
            &[
                "missive",
                "barrier",
                "team",
                "--context",
                "ctx-terminal",
                "--local",
                "--json",
            ],
            &environment,
            temp.path(),
        );

        assert_eq!(code, expected_code.as_i32(), "stderr: {stderr}");
        let value = json_envelope(&stdout, "barrier_result");
        assert_eq!(value["data"]["status"], expected_status);
        assert_eq!(value["data"]["members"][0]["status"], expected_status);
        assert_eq!(value["data"]["members"][0]["state"], expected_status);
        assert_eq!(
            json_error(&stderr)["data"]["exit_code"],
            expected_code.as_u8()
        );
    }
}

#[test]
fn barrier_quorum_continue_policy_can_succeed_after_partial_failure() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let alpha = MockA2aServer::start();
    let beta = MockA2aServer::start();
    add_agent("alpha", alpha.base_url(), &environment, temp.path());
    add_agent("beta", beta.base_url(), &environment, temp.path());
    create_group(&environment, temp.path());
    add_member("team", "alpha", "rank-0", &environment, temp.path());
    add_member("team", "beta", "rank-1", &environment, temp.path());
    upsert_local_task(
        &home,
        "alpha",
        "task-alpha",
        "ctx-quorum",
        TaskState::Completed,
    );
    upsert_local_task(&home, "beta", "task-beta", "ctx-quorum", TaskState::Failed);

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "barrier",
            "team",
            "--context",
            "ctx-quorum",
            "--local",
            "--required",
            "1",
            "--failure-policy",
            "continue",
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_envelope(&stdout, "barrier_result");
    assert_eq!(value["data"]["status"], "succeeded");
    assert_eq!(value["data"]["required"], 1);
    assert_eq!(value["data"]["reached_count"], 1);
    assert_eq!(value["data"]["failure_count"], 1);
}

#[test]
fn barrier_requested_non_terminal_state_succeeds() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let server = MockA2aServer::start();
    add_agent("alpha", server.base_url(), &environment, temp.path());
    create_group(&environment, temp.path());
    add_member("team", "alpha", "rank-0", &environment, temp.path());
    upsert_local_task(
        &home,
        "alpha",
        "task-working",
        "ctx-working",
        TaskState::Working,
    );

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "barrier",
            "team",
            "--context",
            "ctx-working",
            "--local",
            "--state",
            "working",
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_envelope(&stdout, "barrier_result");
    assert_eq!(value["data"]["status"], "succeeded");
    assert_eq!(
        value["data"]["success_states"],
        serde_json::json!(["working"])
    );
    assert_eq!(value["data"]["members"][0]["status"], "satisfied");
}

#[test]
fn barrier_timeout_exits_deterministically() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let server = MockA2aServer::start();
    add_agent("alpha", server.base_url(), &environment, temp.path());
    create_group(&environment, temp.path());
    add_member("team", "alpha", "rank-0", &environment, temp.path());
    upsert_local_task(
        &home,
        "alpha",
        "task-working",
        "ctx-timeout",
        TaskState::Working,
    );

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "barrier",
            "team",
            "--context",
            "ctx-timeout",
            "--local",
            "--timeout",
            "25ms",
            "--interval",
            "10ms",
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(
        code,
        MissiveExitCode::TaskTimeout.as_i32(),
        "stderr: {stderr}"
    );
    let value = json_envelope(&stdout, "barrier_result");
    assert_eq!(value["data"]["status"], "timeout");
    assert_eq!(value["data"]["pending_count"], 1);
    assert_eq!(value["data"]["members"][0]["status"], "pending");
    assert_eq!(
        json_error(&stderr)["data"]["exit_code"],
        MissiveExitCode::TaskTimeout.as_u8()
    );
}
