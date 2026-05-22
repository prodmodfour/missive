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

fn create_weighted_team(environment: &BTreeMap<String, String>, current_dir: &Path) {
    add_agent("alpha", 8101, environment, current_dir);
    add_agent("beta", 8102, environment, current_dir);

    let (code, _stdout, stderr) = run(
        &[
            "missive",
            "group",
            "create",
            "team",
            "--routing-policy",
            "weighted",
            "--json",
        ],
        environment,
        current_dir,
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");

    let (code, _stdout, stderr) = run(
        &[
            "missive",
            "group",
            "add",
            "team",
            "alpha",
            "--rank",
            "rank-0",
            "--tag",
            "writer",
            "--weight",
            "1.0",
            "--routing-metadata",
            "capabilities=[\"draft\",\"summarise\"]",
            "--json",
        ],
        environment,
        current_dir,
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");

    let (code, _stdout, stderr) = run(
        &[
            "missive",
            "group",
            "add",
            "team",
            "beta",
            "--rank",
            "rank-1",
            "--tag",
            "reviewer",
            "--weight",
            "3.0",
            "--routing-metadata",
            "capabilities=[\"review\"]",
            "--json",
        ],
        environment,
        current_dir,
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
}

#[test]
fn route_explain_group_weighted_json_selects_highest_weight() {
    let temp = tempdir().expect("tempdir");
    let environment = isolated_env(&temp.path().join("missive-home"));
    create_weighted_team(&environment, temp.path());

    let (code, stdout, stderr) = run(
        &["missive", "route", "explain", "--group", "team", "--json"],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_success(&stdout, "route_explain");
    assert_eq!(value["data"]["source"]["kind"], "group");
    assert_eq!(value["data"]["policy_source"], "group");
    assert_eq!(value["data"]["plan"]["policy"], "weighted");
    assert_eq!(
        value["data"]["plan"]["selected"],
        serde_json::json!(["beta"])
    );
    assert_eq!(value["data"]["plan"]["decisions"][1]["order"], 0);
}

#[test]
fn route_explain_can_override_policy_and_filter_by_tag_or_capability() {
    let temp = tempdir().expect("tempdir");
    let environment = isolated_env(&temp.path().join("missive-home"));
    create_weighted_team(&environment, temp.path());

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "route",
            "explain",
            "--group",
            "team",
            "--policy",
            "tag-match",
            "--tag",
            "writer",
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "route_explain");
    assert_eq!(value["data"]["policy_source"], "cli");
    assert_eq!(
        value["data"]["plan"]["selected"],
        serde_json::json!(["alpha"])
    );
    assert_eq!(
        value["data"]["plan"]["decisions"][0]["matched_tags"],
        serde_json::json!(["writer"])
    );

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "route",
            "explain",
            "--group",
            "team",
            "--policy",
            "capability-match",
            "--capability",
            "review",
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "route_explain");
    assert_eq!(
        value["data"]["plan"]["selected"],
        serde_json::json!(["beta"])
    );
    assert_eq!(
        value["data"]["plan"]["decisions"][1]["matched_capabilities"],
        serde_json::json!(["review"])
    );
}

#[test]
fn route_explain_human_explicit_agents_reports_round_robin_cursor() {
    let temp = tempdir().expect("tempdir");
    let environment = isolated_env(&temp.path().join("missive-home"));
    add_agent("alpha", 8111, &environment, temp.path());
    add_agent("beta", 8112, &environment, temp.path());

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "route",
            "explain",
            "--agent",
            "alpha",
            "--agent",
            "beta",
            "--policy",
            "round-robin",
            "--cursor",
            "1",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    assert!(stdout.contains("Route explain for profile 'default'"));
    assert!(stdout.contains("policy: round-robin (cli)"));
    assert!(stdout.contains("next_round_robin_cursor: 2"));
    assert!(stdout.contains("selected: beta"));
}

#[test]
fn route_explain_rejects_invalid_policy_and_candidate_source() {
    let temp = tempdir().expect("tempdir");
    let environment = isolated_env(&temp.path().join("missive-home"));
    add_agent("alpha", 8121, &environment, temp.path());

    let (code, _stdout, stderr) = run(
        &[
            "missive",
            "route",
            "explain",
            "--agent",
            "alpha",
            "--policy",
            "least-latency",
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
            .contains("unknown routing policy")
    );

    let (code, _stdout, stderr) = run(
        &["missive", "route", "explain", "--json"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Usage.as_i32());
    assert!(
        json_error(&stderr)["data"]["message"]
            .as_str()
            .expect("message")
            .contains("requires --group")
    );
}

#[test]
fn invalid_routing_policy_config_fails_before_execution() {
    let temp = tempdir().expect("tempdir");
    let config_path = temp.path().join("missive.toml");
    std::fs::write(
        &config_path,
        r#"
schema_version = "missive.config.v1"
default_profile = "default"

[routing]
default_policy = "least-latency"

[profiles.default]
"#,
    )
    .expect("write config");
    let config_arg = config_path.to_string_lossy().into_owned();

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "route",
            "explain",
            "--agent",
            "alpha",
            "--config",
            &config_arg,
            "--json",
        ],
        &BTreeMap::new(),
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Config.as_i32());
    assert!(stdout.is_empty());
    assert!(
        json_error(&stderr)["data"]["message"]
            .as_str()
            .expect("message")
            .contains("routing.default_policy")
    );
}
