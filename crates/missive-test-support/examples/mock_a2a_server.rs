use std::env;
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use missive_test_support::{
    MockA2aServer, artifact_update_event, send_message_response_task, status_update_event,
    task_json,
};

const EXAMPLE_TASK_ID: &str = "task-example-1";
const EXAMPLE_CONTEXT_ID: &str = "ctx-example-1";
const STREAM_TASK_ID: &str = "task-stream-example-1";
const STREAM_CONTEXT_ID: &str = "ctx-stream-example-1";

fn main() {
    let ready_file = parse_ready_file(env::args().skip(1).collect());

    let server = MockA2aServer::start();
    configure_server(&server);

    if let Some(path) = ready_file {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create ready-file parent directory");
        }
        fs::write(&path, server.base_url()).expect("write ready file");
    }

    println!("{}", server.base_url());
    eprintln!("missive mock A2A server listening on {}", server.base_url());

    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}

fn parse_ready_file(args: Vec<String>) -> Option<PathBuf> {
    let mut ready_file = None;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--ready-file" => {
                let path = iter
                    .next()
                    .unwrap_or_else(|| panic!("--ready-file requires a path"));
                ready_file = Some(PathBuf::from(path));
            }
            "--help" | "-h" => {
                println!(
                    "Usage: cargo run -p missive-test-support --example mock_a2a_server -- [--ready-file PATH]\n\nStarts the local mock A2A server used by top-level missive example scripts."
                );
                std::process::exit(0);
            }
            other => panic!("unknown argument: {other}"),
        }
    }
    ready_file
}

fn configure_server(server: &MockA2aServer) {
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
