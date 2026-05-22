# BUILD_NOTES.md

## Current state

Tickets 000, 001, 002, and 003 are complete. The repository uses the target Cargo workspace layout for `missive` with these crates:

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

`scripts/quality-gate.sh` is now the hardened default gate for autonomous cycles. It runs shell checks, secret and generated/private-file guardrails, Rust feature checks, formatting, clippy with warnings denied, workspace tests, doc tests, docs with warnings denied, debug/release builds, and optional installed dependency checks. `MISSIVE_AGGRESSIVE_TESTS=1` enables deeper optional checks without editing the script.

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

Additional targeted/aggressive validation run during this cycle:

```bash
bash -n scripts/quality-gate.sh scripts/check-no-secrets.sh scripts/check-no-generated-private-files.sh scripts/bootstrap-tools.sh
shellcheck scripts/*.sh scripts/lib/*.sh
MISSIVE_AGGRESSIVE_TESTS=1 scripts/quality-gate.sh
```

Aggressive mode passed. It additionally ran `cargo llvm-cov --workspace --all-features --no-report`, repeated installed dependency checks, and ran a bounded `cargo mutants --workspace --check` smoke shard using a temporary output directory. It skipped `cargo-nextest` because it is not installed, skipped miri because the component is unavailable for the active stable toolchain, and skipped fuzz, benchmark, and Docker checks because there are no fuzz targets, benchmark sources, Dockerfile, Compose file, devcontainer, or Docker integration script yet.

Environment/tooling notes: the aggressive coverage check ensured the `llvm-tools-preview` rustup component (`llvm-tools-x86_64-unknown-linux-gnu`) is installed. No new cargo subcommands or OS packages were installed manually during this cycle.

## Latest cycle notes

Implemented ticket 003 — Harden Rust quality gate.

Included:

* expanded `scripts/quality-gate.sh` into explicit shell, guardrail, Rust feature-check, formatting/linting, test, documentation, build, dependency, aggressive, Docker, `just`, and Node adjunct stages
* added default `cargo check` coverage for default, all-features, and no-default-features workspace builds
* added docs build enforcement with `RUSTDOCFLAGS=-Dwarnings`
* required the guardrail scripts to exist and run during the gate
* enhanced secret scanning to cover tracked files and untracked non-ignored files with labelled findings
* enhanced generated/private-file scanning to cover tracked files and untracked non-ignored files, including local key files and mutation output
* added optional `shellcheck` usage to the quality gate and bootstrap tooling
* added bounded aggressive paths for nextest, coverage, advisory/dependency checks, deny policy when configured, miri, mutation compile smoke, fuzz smoke, benchmark compilation, and Docker/devcontainer validation
* kept cargo-mutants output in a temporary directory so aggressive checks do not dirty the working tree
* documented the hardened default and aggressive gate behaviour in `docs/tooling.md` and refreshed README validation commands

## Known blockers

None known.

## Limitations

The `missive` binary is still a placeholder. Real CLI flags, subcommands, output rendering, configuration, A2A integration, persistence, gateway behaviour, adapters, and collectives remain for later tickets.

Optional exhaustive validation tools are not all installed in this environment yet. The default quality gate passes without them and uses installed optional tools automatically. `cargo-nextest` is currently missing, and miri is unavailable for the active stable toolchain.

There is not yet a `cargo-deny` policy file; the quality gate skips deny checks until the supply-chain policy ticket introduces one.

## Next recommended ticket

Ticket 004 — Create architecture decision records scaffold.
