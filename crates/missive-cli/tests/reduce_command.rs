use std::collections::BTreeMap;
use std::path::Path;

use missive_cli::{OUTPUT_SCHEMA_VERSION, run_from_with_environment};
use missive_core::MissiveExitCode;
use missive_store::Store;
use missive_test_support::{
    MockA2aServer, send_message_response_message, send_message_response_task, task_json,
};
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

fn setup_group_with_agents(
    environment: &BTreeMap<String, String>,
    current_dir: &Path,
    alpha_base_url: &str,
    beta_base_url: &str,
) {
    add_agent("alpha", alpha_base_url, environment, current_dir);
    add_agent("beta", beta_base_url, environment, current_dir);
    create_group(environment, current_dir);
    add_member("team", "alpha", "rank-0", environment, current_dir);
    add_member("team", "beta", "rank-1", environment, current_dir);
}

fn seed_member_outputs(
    environment: &BTreeMap<String, String>,
    current_dir: &Path,
    context_id: &str,
) -> (MockA2aServer, MockA2aServer) {
    let alpha = MockA2aServer::start();
    alpha
        .handle()
        .set_send_response(send_message_response_task(task_json(
            "task-alpha-reduce",
            context_id,
            "TASK_STATE_COMPLETED",
            "alpha final answer",
        )));
    let beta = MockA2aServer::start();
    beta.handle()
        .set_send_response(send_message_response_task(task_json(
            "task-beta-reduce",
            context_id,
            "TASK_STATE_COMPLETED",
            "beta final answer",
        )));

    setup_group_with_agents(environment, current_dir, alpha.base_url(), beta.base_url());

    let (code, _stdout, stderr) = run(
        &[
            "missive",
            "bcast",
            "team",
            "collect these outputs",
            "--context",
            context_id,
            "--json",
        ],
        environment,
        current_dir,
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");

    (alpha, beta)
}

#[test]
fn reduce_local_summary_records_provenance_and_events() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let context_id = "ctx-reduce-local";
    let (_alpha, _beta) = seed_member_outputs(&environment, temp.path(), context_id);

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "reduce",
            "team",
            "--context",
            context_id,
            "--strategy",
            "summarise",
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_envelope(&stdout, "reduce_result");
    assert_eq!(value["ok"], true);
    let data = &value["data"];
    assert_eq!(data["group"], "team");
    assert_eq!(data["context_id"], context_id);
    assert_eq!(data["strategy"], "summarise");
    assert_eq!(data["status"], "succeeded");
    assert_eq!(data["reducer"]["method"], "local");
    assert_eq!(data["gathered_count"], 2);
    assert_eq!(data["text_input_count"], 2);
    assert!(
        data["reduced_text"]
            .as_str()
            .expect("reduced text")
            .contains("alpha final answer")
    );
    assert_eq!(data["provenance"][0]["agent"], "alpha");
    assert_eq!(data["provenance"][0]["rank"], "rank-0");
    assert_eq!(
        data["provenance"][0]["task"]["task_id"],
        "task-alpha-reduce"
    );
    assert_eq!(
        data["provenance"][0]["messages"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        data["provenance"][0]["artifacts"].as_array().unwrap().len(),
        1
    );
    assert_eq!(data["provenance"][1]["agent"], "beta");
    assert!(data["persistence"]["reduced_message_id"].as_str().is_some());

    let store = open_store(&home);
    let reduced_message_id = data["persistence"]["reduced_message_id"]
        .as_str()
        .expect("message id")
        .parse()
        .expect("message id parse");
    let reduced_message = store
        .get_message(&reduced_message_id)
        .expect("get message")
        .expect("stored reduced message");
    assert_eq!(reduced_message.direction.as_str(), "local");
    assert_eq!(
        reduced_message.metadata.get_str("missive.reduce.strategy"),
        Some("summarise")
    );
    let events = store.list_events().expect("events");
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "missive.reduce.started")
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "missive.reduce.input.gathered")
            .count(),
        2
    );
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "missive.reduce.completed")
    );
}

#[test]
fn reduce_can_send_prompt_to_mocked_reducer_agent() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let context_id = "ctx-reduce-agent";
    let (_alpha, _beta) = seed_member_outputs(&environment, temp.path(), context_id);
    let reducer = MockA2aServer::start();
    reducer
        .handle()
        .set_send_response(send_message_response_message(
            "msg-reducer-response",
            context_id,
            "remote reduced answer",
        ));
    add_agent("reducer", reducer.base_url(), &environment, temp.path());

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "reduce",
            "team",
            "--context",
            context_id,
            "--strategy",
            "merge",
            "--reducer-agent",
            "reducer",
            "--template",
            "Reduce {{group}}/{{context_id}} with {{strategy}}:\n{{inputs}}",
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_envelope(&stdout, "reduce_result");
    let data = &value["data"];
    assert_eq!(data["reducer"]["method"], "agent");
    assert_eq!(data["reducer"]["agent"]["agent"], "reducer");
    assert_eq!(data["reducer"]["agent"]["response_shape"], "message");
    assert_eq!(data["reduced_text"], "remote reduced answer");
    assert_eq!(data["provenance"].as_array().expect("provenance").len(), 2);

    let send_request = reducer
        .requests()
        .into_iter()
        .find(|request| request.path == "/a2a/message:send")
        .expect("reducer send request");
    let body = send_request.json_body().expect("send body");
    let prompt = body["message"]["parts"][0]["text"]
        .as_str()
        .expect("prompt text");
    assert!(prompt.contains("Reduce team/ctx-reduce-agent with merge"));
    assert!(prompt.contains("alpha final answer"));
    assert!(prompt.contains("beta final answer"));
}

#[test]
fn reduce_without_gathered_inputs_fails_clearly() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    setup_group_with_agents(
        &environment,
        temp.path(),
        "http://127.0.0.1:18081",
        "http://127.0.0.1:18082",
    );

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "reduce",
            "team",
            "--context",
            "ctx-missing-reduce",
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Usage.as_i32());
    assert!(stdout.is_empty());
    let error: Value = serde_json::from_str(&stderr).expect("JSON error");
    assert!(
        error["data"]["message"]
            .as_str()
            .expect("message")
            .contains("no gathered member outputs")
    );
}
