# Adapters

Adapters are the boundary between external/local message sources and missive's gateway control plane. They are not agent frameworks and they do not replace A2A; adapters translate source input into missive gateway events and receive redacted gateway updates that can be rendered back to the source.

The shared trait and registry are in place, and the current concrete adapters are `stdio` and `file-drop`. HTTP and external chat adapters remain later tickets.

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

The registry is intentionally generic. The built-in `stdio` and `file-drop` factories can be registered with `register_stdio_adapter` and `register_file_drop_adapter`; later tickets should add factories such as `http`. External chat/platform adapters should stay feature-gated or stubbed until their own ticket.

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

Core config validation checks adapter names and kinds, optional `session_profile` references, busy-input overrides, and metadata key shape. `missive-adapters` converts validated config entries into `AdapterDefinition` values and can filter disabled adapters before startup.

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

The current daemon exposes the adapter event-bus bridge and reports adapter bus events as `gateway_adapter_event` in runtime output when an adapter worker emits them. It does not start configured adapters yet; use `missive adapter stdio` or `missive adapter file-drop` as foreground local adapters today.

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

`missive adapter stdio` and `missive adapter file-drop` are foreground local adapters, not daemon-started workers. `missive gateway run` still does not start configured adapters from `[adapters]`, and HTTP plus external chat adapters remain later tickets. The file-drop adapter uses portable polling rather than OS-specific inotify/FSEvents, does not lock files that are written directly to a final `*.json` name, and relies on producers following the temporary-file-then-rename handoff contract. Busy-input policy is available to future adapter workers but is not invoked automatically by the current stdio or file-drop foreground loops.
