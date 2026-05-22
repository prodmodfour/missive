use std::process::Command;

use serde_json::Value;

#[test]
fn verbose_flag_emits_human_diagnostics_on_stderr() {
    let output = Command::new(env!("CARGO_BIN_EXE_missive"))
        .args(["-v", "doctor"])
        .env_remove("RUST_LOG")
        .env_remove("MISSIVE_LOG_FORMAT")
        .output()
        .expect("missive should run");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr UTF-8");

    assert!(stdout.contains("missive: 'doctor' command parsed"));
    assert!(stderr.contains("INFO missive_observe: diagnostics initialized"));
    assert!(stderr.contains("filter=info"));
}

#[test]
fn rust_log_and_json_log_format_emit_machine_readable_stderr() {
    let output = Command::new(env!("CARGO_BIN_EXE_missive"))
        .args(["doctor"])
        .env("RUST_LOG", "info")
        .env("MISSIVE_LOG_FORMAT", "json")
        .output()
        .expect("missive should run");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr UTF-8");

    assert!(stdout.contains("missive: 'doctor' command parsed"));
    let first_line = stderr.lines().next().expect("one diagnostics line");
    let value: Value = serde_json::from_str(first_line).expect("JSON diagnostics line");

    assert_eq!(value["level"], "INFO");
    assert_eq!(value["target"], "missive_observe");
    assert_eq!(value["fields"]["message"], "diagnostics initialized");
    assert_eq!(value["fields"]["filter"], "info");
    assert_eq!(value["fields"]["format"], "json");
}

#[test]
fn trace_flag_emits_trace_bootstrap_diagnostic() {
    let output = Command::new(env!("CARGO_BIN_EXE_missive"))
        .args(["--trace", "doctor"])
        .env_remove("RUST_LOG")
        .env_remove("MISSIVE_LOG_FORMAT")
        .output()
        .expect("missive should run");

    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("stderr UTF-8");

    assert!(stderr.contains("filter=trace"));
    assert!(stderr.contains("TRACE missive_observe: trace diagnostics enabled"));
}
