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

#[test]
fn agent_registry_json_crud_round_trip() {
    let temp = tempdir().expect("tempdir");
    let environment = isolated_env(&temp.path().join("missive-home"));
    let config_path = temp.path().join("missive.toml");
    std::fs::write(
        &config_path,
        r#"
schema_version = "missive.config.v1"
default_profile = "default"

[profiles.default]

[auth_refs.example-env]
kind = "env"
env = "MISSIVE_TEST_AGENT_TOKEN"
header = "Authorization"
scheme = "Bearer"
"#,
    )
    .expect("write config");
    let config_arg = config_path.to_string_lossy().into_owned();

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "agent",
            "add",
            "echo",
            "http://127.0.0.1:8080",
            "--interface",
            "http+json=http://127.0.0.1:8080/a2a",
            "--binding-preference",
            "http+json",
            "--auth-ref",
            "example-env",
            "--tag",
            "local",
            "--notes",
            "Local echo agent",
            "--metadata",
            "role=echo",
            "--metadata",
            "priority=2",
            "--config",
            &config_arg,
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_success(&stdout, "agent_add");
    assert_eq!(value["data"]["profile"], "default");
    assert_eq!(value["data"]["agent"]["alias"], "echo");
    assert_eq!(value["data"]["agent"]["source"], "local");
    assert_eq!(value["data"]["agent"]["read_only"], false);
    assert_eq!(value["data"]["agent"]["auth_ref"], "example-env");
    assert_eq!(
        value["data"]["agent"]["interface_urls"]["http+json"],
        "http://127.0.0.1:8080/a2a"
    );
    assert_eq!(value["data"]["agent"]["metadata"]["role"], "echo");
    assert_eq!(value["data"]["agent"]["metadata"]["priority"], 2);

    let (code, stdout, stderr) = run(
        &["missive", "agent", "list", "--json"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_success(&stdout, "agent_list");
    assert_eq!(value["data"]["count"], 1);
    assert_eq!(value["data"]["agents"][0]["alias"], "echo");

    let (code, stdout, stderr) = run(
        &["missive", "agent", "show", "echo", "--json"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_success(&stdout, "agent_show");
    assert_eq!(value["data"]["agent"]["alias"], "echo");
    assert_eq!(value["data"]["agent"]["tags"], serde_json::json!(["local"]));

    let (code, stdout, stderr) = run(
        &["missive", "agent", "remove", "echo", "--json"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_success(&stdout, "agent_remove");
    assert_eq!(value["data"]["agent"]["alias"], "echo");

    let (code, stdout, stderr) = run(
        &["missive", "agent", "list", "--json"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_success(&stdout, "agent_list");
    assert_eq!(value["data"]["count"], 0);
}

#[test]
fn agent_registry_human_commands_render_useful_output() {
    let temp = tempdir().expect("tempdir");
    let environment = isolated_env(&temp.path().join("missive-home"));

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "agent",
            "add",
            "planner",
            "http://127.0.0.1:8090",
            "--tag",
            "local",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    assert!(stdout.contains("Added agent 'planner'"));

    let (code, stdout, stderr) = run(&["missive", "agent", "list"], &environment, temp.path());
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    assert!(stdout.contains("Agents for profile 'default'"));
    assert!(stdout.contains("planner"));
    assert!(stdout.contains("tags=local"));

    let (code, stdout, stderr) = run(
        &["missive", "agent", "show", "planner"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    assert!(stdout.contains("Agent planner"));
    assert!(stdout.contains("base_url: http://127.0.0.1:8090"));

    let (code, stdout, stderr) = run(
        &["missive", "agent", "remove", "planner"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    assert!(stdout.contains("Removed agent 'planner'"));
}

#[test]
fn agent_rename_preserves_registry_fields() {
    let temp = tempdir().expect("tempdir");
    let environment = isolated_env(&temp.path().join("missive-home"));

    let (code, _, stderr) = run(
        &[
            "missive",
            "agent",
            "add",
            "old-name",
            "http://127.0.0.1:8070",
            "--tag",
            "blue",
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");

    let (code, stdout, stderr) = run(
        &[
            "missive", "agent", "rename", "old-name", "new-name", "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_success(&stdout, "agent_rename");
    assert_eq!(value["data"]["previous_alias"], "old-name");
    assert_eq!(value["data"]["agent"]["alias"], "new-name");
    assert_eq!(value["data"]["agent"]["tags"], serde_json::json!(["blue"]));

    let (code, _stdout, stderr) = run(
        &["missive", "agent", "show", "old-name", "--json"],
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
fn duplicate_aliases_and_missing_agents_fail_clearly() {
    let temp = tempdir().expect("tempdir");
    let environment = isolated_env(&temp.path().join("missive-home"));

    let (code, _, stderr) = run(
        &[
            "missive",
            "agent",
            "add",
            "echo",
            "http://127.0.0.1:8080",
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "agent",
            "add",
            "echo",
            "http://127.0.0.1:8081",
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Usage.as_i32());
    assert!(stdout.is_empty());
    let value = json_error(&stderr);
    assert_eq!(value["data"]["code"], "missive::validation");
    assert!(
        value["data"]["message"]
            .as_str()
            .expect("message")
            .contains("already exists")
    );

    let (code, stdout, stderr) = run(
        &["missive", "agent", "show", "missing", "--json"],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Usage.as_i32());
    assert!(stdout.is_empty());
    assert!(
        json_error(&stderr)["data"]["message"]
            .as_str()
            .expect("message")
            .contains("not registered")
    );
}

#[test]
fn invalid_alias_is_validated_by_agent_command() {
    let temp = tempdir().expect("tempdir");
    let environment = isolated_env(&temp.path().join("missive-home"));

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "agent",
            "add",
            "BadAlias",
            "http://127.0.0.1:8080",
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Usage.as_i32());
    assert!(stdout.is_empty());
    let value = json_error(&stderr);
    assert_eq!(value["data"]["code"], "missive::validation");
    assert!(
        value["data"]["message"]
            .as_str()
            .expect("message")
            .contains("invalid agent alias")
    );
}

#[test]
fn config_seeded_agents_are_listed_and_read_only() {
    let temp = tempdir().expect("tempdir");
    let missive_home = temp.path().join("missive-home");
    let environment = isolated_env(&missive_home);
    let config_path = temp.path().join("missive.toml");
    std::fs::write(
        &config_path,
        r#"
schema_version = "missive.config.v1"
default_profile = "default"

[profiles.default]

[agents.cfg-agent]
base_url = "http://127.0.0.1:8060"
tags = ["config"]
notes = "Config seeded agent."

[agents.cfg-agent.interface_urls]
"http+json" = "http://127.0.0.1:8060/a2a"
"#,
    )
    .expect("write config");
    let config_arg = config_path.to_string_lossy().into_owned();

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "agent",
            "list",
            "--config",
            &config_arg,
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_success(&stdout, "agent_list");
    assert_eq!(value["data"]["count"], 1);
    assert_eq!(value["data"]["agents"][0]["alias"], "cfg-agent");
    assert_eq!(value["data"]["agents"][0]["source"], "config_seed");
    assert_eq!(value["data"]["agents"][0]["read_only"], true);

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "agent",
            "remove",
            "cfg-agent",
            "--config",
            &config_arg,
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Usage.as_i32());
    assert!(stdout.is_empty());
    let value = json_error(&stderr);
    assert!(
        value["data"]["message"]
            .as_str()
            .expect("message")
            .contains("read-only")
    );
}
