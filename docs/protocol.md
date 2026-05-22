# Protocol mapping

`missive` treats A2A as the canonical protocol layer. The current implemented
protocol behavior covers public Agent Card discovery for registered agents,
official Rust protocol type integration through `missive-a2a`, local interface
negotiation, non-streaming `SendMessage` calls for `missive send`, SSE
`SendStreamingMessage` calls for `missive stream`, remote task
`GetTask`/`ListTasks`/`CancelTask`, and local A2A `contextId` continuity
management through `missive context`.

## Public Agent Card discovery

`missive agent inspect <alias>` and `missive agent refresh <alias>` resolve the
registered agent `base_url` to:

```text
/.well-known/agent-card.json
```

The fetched JSON is parsed as an A2A Agent Card and cached on the agent registry
row with:

* raw public Agent Card JSON
* fetch timestamp
* HTTP `ETag`, when returned
* HTTP `Last-Modified`, when returned

`inspect` uses the cached card when present. `inspect --refresh` and `refresh`
perform a remote request. If cache validators are available, missive sends
`If-None-Match` and/or `If-Modified-Since`; `304 Not Modified` keeps the cached
card and updates the cache validation timestamp.

The current parser deserializes into the official `a2a-lf` `AgentCard` type and
extracts the fields needed for human and machine inspection: provider, agent
version, protocol versions, capabilities, supported interfaces, default
input/output modes, and skills. A small compatibility layer normalizes known
snake_case fixture aliases and inserts an empty `supportedInterfaces` array for
older/pre-release cards that omit the field, preserving the negotiation fallback
introduced earlier. The raw public card JSON remains cached and rendered for
inspection.

## A2A service parameters

`missive-a2a` now centralizes A2A service-parameter handling in a
`ServiceParameters` value. HTTP-based requests apply these parameters as headers:

* `A2A-Version` is sent on every implemented outbound A2A request. The default
  is the official SDK `VERSION` (`1.0`).
* `A2A-Extensions` is sent when the selected config or CLI requests extensions;
  repeated values are rendered as a comma-separated header value.
* Additional non-auth service parameters are accepted as validated HTTP header
  names and values.

The effective protocol defaults come from `[protocol]` or
`[profiles.<name>.protocol]` in `missive.config.v1`. For a single invocation,
`--protocol-version <VERSION>` overrides the configured version,
`--a2a-extension <EXTENSION>` appends an extension, and the
`--service-param NAME=VALUE` flag adds or overrides an extra service parameter.
Current implemented uses are Agent Card discovery/refresh, non-streaming
`missive send`, streaming `missive stream`, and remote `missive task`
get/list/wait/cancel calls; future push clients should reuse the same helper so
request metadata remains consistent.

When an HTTP response body reports the official A2A
`VERSION_NOT_SUPPORTED` error code/reason, missive maps it to a protocol error
with deterministic protocol exit code `76` instead of a generic transport
failure.

`ServiceParameters::to_metadata()` records the version under
`a2a.protocol_version`, requested extensions under `a2a.extensions`, and extra
parameters under `a2a.service_parameters`; task/event persistence code should
copy that metadata when recording outbound request effects.

## Authentication headers

`missive-a2a` exposes an `AuthHeaders` request helper for resolved HTTP auth
headers. Raw values are not serializable, `Debug` renders redacted values, and
reqwest header values are marked sensitive before sending. The CLI currently
builds `AuthHeaders` from agent config auth refs, `--bearer-token-env`, and
repeatable `--header Name:Value` inputs for Agent Card fetch/refresh requests.

Auth resolution is deliberately outside protocol type parsing: config and CLI
code locate secrets in environment variables or platform keyrings, while the A2A
client only validates and applies already-resolved headers. `missive send`,
`missive stream`, and remote `missive task` operations use the same auth headers
for the optional Agent Card fetch and the A2A request.

## Interface negotiation

`missive agent inspect <alias>` now computes a selected A2A interface from the
parsed Agent Card and the agent registry preference. The default local preference
order is:

```text
http+json, json-rpc
```

Agent Card binding names are canonicalized for comparison, so `HTTP+JSON`,
`JSONRPC`, and lowercase config values all map to stable missive binding names.
`gRPC` is recognized as a future extension point but is not locally supported
yet. If a remote card only advertises unsupported bindings, negotiation fails as
a transport error and includes the local support list in the diagnostic.

The selected interface appears in `agent inspect --json` under
`data.selected_interface` with the canonical `binding`, original
`protocol_binding`, selected `url`, optional `tenant`, `protocol_version`, and a
`source` value.

Advanced users and tests can pass `missive agent inspect <alias> --binding
<BINDING>` to require a specific local binding such as `json-rpc`. The override
must still be locally supported and advertised by the Agent Card unless the card
omits `supportedInterfaces` entirely.

For compatibility with older/pre-release Agent Cards that omit
`supportedInterfaces`, missive falls back to explicit registry/config interface
URLs by binding. If no explicit URL is available and the chosen binding is
`http+json`, it uses the registered agent `base_url` as a legacy HTTP+JSON
fallback and reports `protocol_version = "unknown"` with
`source = "base_url_fallback"`.

## Non-streaming SendMessage

`missive send` builds an official `a2a-lf` `SendMessageRequest` containing a
`ROLE_USER` `Message`, rich A2A parts, optional `contextId`/`taskId`, optional
`configuration.acceptedOutputModes`, and non-secret request metadata. Supported
outbound parts are official text parts, `url` file-reference parts for
canonicalized local `file://` URLs, `raw` byte parts for `--file-bytes`, and
structured `data` parts for `--json-part`. MIME values are stored on official
`mediaType` fields and JSON data parts default to `application/json`. The
outbound message id is generated by the official A2A type helper and is also the
local request-message row id.

Transport mapping:

* `http+json` appends `message:send` to the selected interface URL and sends
  `POST <interface>/message:send` with `Content-Type: application/a2a+json`.
* `json-rpc` posts a JSON-RPC 2.0 request with method `SendMessage` to the
  selected JSON-RPC interface URL.

Both transports send `A2A-Version` and any configured extensions/service
parameters, apply resolved auth headers, enforce the selected profile's
`qos.max_request_bytes` on local inputs and the serialized request, and parse the
official `SendMessageResponse` oneof. Responses may be either a direct `Message`
or a `Task`. Direct messages are stored as response message rows. Task responses
are stored in `tasks` with the raw remote task JSON, mapped task state, protocol
version metadata, and a linked response message row using the task status message
when one is present or a local synthetic response row when the task has no status
message.

## Streaming SendStreamingMessage

`missive stream` builds the same official `a2a-lf` `SendMessageRequest` shape and
rich text/file/byte/JSON part set as `missive send`, then validates that the
fetched or cached Agent Card advertises `capabilities.streaming = true`. Passing
`--force` bypasses that local capability check for interoperability testing;
without `--force`, missive fails before opening a stream.

Transport mapping:

* `http+json` appends `message:stream` to the selected interface URL and sends
  `POST <interface>/message:stream` with `Content-Type: application/a2a+json`
  and `Accept: text/event-stream`.
* `json-rpc` posts a JSON-RPC 2.0 request with method `SendStreamingMessage` to
  the selected JSON-RPC interface URL and expects an SSE response.

Each SSE record's `data` field is parsed as either a direct A2A `StreamResponse`
object or a JSON-RPC response whose `result` is a `StreamResponse`. Supported
stream payloads are the official SDK `task`, `message`, `statusUpdate`, and
`artifactUpdate` variants. JSON-RPC stream errors become protocol errors, and
malformed JSON or unknown stream variants include the event sequence number in
the diagnostic.

The CLI processes stream events incrementally. Human output writes one line per
parsed event, NDJSON writes one `stream_event` envelope per event followed by a
`stream_result` envelope, and JSON mode writes one final `stream_result` document
after the stream closes. Every parsed event is appended to the local event journal
with event types `a2a.stream.task`, `a2a.stream.message`,
`a2a.stream.status_update`, or `a2a.stream.artifact_update`; a corresponding
`messages` row with direction `stream_event` is also written. Task and status
events update the local `tasks` row state and preserve A2A protocol-version
metadata. Artifacts embedded in task events are persisted as dedicated artifact
rows. `artifactUpdate` events upsert the referenced artifact row, and appended
chunks merge their parts into the existing A2A artifact JSON while incrementing a
local version. Task subscription/resume and push configuration remain mapped for
later tickets.

## Task GetTask/ListTasks/CancelTask

`missive task get --remote`, `missive task list --remote`, `missive task wait`,
and `missive task cancel` use the official `a2a-lf` task request/response types
behind `missive_a2a::protocol`: `GetTaskRequest`, `ListTasksRequest`,
`ListTasksResponse`, `CancelTaskRequest`, and `Task`.

Transport mapping follows the negotiated interface:

* `http+json` maps `GetTask` to `GET <interface>/tasks/{id}`, `ListTasks` to
  `GET <interface>/tasks`, and `CancelTask` to
  `POST <interface>/tasks/{id}:cancel`. Task ids are percent-encoded as path
  segments. List filters become query parameters such as `contextId`, `status`,
  `statusTimestampAfter`, `pageSize`, `pageToken`, `historyLength`, and
  `includeArtifacts`.
* `json-rpc` posts JSON-RPC 2.0 methods `GetTask`, `ListTasks`, and
  `CancelTask` to the selected JSON-RPC interface URL.

Both transports send `A2A-Version`, optional `A2A-Extensions`, configured extra
service parameters, and resolved auth headers. A task returned by any of these
operations is persisted to the local `tasks` table with raw remote task JSON,
context id, mapped state, protocol-version metadata, and status-message id when
present. Any artifacts embedded in the returned task are also persisted to the
local `artifacts` table with kind, name, MIME type, metadata, raw A2A artifact
JSON, and an incremented version when the same artifact id is observed again.
`ListTasks` persists each returned task before applying local output filters.

`task wait` repeatedly calls `GetTask` unless `--local` is supplied. The wait
loop treats `completed`, `failed`, `cancelled`, and `input_required` as decisive
states and returns deterministic process exit codes documented in `docs/cli.md`.
Timeout is controlled by global `--timeout`; polling cadence is controlled by
`--interval`.

## Context continuity

A2A contexts are represented by opaque `contextId` values. `missive send` and
`missive stream` place `--context CONTEXT_ID` on the outbound official A2A
`Message` and persist the same id on request/response/stream message rows. A2A
`Task` payloads returned by send, stream, or task operations also persist their
`contextId` on the local `tasks` table and ensure a matching local context row
exists.

`missive context` is a local control-plane layer around those ids. It can create
a local context id before the first send, assign a human-friendly name, list/show
context records, fork a child context by recording `parent_context_id` and parent
metadata, mark a context closed locally, and export the context with linked
messages, tasks, and events. These commands do not introduce a proprietary wire
protocol and do not call a remote A2A "close context" endpoint; they preserve and
organize the canonical A2A ids used by implemented message and task calls.

Context export recursively redacts secret-like keys and HTTP authorization
headers before printing. Dedicated A2A task resubscription, push/webhook updates,
and event replay remain for later tickets. Artifact rows can be inspected and
exported separately with `missive task artifact list/show/save/export`.

## Error mapping

* HTTP status failures such as `404 Not Found` map to `missive::transport`.
* Network/TLS/HTTP client failures map to `missive::transport`.
* Unsupported or non-mutual interface bindings map to `missive::transport`.
* A2A `VERSION_NOT_SUPPORTED` responses map to `missive::protocol` with exit
  code `76`.
* Invalid JSON or schema-incompatible Agent Card payloads map to
  `missive::protocol`.

## Official Rust type boundary

`crates/missive-a2a` depends on the official `a2a-lf` crate from
`a2aproject/a2a-rs` and re-exports protocol models from
`missive_a2a::protocol`. Downstream missive crates should use that module rather
than importing `a2a-lf` directly. Current fixture coverage round-trips these
A2A v1.0 shapes through official serde types:

* `AgentCard`, including a richer multi-interface Agent Card with extensions,
  security schemes, and signatures
* `Message`, including text/data and file-byte examples
* `Artifact`, including extension metadata
* `Task`, including completed, input-required, and file-artifact task examples
* `SendMessageRequest` and `SendMessageResponse`
* `StreamResponse` task/message/status/artifact variants, including the push
  webhook payload shape
* `GetTaskRequest`, `ListTasksRequest`/`ListTasksResponse`,
  `CancelTaskRequest`, and `SubscribeToTaskRequest`
* `TaskPushNotificationConfig` plus get/list/delete push config request and list
  response shapes
* JSON-RPC request/response/error envelopes with embedded official payloads
* HTTP binding error values that preserve A2A `google.rpc.ErrorInfo` details

Fixtures live under `tests/fixtures/a2a/1.0/`, and normalized CLI golden outputs
for fixture-backed `agent inspect --json` and `send --json` live under
`tests/fixtures/a2a/1.0/cli/`. The directory name is the protocol major/minor
version used by those wire examples. When updating the upstream SDK or when A2A
wire shapes change, follow the update process in
`tests/fixtures/a2a/1.0/README.md`, then rerun at least:

```bash
cargo test -p missive-a2a --test protocol_fixtures --all-features
cargo test -p missive-cli --test a2a_conformance_fixtures --all-features
```

Authentication material is resolved for implemented Agent Card fetch/refresh,
non-streaming send, streaming send, and task get/list/wait/cancel requests.
Future push protocol calls should reuse `AuthHeaders` plus the CLI/config auth
resolver instead of inventing separate secret handling paths.
