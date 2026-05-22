# ADR 0003 — SQLite local state

Status: Accepted

Date: 2026-05-22

## Context

`missive` needs durable local state for agents, contexts, tasks, messages, artifacts, events, groups, authentication references, push notification configs, gateway jobs, and adapter bindings. The state must work for humans and autonomous agents from a local CLI, survive process restarts, and remain outside the source tree by default.

The project should not require an external database service for normal local operation, tests, or gateway use.

## Decision

Use SQLite as the default local persistence engine. Store runtime data under XDG-compatible state/data directories or `MISSIVE_HOME` when implemented, and manage schema changes through repository-controlled migrations.

The exact Rust database crate is deferred to the schema/repository implementation tickets, but the storage contract is SQLite-backed, transactional where needed, and testable against temporary databases.

## Alternatives considered

* **Plain JSON or TOML files** — easy to inspect and bootstrap, but fragile for concurrent gateway writes, incremental event journals, task updates, and transactional changes.
* **Embedded key-value stores** — can be fast and local, but make relational queries for tasks, contexts, groups, and events harder to inspect and migrate.
* **External PostgreSQL or another server database** — useful for future multi-user deployments, but too heavy for the required local-first CLI and daemon model.
* **No durable store** — keeps early implementation simple, but fails requirements for cached Agent Cards, task continuity, event replay, groups, jobs, and gateway resume.

## Consequences

### Positive

* Local installs can persist useful communication state without running a separate service.
* SQLite supports transactions and enough relational structure for the planned control-plane records.
* Tests can use disposable temporary databases without third-party services.

### Negative

* File locking, migrations, and retention policies must be handled carefully.
* SQLite is not a substitute for a distributed control plane; future remote/shared deployments would need a separate decision.

### Follow-up

* Ticket 010 must keep state paths outside the repository and add lock handling.
* Ticket 011 must define migrations, retention notes, and schema documentation.
* Ticket 012 must expose repository APIs so CLI code does not embed SQL strings.

## References

* [`PROJECT_BRIEF.md`](../../PROJECT_BRIEF.md)
* [`BUILD_TICKETS.md`](../../BUILD_TICKETS.md)
