use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use missive_cli::{OUTPUT_SCHEMA_VERSION, REDACTED, run_from_with_environment};
use missive_core::MissiveExitCode;
use missive_store::Store;
use serde_json::Value;
use tempfile::tempdir;

fn isolated_env(home: &Path) -> BTreeMap<String, String> {
    let mut environment = BTreeMap::from([
        (
            "MISSIVE_HOME".to_owned(),
            home.to_string_lossy().into_owned(),
        ),
        (
            "HOME".to_owned(),
            home.join("user-home").to_string_lossy().into_owned(),
        ),
    ]);
    if let Ok(path) = std::env::var("PATH") {
        environment.insert("PATH".to_owned(), path);
    }
    environment
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

fn doctor_json(stdout: &str) -> Value {
    let value: Value = serde_json::from_str(stdout).expect("stdout should be JSON");
    assert_eq!(value["schema_version"], OUTPUT_SCHEMA_VERSION);
    assert_eq!(value["ok"], true);
    assert_eq!(value["kind"], "doctor");
    value
}

fn check_by_id<'a>(value: &'a Value, id: &str) -> &'a Value {
    value["data"]["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|check| check["id"] == id)
        .unwrap_or_else(|| panic!("missing check {id} in {value:#}"))
}

#[test]
fn doctor_json_no_config_reports_local_checks() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);

    let (code, stdout, stderr) = run(&["missive", "doctor", "--json"], &environment, temp.path());

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = doctor_json(&stdout);
    assert_eq!(value["data"]["profile"], "default");
    assert_eq!(value["data"]["scope"], "local");
    assert!(matches!(
        value["data"]["overall"]["status"].as_str(),
        Some("pass" | "warning")
    ));
    assert_eq!(check_by_id(&value, "binary.version")["status"], "pass");
    assert_eq!(
        check_by_id(&value, "config.discovery_validation")["status"],
        "pass"
    );
    assert_eq!(check_by_id(&value, "state.paths")["status"], "pass");
    assert_eq!(
        check_by_id(&value, "store.sqlite_migrations")["status"],
        "skipped"
    );
}

#[test]
fn doctor_json_reports_valid_populated_config_and_migrated_database_with_redaction() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let config_path = temp.path().join("missive.toml");
    fs::write(
        &config_path,
        r#"
schema_version = "missive.config.v1"
default_profile = "dev"

[profiles.dev]
description = "Development profile"
default_agent = "echo"

[profiles.dev.metadata]
api_token = "value-hidden-in-output"

[agents.echo]
base_url = "http://127.0.0.1:8080"
auth_ref = "prod-token"
tags = ["local"]

[auth_refs.prod-token]
kind = "env"
env = "MISSIVE_PROD_TOKEN"
header = "Authorization"
scheme = "Bearer"
"#,
    )
    .expect("write config");
    let database_path = home
        .join("state")
        .join("profiles")
        .join("dev")
        .join("missive.sqlite3");
    fs::create_dir_all(database_path.parent().expect("database parent")).expect("database dir");
    Store::open(&database_path).expect("migrate profile database");

    let mut environment = isolated_env(&home);
    environment.insert(
        "MISSIVE_CONFIG".to_owned(),
        config_path.to_string_lossy().into_owned(),
    );

    let (code, stdout, stderr) = run(&["missive", "doctor", "--json"], &environment, temp.path());

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let value = doctor_json(&stdout);
    assert_eq!(value["data"]["profile"], "dev");
    assert_eq!(
        check_by_id(&value, "config.discovery_validation")["data"]["source"],
        "environment"
    );
    assert_eq!(
        check_by_id(&value, "store.sqlite_migrations")["status"],
        "pass"
    );
    assert_eq!(
        check_by_id(&value, "store.sqlite_migrations")["data"]["current_version"],
        2
    );
    assert!(!stdout.contains("value-hidden-in-output"));
    assert!(stdout.contains(REDACTED));
}

#[test]
fn doctor_json_reports_invalid_config_as_local_check_failure() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let config_path = temp.path().join("bad.toml");
    fs::write(
        &config_path,
        r#"
schema_version = "missive.config.v1"
default_profile = "missing"

[profiles.default]
"#,
    )
    .expect("write config");
    let mut environment = isolated_env(&home);
    environment.insert(
        "MISSIVE_CONFIG".to_owned(),
        config_path.to_string_lossy().into_owned(),
    );

    let (code, stdout, stderr) = run(&["missive", "doctor", "--json"], &environment, temp.path());

    assert_eq!(code, MissiveExitCode::Config.as_i32());
    let value = doctor_json(&stdout);
    assert_eq!(value["data"]["overall"]["status"], "fail");
    assert_eq!(
        check_by_id(&value, "config.discovery_validation")["status"],
        "fail"
    );
    assert_eq!(check_by_id(&value, "state.paths")["status"], "skipped");
    let error: Value = serde_json::from_str(&stderr).expect("stderr should be JSON error");
    assert_eq!(error["ok"], false);
    assert_eq!(error["data"]["exit_code"], MissiveExitCode::Config.as_u8());
}

#[test]
fn doctor_json_reports_unmigrated_database_state() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let database_path = home
        .join("state")
        .join("profiles")
        .join("default")
        .join("missive.sqlite3");
    fs::create_dir_all(database_path.parent().expect("database parent")).expect("database dir");
    fs::File::create(&database_path).expect("empty sqlite file placeholder");

    let (code, stdout, stderr) = run(&["missive", "doctor", "--json"], &environment, temp.path());

    assert_eq!(code, MissiveExitCode::TemporaryFailure.as_i32());
    let value = doctor_json(&stdout);
    assert_eq!(value["data"]["overall"]["status"], "fail");
    let migration = check_by_id(&value, "store.sqlite_migrations");
    assert_eq!(migration["status"], "fail");
    assert_eq!(migration["data"]["current_version"], Value::Null);
    assert!(
        migration["message"]
            .as_str()
            .expect("message")
            .contains("not current")
    );
    let error: Value = serde_json::from_str(&stderr).expect("stderr should be JSON error");
    assert_eq!(
        error["data"]["exit_code"],
        MissiveExitCode::TemporaryFailure.as_u8()
    );
}

#[test]
fn doctor_tool_availability_uses_deterministic_path_lookup() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let bin = temp.path().join("bin");
    fs::create_dir_all(&bin).expect("bin dir");
    write_fake_executable(&bin, "rustc");
    write_fake_executable(&bin, "cargo");

    let mut environment = isolated_env(&home);
    environment.insert("PATH".to_owned(), bin.to_string_lossy().into_owned());

    let (code, stdout, stderr) = run(&["missive", "doctor", "--json"], &environment, temp.path());

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = doctor_json(&stdout);
    assert_eq!(check_by_id(&value, "tool.rustc")["status"], "pass");
    assert_eq!(check_by_id(&value, "tool.cargo")["status"], "pass");
    assert_eq!(check_by_id(&value, "tool.rustfmt")["status"], "warning");
    assert_eq!(value["data"]["overall"]["status"], "warning");
}

fn write_fake_executable(directory: &Path, stem: &str) {
    let name = if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_owned()
    };
    let path = directory.join(name);
    fs::write(&path, b"#!/bin/sh\nexit 0\n").expect("write fake executable");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&path)
            .expect("fake executable metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("mark fake executable");
    }
}
