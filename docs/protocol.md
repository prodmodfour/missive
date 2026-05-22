# Protocol mapping

`missive` treats A2A as the canonical protocol layer. The current implemented
protocol behavior is intentionally narrow: public Agent Card discovery for
registered agents.

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
Rust SDK/type integration and transport/interface negotiation are later tickets.

## Error mapping

* HTTP status failures such as `404 Not Found` map to `missive::transport`.
* Network/TLS/HTTP client failures map to `missive::transport`.
* Invalid JSON or schema-incompatible Agent Card payloads map to
  `missive::protocol`.

Authentication material is not resolved for public Agent Card discovery yet.
