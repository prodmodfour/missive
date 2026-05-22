# Protocol mapping

`missive` treats A2A as the canonical protocol layer. The current implemented
protocol behavior covers public Agent Card discovery for registered agents and
local interface negotiation for inspected cards.

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

The current parser extracts and renders the Agent Card fields needed for human
and machine inspection: provider, agent version, protocol versions, capabilities,
supported interfaces, default input/output modes, and skills. Full official A2A
Rust SDK/type integration remains a later ticket.

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

Authentication material is not resolved for public Agent Card discovery yet.
