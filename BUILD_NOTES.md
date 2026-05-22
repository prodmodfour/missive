# BUILD_NOTES.md

## Current state

Tickets 000, 001, 002, 003, 004, 005, and 006 are complete. The repository uses the target Cargo workspace layout for `missive` with these crates:

* `crates/missive-cli` — package `missive-cli`, binary `missive`, and placeholder CLI entry point
* `crates/missive-core` — core domain primitive scaffolding, including shared error/result types, strongly typed IDs, timestamps, metadata maps, and envelopes
* `crates/missive-a2a` — A2A protocol/client integration scaffolding
* `crates/missive-store` — persistence scaffolding
* `crates/missive-router` — routing and collectives scaffolding
* `crates/missive-gateway` — gateway daemon scaffolding
* `crates/missive-adapters` — adapter scaffolding
* `crates/missive-observe` — observability scaffolding

The root `Cargo.toml` is a virtual workspace manifest with shared workspace package metadata and shared dependency versions for planned foundational Rust crates.

Autonomous build tooling is documented in `docs/tooling.md`. `scripts/bootstrap-tools.sh` is executable, idempotent, supports `--check`, and can install Rust components, optional cargo tools, and opt-in system dependencies.

`scripts/quality-gate.sh` is the hardened default gate for autonomous cycles. It runs shell checks, secret and generated/private-file guardrails, Rust feature checks, formatting, clippy with warnings denied, workspace tests, doc tests, docs with warnings denied, debug/release builds, and optional installed dependency checks. `MISSIVE_AGGRESSIVE_TESTS=1` enables deeper optional checks without editing the script.

Architecture decision records live under `docs/adr/`, with a template and initial accepted ADRs for Rust workspace structure, A2A-first protocol strategy, SQLite local state, and CLI-first UX. `docs/architecture.md` links the ADRs and records the current high-level crate boundaries, shared error handling contract, and core primitive contract.

`missive-core` now exposes `MissiveError`, `Result<T>`, `ErrorCategory`, `MissiveExitCode`, and `ErrorReport`. The error taxonomy covers I/O, configuration, protocol, transport, storage, authentication, validation, and orchestration failures. Each category has a stable diagnostic code, deterministic exit code mapping for later CLI use, human `Display` rendering, `miette::Diagnostic` metadata, and a serializable JSON/NDJSON report shape.

`missive-core` also exposes `AgentAlias`, `GroupName`, `RankName`, `TransportName`, `ContextId`, `TaskId`, `MessageId`, `EventId`, `MissiveTimestamp`, `Metadata`, and `Envelope<T>`. Named identifiers validate lowercase CLI-safe forms; opaque A2A/local IDs preserve server-provided text while rejecting empty, whitespace, control, or unbounded values. `Metadata` uses deterministic JSON object ordering, and `MissiveTimestamp` uses canonical RFC3339 UTC rendering.

## Quality gates

Latest run:

```bash
scripts/quality-gate.sh
```

Result: passed.

Checks run by the default gate included:

* shell script syntax checks with `bash -n`
* `shellcheck` because it is installed
* secret guardrail across tracked files and untracked non-ignored files
* generated/private-file guardrail across tracked files and untracked non-ignored files
* `cargo check --workspace --all-targets`
* `cargo check --workspace --all-targets --all-features`
* `cargo check --workspace --all-targets --no-default-features`
* `cargo fmt --all -- --check`
* `cargo clippy --workspace --all-targets --all-features -- -D warnings`
* `cargo test --workspace --all-targets --all-features`
* `cargo test --workspace --doc --all-features`
* `RUSTDOCFLAGS=-Dwarnings cargo doc --workspace --all-features --no-deps`
* `cargo build --workspace --all-features`
* `cargo build --workspace --all-features --release`
* `cargo build -p missive-cli --bin missive --release`
* optional `cargo machete` check because it is installed
* optional `cargo audit` check because it is installed

Additional targeted validation run during this cycle:

```bash
cargo test -p missive-core --all-targets
cargo clippy -p missive-core --all-targets --all-features -- -D warnings
cargo audit
```

The targeted checks covered the new core primitive tests and confirmed the final dependency set had no RustSec advisories. A preliminary quality-gate run exposed a RustSec advisory in the `time` crate candidate dependency; the timestamp implementation was switched to `chrono`, and the final quality gate passed.

Environment/tooling notes: no new cargo subcommands or OS packages were installed during this cycle. Adding the core primitive contract updated `Cargo.lock`, added `chrono` for RFC3339 timestamp handling, promoted `serde_json` to a runtime dependency of `missive-core` for metadata values, and added `proptest` as a `missive-core` dev-dependency for identifier property tests.

## Latest cycle notes

Implemented ticket 006 — Implement IDs, timestamps, metadata, and envelope primitives.

Included:

* added strongly typed string wrappers for `AgentAlias`, `GroupName`, `RankName`, `TransportName`, `ContextId`, `TaskId`, `MessageId`, and `EventId`
* implemented `Display`, `FromStr`, `TryFrom`, `Serialize`, and `Deserialize` support for all ID wrappers
* added validation diagnostics for invalid aliases, group names, transport names, and opaque IDs using `MissiveError::validation`
* added property tests for valid named identifier round trips, invalid named identifier rejection, and opaque identifier round trips
* added `MissiveTimestamp` with RFC3339 parse/display/serde support and UTC canonicalization
* added deterministic `Metadata` map helpers for validated keys, insertion, lookup, merge, removal, serde, and ordered JSON output
* added a generic `Envelope<T>` primitive carrying event id, timestamp, metadata, and payload
* documented the core primitive contract in `docs/architecture.md`

## Known blockers

None known.

## Limitations

The `missive` binary is still a placeholder. Real CLI flags, subcommands, output rendering, configuration, A2A integration, persistence, gateway behaviour, adapters, and collectives remain for later tickets.

The core error and primitive contracts are available in `missive-core`, but other crates still use placeholder APIs and have not yet converted operational paths to the shared types. CLI mapping from `MissiveExitCode` to process status is intentionally left for the CLI tickets.

Detailed protocol mapping, storage schema, gateway operations, adapter lifecycle, collectives, security, testing, and runbook documentation remain for later implementation/documentation tickets.

Optional exhaustive validation tools are not all installed in this environment yet. The default quality gate passes without them and uses installed optional tools automatically. `cargo-nextest` is currently missing, and miri is unavailable for the active stable toolchain.

There is not yet a `cargo-deny` policy file; the quality gate skips deny checks until the supply-chain policy ticket introduces one.

## Next recommended ticket

Ticket 007 — Implement CLI skeleton and global flags.
