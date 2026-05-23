# Adapters

Adapters are the boundary between external/local message sources and missive's gateway control plane. They are not agent frameworks and they do not replace A2A; adapters translate source input into missive gateway events and receive redacted gateway updates that can be rendered back to the source.

The shared trait and registry are in place. The current concrete adapters are foreground `stdio`, foreground `file-drop`, and an opt-in daemon-mounted `http` inbound control endpoint. Discord, Slack, Telegram, Matrix, and Email are represented by feature-gated compileable stubs only; see the [external adapter roadmap](adapter-roadmap.md) for required secrets, permissions, platform behaviours, and design boundaries.

## Crate contract

`crates/missive-adapters` exposes:

* `Adapter` — common interface for lifecycle, inbound identity mapping, outbound updates, and acknowledgements.
* `AdapterEventSink` and `AdapterContext` — the gateway-provided event sink an adapter uses after start.
* `AdapterEvent` — runtime events emitted by adapters. Current event types are `missive.adapter.lifecycle`, `missive.adapter.inbound_message`, and `missive.adapter.acknowledgement`.
* `AdapterIdentity` and `AdapterExternalIdentity` — normalization boundary between platform-specific ids and missive source ids used by sessions and busy-input policy.
* `AdapterSession` — source/agent/context routing hints for gateway session continuity.
* `AdapterInboundMessage` and `AdapterInboundPayload` — text or JSON inbound input ready for later command/job mapping.
* `AdapterOutboundUpdate` — redacted gateway output that future adapter workers can deliver to their source.
* `AdapterRegistry` and `AdapterFactory` — deterministic factory lookup by adapter kind.
* `StdioAdapter`, `StdioInputFrame`, `StdioOutputFrame`, and frame helpers — the built-in stdin/stdout JSON/NDJSON adapter boundary.
* `FileDropAdapter`, `FileDropInputFile`, `FileDropOutputFile`, `FileDropPaths`, and handoff helpers — the built-in local directory adapter boundary.
* `HttpAdapter`, `HttpInputFrame`, and `HttpFrameSource` — the built-in local HTTP control-message schema and adapter-event mapping used by `missive gateway run --http-adapter`.
* `ExternalChatStubAdapter`, `ExternalChatPlatform`, and `register_external_chat_adapter_stubs` — feature-gated placeholders for the `discord`, `slack`, `telegram`, `matrix`, and `email` adapter kinds.

The registry is intentionally generic. The built-in factories can be registered with `register_stdio_adapter`, `register_file_drop_adapter`, and `register_http_adapter`. External chat/platform placeholder factories are registered with `register_external_chat_adapter_stubs` only when the relevant `missive-adapters` crate features are enabled; they do not connect to third-party services.

## Configuration schema

Adapter definitions live under `[adapters.<name>]` in `missive.config.v1`:

```toml
[adapters.stdio]
kind = "stdio"
enabled = true
session_profile = "default"

[adapters.stdio.busy_input]
mode = "steer"
unsupported_steer_fallback = "queue"
interrupt_remote_cancel = true
max_queue_depth = 16

[adapters.stdio.settings]
framing = "ndjson"

[adapters.local-drop]
kind = "file-drop"
enabled = true
session_profile = "default"

[adapters.local-drop.settings]
inbox = "/var/tmp/missive-drop/inbox"
outbox = "/var/tmp/missive-drop/outbox"
processed = "/var/tmp/missive-drop/processed"
error = "/var/tmp/missive-drop/error"
```

Core config validation checks adapter names and kinds, optional `session_profile` references, busy-input overrides, and metadata key shape. `missive-adapters` converts validated config entries into `AdapterDefinition` values and can filter disabled adapters before startup. External stub kinds use the same shape, but their settings must contain only non-secret values or secret-reference names such as `auth_ref`; live platform credential resolution is deferred to platform-specific future work.

`settings` is for non-secret adapter-specific values only. Credentials should use config auth refs, environment variables, keyrings, or future adapter-specific secret references rather than raw values in TOML.

## Lifecycle

The intended gateway lifecycle is:

1. Load and validate `MissiveConfig`.
2. Build `AdapterDefinition` values from `[adapters]`.
3. Register adapter factories in `AdapterRegistry`.
4. For each enabled definition, create one adapter instance by kind.
5. Start the adapter with `AdapterContext`, which includes its definition and an `AdapterEventSink`.
6. The adapter maps source identities with `map_identity` and emits `AdapterEvent::inbound_message` for new input.
7. Gateway/session/job workers decide whether to queue, interrupt, steer, send, stream, wait, or reduce.
8. Gateway workers call `deliver_update` for source-visible progress/results and `acknowledge` for accepted/rejected/delivered/failed delivery state.
9. On shutdown or configuration changes, the gateway calls `stop` and records lifecycle state.

The current daemon exposes the adapter event-bus bridge and reports adapter bus events as `gateway_adapter_event` in runtime output when an adapter worker emits them. It can mount the local HTTP inbound adapter when `missive gateway run --http-adapter` is passed. It does not start configured stdio/file-drop adapters from config yet; use `missive adapter stdio` or `missive adapter file-drop` as foreground local adapters today.

## stdin/stdout adapter

`missive adapter stdio` is designed for local subprocess automation. It reads
request frames from stdin and writes response frames to stdout. The frame schema
version is `missive.stdio.v1`.

Modes and framing:

* Single-shot mode reads one JSON frame and writes response frame(s):

  ```bash
  printf '%s\n' '{"schema_version":"missive.stdio.v1","id":"req-1","command":"task_list"}' \
    | missive adapter stdio --mode single-shot --framing json
  ```

* Long-running mode reads one NDJSON frame per line until EOF and writes NDJSON
  responses. Invalid frames produce error frames and the loop continues:

  ```bash
  {
    printf '%s\n' '{"schema_version":"missive.stdio.v1","id":"req-send","command":"send","agent":"echo","message":"hello"}'
    printf '%s\n' '{"schema_version":"missive.stdio.v1","id":"req-tasks","command":"task_list"}'
  } | missive adapter stdio --mode long-running
  ```

Supported `command` values are:

* `send` — fields match the local send command: `agent`, optional `message`,
  `text_parts`, `json_parts`, `files`, `file_bytes`, `mime`, `metadata`,
  `context`, `task`, and `accepted_output_modes`.
* `stream` — same fields as `send`, plus `force` for streaming capability
  interoperability testing.
* `task_get`, `task_list`, `task_wait`, and `task_cancel` — fields mirror the
  corresponding task subcommands (`task_id`, `agent`, `remote`, `local`,
  `state`, `source`, pagination, interval, and history options where relevant).

Example streaming frame:

```bash
printf '%s\n' \
  '{"schema_version":"missive.stdio.v1","id":"req-stream","command":"stream","agent":"echo","message":"hello over stdio"}' \
  | missive adapter stdio --mode long-running
```

For stream frames in long-running mode, each A2A stream update is wrapped as a
stdio response frame with the original request `id`; the final wrapped event is
`stream_result`. This makes the adapter suitable for another agent supervising
`missive` as a child process.

Response frames contain:

```json
{
  "schema_version": "missive.stdio.v1",
  "id": "req-1",
  "ok": true,
  "kind": "stdio_command_output",
  "sequence": 0,
  "data": {"schema_version": "missive.output.v1", "kind": "task_list"}
}
```

Error frames use `ok: false`, `kind: "stdio_error"`, and the same stable
`missive::...` error report shape used by `--json`/`--ndjson` CLI errors.

## File-drop adapter

`missive adapter file-drop` is designed for local filesystem automation when a
source agent cannot or should not keep a subprocess pipe open and no network
inbound service is desired. It polls an inbox directory for complete request
files and writes one JSON result file per request to an outbox.

Example one-shot run:

```bash
mkdir -p /tmp/missive-drop/{inbox,outbox}
cat >/tmp/missive-drop/inbox/req-1.tmp <<'JSON'
{
  "schema_version": "missive.file_drop.v1",
  "id": "req-1",
  "command": "task_list"
}
JSON
mv /tmp/missive-drop/inbox/req-1.tmp /tmp/missive-drop/inbox/req-1.json
MISSIVE_HOME=/tmp/missive-demo \
  missive adapter file-drop \
  --inbox /tmp/missive-drop/inbox \
  --outbox /tmp/missive-drop/outbox \
  --mode once --json
cat /tmp/missive-drop/outbox/req-1.result.json
```

Producer contract and atomic handoff:

* write request content to a temporary name such as `req-1.tmp`, `req-1.part`, or
  a dotfile;
* atomically rename the complete file to a non-hidden `*.json` name;
* the adapter only claims ready `*.json` files and ignores temporary/partial
  names;
* on claim, the adapter atomically renames the input to a hidden processing name
  under the processed directory;
* successful inputs are archived as `<processed>/<original-name>`;
* malformed or unhandled inputs are archived as `<error-dir>/<original-name>`;
* results are written through a temporary outbox file and atomically renamed to
  `<stem>.result.json` or `<stem>.error.json`.

Default archive directories are `<inbox>/processed` and `<inbox>/error`; override
with `--processed` and `--error-dir`. Use `--mode watch` to keep polling,
`--poll-interval` to adjust the polling cadence, `--max-files` to bound a run,
and global `--timeout` to stop a watch run after a duration.

Request files use schema version `missive.file_drop.v1` and reuse the same
foreground command field names as the stdio adapter for `send`, `stream`,
`task_get`, `task_list`, `task_wait`, and `task_cancel`. They also accept
background-job file commands for the already implemented `missive job` surface:
`job_start_send`, `job_start_stream`, `job_start_wait`, `job_start_reduce`,
`job_list`, `job_show`, and `job_cancel`.

Send request example:

```json
{
  "schema_version": "missive.file_drop.v1",
  "id": "send-1",
  "source": {"source_id": "local-agent", "resume_name": "default"},
  "command": "send",
  "agent": "echo",
  "message": "hello from a file-drop request"
}
```

Background wait job request example:

```json
{
  "schema_version": "missive.file_drop.v1",
  "id": "wait-job-1",
  "command": "job_start_wait",
  "task_id": "task-123",
  "agent": "echo",
  "local": true,
  "options": {"max_attempts": 1, "cancel_remote_on_cancel": true}
}
```

Result files contain the file-drop schema marker, request id, `ok`, output kind,
input/archive filenames, and wrapped `missive.stdio.v1` output frames. The
wrapped frame `data` is the normal `missive.output.v1` command envelope. Parse or
validation failures use `ok:false`, `kind:"file_drop_error"`, and the same
`missive::...` error report shape used elsewhere.

## HTTP inbound adapter

`missive gateway run --http-adapter` mounts a local HTTP control endpoint on the same gateway listener. The default endpoint is `POST /adapter/http/v1/messages`, and the default adapter health endpoint is `GET /adapter/http/healthz`.

Use an environment-backed token for local automation:

```bash
MISSIVE_HTTP_ADAPTER_TOKEN=change-me \
MISSIVE_HOME=/tmp/missive-demo \
  missive gateway run \
  --port 7347 \
  --http-adapter \
  --http-adapter-auth-token-env MISSIVE_HTTP_ADAPTER_TOKEN \
  --ndjson
```

Then post one `missive.http.v1` JSON control frame:

```bash
curl -sS \
  -H 'Authorization: Bearer change-me' \
  -H 'Content-Type: application/json' \
  -d '{"schema_version":"missive.http.v1","id":"req-1","command":"task_list"}' \
  http://127.0.0.1:7347/adapter/http/v1/messages
```

Request frames support the same foreground command names and fields as the stdio adapter: `send`, `stream`, `task_get`, `task_list`, `task_wait`, and `task_cancel`. Accepted requests are validated, converted into `AdapterEvent::inbound_message`, forwarded to the gateway event bus, and appended to the event journal as `missive.adapter.http.accepted` with a redacted payload. Rejected requests are appended as `missive.adapter.http.rejected` when possible.

HTTP adapter options on `gateway run` include:

* `--http-adapter-path PATH`
* `--http-adapter-health-path PATH`
* `--http-adapter-auth-token-env ENV`
* `--http-adapter-auth-header HEADER`
* `--http-adapter-auth-scheme SCHEME|none`
* `--http-adapter-max-body-bytes BYTES`
* `--http-adapter-rate-limit N`

`GET /adapter/http/healthz` reports accepted/rejected counters, body/rate limits, and a redacted auth view. The current HTTP adapter is an ingress/event-bus boundary; command dispatch, gateway session rotation, and busy-input execution for inbound HTTP frames remain later adapter-worker work.

## External chat adapter stubs

`missive-adapters` defines placeholder adapter kinds for Discord, Slack, Telegram, Matrix, and Email. They are disabled by default at the Cargo feature level and can be compiled for registry/identity tests with:

```bash
cargo test -p missive-adapters --features external-chat-stubs
```

Individual feature flags are `adapter-discord`, `adapter-slack`, `adapter-telegram`, `adapter-matrix`, and `adapter-email`; `external-chat-stubs` enables all five. The stubs expose static roadmap metadata and deterministic identity mapping, but `start`, live outbound delivery, and on-platform acknowledgements return configuration errors. This prevents a configured placeholder from being mistaken for a working chat integration.

Do not put raw platform tokens, signing secrets, mailbox passwords, OAuth refresh values, private keys, private workspace names, or private email addresses in repository files. Use auth-ref names in config and keep actual values in environment variables, keyrings, or future platform-specific secret stores.

See [`docs/adapter-roadmap.md`](adapter-roadmap.md) for required secrets, permissions/scopes, platform behaviours, Hermes-inspired boundaries, and the checklist future live adapter work must satisfy.

## Gateway event bus

Adapters do not depend on `missive-gateway`. They depend only on the `AdapterEventSink` trait. The gateway wraps its internal event bus with a sink implementation, so adapter events can be forwarded without creating a crate cycle.

An inbound adapter message carries:

* configured adapter name;
* stable message id;
* normalized source identity;
* session routing hint with resume name, optional target agent, and optional context id;
* text or JSON payload;
* non-secret metadata.

Gateway event output uses redacted serialized adapter events. Source ids and channel/user identifiers can still be operationally sensitive and should be treated like local runtime state.

## Current limitations

`missive adapter stdio` and `missive adapter file-drop` are foreground local adapters, not daemon-started workers. `missive gateway run --http-adapter` accepts and journals HTTP control frames but does not yet execute them through session/job workers or apply busy-input queue/interrupt/steer actions. `missive gateway run` still does not start configured adapters from `[adapters]`. External chat adapter kinds are compileable stubs only: even when feature flags register factories, they do not connect to Discord, Slack, Telegram, Matrix, or Email, and live implementations remain future work. The file-drop adapter uses portable polling rather than OS-specific inotify/FSEvents, does not lock files that are written directly to a final `*.json` name, and relies on producers following the temporary-file-then-rename handoff contract. Busy-input policy is available to future adapter workers but is not invoked automatically by the current stdio, file-drop, HTTP, or external stub ingress paths.
