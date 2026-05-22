# BUILD_NOTES.md

## Current state

Tickets 000, 001, and 002 are complete. The repository now uses the target Cargo workspace layout for `missive` with these crates:

* `crates/missive-cli` — package `missive-cli`, binary `missive`, and placeholder CLI entry point
* `crates/missive-core` — core domain primitive scaffolding
* `crates/missive-a2a` — A2A protocol/client integration scaffolding
* `crates/missive-store` — persistence scaffolding
* `crates/missive-router` — routing and collectives scaffolding
* `crates/missive-gateway` — gateway daemon scaffolding
* `crates/missive-adapters` — adapter scaffolding
* `crates/missive-observe` — observability scaffolding

The root `Cargo.toml` is a virtual workspace manifest with shared workspace package metadata and shared dependency versions for planned foundational Rust crates.

Autonomous build tooling is documented in `docs/tooling.md`. `scripts/bootstrap-tools.sh` is executable, idempotent, supports `--check`, and can install Rust components, optional cargo tools, and opt-in system dependencies.

## Quality gates

Latest run:

```bash
scripts/quality-gate.sh
```

Result: passed.

Checks run by the gate included:

* shell script syntax checks
* secret guardrail
* generated/private-file guardrail
* `cargo fmt --all -- --check`
* `cargo clippy --workspace --all-targets --all-features -- -D warnings`
* `cargo test --workspace --all-features`
* `cargo test --workspace --doc --all-features`
* `cargo build --workspace --all-features`
* `cargo build --workspace --all-features --release`
* optional `cargo machete` check because it was installed
* optional `cargo audit` check because it was installed

Additional targeted validation run during this cycle:

```bash
bash -n scripts/bootstrap-tools.sh
scripts/bootstrap-tools.sh --help
scripts/bootstrap-tools.sh --check
```

No new tools were installed during this cycle. The bootstrap check reported these optional tools/dependencies as currently missing: `cargo-nextest`, `cargo-dist`, `protoc`, and `sqlite3`. They are not required for the default quality gate and can be installed later with `scripts/bootstrap-tools.sh` or `scripts/bootstrap-tools.sh --system-deps` when a ticket needs them. `cargo audit` updated/read the local RustSec advisory database as part of validation.

## Latest cycle notes

Implemented ticket 002 — Install and document autonomous build tooling.

Included:

* hardened `scripts/bootstrap-tools.sh` with idempotent command checks, `--check`, `--system-deps`, `--docker`, and environment toggles
* cargo-tool installation mapping for `cargo-nextest`, `cargo-llvm-cov`, `cargo-deny`, `cargo-audit`, `cargo-machete`, `cargo-mutants`, `cargo-fuzz`, `sqlx-cli`, `just`, and `cargo-dist`
* opt-in system package handling for `jq`, `protoc`, `sqlite3`, `pkg-config`, `gh`, and Docker where supported
* `docs/tooling.md` covering tool inventory, sudo/package-manager autonomy, bootstrap usage, optional-tool quality-gate behavior, and manual install examples
* README, autonomous build docs, and usage docs updated to link the tooling workflow

## Known blockers

None known.

## Limitations

The `missive` binary is still a placeholder. Real CLI flags, subcommands, output rendering, configuration, A2A integration, persistence, gateway behaviour, adapters, and collectives remain for later tickets.

Optional exhaustive validation tools are not all installed in this environment yet. The default quality gate passes without them and uses installed optional tools automatically.

## Next recommended ticket

Ticket 003 — Harden Rust quality gate.
