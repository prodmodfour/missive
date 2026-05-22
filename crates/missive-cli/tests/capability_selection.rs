use std::collections::BTreeMap;
use std::path::Path;

use missive_cli::{OUTPUT_SCHEMA_VERSION, run_from_with_environment};
use missive_core::MissiveExitCode;
use missive_test_support::MockA2aServer;
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
            "capability-match",
            "--json",
        ],
        environment,
        current_dir,
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
}

struct MemberSpec<'a> {
    group: &'a str,
    alias: &'a str,
    rank: &'a str,
    tag: &'a str,
    weight: &'a str,
    capability: &'a str,
}

fn add_member(spec: MemberSpec<'_>, environment: &BTreeMap<String, String>, current_dir: &Path) {
    let metadata = format!("capabilities=[\"{}\"]", spec.capability);
    let (code, _stdout, stderr) = run(
        &[
            "missive",
            "group",
            "add",
            spec.group,
            spec.alias,
            "--rank",
            spec.rank,
            "--tag",
            spec.tag,
            "--weight",
            spec.weight,
            "--routing-metadata",
            &metadata,
            "--json",
        ],
        environment,
        current_dir,
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
}

#[test]
fn agent_capabilities_fetches_and_reuses_cached_agent_card() {
    let temp = tempdir().expect("tempdir");
    let environment = isolated_env(&temp.path().join("missive-home"));
    let server = MockA2aServer::start();
    add_agent("echo", server.base_url(), &environment, temp.path());

    let (code, stdout, stderr) = run(
        &["missive", "agent", "capabilities", "echo", "--json"],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "agent_capabilities");
    assert_eq!(value["data"]["count"], 1);
    assert_eq!(value["data"]["agents"][0]["alias"], "echo");
    assert_eq!(value["data"]["agents"][0]["cache"]["status"], "fetched");
    assert_eq!(value["data"]["agents"][0]["supports_streaming"], true);
    assert_eq!(
        value["data"]["agents"][0]["supports_push_notifications"],
        true
    );
    assert!(
        value["data"]["agents"][0]["capability_labels"]
            .as_array()
            .expect("labels")
            .iter()
            .any(|label| label == "echo")
    );
    assert!(
        value["data"]["agents"][0]["input_modes"]
            .as_array()
            .expect("input modes")
            .iter()
            .any(|mode| mode == "application/json")
    );
    assert_eq!(server.requests().len(), 1);

    let (code, stdout, stderr) = run(
        &["missive", "agent", "capabilities", "echo", "--json"],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "agent_capabilities");
    assert_eq!(value["data"]["agents"][0]["cache"]["status"], "cached");
    assert_eq!(
        server.requests().len(),
        1,
        "second summary should reuse cache"
    );
}

#[test]
fn group_capabilities_summarize_member_cards_and_local_hints() {
    let temp = tempdir().expect("tempdir");
    let environment = isolated_env(&temp.path().join("missive-home"));
    let alpha = MockA2aServer::start();
    let beta = MockA2aServer::builder().push_notifications(false).start();
    add_agent("alpha", alpha.base_url(), &environment, temp.path());
    add_agent("beta", beta.base_url(), &environment, temp.path());
    create_group(&environment, temp.path());
    add_member(
        MemberSpec {
            group: "team",
            alias: "alpha",
            rank: "rank-0",
            tag: "writer",
            weight: "1.0",
            capability: "draft",
        },
        &environment,
        temp.path(),
    );
    add_member(
        MemberSpec {
            group: "team",
            alias: "beta",
            rank: "rank-1",
            tag: "reviewer",
            weight: "2.0",
            capability: "review",
        },
        &environment,
        temp.path(),
    );

    let (code, stdout, stderr) = run(
        &["missive", "group", "capabilities", "team", "--json"],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "group_capabilities");
    assert_eq!(value["data"]["member_count"], 2);
    assert_eq!(value["data"]["aggregate"]["streaming_supported"], 2);
    assert_eq!(value["data"]["aggregate"]["push_supported"], 1);
    assert!(
        value["data"]["aggregate"]["capability_labels"]
            .as_array()
            .expect("labels")
            .iter()
            .any(|label| label == "review")
    );
    assert!(
        value["data"]["aggregate"]["tags"]
            .as_array()
            .expect("tags")
            .iter()
            .any(|tag| tag == "writer")
    );
    assert_eq!(alpha.requests().len(), 1);
    assert_eq!(beta.requests().len(), 1);
}

#[test]
fn route_capability_match_refreshes_cards_and_tie_breaks_by_weight() {
    let temp = tempdir().expect("tempdir");
    let environment = isolated_env(&temp.path().join("missive-home"));
    let alpha = MockA2aServer::start();
    let beta = MockA2aServer::start();
    add_agent("alpha", alpha.base_url(), &environment, temp.path());
    add_agent("beta", beta.base_url(), &environment, temp.path());
    create_group(&environment, temp.path());
    add_member(
        MemberSpec {
            group: "team",
            alias: "alpha",
            rank: "rank-0",
            tag: "worker",
            weight: "1.0",
            capability: "draft",
        },
        &environment,
        temp.path(),
    );
    add_member(
        MemberSpec {
            group: "team",
            alias: "beta",
            rank: "rank-1",
            tag: "worker",
            weight: "3.0",
            capability: "draft",
        },
        &environment,
        temp.path(),
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
            "echo",
            "--input-mode",
            "application/json",
            "--output-mode",
            "text/plain",
            "--streaming",
            "--push-notifications",
            "--refresh-capabilities",
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "route_explain");
    assert_eq!(
        value["data"]["plan"]["selected"],
        serde_json::json!(["beta", "alpha"])
    );
    assert_eq!(value["data"]["plan"]["decisions"][1]["order"], 0);
    assert_eq!(
        value["data"]["plan"]["decisions"][1]["matched_input_modes"],
        serde_json::json!(["application/json"])
    );
    assert_eq!(alpha.requests().len(), 1);
    assert_eq!(beta.requests().len(), 1);
}

#[test]
fn route_capability_match_reports_missing_cached_agent_card_data() {
    let temp = tempdir().expect("tempdir");
    let environment = isolated_env(&temp.path().join("missive-home"));
    add_agent("alpha", "http://127.0.0.1:9", &environment, temp.path());

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "route",
            "explain",
            "--agent",
            "alpha",
            "--policy",
            "capability-match",
            "--streaming",
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "route_explain");
    assert_eq!(value["data"]["plan"]["status"], "no_match");
    assert_eq!(
        value["data"]["plan"]["decisions"][0]["missing_requirements"],
        serde_json::json!(["streaming:unknown"])
    );
    assert!(
        value["data"]["plan"]["decisions"][0]["reason"]
            .as_str()
            .expect("reason")
            .contains("refresh")
    );
}
