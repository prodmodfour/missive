#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use missive_cli::events::replay_event_records_for_fuzzing;
use missive_core::{AgentAlias, ContextId, EventId, GroupName, Metadata, MissiveTimestamp, TaskId};
use missive_store::EventRecord;
use serde_json::{Value, json};

const MAX_EVENTS: usize = 64;
const BASE_TIMESTAMP: i64 = 1_700_000_000;

#[derive(Debug, Arbitrary)]
struct EventReplayInput {
    events: Vec<FuzzEvent>,
}

#[derive(Debug, Arbitrary)]
struct FuzzEvent {
    sequence_hint: u16,
    source: u8,
    event_type: u8,
    agent: Option<u8>,
    context: Option<u8>,
    task: Option<u8>,
    group: Option<u8>,
    state: Option<u8>,
    nested_shape: bool,
    redacted: bool,
}

fuzz_target!(|input: EventReplayInput| {
    let records = input
        .events
        .into_iter()
        .take(MAX_EVENTS)
        .enumerate()
        .map(|(index, event)| event.into_record(index))
        .collect::<Vec<_>>();

    let _ = replay_event_records_for_fuzzing(&records);
});

impl FuzzEvent {
    fn into_record(self, index: usize) -> EventRecord {
        let event_id = EventId::new(format!("evt/fuzz/{index}")).expect("valid fuzz event id");
        let timestamp = MissiveTimestamp::from_unix_timestamp(BASE_TIMESTAMP + index as i64)
            .expect("valid fuzz timestamp");
        let agent_alias = self.agent.map(valid_agent_alias);
        let context_id = self.context.map(valid_context_id);
        let task_id = self.task.map(valid_task_id);
        let group_name = self.group.map(valid_group_name);
        let state = self.state.map(valid_state);
        let payload_json = build_payload(state.as_deref(), self.nested_shape, context_id.as_ref());

        EventRecord {
            sequence: i64::from(self.sequence_hint) + index as i64,
            event_id,
            timestamp,
            source: valid_source(self.source).to_owned(),
            event_type: valid_event_type(self.event_type).to_owned(),
            agent_alias,
            context_id,
            task_id,
            group_name,
            gateway_job_id: None,
            adapter_binding_id: None,
            payload_json,
            metadata: Metadata::new(),
            redacted: self.redacted,
        }
    }
}

fn valid_agent_alias(value: u8) -> AgentAlias {
    AgentAlias::new(format!("agent-{}", value % 8)).expect("valid fuzz agent alias")
}

fn valid_context_id(value: u8) -> ContextId {
    ContextId::new(format!("ctx/fuzz/{}", value % 16)).expect("valid fuzz context id")
}

fn valid_task_id(value: u8) -> TaskId {
    TaskId::new(format!("task/fuzz/{}", value % 16)).expect("valid fuzz task id")
}

fn valid_group_name(value: u8) -> GroupName {
    GroupName::new(format!("group-{}", value % 4)).expect("valid fuzz group name")
}

fn valid_source(value: u8) -> &'static str {
    match value % 5 {
        0 => "cli",
        1 => "gateway",
        2 => "adapter:stdio",
        3 => "adapter:file-drop",
        _ => "test",
    }
}

fn valid_event_type(value: u8) -> &'static str {
    match value % 8 {
        0 => "a2a.task.updated",
        1 => "a2a.stream.status",
        2 => "a2a.stream.task",
        3 => "missive.context.created",
        4 => "missive.bcast.member.completed",
        5 => "missive.gateway.job.completed",
        6 => "missive.gather.completed",
        _ => "missive.reduce.completed",
    }
}

fn valid_state(value: u8) -> String {
    match value % 7 {
        0 => "submitted",
        1 => "working",
        2 => "input-required",
        3 => "completed",
        4 => "failed",
        5 => "cancelled",
        _ => "TASK_STATE_COMPLETED",
    }
    .to_owned()
}

fn build_payload(state: Option<&str>, nested_shape: bool, context_id: Option<&ContextId>) -> Value {
    let context_value = context_id.map(ToString::to_string);
    match (state, nested_shape) {
        (Some(state), true) => json!({
            "task": {
                "status": { "state": state },
                "contextId": context_value,
            },
            "context": {
                "state": state,
                "name": "fuzz-context",
            }
        }),
        (Some(state), false) => json!({
            "state": state,
            "context_id": context_value,
        }),
        (None, true) => json!({
            "context": {
                "name": "fuzz-context",
            },
            "contextId": context_value,
        }),
        (None, false) => json!({
            "message": "fuzz event",
            "context_id": context_value,
        }),
    }
}
