use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use missive_cli::run_from_with_environment;
use missive_core::MissiveExitCode;
use serde_json::{Value, json};
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

fn write_ready_json(path: &Path, value: Value) {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(&value).expect("json bytes")).expect("write tmp");
    fs::rename(&tmp, path).expect("atomic rename to ready file");
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("read json file"))
        .expect("json file should parse")
}

#[test]
fn file_drop_processes_ready_request_and_ignores_partial_file() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let inbox = temp.path().join("inbox");
    let outbox = temp.path().join("outbox");
    fs::create_dir_all(&inbox).expect("inbox");
    fs::write(inbox.join("partial.tmp"), "not complete yet").expect("partial");
    write_ready_json(
        &inbox.join("req-list.json"),
        json!({
            "schema_version": "missive.file_drop.v1",
            "id": "drop-list",
            "command": "task_list"
        }),
    );

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "adapter",
            "file-drop",
            "--inbox",
            inbox.to_str().expect("inbox path"),
            "--outbox",
            outbox.to_str().expect("outbox path"),
            "--mode",
            "once",
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let summary = serde_json::from_str::<Value>(&stdout).expect("summary JSON");
    assert_eq!(summary["kind"], "adapter_file_drop");
    assert_eq!(summary["data"]["processed_files"], 1);
    assert_eq!(summary["data"]["succeeded"], 1);
    assert_eq!(summary["data"]["failed"], 0);

    assert!(inbox.join("partial.tmp").exists());
    assert!(inbox.join("processed").join("req-list.json").exists());
    let result_path = outbox.join("req-list.result.json");
    assert!(result_path.exists());
    let result = read_json(&result_path);
    assert_eq!(result["schema_version"], "missive.file_drop.v1");
    assert_eq!(result["id"], "drop-list");
    assert_eq!(result["ok"], true);
    assert_eq!(result["outputs"].as_array().expect("outputs").len(), 1);
    assert_eq!(result["outputs"][0]["data"]["kind"], "task_list");
    assert_eq!(result["outputs"][0]["data"]["data"]["count"], 0);
}

#[test]
fn file_drop_moves_malformed_ready_file_to_error_dir() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let inbox = temp.path().join("inbox");
    let outbox = temp.path().join("outbox");
    fs::create_dir_all(&inbox).expect("inbox");
    fs::write(inbox.join("bad.json"), "not-json").expect("bad file");

    let (code, stdout, stderr) = run(
        &[
            "missive",
            "adapter",
            "file-drop",
            "--inbox",
            inbox.to_str().expect("inbox path"),
            "--outbox",
            outbox.to_str().expect("outbox path"),
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let summary = serde_json::from_str::<Value>(&stdout).expect("summary JSON");
    assert_eq!(summary["data"]["processed_files"], 1);
    assert_eq!(summary["data"]["succeeded"], 0);
    assert_eq!(summary["data"]["failed"], 1);

    assert!(inbox.join("error").join("bad.json").exists());
    let result = read_json(&outbox.join("bad.error.json"));
    assert_eq!(result["ok"], false);
    assert_eq!(result["kind"], "file_drop_error");
    assert_eq!(result["error"]["code"], "missive::validation");
}

#[test]
fn file_drop_job_file_can_enqueue_background_wait_job() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("missive-home");
    let environment = isolated_env(&home);
    let inbox = temp.path().join("inbox");
    let outbox = temp.path().join("outbox");
    fs::create_dir_all(&inbox).expect("inbox");

    let (code, _stdout, stderr) = run(
        &[
            "missive",
            "agent",
            "add",
            "echo",
            "http://127.0.0.1:1",
            "--json",
        ],
        &environment,
        temp.path(),
    );
    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");

    write_ready_json(
        &inbox.join("wait-job.json"),
        json!({
            "schema_version": "missive.file_drop.v1",
            "id": "drop-wait-job",
            "command": "job_start_wait",
            "task_id": "task-file-drop-wait",
            "agent": "echo",
            "local": true,
            "options": {"max_attempts": 1}
        }),
    );

    let (code, _stdout, stderr) = run(
        &[
            "missive",
            "adapter",
            "file-drop",
            "--inbox",
            inbox.to_str().expect("inbox path"),
            "--outbox",
            outbox.to_str().expect("outbox path"),
            "--json",
        ],
        &environment,
        temp.path(),
    );

    assert_eq!(code, MissiveExitCode::Success.as_i32(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    let result = read_json(&outbox.join("wait-job.result.json"));
    assert_eq!(result["ok"], true);
    assert_eq!(result["outputs"][0]["data"]["kind"], "job_start");
    assert_eq!(result["outputs"][0]["data"]["data"]["job"]["kind"], "wait");
    assert_eq!(
        result["outputs"][0]["data"]["data"]["job"]["task_id"],
        "task-file-drop-wait"
    );
}
