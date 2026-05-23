#![no_main]

use libfuzzer_sys::fuzz_target;
use missive_a2a::protocol;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

const MAX_INPUT_BYTES: usize = 128 * 1024;

fn try_roundtrip<T>(value: &Value)
where
    T: DeserializeOwned + Serialize,
{
    if let Ok(parsed) = serde_json::from_value::<T>(value.clone()) {
        let _ = serde_json::to_value(parsed);
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    let Ok(value) = serde_json::from_slice::<Value>(data) else {
        return;
    };

    try_roundtrip::<protocol::AgentCard>(&value);
    try_roundtrip::<protocol::AgentCapabilities>(&value);
    try_roundtrip::<protocol::AgentSkill>(&value);
    try_roundtrip::<protocol::Message>(&value);
    try_roundtrip::<protocol::Part>(&value);
    try_roundtrip::<protocol::Artifact>(&value);
    try_roundtrip::<protocol::Task>(&value);
    try_roundtrip::<protocol::TaskStatus>(&value);
    try_roundtrip::<protocol::TaskStatusUpdateEvent>(&value);
    try_roundtrip::<protocol::TaskArtifactUpdateEvent>(&value);
    try_roundtrip::<protocol::StreamResponse>(&value);
    try_roundtrip::<protocol::SendMessageRequest>(&value);
    try_roundtrip::<protocol::SendMessageResponse>(&value);
    try_roundtrip::<protocol::ListTasksResponse>(&value);
    try_roundtrip::<protocol::TaskPushNotificationConfig>(&value);
    try_roundtrip::<protocol::ListTaskPushNotificationConfigsResponse>(&value);
    try_roundtrip::<protocol::JsonRpcRequest>(&value);
    try_roundtrip::<protocol::JsonRpcResponse>(&value);
    try_roundtrip::<protocol::JsonRpcError>(&value);
    try_roundtrip::<protocol::AuthenticationInfo>(&value);
});
