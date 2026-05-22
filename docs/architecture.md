# Architecture

`missive` is currently in its foundation phase. The high-level architecture follows the project brief and is being built ticket by ticket; this document links the initial architecture decisions that constrain later implementation.

## Current workspace boundaries

```text
crates/missive-cli        -> command parsing, output rendering, exit codes
crates/missive-core       -> domain types, errors, config, IDs, envelopes
crates/missive-a2a        -> A2A protocol/client integration and compatibility fixtures
crates/missive-store      -> SQLite migrations and repository APIs
crates/missive-router     -> agent selection, policies, groups, collectives
crates/missive-gateway    -> daemon, subscriptions, webhooks, jobs, sessions
crates/missive-adapters   -> stdin/stdout, file, HTTP, future chat adapters
crates/missive-observe    -> tracing, logs, diagnostics, event export helpers
```

Recommended flow from the project brief:

```text
CLI/adapters -> command model -> router/session/context -> A2A client -> remote agent
                                           |              -> store/events/artifacts
                                           |              -> gateway jobs/subscriptions/webhooks
```

## Architecture decision records

The ADR index lives in [`docs/adr/`](adr/README.md). Initial accepted records are:

* [ADR 0001 — Rust workspace structure](adr/0001-rust-workspace-structure.md)
* [ADR 0002 — A2A-first protocol strategy](adr/0002-a2a-first-protocol-strategy.md)
* [ADR 0003 — SQLite local state](adr/0003-sqlite-local-state.md)
* [ADR 0004 — CLI-first UX](adr/0004-cli-first-ux.md)

These records are intentionally scoped to project-defining decisions. Detailed protocol mappings, storage schema, gateway operation, adapters, collectives, security, and runbook documentation will be expanded by their own implementation tickets.
