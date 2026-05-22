# BUILD_NOTES.md

## Current state

Tickets 000, 001, 002, 003, 004, 005, 006, 007, 008, 009, and 010 are complete. The repository uses the target Cargo workspace layout for `missive` with these crates:

* `crates/missive-cli` — package `missive-cli`, binary `missive`, clap-derived CLI skeleton, global flags, configuration loading from CLI/env/discovery, output rendering contract, redaction helpers, help snapshots, and placeholder execution status
* `crates/missive-core` — core domain primitive scaffolding, including shared error/result types, strongly typed IDs, timestamps, metadata maps, envelopes, configuration schema, config discovery, profile validation, and redacted config rendering
* `crates/missive-a2a` — A2A protocol/client integration scaffolding
* `crates/missive-store` — persistence scaffolding with local state path resolution, profile-specific data/state/cache directories, SQLite database path resolution, and process locks for state mutation and gateway operation
* `crates/missive-router` — routing and collectives scaffolding
* `crates/missive-gateway` — gateway daemon scaffolding
* `crates/missive-adapters` — adapter scaffolding
* `crates/missive-observe` — observability scaffolding

The root `Cargo.toml` is a virtual workspace manifest with shared workspace package metadata and shared dependency versions for planned foundational Rust crates.

Autonomous build tooling is documented in `docs/tooling.md`. `scripts/bootstrap-tools.sh` is executable, idempotent, supports `--check`, and can install Rust components, optional cargo tools, and opt-in system dependencies.

`scripts/quality-gate.sh` is the hardened default gate for autonomous cycles. It runs shell checks, secret and generated/private-file guardrails, Rust feature checks, formatting, clippy with warnings denied, workspace tests, doc tests, docs with warnings denied, debug/release builds, and optional installed dependency checks. `MISSIVE_AGGRESSIVE_TESTS=1` enables deeper optional checks without editing the script.

Architecture decision records live under `docs/adr/`, with a template and initial accepted ADRs for Rust workspace structure, A2A-first protocol strategy, SQLite local state, and CLI-first UX. `docs/architecture.md` links the ADRs and records the current high-level crate boundaries, shared error handling contract, core primitive contract, CLI command skeleton, configuration contract, output rendering contract, and store path/lock contract.

`missive-core` exposes `MissiveError`, `Result<T>`, `ErrorCategory`, `MissiveExitCode`, and `ErrorReport`. The error taxonomy covers I/O, configuration, protocol, transport, storage, authentication, validation, and orchestration failures. Each category has a stable diagnostic code, deterministic exit code mapping for CLI use, human `Display` rendering, `miette::Diagnostic` metadata, and a serializable JSON/NDJSON report shape.

`missive-core` also exposes `AgentAlias`, `GroupName`, `RankName`, `TransportName`, `ContextId`, `TaskId`, `MessageId`, `EventId`, `MissiveTimestamp`, `Metadata`, and `Envelope<T>`. Named identifiers validate lowercase CLI-safe forms; opaque A2A/local IDs preserve server-provided text while rejecting empty, whitespace, control, or unbounded values. `Metadata` uses deterministic JSON object ordering, and `MissiveTimestamp` uses canonical RFC3339 UTC rendering.

The core configuration layer exposes `MissiveConfig`, `ConfigDiscovery`, `LoadedConfig`, and schema structs for profiles, agents, auth refs, storage, output, gateway, adapters, and quality of service. Discovery precedence is `--config`, `MISSIVE_CONFIG`, repository-local config when `MISSIVE_REPO_CONFIG=1`, XDG config locations, then built-in defaults. Config parsing rejects unknown fields, validates cross references, and provides redacted JSON rendering.

The store path layer exposes `StatePathResolver`, `StatePaths`, `StatePlatform`, `StatePathSource`, `ProcessLockKind`, and `ProcessLock`. Runtime state defaults to XDG-compatible roots on Linux/Unix-like platforms, macOS `~/Library` fallbacks when XDG variables are absent, or `MISSIVE_HOME` when explicitly set. Paths include `profiles/<profile>`, relative database paths resolve under the selected profile state directory, and lock files live under `<state-dir>/locks/`.

The `missive` binary uses clap derive and exposes help pages for `agent`, `send`, `stream`, `task`, `context`, `group`, `gateway`, `webhook`, `push`, `doctor`, `logs`, `events`, `completion`, and `manpage`. Global flags parse at every command level: `--json`, `--ndjson`, `--quiet`, `--no-color`, `--config`, `--profile`, `--timeout`, `--trace`, and `--verbose`.

The current CLI output contract supports human, JSON, NDJSON, and quiet renderers. Skeletal commands load/validate config, then emit a stable `missive.output.v1` `command_status` envelope in machine-readable modes; NDJSON emits one compact JSON object per line with `sequence`. Structured execution errors render as `kind: "error"` envelopes when `--json` or `--ndjson` is active. The renderer recursively redacts secret-like JSON fields and HTTP-style authorization headers before writing output.

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
cargo test -p missive-store --all-targets
cargo clippy -p missive-store --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo machete
```

The targeted checks covered Linux/XDG and macOS fallback state path resolution, `MISSIVE_HOME` overrides, profile-specific directories, explicit database path expansion, prevention of relative database path traversal, side-effect-free path resolution, directory creation, state/gateway lock files, and concurrent lock contention.

Environment/tooling notes: no new cargo subcommands or OS packages were installed during this cycle. Implementing process locks added the direct workspace dependency `fs2`.

## Latest cycle notes

Implemented ticket 010 — Implement local state paths and lock handling.

Included:

* added `crates/missive-store/src/paths.rs` with `StatePathResolver`, `StatePaths`, `StatePlatform`, `StatePathSource`, `ProcessLockKind`, and `ProcessLock`
* supported `MISSIVE_HOME` as an absolute all-in-one runtime root with `data/`, `state/`, and `cache/` profile subdirectories
* resolved Linux/Unix-like XDG data/state/cache roots with `$HOME` fallbacks and macOS `~/Library` fallbacks when XDG variables are absent
* made every default runtime path include `profiles/<profile>` so selected profiles get separate state directories
* resolved omitted SQLite database paths to `<state-dir>/missive.sqlite3`
* resolved explicit `storage.database_path` values as absolute paths, `~/` home paths, or relative paths under the selected profile state directory
* rejected relative database paths containing `..` to prevent escaping the profile state directory
* kept path resolution side-effect free and added explicit `StatePaths::ensure_directories()` for later store/gateway code
* added OS-level file locks through `fs2` with lock files `state.lock` and `gateway.lock` under `<state-dir>/locks/`
* mapped nonblocking lock contention to a storage error with actionable help text
* documented state path precedence, database path handling, and lock files in `docs/configuration.md`, `docs/architecture.md`, and `README.md`

## Known blockers

None known.

## Limitations

The `missive` binary has a real command tree, global parser, configuration discovery/profile validation, and output rendering contract, but command behaviour is still intentionally skeletal. Real A2A integration, persistence, gateway behaviour, adapters, collectives, shell completion generation, and manpage generation remain for later tickets.

Configuration supports schema validation and secret-free summaries, but it does not yet resolve or send authentication material. Auth refs point to environment variables or keyring entries for later auth handling.

The store layer now resolves state paths and process locks, but SQLite schema migrations, database opening, repository APIs, retention policies, and durable event/task persistence are not implemented yet.

The config schema includes gateway, adapter, and QoS defaults, but those values do not yet start a gateway, enforce timeouts, run adapters, or manage background jobs.

The `--json`, `--ndjson`, and `--quiet` flags override config output defaults. `--config` and `--profile` are now active. The `--timeout`, `--trace`, `--verbose`, and `--no-color` flags are still parsed but do not yet drive timeout enforcement, tracing, verbose diagnostics, or color control.

Redaction is best-effort at the config and CLI output boundaries for structured values rendered through the current helpers. Authentication input handling, trace/log redaction, storage safety beyond path/lock handling, and adapter/webhook trust boundaries remain for later security and observability tickets.

The core error, primitive, configuration, state path, and lock contracts are available, but other crates still use placeholder APIs and have not yet converted operational paths to the shared types.

Detailed protocol mapping, SQLite schema, gateway operations, adapter lifecycle, collectives, testing, and runbook documentation remain for later implementation/documentation tickets.

Optional exhaustive validation tools are not all installed in this environment yet. The default quality gate passes without them and uses installed optional tools automatically. `cargo-nextest` is currently missing, and miri is unavailable for the active stable toolchain.

There is not yet a `cargo-deny` policy file; the quality gate skips deny checks until the supply-chain policy ticket introduces one.

## Next recommended ticket

Ticket 011 — Design SQLite schema and migrations.
