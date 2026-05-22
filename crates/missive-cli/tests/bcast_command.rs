use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use missive_cli::{OUTPUT_SCHEMA_VERSION, run_from_with_environment};
use missive_core::MissiveExitCode;
use missive_store::{MessageDirection, Store, TaskState};
use missive_test_support::{MockA2aServer, send_message_response_task, task_json};
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

fn json_envelope(stdout: &str, expected_kind: &str) -> Value {
    let value: Value = serde_json::from_str(stdout).expect("stdout should be JSON");
    assert_eq!(value["schema_version"], OUTPUT_SCHEMA_VERSION);
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

#[test]
fn bcast_concurrent_success_persists_member_tasks_messages_and_events() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let context_id = "ctx-bcast-success";

    let alpha = MockA2aServer::start();
    alpha
        .handle()
        .set_send_response(send_message_response_task(task_json(
            "task-alpha-bcast",
            context_id,
            "TASK_STATE_SUBMITTED",
            "alpha accepted",
        )));
    let beta = MockA2aServer::start();
    beta.handle()
        .set_send_response(send_message_response_task(task_json(
            "task-beta-bcast",
            context_id,
            "TASK_STATE_WORKING",
            "beta working",
        )));

    add_agent("alpha", alpha.base_url(), &environment, temp.path());
    add_agent("beta", beta.base_url(), &environment, temp.path());
    create_group(&environment, temp.path());
    add_member("team", "alpha", "rank-0", &environment, temp.path());
    add_member("team", "beta", "rank-1", &environment, temp.path());

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "bcast",
            "team",
            "hello collective",
            "--context",
            context_id,
            "--execution",
            "concurrent",
            "--accepted-output-mode",
            "text/plain",
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_envelope(&stdout, "bcast_result");
    assert_eq!(value["ok"], true);
    let data = &value["data"];
    assert_eq!(data["group"], "team");
    assert_eq!(data["execution"], "concurrent");
    assert_eq!(data["failure_policy"], "stop");
    assert_eq!(data["status"], "succeeded");
    assert_eq!(data["request"]["context_id"], context_id);
    assert_eq!(data["request"]["context_created"], true);
    assert_eq!(data["request"]["part_count"], 1);
    assert_eq!(data["member_count"], 2);
    assert_eq!(data["success_count"], 2);
    assert_eq!(data["failure_count"], 0);
    assert_eq!(data["members"].as_array().expect("members").len(), 2);
    assert_eq!(data["members"][0]["agent"], "alpha");
    assert_eq!(data["members"][0]["rank"], "rank-0");
    assert_eq!(data["members"][0]["task_id"], "task-alpha-bcast");
    assert_eq!(data["members"][0]["state"], "submitted");
    assert_eq!(data["members"][1]["agent"], "beta");
    assert_eq!(data["members"][1]["task_id"], "task-beta-bcast");
    assert_eq!(data["members"][1]["state"], "working");

    for requests in [alpha.requests(), beta.requests()] {
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].path, "/.well-known/agent-card.json");
        assert_eq!(requests[1].method, "POST");
        assert_eq!(requests[1].path, "/a2a/message:send");
        assert_eq!(
            requests[1].header("a2a-version"),
            Some("1.0"),
            "A2A-Version should be sent to every member"
        );
        let body: Value = requests[1].json_body().expect("request JSON");
        assert_eq!(body["message"]["contextId"], context_id);
        assert_eq!(body["message"]["parts"][0]["text"], "hello collective");
        assert_eq!(
            body["configuration"]["acceptedOutputModes"],
            json!(["text/plain"])
        );
    }

    let store = open_store(&home);
    let tasks = store.list_tasks().expect("tasks");
    assert_eq!(tasks.len(), 2);
    assert!(tasks.iter().any(|task| {
        task.task_id.as_str() == "task-alpha-bcast" && task.state == TaskState::Submitted
    }));
    assert!(tasks.iter().any(|task| {
        task.task_id.as_str() == "task-beta-bcast" && task.state == TaskState::Working
    }));
    let messages = store.list_messages().expect("messages");
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.direction == MessageDirection::Request)
            .count(),
        2
    );
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.direction == MessageDirection::Response)
            .count(),
        2
    );
    let events = store.list_events().expect("events");
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "missive.bcast.started")
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "missive.bcast.member.succeeded")
            .count(),
        2
    );
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "missive.bcast.completed")
    );
}

#[test]
fn bcast_continue_policy_reports_partial_failure() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let context_id = "ctx-bcast-partial";

    let good = MockA2aServer::start();
    good.handle()
        .set_send_response(send_message_response_task(task_json(
            "task-good-bcast",
            context_id,
            "TASK_STATE_COMPLETED",
            "good done",
        )));
    let bad = MockA2aServer::builder().malformed_send_response().start();

    add_agent("good", good.base_url(), &environment, temp.path());
    add_agent("bad", bad.base_url(), &environment, temp.path());
    create_group(&environment, temp.path());
    add_member("team", "good", "rank-0", &environment, temp.path());
    add_member("team", "bad", "rank-1", &environment, temp.path());

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "bcast",
            "team",
            "hello partial",
            "--context",
            context_id,
            "--failure-policy",
            "continue",
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_envelope(&stdout, "bcast_result");
    let data = &value["data"];
    assert_eq!(data["status"], "partial_failure");
    assert_eq!(data["success_count"], 1);
    assert_eq!(data["failure_count"], 1);
    assert_eq!(data["members"][0]["status"], "succeeded");
    assert_eq!(data["members"][0]["task_id"], "task-good-bcast");
    assert_eq!(data["members"][1]["status"], "failed");
    assert_eq!(data["members"][1]["agent"], "bad");
    assert_eq!(data["members"][1]["error"]["code"], "missive::protocol");

    let store = open_store(&home);
    assert_eq!(store.list_tasks().expect("tasks").len(), 1);
    let events = store.list_events().expect("events");
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "missive.bcast.member.failed")
    );
    assert!(events.iter().any(|event| {
        event.event_type == "missive.bcast.completed"
            && event.payload_json["status"] == "partial_failure"
    }));
}

#[test]
fn bcast_timeout_returns_summary_and_timeout_exit_code() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let context_id = "ctx-bcast-timeout";
    let slow = MockA2aServer::builder()
        .send_response_delay(Duration::from_millis(200))
        .start();
    slow.handle()
        .set_send_response(send_message_response_task(task_json(
            "task-timeout-late",
            context_id,
            "TASK_STATE_SUBMITTED",
            "too late",
        )));

    add_agent("slow", slow.base_url(), &environment, temp.path());
    create_group(&environment, temp.path());
    add_member("team", "slow", "rank-0", &environment, temp.path());

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "bcast",
            "team",
            "hello timeout",
            "--context",
            context_id,
            "--timeout",
            "50ms",
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
    assert!(!stderr.is_empty(), "timeout should render an error");
    let value = json_envelope(&stdout, "bcast_result");
    let data = &value["data"];
    assert_eq!(data["status"], "timeout");
    assert_eq!(data["success_count"], 0);
    assert_eq!(data["failure_count"], 1);
    assert_eq!(data["members"][0]["status"], "timeout");
    assert_eq!(data["members"][0]["agent"], "slow");

    let store = open_store(&home);
    assert!(store.list_tasks().expect("tasks").is_empty());
    assert!(
        store
            .list_events()
            .expect("events")
            .iter()
            .any(|event| event.event_type == "missive.bcast.member.timeout")
    );
}
