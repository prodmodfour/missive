use std::collections::BTreeMap;
use std::path::Path;

use missive_cli::{OUTPUT_SCHEMA_VERSION, run_from_with_environment};
use missive_core::MissiveExitCode;
use missive_store::Store;
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

#[test]
fn gather_json_preserves_rank_order_and_exports_artifacts_safely() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let context_id = "ctx-gather-success";

    let alpha = MockA2aServer::start();
    alpha
        .handle()
        .set_send_response(send_message_response_task(task_json(
            "task-alpha-gather",
            context_id,
            "TASK_STATE_COMPLETED",
            "alpha final answer",
        )));
    let beta = MockA2aServer::start();
    beta.handle()
        .set_send_response(send_message_response_task(task_json(
            "task-beta-gather",
            context_id,
            "TASK_STATE_COMPLETED",
            "beta final answer",
        )));

    setup_group_with_agents(&environment, temp.path(), alpha.base_url(), beta.base_url());

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
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");

    let export_dir = temp.path().join("exports");
    let export_arg = export_dir.to_string_lossy().into_owned();
    let (code, stdout, stderr) = run(
        &[
            "missive",
            "gather",
            "team",
            "--context",
            context_id,
            "--output-dir",
            &export_arg,
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_envelope(&stdout, "gather_result");
    assert_eq!(value["ok"], true);
    let data = &value["data"];
    assert_eq!(data["group"], "team");
    assert_eq!(data["context_id"], context_id);
    assert_eq!(data["status"], "succeeded");
    assert_eq!(data["member_count"], 2);
    assert_eq!(data["gathered_count"], 2);
    assert_eq!(data["missing_count"], 0);
    assert_eq!(data["artifact_count"], 2);
    assert_eq!(data["exported_artifact_count"], 2);
    assert_eq!(data["members"][0]["agent"], "alpha");
    assert_eq!(data["members"][0]["rank"], "rank-0");
    assert_eq!(data["members"][0]["task"]["task_id"], "task-alpha-gather");
    assert_eq!(data["members"][0]["text"], "alpha final answer");
    assert_eq!(data["members"][1]["agent"], "beta");
    assert_eq!(data["members"][1]["rank"], "rank-1");
    assert_eq!(data["members"][1]["text"], "beta final answer");

    let alpha_export = data["members"][0]["exported_artifacts"][0]["path"]
        .as_str()
        .expect("alpha export path");
    let beta_export = data["members"][1]["exported_artifacts"][0]["path"]
        .as_str()
        .expect("beta export path");
    assert_ne!(alpha_export, beta_export);
    assert!(alpha_export.contains("rank-0-alpha-answer"));
    assert!(beta_export.contains("rank-1-beta-answer"));
    assert!(Path::new(alpha_export).is_file());
    assert!(Path::new(beta_export).is_file());

    let store = open_store(&home);
    let events = store.list_events().expect("events");
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "missive.gather.started")
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "missive.gather.member.gathered")
            .count(),
        2
    );
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "missive.gather.completed")
    );

    let (code, _stdout, stderr) = run(
        &[
            "missive",
            "gather",
            "team",
            "--context",
            context_id,
            "--output-dir",
            &export_arg,
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Usage.as_i32());
    assert!(
        json_error(&stderr)["data"]["message"]
            .as_str()
            .expect("message")
            .contains("refusing to overwrite")
    );
}

#[test]
fn gather_represents_missing_outputs_and_supports_ndjson() {
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
            "gather",
            "team",
            "--context",
            "ctx-missing-gather",
            "--ndjson",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let lines = stdout.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "stdout: {stdout}");
    let value: Value = serde_json::from_str(lines[0]).expect("NDJSON line");
    assert_eq!(value["schema_version"], OUTPUT_SCHEMA_VERSION);
    assert_eq!(value["sequence"], 0);
    assert_eq!(value["kind"], "gather_result");
    let data = &value["data"];
    assert_eq!(data["status"], "missing");
    assert_eq!(data["gathered_count"], 0);
    assert_eq!(data["missing_count"], 2);
    assert_eq!(data["members"][0]["agent"], "alpha");
    assert_eq!(data["members"][0]["status"], "missing_task");
    assert_eq!(data["members"][1]["agent"], "beta");
    assert_eq!(data["members"][1]["status"], "missing_task");
}
