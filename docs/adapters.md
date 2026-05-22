# Adapters

Adapters are the boundary between external/local message sources and missive's gateway control plane. They are not agent frameworks and they do not replace A2A; adapters translate source input into missive gateway events and receive redacted gateway updates that can be rendered back to the source.

The shared trait and registry are in place, and ticket 044 adds the first concrete adapter: `stdio`. File-drop, HTTP, and external chat adapters remain later tickets.

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

The registry is intentionally generic. The built-in `stdio` factory can be registered with `register_stdio_adapter`; later tickets should add factories such as `file` or `http`. External chat/platform adapters should stay feature-gated or stubbed until their own ticket.

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

The current daemon exposes the adapter event-bus bridge and reports adapter bus events as `gateway_adapter_event` in runtime output when an adapter worker emits them. It does not start configured adapters yet; use `missive adapter stdio` as a foreground subprocess adapter today.

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

`missive adapter stdio` is a foreground subprocess adapter, not a daemon-started worker. `missive gateway run` still does not start configured adapters from `[adapters]`, and the file-drop, HTTP, and external chat adapters remain later tickets. Busy-input policy is available to future adapter workers but is not invoked automatically by the current stdio foreground loop.
