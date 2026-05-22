use std::fmt::Debug;

use missive_a2a::{AgentCard, AgentCardExt, protocol};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

const AGENT_CARD: &str = include_str!("../../../tests/fixtures/a2a/1.0/agent_card.json");
const MESSAGE: &str = include_str!("../../../tests/fixtures/a2a/1.0/message.json");
const TASK: &str = include_str!("../../../tests/fixtures/a2a/1.0/task.json");
const SEND_MESSAGE_REQUEST: &str =
    include_str!("../../../tests/fixtures/a2a/1.0/send_message_request.json");
const SEND_MESSAGE_RESPONSE_TASK: &str =
    include_str!("../../../tests/fixtures/a2a/1.0/send_message_response_task.json");
const TASK_PUSH_NOTIFICATION_CONFIG: &str =
    include_str!("../../../tests/fixtures/a2a/1.0/task_push_notification_config.json");

#[test]
fn official_agent_card_fixture_round_trips_and_missive_validates_it() {
    round_trip::<protocol::AgentCard>("agent_card", AGENT_CARD);

    let value = serde_json::from_str::<Value>(AGENT_CARD).expect("fixture JSON");
    let card = AgentCard::from_json(value).expect("missive-compatible Agent Card");

    assert_eq!(card.name, "Fixture Echo Agent");
    assert_eq!(card.protocol_versions(), vec!["1.0"]);
}

#[test]
fn official_message_and_task_fixtures_round_trip() {
    round_trip::<protocol::Message>("message", MESSAGE);
    round_trip::<protocol::Task>("task", TASK);
}

#[test]
fn official_request_response_and_push_fixtures_round_trip() {
    round_trip::<protocol::SendMessageRequest>("send_message_request", SEND_MESSAGE_REQUEST);
    round_trip::<protocol::SendMessageResponse>(
        "send_message_response_task",
        SEND_MESSAGE_RESPONSE_TASK,
    );
    round_trip::<protocol::TaskPushNotificationConfig>(
        "task_push_notification_config",
        TASK_PUSH_NOTIFICATION_CONFIG,
    );
}

fn round_trip<T>(name: &str, fixture: &str)
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
}
