use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use missive_cli::{OUTPUT_SCHEMA_VERSION, REDACTED, run_from_with_environment};
use missive_core::MissiveExitCode;
use missive_store::Store;
use missive_test_support::MockA2aServer;
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
    assert_eq!(value["data"]["scope"], "local_remote_gateway");
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
    assert_eq!(check_by_id(&value, "a2a.endpoints")["status"], "skipped");
    assert_eq!(check_by_id(&value, "gateway.status")["status"], "skipped");
}

#[test]
fn doctor_json_reports_valid_populated_config_and_migrated_database_with_redaction() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let config_path = temp.path().join("missive.toml");
    let server_secret = "doctor-secret-value";
    let server = MockA2aServer::builder()
        .require_auth_header("Authorization", format!("Bearer {server_secret}"))
        .start();
    fs::write(
        &config_path,
        format!(
            r#"
schema_version = "missive.config.v1"
default_profile = "dev"

[profiles.dev]
description = "Development profile"
default_agent = "echo"

[profiles.dev.metadata]
api_token = "value-hidden-in-output"

[agents.echo]
base_url = "{}"
auth_ref = "prod-token"
tags = ["local"]

[auth_refs.prod-token]
kind = "env"
env = "MISSIVE_PROD_TOKEN"
header = "Authorization"
scheme = "Bearer"
"#,
            server.base_url()
        ),
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
    environment.insert("MISSIVE_PROD_TOKEN".to_owned(), server_secret.to_owned());

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
    let endpoint = check_by_id(&value, "a2a.endpoint.echo");
    assert_eq!(endpoint["status"], "pass");
    assert_eq!(endpoint["data"]["auth_configured"], true);
    assert_eq!(
        server.requests()[0].header("authorization"),
        Some("Bearer doctor-secret-value")
    );
    assert!(!stdout.contains("value-hidden-in-output"));
    assert!(!stdout.contains("doctor-secret-value"));
    assert!(!stdout.contains("prod-token"));
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

#[test]
fn doctor_json_reports_unreachable_configured_a2a_endpoint() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let config_path = temp.path().join("missive.toml");
    let unused = unused_local_addr();
    fs::write(
        &config_path,
        format!(
            r#"
schema_version = "missive.config.v1"
default_profile = "default"

[profiles.default]
default_agent = "echo"

[agents.echo]
base_url = "http://{}"
"#,
            unused
        ),
    )
    .expect("write config");
    let mut environment = isolated_env(&home);
    environment.insert(
        "MISSIVE_CONFIG".to_owned(),
        config_path.to_string_lossy().into_owned(),
    );

    let (code, stdout, stderr) = run(
        &["missive", "doctor", "--json", "--timeout", "200ms"],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Unavailable.as_i32());
    let value = doctor_json(&stdout);
    let endpoint = check_by_id(&value, "a2a.endpoint.echo");
    assert_eq!(endpoint["status"], "fail");
    assert_eq!(endpoint["data"]["auth_configured"], false);
    let error: Value = serde_json::from_str(&stderr).expect("stderr should be JSON error");
    assert_eq!(
        error["data"]["exit_code"],
        MissiveExitCode::Unavailable.as_u8()
    );
}

#[test]
fn doctor_json_reports_running_gateway_status_when_reachable() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let config_path = temp.path().join("missive.toml");
    let (gateway_addr, gateway_thread) = start_gateway_status_server("dev");
    fs::write(
        &config_path,
        format!(
            r#"
schema_version = "missive.config.v1"
default_profile = "dev"

[profiles.dev]

[profiles.dev.gateway]
enabled = false
bind_address = "{}"
job_concurrency = 2
"#,
            gateway_addr
        ),
    )
    .expect("write config");
    let mut environment = isolated_env(&home);
    environment.insert(
        "MISSIVE_CONFIG".to_owned(),
        config_path.to_string_lossy().into_owned(),
    );

    let (code, stdout, stderr) = run(&["missive", "doctor", "--json"], &environment, temp.path());
    gateway_thread.join().expect("gateway fixture thread");

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    let value = doctor_json(&stdout);
    let gateway = check_by_id(&value, "gateway.status");
    assert_eq!(gateway["status"], "pass");
    assert_eq!(gateway["data"]["configured_enabled"], false);
    assert_eq!(gateway["data"]["remote_profile"], "dev");
}

#[test]
fn doctor_json_reports_configured_gateway_unavailable() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let config_path = temp.path().join("missive.toml");
    let unused = unused_local_addr();
    fs::write(
        &config_path,
        format!(
            r#"
schema_version = "missive.config.v1"
default_profile = "default"

[profiles.default]

[gateway]
enabled = true
bind_address = "{}"
"#,
            unused
        ),
    )
    .expect("write config");
    let mut environment = isolated_env(&home);
    environment.insert(
        "MISSIVE_CONFIG".to_owned(),
        config_path.to_string_lossy().into_owned(),
    );

    let (code, stdout, stderr) = run(
        &["missive", "doctor", "--json", "--timeout", "200ms"],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Unavailable.as_i32());
    let value = doctor_json(&stdout);
    let gateway = check_by_id(&value, "gateway.status");
    assert_eq!(gateway["status"], "fail");
    assert_eq!(gateway["data"]["configured_enabled"], true);
    let error: Value = serde_json::from_str(&stderr).expect("stderr should be JSON error");
    assert_eq!(
        error["data"]["exit_code"],
        MissiveExitCode::Unavailable.as_u8()
    );
}

fn unused_local_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind unused local address");
    listener.local_addr().expect("local addr")
}

fn start_gateway_status_server(profile: &'static str) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind gateway fixture");
    listener
        .set_nonblocking(true)
        .expect("gateway fixture nonblocking");
    let addr = listener.local_addr().expect("gateway fixture address");
    let join = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match listener.accept() {
                Ok((mut stream, _peer)) => {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                    let mut buffer = [0_u8; 1024];
                    let _ = stream.read(&mut buffer);
                    let body = serde_json::json!({
                        "ok": true,
                        "status": "ok",
                        "endpoint": "status",
                        "profile": profile,
                        "bind_address": addr.to_string(),
                        "uptime_ms": 12_u64,
                        "job_concurrency": 2_u64,
                        "event_bus_events": 1_u64,
                        "components": [
                            {"name": "supervisor", "status": "running"},
                            {"name": "health_http", "status": "running"}
                        ]
                    })
                    .to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(response.as_bytes());
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });
    (addr, join)
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
