# ADR 0004 — CLI-first UX

Status: Accepted

Date: 2026-05-22

## Context

`missive` should feel like `curl` for agent messages, `kubectl` for agent communication state, and MPI-style collective operations for multi-agent workflows. The project must serve both humans and autonomous agents, which means no critical behavior can depend on an interactive-only path.

The CLI must eventually expose stable human-readable, JSON, and NDJSON output modes; deterministic exit codes; stdin/stdout workflows; and automation-friendly commands for discovery, messaging, tasks, contexts, groups, gateway operation, adapters, and diagnostics.

## Decision

Treat the `missive` binary as the primary user and automation interface. Command behavior should be designed CLI-first, with stable machine-readable output contracts alongside human-readable summaries. Library crates support the CLI, gateway, and adapters rather than defining a separate agent framework surface.

Interactive enhancements can be added later, but they must not replace non-interactive flags, stdin/stdout behavior, or deterministic outputs.

## Alternatives considered

* **Daemon-first API** — a local daemon is planned, but requiring it for basic discovery, send, and inspect workflows would make simple automation harder and increase setup friction.
* **Rust library-first API** — useful for internal reuse, but the target audience includes humans and non-Rust agents that need a subprocess interface.
* **Interactive TUI-first experience** — can improve human ergonomics later, but it would conflict with mandatory shell/agent automation if treated as the core path.
* **Configuration-only orchestration** — declarative config is useful, but `missive` also needs imperative command workflows for agent inspection, task control, and gateway diagnostics.

## Consequences

### Positive

* Automation can call `missive` as a stable subprocess.
* Human UX and machine UX can evolve together through explicit output renderers.
* Gateway and adapter features can reuse the same command model instead of inventing separate semantics.

### Negative

* CLI compatibility becomes a public contract that must be tested.
* Commands need careful error mapping, redaction, and output stability from early implementation onward.

### Follow-up

* Ticket 007 must add a coherent CLI skeleton and global flags.
* Ticket 008 must define human, JSON, NDJSON, and quiet rendering contracts with redaction.
* Later command tickets should include CLI smoke or snapshot tests where feasible.

## References

* [`PROJECT_BRIEF.md`](../../PROJECT_BRIEF.md)
* [`BUILD_TICKETS.md`](../../BUILD_TICKETS.md)
