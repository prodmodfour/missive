# Testing

`missive` tests should be runnable from a clean checkout without contacting third-party agents by default. The ordered build loop still uses `scripts/quality-gate.sh` as the required validation entry point for every ticket.

## Local mock A2A server fixtures

Reusable A2A integration fixtures live in the dev-support crate:

```text
crates/missive-test-support
```

The main fixture is `missive_test_support::MockA2aServer`. It starts a local `127.0.0.1` HTTP server on an ephemeral port and serves:

* public Agent Card discovery at `/.well-known/agent-card.json`
* HTTP+JSON A2A endpoints under `/a2a`
* JSON-RPC A2A endpoints at `/rpc`
* SendMessage direct-message or task responses
* SendStreamingMessage SSE streams, including JSON-RPC-wrapped stream events
* GetTask, ListTasks, and CancelTask task routes with controllable task-state queues
* push notification config create/get/list/delete fixture endpoints
* optional auth-header requirements
* optional `VERSION_NOT_SUPPORTED` responses for unsupported `A2A-Version` values
* optional malformed Agent Card, send, task, stream, and JSON-RPC responses
* optional SendMessage response delay for timeout tests

Example integration test setup:

```rust
use missive_test_support::{MockA2aServer, send_message_response_message};

let server = MockA2aServer::start();
server.handle().set_send_response(send_message_response_message(
    "msg-fixture-response",
    "ctx-fixture-response",
    "fixture response",
));

let base_url = server.base_url();
let http_interface = server.http_json_interface_url();
let rpc_interface = server.json_rpc_interface_url();
let requests = server.requests();
```

For task polling tests, enqueue deterministic state transitions before invoking the client:

```rust
server.handle().enqueue_task_states(
    "task-1",
    "ctx-1",
    ["TASK_STATE_WORKING", "TASK_STATE_COMPLETED"],
);
```

For streaming tests, replace the SSE event list:

```rust
use missive_test_support::{artifact_update_event, status_update_event};

server.handle().set_stream_events(vec![
    status_update_event("task-1", "ctx-1", "TASK_STATE_WORKING", Some("thinking")),
    artifact_update_event("task-1", "ctx-1", "artifact-1", "answer", true, true),
    status_update_event("task-1", "ctx-1", "TASK_STATE_COMPLETED", None),
]);
```

For push config tests, `MockA2aServer` serves both HTTP+JSON
`/a2a/tasks/{taskId}/pushNotificationConfigs` routes and JSON-RPC
`Create/Get/List/DeleteTaskPushNotificationConfig` methods. Use
`server.handle().insert_push_config(...)` or run the CLI `push create` command to
seed fixture state.

For auth and protocol-version error paths, configure the builder:

```rust
let server = MockA2aServer::builder()
    .require_auth_header("Authorization", "Bearer fixture-value")
    .supported_protocol_versions(["2.0"])
    .start();
```

The fixture records request method, path, lowercase headers, and UTF-8 body for assertions. It supports task resubscription through `POST`/`GET /a2a/tasks/{id}:subscribe` and JSON-RPC `SubscribeToTask`, returning deterministic SSE events from the same stream-event queue used by `message:stream`. It is intentionally deterministic and local-only; tests must not use it as a proxy to external services.

## A2A conformance fixture suite

Protocol-versioned conformance examples live under:

```text
tests/fixtures/a2a/1.0/
```

The directory name tracks the A2A major/minor protocol version. The suite covers
Agent Cards, messages, tasks, artifacts, streaming events, push notification
configs, JSON-RPC envelopes, HTTP error bodies, and normalized CLI golden outputs
under `tests/fixtures/a2a/1.0/cli/`.

Targeted conformance checks are:

```bash
cargo test -p missive-a2a --test protocol_fixtures --all-features
cargo test -p missive-cli --test a2a_conformance_fixtures --all-features
```

To intentionally refresh CLI golden outputs after a public output contract change,
run:

```bash
MISSIVE_UPDATE_GOLDENS=1 cargo test -p missive-cli --test a2a_conformance_fixtures --all-features
```

Only commit the normalized golden JSON, not local ports, generated message IDs,
timestamps, runtime databases, or captured external traffic. The fixture README
contains the update process for future A2A protocol versions.

## Running the fixture tests

Targeted checks for the reusable mock server and CLI integration are:

```bash
cargo test -p missive-test-support --all-targets
cargo test -p missive-cli --test mock_a2a_server_fixture --all-features
cargo test -p missive-cli --test push_command --all-features
cargo test -p missive-cli --test group_command --all-features
cargo test -p missive-cli --test bcast_command --all-features
cargo test -p missive-cli --test barrier_command --all-features
cargo test -p missive-cli --test gather_command --all-features
cargo test -p missive-cli --test reduce_command --all-features
cargo test -p missive-cli --test job_command --all-features
cargo test -p missive-cli --test gateway_command --all-features
cargo test -p missive-cli --test webhook_command --all-features
```

The default quality gate also runs these tests and the conformance suite through
the workspace test pass:

```bash
scripts/quality-gate.sh
```

## Extending fixtures

The group command integration test uses an isolated `MISSIVE_HOME` and local
agent registry rows to cover group create/list/show, member add/remove, duplicate
rank handling, rename membership preservation, delete cascades, missing reference
validation, and human output. The broadcast collective integration test uses
isolated state plus reusable mock A2A servers to cover concurrent success,
partial failure, timeout through delayed SendMessage responses, per-member
task/message persistence, request shape, and `missive.bcast.*` event rows. The barrier collective integration test covers consuming `bcast_result` JSON from stdin, remote GetTask polling to terminal completion, local terminal failure/cancellation exit codes, quorum with `--failure-policy continue`, requested non-terminal states, timeout handling, task persistence, and `missive.barrier.*` event rows. The gather collective integration test covers
rank-ordered local output collection, missing task representation, JSON/NDJSON
output, safe artifact export without accidental overwrite, and
`missive.gather.*` event rows. The reduce collective integration test covers
local deterministic reduction with provenance, mocked reducer-agent prompting via
A2A SendMessage, persisted local reduced-output messages, `missive.reduce.*`
event rows, and the no-gathered-input validation failure. The route explain
integration test uses isolated local registry/group rows to cover weighted,
tag-match, capability-match, and round-robin dry-run explanations, human/JSON
output, candidate-source validation, and invalid routing policy config failures.
The capability-selection integration test uses reusable mock A2A servers to cover
`agent capabilities` cache fetch/reuse, `group capabilities` aggregation,
`route explain --refresh-capabilities` Agent Card refresh, matching by skill
label/input mode/output mode/streaming/push support, missing cached capability
data diagnostics, and weight tie-breaking; `crates/missive-router` unit tests
cover every built-in policy's deterministic decision path plus capability-mode
matching. The background job integration test covers `job start/list/show/cancel`
JSON output, raw-request omission from job views, gateway execution of a queued
send job through the reusable mock A2A server, persisted `gateway_jobs` result
state, `missive.gateway.job.*` events, and `job cancel --remote` issuing A2A
`CancelTask`. The gateway command integration test spawns the
`missive` binary, waits for the local `/healthz` endpoint, checks `/status`
component JSON over loopback HTTP, verifies graceful `--timeout` shutdown,
checks NDJSON lifecycle output, and inspects persisted `missive.gateway.*` event
journal rows. It also covers service management dry runs: generated Linux
systemd unit content, planned `systemctl` commands, refusal to embed
secret-looking environment variables, and the safety requirement that `--system`
installs provide an explicit `MISSIVE_HOME`. The `missive-gateway` crate tests
also exercise service file generation for systemd and launchd, subscription
resume by seeding an in-flight task plus a persisted `task_subscription` job,
serving local mock `SubscribeToTask` SSE updates, verifying terminal cleanup,
checking bounded retry/backoff metadata for malformed streams, and background
job helpers for public job-kind recognition plus restart pickup of expired
running jobs. Gateway session unit tests additionally use fixed clocks to cover
daily, idle, and combined reset policy boundaries without wall-clock sleeps.
Gateway busy-input unit tests cover queue, interrupt, steer, unsupported-steer
fallback to queue/interrupt, queue-depth limits, and the no-active-operation
start path without needing live daemon adapters. `crates/missive-adapters` unit
tests cover deriving adapter definitions from config, duplicate/missing registry
factories, disabled adapter handling, cross-adapter event rejection, the built-in
stdio frame parser/writer, stdio invalid-frame diagnostics, stdio streaming
output frame sequencing, and fake/stdio adapters that map identity, emit
lifecycle/inbound-message events, accept outbound updates, and record
acknowledgements. `crates/missive-cli` also has `adapter_stdio_command`
integration coverage for valid long-running task frames, invalid frame recovery,
and wrapped streaming NDJSON output through the reusable local A2A mock server.
`crates/missive-gateway` also has a fake adapter event-sink test proving adapter inbound messages can enter
the daemon event bus and update runtime output. The webhook
command integration test similarly spawns the binary, waits for local
`/healthz`, posts unauthorized, malformed, and valid A2A `StreamResponse`
payloads over loopback HTTP, verifies graceful `--max-events` shutdown, checks
NDJSON output, and inspects the persisted event journal. Neither test contacts
external tunnel providers or third-party services.

When later tickets add adapters, additional collectives, or compatibility suites, prefer extending `crates/missive-test-support` instead of adding another one-off TCP mock inside a test file. Keep fixture changes focused on protocol surfaces needed by the ticket:

1. add a helper or route to `MockA2aServer`/`MockA2aHandle`
2. cover it with a local test in `crates/missive-test-support`
3. add a CLI or crate integration test that consumes the helper
4. document any new endpoint shape or limitation here

Do not commit runtime databases, logs, sockets, pid files, captured external traffic, real credentials, or private service URLs as fixture data.
