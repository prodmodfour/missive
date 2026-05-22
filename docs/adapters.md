# Adapters

Adapters are the boundary between external/local message sources and missive's gateway control plane. They are not agent frameworks and they do not replace A2A; adapters translate source input into missive gateway events and receive redacted gateway updates that can be rendered back to the source.

Ticket 043 defines the shared trait and registry only. The stdio, file-drop, HTTP, and external chat adapters remain later tickets.

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

The registry is intentionally generic. Built-in adapters added by later tickets should register factories such as `stdio`, `file`, or `http`; external chat/platform adapters should stay feature-gated or stubbed until their own ticket.

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

The current daemon exposes the adapter event-bus bridge and reports adapter bus events as `gateway_adapter_event` in runtime output when a future adapter worker emits them. It does not start configured adapters yet.

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

No production adapter is started by `missive gateway run` yet. The trait, registry, config conversion, fake-adapter tests, and gateway event-bus bridge are in place for later adapter tickets. Busy-input policy is available to future adapter workers but is not invoked by a live adapter path yet.
