# BUILD_NOTES.md

## Current state

Tickets 000, 001, 002, 003, 004, 005, 006, 007, and 008 are complete. The repository uses the target Cargo workspace layout for `missive` with these crates:

* `crates/missive-cli` — package `missive-cli`, binary `missive`, clap-derived CLI skeleton, global flags, output rendering contract, redaction helpers, help snapshots, and placeholder execution status
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

Architecture decision records live under `docs/adr/`, with a template and initial accepted ADRs for Rust workspace structure, A2A-first protocol strategy, SQLite local state, and CLI-first UX. `docs/architecture.md` links the ADRs and records the current high-level crate boundaries, shared error handling contract, core primitive contract, CLI command skeleton, and output rendering contract.

`missive-core` exposes `MissiveError`, `Result<T>`, `ErrorCategory`, `MissiveExitCode`, and `ErrorReport`. The error taxonomy covers I/O, configuration, protocol, transport, storage, authentication, validation, and orchestration failures. Each category has a stable diagnostic code, deterministic exit code mapping for CLI use, human `Display` rendering, `miette::Diagnostic` metadata, and a serializable JSON/NDJSON report shape.

`missive-core` also exposes `AgentAlias`, `GroupName`, `RankName`, `TransportName`, `ContextId`, `TaskId`, `MessageId`, `EventId`, `MissiveTimestamp`, `Metadata`, and `Envelope<T>`. Named identifiers validate lowercase CLI-safe forms; opaque A2A/local IDs preserve server-provided text while rejecting empty, whitespace, control, or unbounded values. `Metadata` uses deterministic JSON object ordering, and `MissiveTimestamp` uses canonical RFC3339 UTC rendering.

The `missive` binary uses clap derive and exposes help pages for `agent`, `send`, `stream`, `task`, `context`, `group`, `gateway`, `webhook`, `push`, `doctor`, `logs`, `events`, `completion`, and `manpage`. Global flags parse at every command level: `--json`, `--ndjson`, `--quiet`, `--no-color`, `--config`, `--profile`, `--timeout`, `--trace`, and `--verbose`.

The current CLI output contract supports human, JSON, NDJSON, and quiet renderers. Skeletal commands emit a stable `missive.output.v1` `command_status` envelope in machine-readable modes; NDJSON emits one compact JSON object per line with `sequence`. Structured execution errors render as `kind: "error"` envelopes when `--json` or `--ndjson` is active. The renderer recursively redacts secret-like JSON fields and HTTP-style authorization headers before writing output.

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
cargo fmt --all
cargo test -p missive-cli --all-targets
cargo clippy -p missive-cli --all-targets --all-features -- -D warnings
cargo run -q -p missive-cli --bin missive -- agent --json
cargo run -q -p missive-cli --bin missive -- events --ndjson
cargo run -q -p missive-cli --bin missive -- doctor --quiet | wc -c
cargo run -q -p missive-cli --bin missive -- agent --json --ndjson
```

The targeted checks covered the new renderer module, every current command in JSON and NDJSON modes, quiet success output, structured machine-readable validation errors, and redaction helpers for secret-like JSON fields and HTTP authorization headers.

Environment/tooling notes: no new cargo subcommands or OS packages were installed during this cycle. Implementing the output contract added `serde` and `serde_json` as direct dependencies of `missive-cli`; both were already workspace-managed dependencies.

## Latest cycle notes

Implemented ticket 008 — Implement output rendering contract.

Included:

* added `crates/missive-cli/src/output.rs` with `OutputMode`, `CommandStatus`, JSON/NDJSON envelope writing, quiet handling, and human output rendering
* introduced stable machine-readable fields: `schema_version`, `ok`, `kind`, `sequence` for NDJSON, and `data`
* wired command execution to render skeletal command status through the selected mode instead of printing ad hoc text
* rendered parsed execution errors through the same machine-readable contract when `--json` or `--ndjson` is active
* rejected conflicting `--json` and `--ndjson` execution modes with deterministic usage exit code `64`
* added recursive redaction helpers for secret-like keys, token-like text, and HTTP-style authorization headers
* added unit and integration tests for output mode selection, all current commands in JSON/NDJSON modes, quiet mode, structured errors, and redaction
* documented the output envelope and redaction behavior in `docs/cli.md`, `docs/architecture.md`, `README.md`, and `SECURITY.md`

## Known blockers

None known.

## Limitations

The `missive` binary has a real command tree, global parser, and output rendering contract, but command behaviour is still intentionally skeletal. Real configuration discovery, A2A integration, persistence, gateway behaviour, adapters, collectives, shell completion generation, and manpage generation remain for later tickets.

The `--json`, `--ndjson`, and `--quiet` flags now drive the current skeleton output and execution-error rendering. The `--config`, `--profile`, `--timeout`, `--trace`, `--verbose`, and `--no-color` flags are still parsed but do not yet drive config loading, timeout enforcement, tracing, verbose diagnostics, or color control.

Redaction is best-effort at the CLI output boundary for structured values rendered through the new renderer. Authentication input handling, trace/log redaction, config-secret rendering, and storage safety remain for later security and observability tickets.

The core error and primitive contracts are available in `missive-core`, but other crates still use placeholder APIs and have not yet converted operational paths to the shared types.

Detailed protocol mapping, storage schema, gateway operations, adapter lifecycle, collectives, testing, and runbook documentation remain for later implementation/documentation tickets.

Optional exhaustive validation tools are not all installed in this environment yet. The default quality gate passes without them and uses installed optional tools automatically. `cargo-nextest` is currently missing, and miri is unavailable for the active stable toolchain.

There is not yet a `cargo-deny` policy file; the quality gate skips deny checks until the supply-chain policy ticket introduces one.

## Next recommended ticket

Ticket 009 — Implement configuration discovery and profiles.
