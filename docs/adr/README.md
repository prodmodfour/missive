# Architecture decision records

Architecture decision records (ADRs) capture durable project choices for `missive`: the context, decision, alternatives considered, and consequences. They are intentionally short and should be updated by adding a new ADR rather than rewriting history when a decision changes.

## Status vocabulary

Use one of these status values in the `Status:` field:

* `Proposed` — under discussion and not yet binding.
* `Accepted` — current project direction.
* `Deprecated` — still documented for history but no longer recommended.
* `Superseded` — replaced by another ADR; include a link to the successor.

Initial ADRs use `Accepted` because they document project-defining constraints captured in the README and maintainer documentation.

## Records

* [0001 — Rust workspace structure](0001-rust-workspace-structure.md)
* [0002 — A2A-first protocol strategy](0002-a2a-first-protocol-strategy.md)
* [0003 — SQLite local state](0003-sqlite-local-state.md)
* [0004 — CLI-first UX](0004-cli-first-ux.md)
* [0005 — Official A2A Rust protocol types](0005-official-a2a-rust-types.md)

## Template

Use [template.md](template.md) for new records. Keep numbering monotonic and prefer one decision per ADR.
