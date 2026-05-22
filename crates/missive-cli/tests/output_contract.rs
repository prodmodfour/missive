use std::collections::BTreeMap;
use std::path::Path;

use missive_cli::{
    OUTPUT_SCHEMA_VERSION, REDACTED, redact_header, redact_json, required_subcommands,
    run_from_with_environment,
};
use missive_core::MissiveExitCode;
use serde_json::{Value, json};
use tempfile::tempdir;

fn run(args: &[&str]) -> (i32, String, String) {
    run_with_env(args, &BTreeMap::new(), Path::new("."))
}

fn run_with_env(
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

#[test]
fn json_output_parses_for_every_current_skeletal_command() {
    for command in required_subcommands()
        .into_iter()
        .filter(|command| *command != "send")
    {
        let (code, stdout, stderr) = run(&["missive", command, "--json"]);

        assert_eq!(code, MissiveExitCode::Success.as_i32(), "command {command}");
        assert!(stderr.is_empty(), "stderr for {command}: {stderr}");

        let value: Value = serde_json::from_str(&stdout).unwrap_or_else(|error| {
            panic!("JSON output for {command} should parse: {error}\n{stdout}")
        });

        assert_eq!(value["schema_version"], OUTPUT_SCHEMA_VERSION);
        assert_eq!(value["ok"], true);
        assert_eq!(value["kind"], "command_status");
        assert_eq!(value["data"]["command"], command);
        assert_eq!(value["data"]["status"], "parsed");
        assert_eq!(value["data"]["implemented"], false);
        assert!(value.get("sequence").is_none());
    }
}

#[test]
fn ndjson_output_is_one_json_object_per_line_for_every_current_skeletal_command() {
    for command in required_subcommands()
        .into_iter()
        .filter(|command| *command != "send")
    {
        let (code, stdout, stderr) = run(&["missive", command, "--ndjson"]);

        assert_eq!(code, MissiveExitCode::Success.as_i32(), "command {command}");
        assert!(stderr.is_empty(), "stderr for {command}: {stderr}");

        let lines: Vec<_> = stdout.lines().collect();
        assert_eq!(lines.len(), 1, "NDJSON output for {command}: {stdout:?}");

        let value: Value = serde_json::from_str(lines[0])
            .unwrap_or_else(|error| panic!("NDJSON line for {command} should parse: {error}"));

        assert_eq!(value["schema_version"], OUTPUT_SCHEMA_VERSION);
        assert_eq!(value["ok"], true);
        assert_eq!(value["kind"], "command_status");
        assert_eq!(value["sequence"], 0);
        assert_eq!(value["data"]["command"], command);
    }
}

#[test]
fn quiet_mode_suppresses_success_output() {
    let (code, stdout, stderr) = run(&["missive", "doctor", "--quiet"]);

    assert_eq!(code, MissiveExitCode::Success.as_i32());
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
}

#[test]
fn explicit_config_file_loads_and_reports_secret_free_summary() {
    let config_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples/config/minimal.toml");
    let config_arg = config_path.to_string_lossy().into_owned();
    let (code, stdout, stderr) = run(&["missive", "agent", "--config", &config_arg, "--json"]);

    assert_eq!(code, MissiveExitCode::Success.as_i32());
    assert!(stderr.is_empty(), "stderr: {stderr}");

    let value: Value = serde_json::from_str(&stdout).expect("JSON output should parse");
    assert_eq!(value["data"]["config"]["source"], "explicit_path");
    assert_eq!(value["data"]["config"]["profile"], "default");
    assert_eq!(value["data"]["config"]["agent_count"], 1);
    assert_eq!(value["data"]["config"]["auth_ref_count"], 1);
    assert!(!stdout.contains("MISSIVE_EXAMPLE_BEARER_TOKEN_VALUE"));
}

#[test]
fn config_output_default_selects_machine_readable_mode() {
    let temp = tempdir().expect("tempdir");
    let config_path = temp.path().join("missive.toml");
    std::fs::write(
        &config_path,
        r#"
schema_version = "missive.config.v1"
default_profile = "default"

[output]
format = "json"

[profiles.default]
"#,
    )
    .expect("write config");
    let config_arg = config_path.to_string_lossy().into_owned();

    let (code, stdout, stderr) = run(&["missive", "agent", "--config", &config_arg]);

    assert_eq!(code, MissiveExitCode::Success.as_i32());
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value: Value = serde_json::from_str(&stdout).expect("config default should choose JSON");
    assert_eq!(value["data"]["config"]["output_format"], "json");
}

#[test]
fn missive_config_environment_path_is_honored() {
    let temp = tempdir().expect("tempdir");
    let config_path = temp.path().join("missive.toml");
    std::fs::write(
        &config_path,
        r#"
schema_version = "missive.config.v1"
default_profile = "default"

[profiles.default]
"#,
    )
    .expect("write config");
    let environment = BTreeMap::from([(
        "MISSIVE_CONFIG".to_owned(),
        config_path.to_string_lossy().into_owned(),
    )]);

    let (code, stdout, stderr) =
        run_with_env(&["missive", "doctor", "--json"], &environment, temp.path());

    assert_eq!(code, MissiveExitCode::Success.as_i32());
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value: Value = serde_json::from_str(&stdout).expect("JSON output should parse");
    assert_eq!(value["data"]["config"]["source"], "environment");
}

#[test]
fn invalid_config_fails_with_actionable_machine_readable_diagnostics() {
    let temp = tempdir().expect("tempdir");
    let config_path = temp.path().join("bad.toml");
    std::fs::write(
        &config_path,
        r#"
schema_version = "missive.config.v1"
default_profile = "missing"

[profiles.default]
"#,
    )
    .expect("write config");
    let config_arg = config_path.to_string_lossy().into_owned();

    let (code, stdout, stderr) = run(&["missive", "agent", "--config", &config_arg, "--json"]);

    assert_eq!(code, MissiveExitCode::Config.as_i32());
    assert!(stdout.is_empty());
    let value: Value = serde_json::from_str(&stderr).expect("JSON error should parse");
    assert_eq!(value["ok"], false);
    assert_eq!(value["data"]["code"], "missive::config");
    assert!(
        value["data"]["message"]
            .as_str()
            .expect("message")
            .contains("default_profile")
    );
    assert!(value["data"]["help"].as_str().is_some());
}

#[test]
fn machine_readable_errors_use_stable_json_shape_and_exit_code() {
    let (code, stdout, stderr) = run(&["missive", "agent", "--json", "--ndjson"]);

    assert_eq!(code, MissiveExitCode::Usage.as_i32());
    assert!(stdout.is_empty());

    let lines: Vec<_> = stderr.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "stderr should be one NDJSON error: {stderr:?}"
    );
    let value: Value = serde_json::from_str(lines[0]).expect("error line should parse as JSON");

    assert_eq!(value["schema_version"], OUTPUT_SCHEMA_VERSION);
    assert_eq!(value["ok"], false);
    assert_eq!(value["kind"], "error");
    assert_eq!(value["sequence"], 0);
    assert_eq!(value["data"]["code"], "missive::validation");
    assert_eq!(value["data"]["category"], "validation");
    assert_eq!(value["data"]["exit_code"], MissiveExitCode::Usage.as_u8());
}

#[test]
fn redaction_helpers_do_not_emit_secret_like_values() {
    let hidden = "value-hidden-in-output";

    assert_eq!(
        redact_header("Authorization", format!("Bearer {hidden}").as_str()),
        format!("Bearer {REDACTED}")
    );
    assert_eq!(redact_header("X-Api-Key", hidden), REDACTED);

    let redacted = redact_json(&json!({
        "token": hidden,
        "headers": {
            "Authorization": format!("Bearer {hidden}"),
            "X-Request-Id": "request-123"
        },
        "public": "visible"
    }));
    let rendered = redacted.to_string();

    assert_eq!(redacted["token"], REDACTED);
    assert_eq!(
        redacted["headers"]["Authorization"],
        format!("Bearer {REDACTED}")
    );
    assert_eq!(redacted["headers"]["X-Request-Id"], "request-123");
    assert_eq!(redacted["public"], "visible");
    assert!(!rendered.contains(hidden));
}
