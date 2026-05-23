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
const EXAMPLE_TASK_TEXT: &str = "example task completed";
const STREAM_TASK_ID: &str = "task-stream-example-1";
const STREAM_CONTEXT_ID: &str = "ctx-stream-example-1";

#[derive(Debug, Clone)]
struct ServerOptions {
    ready_file: Option<PathBuf>,
    task_id: String,
    context_id: String,
    task_text: String,
    stream_task_id: String,
    stream_context_id: String,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            ready_file: None,
            task_id: EXAMPLE_TASK_ID.to_owned(),
            context_id: EXAMPLE_CONTEXT_ID.to_owned(),
            task_text: EXAMPLE_TASK_TEXT.to_owned(),
            stream_task_id: STREAM_TASK_ID.to_owned(),
            stream_context_id: STREAM_CONTEXT_ID.to_owned(),
        }
    }
}

fn main() {
    let options = parse_options(env::args().skip(1).collect());

    let server = MockA2aServer::start();
    configure_server(&server, &options);

    if let Some(path) = options.ready_file {
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

fn parse_options(args: Vec<String>) -> ServerOptions {
    let mut options = ServerOptions::default();
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--ready-file" => {
                let path = iter
                    .next()
                    .unwrap_or_else(|| panic!("--ready-file requires a path"));
                options.ready_file = Some(PathBuf::from(path));
            }
            "--task-id" => {
                options.task_id = iter
                    .next()
                    .unwrap_or_else(|| panic!("--task-id requires a value"));
            }
            "--context-id" => {
                options.context_id = iter
                    .next()
                    .unwrap_or_else(|| panic!("--context-id requires a value"));
            }
            "--task-text" => {
                options.task_text = iter
                    .next()
                    .unwrap_or_else(|| panic!("--task-text requires a value"));
            }
            "--stream-task-id" => {
                options.stream_task_id = iter
                    .next()
                    .unwrap_or_else(|| panic!("--stream-task-id requires a value"));
            }
            "--stream-context-id" => {
                options.stream_context_id = iter
                    .next()
                    .unwrap_or_else(|| panic!("--stream-context-id requires a value"));
            }
            "--help" | "-h" => {
                println!(
                    "Usage: cargo run -p missive-test-support --example mock_a2a_server -- [OPTIONS]\n\nStarts the local mock A2A server used by top-level missive example scripts.\n\nOptions:\n  --ready-file PATH        Write the selected base URL to PATH.\n  --task-id ID             Task id returned by SendMessage and GetTask.\n  --context-id ID          Context id used by the task response.\n  --task-text TEXT         Status message text for the completed task.\n  --stream-task-id ID      Task id used by streaming fixtures.\n  --stream-context-id ID   Context id used by streaming fixtures."
                );
                std::process::exit(0);
            }
            other => panic!("unknown argument: {other}"),
        }
    }
    options
}

fn configure_server(server: &MockA2aServer, options: &ServerOptions) {
    let handle = server.handle();
    let completed = task_json(
        &options.task_id,
        &options.context_id,
        "TASK_STATE_COMPLETED",
        &options.task_text,
    );
    handle.set_send_response(send_message_response_task(completed.clone()));
    handle.enqueue_task_sequence(
        options.task_id.clone(),
        [
            task_json(
                &options.task_id,
                &options.context_id,
                "TASK_STATE_WORKING",
                "example task is working",
            ),
            completed,
        ],
    );
    handle.set_stream_events(vec![
        status_update_event(
            &options.stream_task_id,
            &options.stream_context_id,
            "TASK_STATE_WORKING",
            Some("stream example started"),
        ),
        artifact_update_event(
            &options.stream_task_id,
            &options.stream_context_id,
            "artifact-stream-example-1",
            "stream example artifact",
            true,
            true,
        ),
        status_update_event(
            &options.stream_task_id,
            &options.stream_context_id,
            "TASK_STATE_COMPLETED",
            Some("stream example completed"),
        ),
    ]);
}
