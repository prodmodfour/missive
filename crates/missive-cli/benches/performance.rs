use std::collections::BTreeMap;
use std::hint::black_box;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use missive_cli::{Cli, execute_with_environment};
use missive_core::{
    AgentAlias, ContextId, EventId, GroupName, MessageId, Metadata, MissiveConfig,
    MissiveTimestamp, RankName, RoutingPolicyKind, TaskId,
};
use missive_router::{RouteCandidate, RoutePlanInput, explain_route};
use missive_store::{
    AgentUpsert, ArtifactId, ArtifactKind, ArtifactUpsert, ContextUpsert, EventInsert, EventRecord,
    GroupMemberUpsert, GroupUpsert, MessageDirection, MessageInsert, MessageRole,
    StatePathResolver, Store, TaskState, TaskUpsert,
};
use serde_json::{Value, json};
use tempfile::TempDir;

const CONFIG_AGENT_COUNT: usize = 32;
const ROUTING_CANDIDATE_COUNT: usize = 128;
const EVENT_REPLAY_COUNT: usize = 512;
const STREAM_EVENT_COUNT: usize = 256;
const STORE_OPERATION_COUNT: usize = 32;
const COLLECTIVE_MEMBER_COUNT: usize = 16;

fn bench_config_load(c: &mut Criterion) {
    let config_toml = benchmark_config_toml(CONFIG_AGENT_COUNT);
    let parsed_config = MissiveConfig::from_toml_str(&config_toml).expect("benchmark config");

    let mut group = c.benchmark_group("config_load");
    group.throughput(Throughput::Bytes(config_toml.len() as u64));
    group.bench_function("parse_validate_32_agents", |b| {
        b.iter(|| {
            let config = MissiveConfig::from_toml_str(black_box(&config_toml))
                .expect("benchmark config should parse");
            black_box(config)
        })
    });
    group.bench_function("redacted_render_32_agents", |b| {
        b.iter(|| {
            let rendered = parsed_config
                .to_redacted_json()
                .expect("benchmark config should render");
            black_box(rendered)
        })
    });
    group.finish();
}

fn bench_store_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("store_operations");
    group.throughput(Throughput::Elements(STORE_OPERATION_COUNT as u64));
    group.bench_function("repository_crud_batch", |b| {
        b.iter_batched(
            seed_store_for_operations,
            exercise_store_operations,
            BatchSize::SmallInput,
        )
    });
    group.finish();
}

fn bench_routing(c: &mut Criterion) {
    let mut group = c.benchmark_group("routing");
    group.throughput(Throughput::Elements(ROUTING_CANDIDATE_COUNT as u64));

    for policy in [
        RoutingPolicyKind::CapabilityMatch,
        RoutingPolicyKind::Weighted,
        RoutingPolicyKind::Broadcast,
        RoutingPolicyKind::Quorum,
    ] {
        let input = route_plan_input(policy, ROUTING_CANDIDATE_COUNT);
        group.bench_function(policy.as_str(), |b| {
            b.iter(|| {
                let plan = explain_route(black_box(&input)).expect("route plan");
                black_box(plan)
            })
        });
    }

    group.finish();
}

fn bench_event_replay(c: &mut Criterion) {
    let records = event_replay_records(EVENT_REPLAY_COUNT);

    let mut group = c.benchmark_group("event_replay");
    group.throughput(Throughput::Elements(records.len() as u64));
    group.bench_function("replay_512_records", |b| {
        b.iter(|| {
            let replay = missive_cli::events::replay_event_records_for_fuzzing(black_box(&records))
                .expect("event replay");
            black_box(replay)
        })
    });
    group.finish();
}

fn bench_streaming_event_parsing(c: &mut Criterion) {
    let payloads = stream_event_payloads(STREAM_EVENT_COUNT);

    let mut group = c.benchmark_group("streaming_event_parsing");
    group.throughput(Throughput::Elements(payloads.len() as u64));
    group.bench_function("parse_256_events", |b| {
        b.iter(|| {
            let mut parsed = 0_usize;
            for (sequence, payload) in payloads.iter().enumerate() {
                let (event, raw_json) = missive_a2a::parse_stream_event_data_for_benchmarks(
                    black_box(payload),
                    sequence as u64,
                )
                .expect("stream event should parse");
                parsed = parsed.saturating_add(raw_json_size_hint(&raw_json));
                black_box(event);
            }
            black_box(parsed)
        })
    });
    group.finish();
}

fn bench_group_collectives(c: &mut Criterion) {
    let mut group = c.benchmark_group("group_collectives");
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(100));
    group.measurement_time(Duration::from_millis(500));
    group.throughput(Throughput::Elements(COLLECTIVE_MEMBER_COUNT as u64));

    group.bench_function("gather_local_outputs_cli", |b| {
        b.iter_batched(
            seed_collective_fixture,
            |fixture| run_collective_command(fixture, CollectiveCommand::Gather),
            BatchSize::SmallInput,
        )
    });
    group.bench_function("reduce_summarise_local_cli", |b| {
        b.iter_batched(
            seed_collective_fixture,
            |fixture| run_collective_command(fixture, CollectiveCommand::Reduce),
            BatchSize::SmallInput,
        )
    });

    group.finish();
}

fn benchmark_config_toml(agent_count: usize) -> String {
    let mut config = String::from(
        r#"schema_version = "missive.config.v1"
default_profile = "default"

[profiles.default]
description = "Benchmark profile with several local A2A agents"
default_agent = "agent-00"

[profiles.default.protocol]
protocol_version = "1.0"
extensions = ["bench.extension"]

[profiles.default.protocol.service_parameters]
"X-Missive-Bench" = "enabled"

[profiles.default.routing]
default_policy = "capability-match"

[profiles.default.qos]
timeout = "20s"
connect_timeout = "5s"
retry_attempts = 2
retry_backoff = "100ms"
max_request_bytes = 2097152
concurrency = 8

[gateway]
enabled = true
bind_address = "127.0.0.1:7347"
job_concurrency = 4

[adapters.local_stdio]
kind = "stdio"
enabled = true
session_profile = "default"

"#,
    );

    for index in 0..agent_count {
        let port = 20_000 + index;
        let tier = index % 4;
        config.push_str(&format!(
            r#"[agents.agent-{index:02}]
base_url = "http://127.0.0.1:{port}"
binding_preference = ["http+json", "json-rpc"]
tags = ["bench", "tier{tier}"]
notes = "Benchmark agent {index}"

[agents.agent-{index:02}.interface_urls]
"http+json" = "http://127.0.0.1:{port}/a2a"
"json-rpc" = "http://127.0.0.1:{port}/rpc"

[agents.agent-{index:02}.metadata]
capability = "summarise"
rank = {index}

"#
        ));
    }

    config
}

fn seed_store_for_operations() -> Store {
    let store = Store::open_in_memory().expect("in-memory store");
    for index in 0..4 {
        let alias = agent_alias(index);
        store
            .upsert_agent(&AgentUpsert::new(
                alias,
                format!("http://127.0.0.1:{}", 21_000 + index),
            ))
            .expect("seed agent");
    }
    store
}

fn exercise_store_operations(store: Store) -> usize {
    let context_id = ContextId::new("ctx-store-bench").expect("context id");
    store
        .upsert_context(&ContextUpsert::new(context_id.clone()))
        .expect("upsert context");

    for index in 0..STORE_OPERATION_COUNT {
        let alias = agent_alias(index % 4);
        let task_id = TaskId::new(format!("task-store-{index:03}")).expect("task id");
        let message_id = MessageId::new(format!("msg-store-{index:03}")).expect("message id");
        let artifact_id =
            ArtifactId::new(format!("artifact-store-{index:03}")).expect("artifact id");

        let mut task = TaskUpsert::new(task_id.clone(), alias.clone(), TaskState::Completed);
        task.context_id = Some(context_id.clone());
        task.last_message_id = Some(message_id.clone());
        task.completed_at = Some(benchmark_timestamp(index));
        task.remote_task_json = Some(task_json(&task_id, &context_id, "TASK_STATE_COMPLETED"));
        store.upsert_task(&task).expect("upsert task");

        let mut message = MessageInsert::new(
            message_id,
            MessageDirection::Response,
            message_json(index, &task_id, &context_id),
        );
        message.agent_alias = Some(alias.clone());
        message.context_id = Some(context_id.clone());
        message.task_id = Some(task_id.clone());
        message.role = Some(MessageRole::Agent);
        store.insert_message(&message).expect("insert message");

        let mut artifact = ArtifactUpsert::new(artifact_id, task_id.clone());
        artifact.context_id = Some(context_id.clone());
        artifact.name = Some(format!("artifact-{index:03}.md"));
        artifact.mime_type = Some("text/markdown".to_owned());
        artifact.kind = ArtifactKind::Text;
        artifact.content_json = Some(artifact_json(index));
        store.upsert_artifact(&artifact).expect("upsert artifact");

        let mut event = EventInsert::new(
            EventId::new(format!("event-store-{index:03}")).expect("event id"),
            "cli",
            "a2a.task.updated",
            json!({
                "task": {
                    "taskId": task_id.as_str(),
                    "contextId": context_id.as_str(),
                    "status": { "state": "TASK_STATE_COMPLETED" }
                }
            }),
        );
        event.agent_alias = Some(alias);
        event.context_id = Some(context_id.clone());
        event.task_id = Some(task_id);
        store.append_event(&event).expect("append event");
    }

    let total = store.list_agents().expect("list agents").len()
        + store.list_tasks().expect("list tasks").len()
        + store.list_messages().expect("list messages").len()
        + store.list_artifacts().expect("list artifacts").len()
        + store.list_events().expect("list events").len();
    black_box(total)
}

fn route_plan_input(policy: RoutingPolicyKind, candidate_count: usize) -> RoutePlanInput {
    let mut candidates = Vec::with_capacity(candidate_count);
    for index in 0..candidate_count {
        let mut candidate = RouteCandidate::new(agent_alias(index));
        candidate.rank = Some(RankName::new(format!("rank-{index:03}")).expect("rank"));
        candidate.tags = vec!["bench".to_owned(), format!("tier{}", index % 4)];
        candidate.capabilities = if index % 2 == 0 {
            vec!["summarise".to_owned(), "merge".to_owned()]
        } else {
            vec!["rank".to_owned()]
        };
        candidate.input_modes = vec!["text/plain".to_owned(), "application/json".to_owned()];
        candidate.output_modes = if index % 3 == 0 {
            vec!["text/markdown".to_owned(), "application/json".to_owned()]
        } else {
            vec!["text/plain".to_owned()]
        };
        candidate.supports_streaming = Some(index % 2 == 0);
        candidate.supports_push_notifications = Some(index % 5 == 0);
        candidate.capability_cache_status = Some("cached".to_owned());
        candidate.weight = ((index % 8) + 1) as f64;
        candidate
            .metadata
            .insert_str("bench.group", format!("group-{}", index % 8))
            .expect("metadata");
        candidates.push(candidate);
    }

    RoutePlanInput {
        policy,
        candidates,
        preferred_agent: Some(agent_alias(0)),
        required_tags: vec!["bench".to_owned()],
        required_capabilities: vec!["summarise".to_owned()],
        required_input_modes: vec!["text/plain".to_owned()],
        required_output_modes: vec!["text/markdown".to_owned()],
        require_streaming: true,
        require_push_notifications: false,
        round_robin_cursor: 17,
        quorum: Some(candidate_count / 2 + 1),
    }
}

fn event_replay_records(count: usize) -> Vec<EventRecord> {
    (0..count)
        .map(|index| {
            let alias = agent_alias(index % 16);
            let context_id =
                ContextId::new(format!("ctx-replay-{}", index % 8)).expect("context id");
            let task_id = TaskId::new(format!("task-replay-{:03}", index % 64)).expect("task id");
            let state = match index % 4 {
                0 => "TASK_STATE_SUBMITTED",
                1 => "TASK_STATE_WORKING",
                2 => "TASK_STATE_COMPLETED",
                _ => "TASK_STATE_FAILED",
            };
            EventRecord {
                sequence: (index + 1) as i64,
                event_id: EventId::new(format!("event-replay-{index:04}")).expect("event id"),
                timestamp: benchmark_timestamp(index),
                source: if index % 3 == 0 { "gateway" } else { "cli" }.to_owned(),
                event_type: if index % 5 == 0 {
                    "missive.context.updated"
                } else {
                    "a2a.task.updated"
                }
                .to_owned(),
                agent_alias: Some(alias),
                context_id: Some(context_id.clone()),
                task_id: Some(task_id.clone()),
                group_name: Some(GroupName::new("bench").expect("group")),
                gateway_job_id: None,
                adapter_binding_id: None,
                payload_json: json!({
                    "context": {
                        "name": format!("context-{}", index % 8),
                        "state": "open"
                    },
                    "task": {
                        "taskId": task_id.as_str(),
                        "contextId": context_id.as_str(),
                        "status": { "state": state }
                    }
                }),
                metadata: Metadata::new(),
                redacted: true,
            }
        })
        .collect()
}

fn stream_event_payloads(count: usize) -> Vec<String> {
    (0..count)
        .map(|index| {
            let task_id = format!("task-stream-{index:03}");
            let context_id = format!("ctx-stream-{}", index % 8);
            let response = match index % 3 {
                0 => json!({
                    "statusUpdate": {
                        "taskId": task_id,
                        "contextId": context_id,
                        "status": {
                            "state": "TASK_STATE_WORKING",
                            "message": {
                                "messageId": format!("msg-stream-status-{index:03}"),
                                "contextId": context_id,
                                "taskId": task_id,
                                "role": "ROLE_AGENT",
                                "parts": [{ "text": format!("working on item {index}"), "mediaType": "text/plain" }]
                            },
                            "timestamp": "2025-01-02T03:04:05Z"
                        }
                    }
                }),
                1 => json!({
                    "message": {
                        "messageId": format!("msg-stream-direct-{index:03}"),
                        "contextId": context_id,
                        "role": "ROLE_AGENT",
                        "parts": [{ "text": format!("streamed answer {index}"), "mediaType": "text/plain" }]
                    }
                }),
                _ => json!({
                    "artifactUpdate": {
                        "taskId": task_id,
                        "contextId": context_id,
                        "artifact": {
                            "artifactId": format!("artifact-stream-{index:03}"),
                            "name": format!("report-{index:03}.md"),
                            "parts": [{ "text": format!("# report {index}\n\nbody"), "mediaType": "text/markdown" }]
                        },
                        "append": false,
                        "lastChunk": true
                    }
                }),
            };

            if index % 2 == 0 {
                response.to_string()
            } else {
                json!({
                    "jsonrpc": "2.0",
                    "id": format!("stream-{index:03}"),
                    "result": response
                })
                .to_string()
            }
        })
        .collect()
}

fn raw_json_size_hint(value: &Value) -> usize {
    match value {
        Value::Object(object) => object.len(),
        Value::Array(items) => items.len(),
        Value::String(text) => text.len(),
        Value::Number(_) | Value::Bool(_) | Value::Null => 1,
    }
}

#[derive(Clone, Copy)]
enum CollectiveCommand {
    Gather,
    Reduce,
}

struct CollectiveFixture {
    _temp_dir: TempDir,
    env: BTreeMap<String, String>,
    current_dir: PathBuf,
    gather_cli: Cli,
    reduce_cli: Cli,
}

fn seed_collective_fixture() -> CollectiveFixture {
    let temp_dir = TempDir::new().expect("temporary collective fixture");
    let current_dir = temp_dir.path().join("work");
    std::fs::create_dir_all(&current_dir).expect("collective fixture current dir");

    let missive_home = temp_dir.path().join("missive-home");
    let home = temp_dir.path().join("home");
    let xdg_config_home = temp_dir.path().join("xdg-config");
    let xdg_config_dirs = temp_dir.path().join("xdg-config-dirs");
    for dir in [&missive_home, &home, &xdg_config_home, &xdg_config_dirs] {
        std::fs::create_dir_all(dir).expect("collective fixture directory");
    }

    let env = BTreeMap::from([
        (
            "MISSIVE_HOME".to_owned(),
            missive_home.display().to_string(),
        ),
        ("HOME".to_owned(), home.display().to_string()),
        (
            "XDG_CONFIG_HOME".to_owned(),
            xdg_config_home.display().to_string(),
        ),
        (
            "XDG_CONFIG_DIRS".to_owned(),
            xdg_config_dirs.display().to_string(),
        ),
    ]);
    seed_collective_store(&env);

    CollectiveFixture {
        _temp_dir: temp_dir,
        env,
        current_dir,
        gather_cli: Cli::parse_from([
            "missive",
            "--json",
            "gather",
            "bench",
            "--context",
            "ctx-collective-bench",
        ]),
        reduce_cli: Cli::parse_from([
            "missive",
            "--json",
            "reduce",
            "bench",
            "--context",
            "ctx-collective-bench",
            "--strategy",
            "summarise",
        ]),
    }
}

fn seed_collective_store(env: &BTreeMap<String, String>) {
    let config = MissiveConfig::default();
    let paths = StatePathResolver::new()
        .with_env(env.clone())
        .resolve_config(&config, "default")
        .expect("collective state paths");
    paths.ensure_directories().expect("collective state dirs");
    let store = Store::open(paths.database_path()).expect("collective store");

    let group_name = GroupName::new("bench").expect("group name");
    let context_id = ContextId::new("ctx-collective-bench").expect("context id");
    store
        .upsert_context(&ContextUpsert::new(context_id.clone()))
        .expect("collective context");

    let mut group = GroupUpsert::new(group_name.clone());
    group.routing_policy = "broadcast".to_owned();
    group.notes = Some("local benchmark collective group".to_owned());
    store.upsert_group(&group).expect("collective group");

    for index in 0..COLLECTIVE_MEMBER_COUNT {
        let alias = agent_alias(index);
        store
            .upsert_agent(&AgentUpsert::new(
                alias.clone(),
                format!("http://127.0.0.1:{}", 22_000 + index),
            ))
            .expect("collective agent");

        let mut member = GroupMemberUpsert::new(
            group_name.clone(),
            alias.clone(),
            RankName::new(format!("rank-{index:02}")).expect("rank"),
        );
        member.tags = vec!["bench".to_owned(), format!("tier{}", index % 4)];
        member.weight = ((index % 4) + 1) as f64;
        store
            .upsert_group_member(&member)
            .expect("collective member");

        let task_id = TaskId::new(format!("task-collective-{index:02}")).expect("task id");
        let message_id = MessageId::new(format!("msg-collective-{index:02}")).expect("message id");
        let mut task = TaskUpsert::new(task_id.clone(), alias.clone(), TaskState::Completed);
        task.context_id = Some(context_id.clone());
        task.last_message_id = Some(message_id.clone());
        task.completed_at = Some(benchmark_timestamp(index));
        task.remote_task_json = Some(task_json(&task_id, &context_id, "TASK_STATE_COMPLETED"));
        store.upsert_task(&task).expect("collective task");

        let mut message = MessageInsert::new(
            message_id,
            MessageDirection::Response,
            message_json(index, &task_id, &context_id),
        );
        message.agent_alias = Some(alias);
        message.context_id = Some(context_id.clone());
        message.task_id = Some(task_id);
        message.role = Some(MessageRole::Agent);
        store.insert_message(&message).expect("collective message");
    }
}

fn run_collective_command(fixture: CollectiveFixture, command: CollectiveCommand) -> usize {
    let cli = match command {
        CollectiveCommand::Gather => &fixture.gather_cli,
        CollectiveCommand::Reduce => &fixture.reduce_cli,
    };
    let mut stdout = Vec::with_capacity(16 * 1024);
    execute_with_environment(cli, &fixture.env, &fixture.current_dir, &mut stdout)
        .expect("collective command should execute");
    black_box(stdout.len())
}

fn agent_alias(index: usize) -> AgentAlias {
    AgentAlias::new(format!("agent-{index:02}")).expect("agent alias")
}

fn benchmark_timestamp(index: usize) -> MissiveTimestamp {
    MissiveTimestamp::from_unix_timestamp(1_735_787_045 + index as i64)
        .expect("benchmark timestamp")
}

fn task_json(task_id: &TaskId, context_id: &ContextId, state: &str) -> Value {
    json!({
        "taskId": task_id.as_str(),
        "contextId": context_id.as_str(),
        "status": {
            "state": state,
            "timestamp": "2025-01-02T03:04:05Z"
        }
    })
}

fn message_json(index: usize, task_id: &TaskId, context_id: &ContextId) -> Value {
    json!({
        "messageId": format!("protocol-message-{index:03}"),
        "contextId": context_id.as_str(),
        "taskId": task_id.as_str(),
        "role": "ROLE_AGENT",
        "parts": [
            {
                "text": format!("member {index} completed benchmark work"),
                "mediaType": "text/plain"
            }
        ],
        "metadata": {
            "fixture": "benchmark"
        }
    })
}

fn artifact_json(index: usize) -> Value {
    json!({
        "artifactId": format!("artifact-store-{index:03}"),
        "name": format!("artifact-{index:03}.md"),
        "parts": [
            {
                "text": format!("# artifact {index}\n\nbenchmark body"),
                "mediaType": "text/markdown"
            }
        ],
        "metadata": {
            "fixture": "benchmark"
        }
    })
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(20)
        .warm_up_time(Duration::from_millis(100))
        .measurement_time(Duration::from_millis(750));
    targets =
        bench_config_load,
        bench_store_operations,
        bench_routing,
        bench_event_replay,
        bench_streaming_event_parsing,
        bench_group_collectives
}
criterion_main!(benches);
