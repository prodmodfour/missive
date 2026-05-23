# ADR 0005 — Official A2A Rust protocol types

Status: Accepted

Date: 2026-05-22

## Context

`missive` evaluated the available A2A Rust SDK options and chose to use official protocol types where practical while documenting compatibility behavior with fixtures. The project prefers wrapping official `a2a-rs` types instead of maintaining duplicate protocol structs.

The relevant options found during evaluation were:

* `a2a-lf` from [`a2aproject/a2a-rs`](https://github.com/a2aproject/a2a-rs), published on crates.io, Apache-2.0, Rust 1.85, with core protocol serde types, protocol constants, and no client/server dependency requirement.
* `a2a-client-lf` and `a2a-server-lf` from the same workspace, useful later for transport behavior but broader than the current type-integration decision.
* Older or unofficial crates such as `a2a-rs`, `a2a-rs-core`, and `a2a-protocol-types`; these either come from different repositories, require a newer Rust version than this workspace, or duplicate the official LF SDK surface.

## Decision

Use the official A2A Rust SDK core crate through crates.io:

```toml
a2a = { package = "a2a-lf", version = "0.3" }
```

`crates/missive-a2a` re-exports official SDK types from `missive_a2a::protocol` and aliases the public Agent Card inspection types to the official structs. Future missive crates should import A2A protocol models through `missive-a2a` rather than depending on `a2a-lf` directly.

Keep a small compatibility parser at the `missive-a2a` edge for public Agent Card discovery. The parser deserializes into the official `a2a-lf::AgentCard` type after normalizing known fixture aliases such as snake_case field names and after adding an empty `supportedInterfaces` array for older/pre-release cards that omit the field. Raw Agent Card JSON is still cached and rendered so optional fields that are not needed for current inspection are not lost.

Do not adopt `a2a-client-lf` or `a2a-server-lf` yet. Send, stream, task, push, gateway, and server behavior can evaluate transport-specific dependencies with concrete acceptance tests when that work needs them.

## Update process

For normal crates.io updates:

```bash
cargo update -p a2a-lf --precise <version>
cargo test -p missive-a2a --all-targets
scripts/quality-gate.sh
```

After updating, inspect upstream release notes, update A2A fixtures under `tests/fixtures/a2a/<version>/` when the wire format changes, and update this ADR or add a successor ADR if the strategy changes.

If `missive` ever switches to an unreleased Git dependency, the dependency must pin an exact revision and document the repository URL, revision, and reason for avoiding crates.io.

## Alternatives considered

* **Use `a2a-client-lf` immediately** — this may reduce future transport work, but it would broaden this decision into client behavior before missive's storage, output, and error contracts are considered for those paths.
* **Use another crates.io A2A type crate** — some crates expose useful models, but the project brief specifically prefers the official `a2a-rs` SDK and the official LF crate matches the workspace Rust version.
* **Continue hand-rolled Agent Card structs** — this preserved current behavior but duplicated protocol models and increased drift risk. The chosen compatibility parser keeps only the edge normalization missive needs today.
* **Generate types from protobuf now** — useful for future gRPC/protobuf work, but heavier than necessary for the current serde round-trip and Agent Card inspection requirements.

## Consequences

### Positive

* Message, task, artifact, push-config, and Agent Card models now come from the official SDK surface instead of local duplicate structs.
* Upstream protocol churn is isolated behind `missive-a2a`.
* Minimal A2A fixtures round-trip through official serde types.
* The current Agent Card cache/inspect behavior remains compatible with older cards that lack `supportedInterfaces`.

### Negative

* `missive-a2a` still needs a small normalization layer for legacy card shapes and fixture aliases.
* Optional Agent Card security fields are preserved in raw JSON but are not yet interpreted automatically by missive; implemented outbound authentication is supplied through missive auth refs, environment variables, keyrings, or CLI headers.
* The official client/server crates are still not integrated; missive implements the currently supported Agent Card, send, stream, task, push, subscription, gateway, and webhook HTTP behavior behind `crates/missive-a2a` and local gateway crates.

## References

* [`README.md`](../../README.md)
* [`docs/protocol.md`](../protocol.md)
* [`a2aproject/a2a-rs`](https://github.com/a2aproject/a2a-rs)
