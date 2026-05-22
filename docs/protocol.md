# Protocol mapping

`missive` treats A2A as the canonical protocol layer. The current implemented
protocol behavior covers public Agent Card discovery for registered agents,
official Rust protocol type integration through `missive-a2a`, local interface
negotiation, and non-streaming `SendMessage` calls for `missive send`.

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
Current implemented uses are Agent Card discovery/refresh and non-streaming
`missive send`; future stream, task, and push clients should reuse the same
helper so request metadata remains consistent.

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
client only validates and applies already-resolved headers. `missive send` uses
the same auth headers for the optional Agent Card fetch and the A2A send request.

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
`ROLE_USER` `Message`, text parts, optional `contextId`/`taskId`, optional
`configuration.acceptedOutputModes`, and non-secret request metadata. The
outbound message id is generated by the official A2A type helper and is also the
local request-message row id.

Transport mapping:

* `http+json` appends `message:send` to the selected interface URL and sends
  `POST <interface>/message:send` with `Content-Type: application/a2a+json`.
* `json-rpc` posts a JSON-RPC 2.0 request with method `SendMessage` to the
  selected JSON-RPC interface URL.

Both transports send `A2A-Version` and any configured extensions/service
parameters, apply resolved auth headers, and parse the official
`SendMessageResponse` oneof. Responses may be either a direct `Message` or a
`Task`. Direct messages are stored as response message rows. Task responses are
stored in `tasks` with the raw remote task JSON, mapped task state, protocol
version metadata, and a linked response message row using the task status message
when one is present or a local synthetic response row when the task has no status
message.

The send implementation is non-streaming only. SSE streaming, artifact
persistence/export, task polling, cancellation, richer file/JSON parts, and push
configuration remain mapped for later tickets.

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

* `AgentCard`
* `Message`
* `Task`
* `SendMessageRequest`
* `SendMessageResponse`
* `TaskPushNotificationConfig`

Fixtures live under `tests/fixtures/a2a/1.0/`. Update the fixtures and rerun
`cargo test -p missive-a2a --all-targets` when updating the upstream SDK or when
A2A wire shapes change.

Authentication material is resolved for implemented Agent Card fetch/refresh and
non-streaming send requests. Future stream, task, and push protocol calls should
reuse `AuthHeaders` plus the CLI/config auth resolver instead of inventing
separate secret handling paths.
