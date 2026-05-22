# Build tooling

This page documents local tooling for developing and validating `missive`.
The required default path stays small; optional tools improve diagnostics,
coverage, supply-chain checks, fuzzing, mutation testing, release packaging, and
future gateway/container validation.

## Quick start

From the repository root:

```bash
scripts/bootstrap-tools.sh --check
scripts/bootstrap-tools.sh
scripts/quality-gate.sh
```

`--check` reports missing tools without installing anything. Running the script
without `--check` installs Rust components and cargo subcommands where possible.
The script is intentionally idempotent and best-effort: it skips commands that
already exist and warns, rather than failing the whole run, when optional tools
cannot be installed.

To let the script install supported operating-system packages such as `jq`,
`protoc`, `sqlite3`, and `pkg-config`, opt in explicitly:

```bash
scripts/bootstrap-tools.sh --system-deps
```

The autonomous build agent is allowed to use `sudo`, package managers, `rustup`,
`cargo install`, Docker, and local services for build and test dependencies when
a ticket needs them. Any notable installation commands run during a ticket must
be recorded in `BUILD_NOTES.md`.

## Bootstrap script controls

`scripts/bootstrap-tools.sh` supports these options:

| Option | Effect |
| --- | --- |
| `--check` | Report missing tools without installing anything. |
| `--no-rustup` | Skip Rust toolchain and component setup. |
| `--no-cargo-tools` | Skip optional cargo subcommand installation. |
| `--system-deps` | Install supported OS packages with `apt-get` or `brew`. |
| `--docker` | Include best-effort Docker installation where supported. |

Equivalent environment toggles are available for automation:

```bash
MISSIVE_BOOTSTRAP_RUSTUP=0 scripts/bootstrap-tools.sh
MISSIVE_BOOTSTRAP_CARGO_TOOLS=0 scripts/bootstrap-tools.sh
MISSIVE_BOOTSTRAP_SYSTEM_DEPS=1 scripts/bootstrap-tools.sh
MISSIVE_BOOTSTRAP_DOCKER=1 scripts/bootstrap-tools.sh
MISSIVE_BOOTSTRAP_CARGO_INSTALL_LOCKED=0 scripts/bootstrap-tools.sh
```

## Tool inventory

| Tool | Required by default gate? | Used when present? | Purpose |
| --- | --- | --- | --- |
| Rust stable, `cargo` | Yes | Yes | Build and test the Rust workspace. |
| `rustfmt` | Yes | Yes | Formatting checks. |
| `clippy` | Yes | Yes | Lints with warnings denied. |
| `cargo-machete` | No | Yes | Optional unused-dependency check. |
| `cargo-audit` | No | Yes | Optional RustSec advisory check. |
| `cargo-deny` | No | When deny config exists | Future license/advisory/duplicate policy checks. |
| `cargo-nextest` | No | Aggressive gate | Faster workspace test runner. |
| `cargo-llvm-cov` | No | Aggressive gate | Coverage smoke checks. |
| `miri` | No | Aggressive gate | Undefined-behaviour-oriented Rust test checks. |
| `cargo-mutants` | No | Aggressive gate | Bounded mutation-test smoke checks. |
| `cargo-fuzz` | No | Aggressive gate when fuzz targets exist | Fuzz smoke runs. |
| `sqlx` (`sqlx-cli`) | No | Future store tickets | SQLite migration and query tooling if sqlx is selected. |
| `just` | No | If a `justfile` with `ci` exists | Optional command runner. |
| `jq` | No | Helper scripts | JSON processing for repository automation. |
| `gh` | No | Helper scripts | Optional GitHub issue creation. |
| `protoc` | No | Future protocol work | Protocol buffer generation if needed. |
| `sqlite3` | No | Future store/debug work | Inspect local SQLite databases in tests. |
| Docker | No | Future container tickets | Local container/devcontainer validation. |

## Quality gate behavior

`scripts/quality-gate.sh` must pass from a normal checkout with only required
Rust tooling installed. Optional tools are handled as follows:

* if an optional tool is missing, the gate emits a warning and skips that check;
* if an optional tool is present, the gate runs the matching check;
* `MISSIVE_AGGRESSIVE_TESTS=1` enables deeper optional checks such as nextest,
  coverage, miri, mutation, and fuzz smoke tests when those tools/targets exist.

Run the default gate for every ticket:

```bash
scripts/quality-gate.sh
```

Run deeper validation when feasible:

```bash
MISSIVE_AGGRESSIVE_TESTS=1 scripts/quality-gate.sh
```

## Manual package examples

The bootstrap script is the preferred entry point, but manual installation is
allowed when a ticket needs it. Examples:

```bash
rustup toolchain install stable --profile default --component rustfmt --component clippy
cargo install --locked cargo-audit
cargo install --locked cargo-machete
sudo apt-get update
sudo apt-get install -y jq protobuf-compiler sqlite3 pkg-config
```

Keep runtime state, generated reports, credentials, database files, and local
machine configuration out of version control.
