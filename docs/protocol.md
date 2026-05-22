# Protocol mapping

`missive` treats A2A as the canonical protocol layer. The current implemented
protocol behavior covers public Agent Card discovery for registered agents,
official Rust protocol type integration through `missive-a2a`, and local
interface negotiation for inspected cards.

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

## Error mapping

* HTTP status failures such as `404 Not Found` map to `missive::transport`.
* Network/TLS/HTTP client failures map to `missive::transport`.
* Unsupported or non-mutual interface bindings map to `missive::transport`.
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

Authentication material is not resolved for public Agent Card discovery yet.
