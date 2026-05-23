use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use missive_core::MissiveExitCode;
use missive_test_support::{
    MockA2aServer, artifact_update_event, send_message_response_task, status_update_event,
    task_json,
};
use tempfile::tempdir;

const EXAMPLE_TASK_ID: &str = "task-example-1";
const EXAMPLE_CONTEXT_ID: &str = "ctx-example-1";
const STREAM_TASK_ID: &str = "task-stream-example-1";
const STREAM_CONTEXT_ID: &str = "ctx-stream-example-1";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf()
}

fn configure_example_server(server: &MockA2aServer) {
    let handle = server.handle();
    let completed = task_json(
        EXAMPLE_TASK_ID,
        EXAMPLE_CONTEXT_ID,
        "TASK_STATE_COMPLETED",
        "example task completed",
    );
    handle.set_send_response(send_message_response_task(completed.clone()));
    handle.enqueue_task_sequence(
        EXAMPLE_TASK_ID,
        [
            task_json(
                EXAMPLE_TASK_ID,
                EXAMPLE_CONTEXT_ID,
                "TASK_STATE_WORKING",
                "example task is working",
            ),
            completed,
        ],
    );
    handle.set_stream_events(vec![
        status_update_event(
            STREAM_TASK_ID,
            STREAM_CONTEXT_ID,
            "TASK_STATE_WORKING",
            Some("stream example started"),
        ),
        artifact_update_event(
            STREAM_TASK_ID,
            STREAM_CONTEXT_ID,
            "artifact-stream-example-1",
            "stream example artifact",
            true,
            true,
        ),
        status_update_event(
            STREAM_TASK_ID,
            STREAM_CONTEXT_ID,
            "TASK_STATE_COMPLETED",
            Some("stream example completed"),
        ),
    ]);
}

fn wait_for_child(mut child: Child, timeout: Duration) -> (ExitStatus, String, String) {
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll child") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("example smoke script did not exit before timeout");
        }
        thread::sleep(Duration::from_millis(50));
    };

    let mut stdout = String::new();
    let mut stderr = String::new();
    child
        .stdout
        .take()
        .expect("stdout pipe")
        .read_to_string(&mut stdout)
        .expect("read stdout");
    child
        .stderr
        .take()
        .expect("stderr pipe")
        .read_to_string(&mut stderr)
        .expect("read stderr");
    (status, stdout, stderr)
}

#[test]
fn top_level_command_examples_run_against_mock_a2a_server() {
    let server = MockA2aServer::start();
    configure_example_server(&server);

    let temp = tempdir().expect("tempdir");
    let root = repo_root();
    let script = root.join("examples/run-smoke.sh");

    let child = Command::new("bash")
        .arg(script)
        .current_dir(&root)
        .env("MISSIVE_BIN", env!("CARGO_BIN_EXE_missive"))
        .env("MISSIVE_EXAMPLE_A2A_BASE_URL", server.base_url())
        .env("MISSIVE_EXAMPLE_WORKDIR", temp.path().join("examples"))
        .env_remove("MISSIVE_HOME")
        .env_remove("MISSIVE_CONFIG")
        .env_remove("MISSIVE_REPO_CONFIG")
        .env_remove("RUST_LOG")
        .env_remove("MISSIVE_LOG_FORMAT")
        .env_remove("MISSIVE_LOG_JSON")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn example smoke script");

    let (status, stdout, stderr) = wait_for_child(child, Duration::from_secs(60));
    assert_eq!(
        status.code(),
        Some(MissiveExitCode::Success.as_i32()),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stderr.is_empty(), "stderr:\n{stderr}\nstdout:\n{stdout}");

    for marker in [
        "demo-agent-registry.sh",
        "demo-send.sh",
        "demo-stream-tasks.sh",
        "demo-contexts-groups.sh",
        "demo-gateway.sh",
        "agent_inspect",
        "send_result",
        "stream_result",
        "task_wait",
        "context_export",
        "group_capabilities",
        "gateway_started",
    ] {
        assert!(
            stdout.contains(marker),
            "missing marker {marker:?} in stdout:\n{stdout}"
        );
    }
}
