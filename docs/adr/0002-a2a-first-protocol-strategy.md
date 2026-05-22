# ADR 0002 — A2A-first protocol strategy

Status: Accepted

Date: 2026-05-22

## Context

A2A compatibility is mandatory for `missive`. The project must support Agent Card discovery, interface negotiation, message sending, streaming, task management, push notification configuration, context/task continuity, service parameters, and protocol-version handling. `PROJECT_BRIEF.md` also directs the project to evaluate and preferably wrap the official `a2a-rs` Rust SDK rather than unnecessarily hand-rolling protocol types.

The protocol implementation must still serve `missive` as a communication control plane: CLI output contracts, local persistence, gateway jobs, routing, and adapters must remain stable even when the upstream protocol or SDK changes.

## Decision

Make A2A the canonical protocol layer. Implement A2A-facing code in `crates/missive-a2a`, prefer official `a2a-rs` protocol types where practical, and isolate compatibility code behind `missive-a2a` when direct SDK use is not viable for a ticket.

When `a2a-rs` is used as a Git or unreleased dependency, pin the revision and document the update process. When `missive` must temporarily define compatibility models, keep them minimal, serde-tested, and covered by A2A conformance fixtures so they can be replaced by official types later.

## Alternatives considered

* **Wrap official `a2a-rs` types directly everywhere** — maximizes conformance and reduces duplicated protocol structs, but it can leak upstream API churn into CLI, store, router, and gateway crates. The chosen strategy wraps use through `missive-a2a` to keep blast radius small.
* **Hand-roll all protocol models** — offers immediate control over serde shapes and error mapping, but it risks drifting from A2A and duplicates the maintenance burden of the official SDK. This is only acceptable as a temporary compatibility layer when the SDK cannot satisfy a concrete feature.
* **Generate models from protocol schemas only** — can produce consistent types, but generated code can be hard to curate for CLI-friendly diagnostics, redaction, and persistence mapping. It remains a possible internal implementation technique within `missive-a2a`.
* **Support multiple protocols equally from the start** — broadens scope beyond the project goal. Non-A2A adapters can exist later, but A2A remains the canonical protocol boundary.

## Consequences

### Positive

* Protocol compatibility decisions are localized to `missive-a2a`.
* Future SDK updates can be tested against fixtures before reaching CLI and gateway users.
* The project avoids inventing a proprietary agent protocol.

### Negative

* `missive-a2a` must maintain mapping layers for errors, tasks, messages, artifacts, and service parameters.
* Some tickets may need temporary compatibility structs while upstream SDK support matures.

### Follow-up

* ADR 0005 documents the concrete official `a2a-lf` integration and compatibility parser selected by ticket 016.
* A2A conformance fixtures should continue to be expanded and versioned before broader protocol behavior is treated as stable.

## References

* [`PROJECT_BRIEF.md`](../../PROJECT_BRIEF.md)
* [`BUILD_TICKETS.md`](../../BUILD_TICKETS.md)
