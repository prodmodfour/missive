use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use missive_cli::{OUTPUT_SCHEMA_VERSION, REDACTED, run_from_with_environment};
use missive_core::MissiveExitCode;
use serde_json::{Value, json};
use tempfile::tempdir;

fn isolated_env(home: &Path) -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "MISSIVE_HOME".to_owned(),
            home.to_string_lossy().into_owned(),
        ),
        (
            "HOME".to_owned(),
            home.join("user-home").to_string_lossy().into_owned(),
        ),
    ])
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

#[test]
fn logs_json_reports_actionable_empty_sources_without_config() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);

    let (code, stdout, stderr) = run(&["missive", "logs", "--json"], &environment, temp.path());

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = json_success(&stdout, "logs");
    assert_eq!(value["data"]["profile"], "default");
    assert_eq!(value["data"]["count"], 0);
    assert!(
        value["data"]["records"]
            .as_array()
            .expect("records")
            .is_empty()
    );
    let sources = value["data"]["sources"].as_array().expect("sources");
    assert!(
        sources
            .iter()
            .any(|source| source["name"] == "profile-files")
    );
    assert!(
        sources
            .iter()
            .any(|source| source["name"] == "event-journal")
    );
    assert!(
        sources
            .iter()
            .any(|source| source["name"] == "gateway-service")
    );
    assert!(
        value["data"]["message"]
            .as_str()
            .expect("message")
            .contains("No local log records")
    );
}

#[test]
fn logs_json_reads_profile_file_records_and_redacts_secrets() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let log_dir = home
        .join("state")
        .join("profiles")
        .join("default")
        .join("logs");
    fs::create_dir_all(&log_dir).expect("log dir");
    fs::write(
        log_dir.join("gateway.log"),
        concat!(
            "INFO starting token=value-hidden-in-output\n",
            "{\"timestamp\":\"2026-01-01T00:00:00Z\",\"level\":\"INFO\",\"target\":\"missive_gateway\",\"message\":\"Authorization: Bearer value-hidden-in-output\",\"fields\":{\"api_key\":\"value-hidden-in-output\"}}\n"
        ),
    )
    .expect("write log");

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "logs",
            "--source",
            "profile-files",
            "--limit",
            "5",
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = json_success(&stdout, "logs");
    assert_eq!(value["data"]["count"], 2);
    assert_eq!(
        value["data"]["sources"],
        json!([
            {
                "name": "profile-files",
                "kind": "file_directory",
                "available": true,
                "status": "available",
                "path": log_dir.to_string_lossy(),
                "message": "Read 2 record(s) from 1 profile log file(s).",
                "hints": []
            }
        ])
    );
    assert_eq!(
        value["data"]["records"][0]["message"],
        format!("INFO starting token={REDACTED}")
    );
    assert_eq!(value["data"]["records"][1]["level"], "info");
    assert_eq!(value["data"]["records"][1]["target"], "missive_gateway");
    assert_eq!(
        value["data"]["records"][1]["fields"]["fields"]["api_key"],
        REDACTED
    );
    assert!(!stdout.contains("value-hidden-in-output"));
}

#[test]
fn logs_ndjson_emits_source_and_record_envelopes() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let log_dir = home
        .join("state")
        .join("profiles")
        .join("default")
        .join("logs");
    fs::create_dir_all(&log_dir).expect("log dir");
    fs::write(
        log_dir.join("gateway.log"),
        "WARN cookie: value-hidden-in-output\n",
    )
    .expect("write log");

    let (code, stdout, stderr) = run(
        &["missive", "logs", "--source", "profile-files", "--ndjson"],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let lines = ndjson_lines(&stdout);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["kind"], "log_source");
    assert_eq!(lines[0]["sequence"], 0);
    assert_eq!(lines[1]["kind"], "log_record");
    assert_eq!(lines[1]["sequence"], 1);
    assert_eq!(
        lines[1]["data"]["message"],
        format!("WARN cookie: {REDACTED}")
    );
    assert!(!stdout.contains("value-hidden-in-output"));
}
