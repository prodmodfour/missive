# BUILD_NOTES.md

## Current state

Tickets 000, 001, 002, 003, 004, and 005 are complete. The repository uses the target Cargo workspace layout for `missive` with these crates:

* `crates/missive-cli` — package `missive-cli`, binary `missive`, and placeholder CLI entry point
* `crates/missive-core` — core domain primitive scaffolding, including shared error/result types
* `crates/missive-a2a` — A2A protocol/client integration scaffolding
* `crates/missive-store` — persistence scaffolding
* `crates/missive-router` — routing and collectives scaffolding
* `crates/missive-gateway` — gateway daemon scaffolding
* `crates/missive-adapters` — adapter scaffolding
* `crates/missive-observe` — observability scaffolding

The root `Cargo.toml` is a virtual workspace manifest with shared workspace package metadata and shared dependency versions for planned foundational Rust crates.

Autonomous build tooling is documented in `docs/tooling.md`. `scripts/bootstrap-tools.sh` is executable, idempotent, supports `--check`, and can install Rust components, optional cargo tools, and opt-in system dependencies.

`scripts/quality-gate.sh` is the hardened default gate for autonomous cycles. It runs shell checks, secret and generated/private-file guardrails, Rust feature checks, formatting, clippy with warnings denied, workspace tests, doc tests, docs with warnings denied, debug/release builds, and optional installed dependency checks. `MISSIVE_AGGRESSIVE_TESTS=1` enables deeper optional checks without editing the script.

Architecture decision records live under `docs/adr/`, with a template and initial accepted ADRs for Rust workspace structure, A2A-first protocol strategy, SQLite local state, and CLI-first UX. `docs/architecture.md` links the ADRs and records the current high-level crate boundaries and shared error handling contract.

`missive-core` now exposes `MissiveError`, `Result<T>`, `ErrorCategory`, `MissiveExitCode`, and `ErrorReport`. The error taxonomy covers I/O, configuration, protocol, transport, storage, authentication, validation, and orchestration failures. Each category has a stable diagnostic code, deterministic exit code mapping for later CLI use, human `Display` rendering, `miette::Diagnostic` metadata, and a serializable JSON/NDJSON report shape.

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
```

The targeted checks covered the new core error rendering tests before the full quality gate.

Environment/tooling notes: no new cargo subcommands or OS packages were installed during this cycle. Adding the core error contract pulled existing workspace-planned Rust dependencies into `Cargo.lock` (`thiserror`, `miette`, `serde`, and `serde_json` for tests).

## Latest cycle notes

Implemented ticket 005 — Implement core error and result types.

Included:

* added `crates/missive-core/src/error.rs` with shared `MissiveError` and `Result<T>` primitives
* added the core error taxonomy for I/O, configuration, protocol, transport, storage, authentication, validation, and orchestration failures
* added deterministic category-to-exit-code mapping for later CLI error handling
* added `ErrorReport` for stable JSON/NDJSON rendering of errors
* implemented `miette::Diagnostic` metadata for diagnostic codes and optional help text
* added representative unit tests for human rendering, miette rendering, JSON rendering, source chains, constructor coverage, and exit codes
* documented the shared error handling contract in `docs/architecture.md`

## Known blockers

None known.

## Limitations

The `missive` binary is still a placeholder. Real CLI flags, subcommands, output rendering, configuration, A2A integration, persistence, gateway behaviour, adapters, and collectives remain for later tickets.

The new error contract is available in `missive-core`, but other crates still use placeholder APIs and have not yet converted real operational paths to return `missive_core::Result<T>`. CLI mapping from `MissiveExitCode` to process status is intentionally left for the CLI tickets.

Detailed protocol mapping, storage schema, gateway operations, adapter lifecycle, collectives, security, testing, and runbook documentation remain for later implementation/documentation tickets.

Optional exhaustive validation tools are not all installed in this environment yet. The default quality gate passes without them and uses installed optional tools automatically. `cargo-nextest` is currently missing, and miri is unavailable for the active stable toolchain.

There is not yet a `cargo-deny` policy file; the quality gate skips deny checks until the supply-chain policy ticket introduces one.

## Next recommended ticket

Ticket 006 — Implement IDs, timestamps, metadata, and envelope primitives.
