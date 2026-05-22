use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};

use missive_a2a::{AgentCard, AgentCardExt, protocol};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

const A2A_FIXTURE_VERSION: &str = "1.0";

#[test]
fn fixture_directory_tracks_protocol_version_and_valid_json() {
    let root = fixture_root();
    assert_eq!(
        root.file_name().and_then(|name| name.to_str()),
        Some(A2A_FIXTURE_VERSION)
    );

    let readme = fs::read_to_string(root.join("README.md")).expect("fixture README");
    assert!(readme.contains("A2A 1.0 conformance fixtures"));
    assert!(readme.contains("Update process for future protocol versions"));

    let json_fixtures = json_fixture_paths(&root);
    assert!(
        json_fixtures.len() >= 20,
        "expected a broad A2A conformance fixture set"
    );

    for path in json_fixtures {
        let text = fs::read_to_string(&path).expect("fixture file should be readable");
        serde_json::from_str::<Value>(&text)
            .unwrap_or_else(|error| panic!("{} should be valid JSON: {error}", path.display()));
    }
}

#[test]
fn official_agent_card_fixtures_round_trip_and_missive_validates_them() {
    for name in ["agent_card.json", "agent_card_geospatial.json"] {
        round_trip_fixture::<protocol::AgentCard>(name);

        let value = fixture_value(name);
        let card = AgentCard::from_json(value).unwrap_or_else(|error| {
            panic!("{name} should be a missive-compatible Agent Card: {error}")
        });

        assert!(
            card.protocol_versions()
                .iter()
                .any(|version| version == A2A_FIXTURE_VERSION),
            "{name} should advertise A2A {A2A_FIXTURE_VERSION}"
        );
        assert!(
            !card.supported_interfaces.is_empty(),
            "{name} should declare at least one supported interface"
        );
    }
}

#[test]
fn official_message_task_artifact_and_stream_fixtures_round_trip() {
    for name in ["message.json", "message_file_upload.json"] {
        round_trip_fixture::<protocol::Message>(name);
    }

    round_trip_fixture::<protocol::Artifact>("artifact_citation.json");

    for name in [
        "task.json",
        "task_input_required.json",
        "task_file_artifact.json",
    ] {
        round_trip_fixture::<protocol::Task>(name);
    }

    for name in [
        "stream_response_task.json",
        "stream_response_message.json",
        "stream_response_status_update.json",
        "stream_response_artifact_update.json",
        "push_notification_payload_status_update.json",
    ] {
        round_trip_fixture::<protocol::StreamResponse>(name);
    }
}

#[test]
fn official_operation_and_push_config_fixtures_round_trip() {
    for name in [
        "send_message_request.json",
        "send_message_request_with_push_config.json",
    ] {
        round_trip_fixture::<protocol::SendMessageRequest>(name);
    }

    for name in [
        "send_message_response_task.json",
        "send_message_response_message.json",
    ] {
        round_trip_fixture::<protocol::SendMessageResponse>(name);
    }

    round_trip_fixture::<protocol::GetTaskRequest>("get_task_request.json");
    round_trip_fixture::<protocol::ListTasksRequest>("list_tasks_request.json");
    round_trip_fixture::<protocol::ListTasksResponse>("list_tasks_response.json");
    round_trip_fixture::<protocol::CancelTaskRequest>("cancel_task_request.json");
    round_trip_fixture::<protocol::SubscribeToTaskRequest>("subscribe_to_task_request.json");

    round_trip_fixture::<protocol::TaskPushNotificationConfig>(
        "task_push_notification_config.json",
    );
    round_trip_fixture::<protocol::GetTaskPushNotificationConfigRequest>(
        "get_task_push_notification_config_request.json",
    );
    round_trip_fixture::<protocol::ListTaskPushNotificationConfigsRequest>(
        "list_task_push_notification_configs_request.json",
    );
    round_trip_fixture::<protocol::ListTaskPushNotificationConfigsResponse>(
        "list_task_push_notification_configs_response.json",
    );
    round_trip_fixture::<protocol::DeleteTaskPushNotificationConfigRequest>(
        "delete_task_push_notification_config_request.json",
    );
}

#[test]
fn json_rpc_binding_fixtures_round_trip_and_embed_official_payloads() {
    let request =
        round_trip_fixture::<protocol::JsonRpcRequest>("jsonrpc_send_message_request.json");
    assert_eq!(request.method, protocol::jsonrpc_methods::SEND_MESSAGE);
    let params = request.params.expect("SendMessage params");
    serde_json::from_value::<protocol::SendMessageRequest>(params)
        .expect("JSON-RPC params should be a SendMessageRequest");

    let response =
        round_trip_fixture::<protocol::JsonRpcResponse>("jsonrpc_send_message_response_task.json");
    let result = response.result.expect("SendMessage result");
    serde_json::from_value::<protocol::SendMessageResponse>(result)
        .expect("JSON-RPC result should be a SendMessageResponse");

    for (name, expected_code) in [
        (
            "jsonrpc_error_invalid_params.json",
            protocol::error_code::INVALID_PARAMS,
        ),
        (
            "jsonrpc_error_task_not_found.json",
            protocol::error_code::TASK_NOT_FOUND,
        ),
    ] {
        let response = round_trip_fixture::<protocol::JsonRpcResponse>(name);
        let error = response
            .error
            .unwrap_or_else(|| panic!("{name} should contain an error"));
        assert_eq!(error.code, expected_code);
        assert!(
            error.data.as_ref().is_some_and(Value::is_array),
            "{name} should include structured error data"
        );
    }
}

#[test]
fn http_error_fixtures_preserve_a2a_error_info() {
    for (name, expected_status, expected_reason) in [
        ("http_error_task_not_found.json", 404, "TASK_NOT_FOUND"),
        (
            "http_error_version_not_supported.json",
            400,
            "VERSION_NOT_SUPPORTED",
        ),
    ] {
        let value = fixture_value(name);
        assert_eq!(value["error"]["code"], expected_status);
        let details = value["error"]["details"]
            .as_array()
            .unwrap_or_else(|| panic!("{name} should contain error.details"));
        assert!(details.iter().any(|detail| {
            detail["@type"] == "type.googleapis.com/google.rpc.ErrorInfo"
                && detail["reason"] == expected_reason
                && detail["domain"] == "a2a-protocol.org"
        }));
    }
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/a2a")
        .join(A2A_FIXTURE_VERSION)
}

fn fixture_text(name: &str) -> String {
    let path = fixture_root().join(name);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read fixture {}: {error}", path.display()))
}

fn fixture_value(name: &str) -> Value {
    serde_json::from_str(&fixture_text(name))
        .unwrap_or_else(|error| panic!("{name} should be valid JSON: {error}"))
}

fn round_trip_fixture<T>(name: &str) -> T
where
    T: DeserializeOwned + Serialize + PartialEq + Debug,
{
    round_trip(name, &fixture_text(name))
}

fn round_trip<T>(name: &str, fixture: &str) -> T
where
    T: DeserializeOwned + Serialize + PartialEq + Debug,
{
    let parsed = serde_json::from_str::<T>(fixture).unwrap_or_else(|error| {
        panic!("{name} fixture should parse with official A2A SDK type: {error}")
    });
    let value = serde_json::to_value(&parsed)
        .unwrap_or_else(|error| panic!("{name} fixture should serialize: {error}"));
    let reparsed = serde_json::from_value::<T>(value)
        .unwrap_or_else(|error| panic!("{name} fixture should parse after serialization: {error}"));

    assert_eq!(parsed, reparsed, "{name} should round-trip through serde");
    parsed
}

fn json_fixture_paths(root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    collect_json_fixture_paths(root, &mut paths);
    paths.sort();
    paths
}

fn collect_json_fixture_paths(path: &Path, paths: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(path).unwrap_or_else(|error| {
        panic!(
            "failed to read fixture directory {}: {error}",
            path.display()
        )
    }) {
        let entry = entry.expect("fixture directory entry");
        let path = entry.path();
        if path.is_dir() {
            collect_json_fixture_paths(&path, paths);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
            paths.push(path);
        }
    }
}
