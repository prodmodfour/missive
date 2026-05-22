use std::collections::BTreeMap;
use std::path::Path;

use missive_cli::{OUTPUT_SCHEMA_VERSION, run_from_with_environment};
use missive_core::MissiveExitCode;
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

fn json_error(stderr: &str) -> Value {
    let value: Value = serde_json::from_str(stderr).expect("stderr should be JSON");
    assert_eq!(value["schema_version"], OUTPUT_SCHEMA_VERSION);
    assert_eq!(value["ok"], false);
    assert_eq!(value["kind"], "error");
    value
}

fn add_agent(alias: &str, port: u16, environment: &BTreeMap<String, String>, current_dir: &Path) {
    let base_url = format!("http://127.0.0.1:{port}");
    let (code, _stdout, stderr) = run(
        &["missive", "agent", "add", alias, &base_url, "--json"],
        environment,
        current_dir,
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
}

#[test]
fn group_json_crud_round_trip_preserves_membership() {
    let temp = tempdir().expect("tempdir");
    let environment = isolated_env(&temp.path().join("missive-home"));
    add_agent("echo", 8081, &environment, temp.path());
    add_agent("planner", 8082, &environment, temp.path());

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "group",
            "create",
            "team",
            "--routing-policy",
            "weighted",
            "--notes",
            "Routing test team",
            "--metadata",
            "purpose=test",
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_success(&stdout, "group_create");
    assert_eq!(value["data"]["group"]["name"], "team");
    assert_eq!(value["data"]["group"]["routing_policy"], "weighted");
    assert_eq!(value["data"]["group"]["member_count"], 0);
    assert_eq!(value["data"]["group"]["metadata"]["purpose"], "test");

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "group",
            "add",
            "team",
            "echo",
            "--rank",
            "rank-0",
            "--tag",
            "writer",
            "--weight",
            "2.5",
            "--routing-metadata",
            "lane=blue",
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_success(&stdout, "group_add");
    assert_eq!(value["data"]["member"]["agent"], "echo");
    assert_eq!(value["data"]["member"]["rank"], "rank-0");
    assert_eq!(
        value["data"]["member"]["tags"],
        serde_json::json!(["writer"])
    );
    assert_eq!(value["data"]["member"]["weight"], 2.5);
    assert_eq!(value["data"]["member"]["routing_metadata"]["lane"], "blue");

    let (code, _stdout, stderr) = run(
        &[
            "missive", "group", "add", "team", "planner", "--rank", "rank-0", "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Usage.as_i32());
    assert!(
        json_error(&stderr)["data"]["message"]
            .as_str()
            .expect("message")
            .contains("rank")
    );

    let (code, stdout, stderr) = run(
        &[
            "missive", "group", "add", "team", "planner", "--rank", "rank-1", "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    json_success(&stdout, "group_add");

    let (code, stdout, stderr) = run(
        &["missive", "group", "show", "team", "--json"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_success(&stdout, "group_show");
    assert_eq!(value["data"]["group"]["routing_policy"], "weighted");
    assert_eq!(value["data"]["group"]["member_count"], 2);
    assert_eq!(value["data"]["group"]["members"][0]["rank"], "rank-0");
    assert_eq!(value["data"]["group"]["members"][1]["rank"], "rank-1");

    let (code, stdout, stderr) = run(
        &["missive", "group", "rename", "team", "squad", "--json"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_success(&stdout, "group_rename");
    assert_eq!(value["data"]["previous_name"], "team");
    assert_eq!(value["data"]["group"]["name"], "squad");
    assert_eq!(value["data"]["group"]["member_count"], 2);

    let (code, stdout, stderr) = run(
        &["missive", "group", "remove", "squad", "echo", "--json"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_success(&stdout, "group_remove");
    assert_eq!(value["data"]["member"]["agent"], "echo");
    assert_eq!(value["data"]["group"]["member_count"], 1);

    let (code, stdout, stderr) = run(
        &["missive", "group", "list", "--json"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_success(&stdout, "group_list");
    assert_eq!(value["data"]["count"], 1);
    assert_eq!(value["data"]["groups"][0]["name"], "squad");
    assert_eq!(value["data"]["groups"][0]["member_count"], 1);

    let (code, stdout, stderr) = run(
        &["missive", "group", "delete", "squad", "--json"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_success(&stdout, "group_delete");
    assert_eq!(value["data"]["group"]["name"], "squad");

    let (code, stdout, stderr) = run(
        &["missive", "group", "list", "--json"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_success(&stdout, "group_list");
    assert_eq!(value["data"]["count"], 0);
}

#[test]
fn group_commands_validate_missing_references() {
    let temp = tempdir().expect("tempdir");
    let environment = isolated_env(&temp.path().join("missive-home"));

    let (code, _stdout, stderr) = run(
        &[
            "missive", "group", "add", "missing", "ghost", "--rank", "rank-0", "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Usage.as_i32());
    assert!(
        json_error(&stderr)["data"]["message"]
            .as_str()
            .expect("message")
            .contains("does not exist")
    );

    let (code, _stdout, stderr) = run(
        &["missive", "group", "create", "team", "--json"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");

    let (code, _stdout, stderr) = run(
        &[
            "missive", "group", "add", "team", "ghost", "--rank", "rank-0", "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Usage.as_i32());
    assert!(
        json_error(&stderr)["data"]["message"]
            .as_str()
            .expect("message")
            .contains("not registered")
    );
}

#[test]
fn group_human_output_includes_members_and_routing_policy() {
    let temp = tempdir().expect("tempdir");
    let environment = isolated_env(&temp.path().join("missive-home"));
    add_agent("echo", 8091, &environment, temp.path());

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "group",
            "create",
            "team",
            "--routing-policy",
            "direct",
            "--notes",
            "Human group",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    assert!(stdout.contains("Created group 'team'"));
    assert!(stdout.contains("routing_policy: direct"));

    let (code, stdout, stderr) = run(
        &[
            "missive", "group", "add", "team", "echo", "--rank", "rank-0", "--tag", "local",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    assert!(stdout.contains("Added agent 'echo' to group 'team' as rank 'rank-0'"));
    assert!(stdout.contains("rank-0  agent=echo"));

    let (code, stdout, stderr) = run(
        &["missive", "group", "show", "team"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    assert!(stdout.contains("Group team"));
    assert!(stdout.contains("routing_policy: direct"));
    assert!(stdout.contains("rank-0  agent=echo"));
}
