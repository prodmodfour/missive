# BUILD_NOTES.md

## Current state

Tickets 000 and 001 are complete. The repository now uses the target Cargo workspace layout for `missive` with these crates:

* `crates/missive-cli` — package `missive-cli`, binary `missive`, and placeholder CLI entry point
* `crates/missive-core` — core domain primitive scaffolding
* `crates/missive-a2a` — A2A protocol/client integration scaffolding
* `crates/missive-store` — persistence scaffolding
* `crates/missive-router` — routing and collectives scaffolding
* `crates/missive-gateway` — gateway daemon scaffolding
* `crates/missive-adapters` — adapter scaffolding
* `crates/missive-observe` — observability scaffolding

The root `Cargo.toml` is now a virtual workspace manifest with shared workspace package metadata and shared dependency versions for planned foundational Rust crates. The former root placeholder package was removed.

The autonomous build system remains at the repository root and continues to drive work through `BUILD_TICKETS.md`.

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
cargo metadata --format-version 1 --no-deps
cargo build --workspace
cargo run -p missive-cli --bin missive
cargo test --workspace --all-features
cargo machete
```

No new tools were installed during this cycle. `cargo audit` updated/read the local RustSec advisory database as part of validation.

## Latest cycle notes

Implemented ticket 001 — Define Cargo workspace and crate layout.

Included:

* virtual root Cargo workspace with all eight target crates under `crates/`
* shared workspace package metadata and shared dependency version table
* `missive-cli` manifest with `[[bin]] name = "missive"`
* minimal compileable library targets for all crates
* placeholder `missive` binary that reports the workspace crate count
* unit tests covering crate metadata and the target crate layout
* README updates describing the new workspace layout and current placeholder binary command

## Known blockers

None known.

## Limitations

The `missive` binary is still a placeholder. Real CLI flags, subcommands, output rendering, configuration, A2A integration, persistence, gateway behaviour, adapters, and collectives remain for later tickets.

## Next recommended ticket

Ticket 002 — Install and document autonomous build tooling.
