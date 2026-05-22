# ADR 0001 — Rust workspace structure

Status: Accepted

Date: 2026-05-22

## Context

`missive` must become a Rust CLI, protocol abstraction library, local gateway daemon, and agent communication control plane without turning into a general-purpose agent framework. The project needs clear boundaries for CLI behavior, protocol integration, local state, routing, gateway work, adapters, and observability so later autonomous tickets can make focused commits.

The bootstrap tickets already created the target Cargo workspace with these crates:

```text
crates/missive-cli        command parsing, output rendering, exit codes
crates/missive-core       domain types, errors, config, IDs, envelopes
crates/missive-a2a        A2A protocol/client integration and compatibility fixtures
crates/missive-store      SQLite migrations and repository APIs
crates/missive-router     agent selection, policies, groups, collectives
crates/missive-gateway    daemon, subscriptions, webhooks, jobs, sessions
crates/missive-adapters   stdin/stdout, file, HTTP, future chat adapters
crates/missive-observe    tracing, logs, diagnostics, event export helpers
```

## Decision

Use a single Cargo workspace with the root remaining a virtual manifest and implementation split across the eight planned crates. The `missive-cli` package owns the binary named `missive`; shared domain concepts live in `missive-core`; and integration-specific behavior stays in the crate closest to the external boundary.

The workspace is the primary compilation, linting, testing, and documentation unit for `scripts/quality-gate.sh`.

## Alternatives considered

* **Single monolithic CLI crate** — simpler at first, but it would mix protocol, persistence, gateway, routing, and rendering concerns. That would make later tickets more invasive and increase the chance that CLI-only changes break library or daemon behavior.
* **Many highly granular crates from the start** — could create stricter layering, but would add dependency-management overhead before the domain model is stable.
* **Protocol-SDK-only structure** — placing most logic behind an A2A SDK wrapper would underrepresent local control-plane needs such as state, collectives, adapters, and gateway jobs.

## Consequences

### Positive

* Later tickets can implement one subsystem without rewriting unrelated crates.
* The binary name can remain stable while internal crates evolve.
* Integration tests can target public crate boundaries as they become meaningful.

### Negative

* Cross-crate changes require care to avoid circular dependencies.
* Early placeholder crates add boilerplate before behavior exists.

### Follow-up

* Keep crate dependency direction explicit as real APIs are added.
* Revisit boundaries only through a new ADR if a crate becomes an artificial split.

## References

* [`PROJECT_BRIEF.md`](../../PROJECT_BRIEF.md)
* [`BUILD_TICKETS.md`](../../BUILD_TICKETS.md)
