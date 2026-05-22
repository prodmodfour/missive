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

For auth and protocol-version error paths, configure the builder:

```rust
let server = MockA2aServer::builder()
    .require_auth_header("Authorization", "Bearer fixture-value")
    .supported_protocol_versions(["2.0"])
    .start();
```

The fixture records request method, path, lowercase headers, and UTF-8 body for assertions. It is intentionally deterministic and local-only; tests must not use it as a proxy to external services.

## Running the fixture tests

Targeted checks for the fixture and its CLI integration are:

```bash
cargo test -p missive-test-support --all-targets
cargo test -p missive-cli --test mock_a2a_server_fixture --all-features
```

The default quality gate also runs these tests through the workspace test pass:

```bash
scripts/quality-gate.sh
```

## Extending fixtures

When later tickets add push clients, gateway subscriptions, adapters, collectives, or compatibility suites, prefer extending `crates/missive-test-support` instead of adding another one-off TCP mock inside a test file. Keep fixture changes focused on protocol surfaces needed by the ticket:

1. add a helper or route to `MockA2aServer`/`MockA2aHandle`
2. cover it with a local test in `crates/missive-test-support`
3. add a CLI or crate integration test that consumes the helper
4. document any new endpoint shape or limitation here

Do not commit runtime databases, logs, sockets, pid files, captured external traffic, real credentials, or private service URLs as fixture data.
