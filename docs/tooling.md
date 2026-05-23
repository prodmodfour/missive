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
`shellcheck`, `protoc`, `sqlite3`, and `pkg-config`, opt in explicitly:

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
| `shellcheck` | No | Yes | Optional shell-script linting in addition to `bash -n`. |
| `cargo-machete` | No | Yes | Optional unused-dependency check. |
| `cargo-audit` | No | Yes | Optional RustSec advisory check. |
| `cargo-deny` | No | When deny config exists | Future license/advisory/duplicate policy checks. |
| `cargo-dist` | No | Optional reference tool | Release archive/install-script generation; missive currently uses local equivalent scripts. |
| `tar` plus `sha256sum` or `shasum` | Yes for release dry runs | Yes | Local release archive creation and checksum verification through `scripts/release-package.sh` and `scripts/install-release.sh`. |
| `cargo-nextest` | No | Aggressive gate | Faster workspace test runner. |
| `cargo-llvm-cov` | No | Aggressive gate | Coverage smoke checks. |
| `miri` | No | Aggressive gate | Undefined-behaviour-oriented Rust test checks. |
| `cargo-mutants` | No | Aggressive gate | Bounded mutation compile smoke checks. |
| `cargo-fuzz` | No | Aggressive gate when fuzz targets exist | Fuzz smoke runs. |
| Criterion via `cargo bench` | No | Aggressive gate compiles when benchmark sources exist | Local benchmark measurements and saved-baseline comparisons. |
| `actionlint` | No | Default gate when present, CI workflow-lint job | GitHub Actions workflow validation. |
| `sqlx` (`sqlx-cli`) | No | Future store tickets | SQLite migration and query tooling if sqlx is selected. |
| `just` | No | If a `justfile` with `ci` exists | Optional command runner. |
| `jq` | No | Helper scripts | JSON processing for repository automation. |
| `gh` | No | Helper scripts | Optional GitHub issue creation. |
| `protoc` | No | Future protocol work | Protocol buffer generation if needed. |
| `sqlite3` | No | Future store/debug work | Inspect local SQLite databases in tests. |
| Docker | No | Aggressive gate when Docker inputs exist | Local container/devcontainer validation through `Dockerfile`, `.devcontainer/devcontainer.json`, and `scripts/docker-integration.sh`. |

## Quality gate behavior

`scripts/quality-gate.sh` must pass from a normal checkout with only required
Rust tooling installed. The default gate is intentionally suitable for every
autonomous cycle and fails on repository hygiene, formatting, lint, test, doc,
or build regressions.

Default checks include:

* `bash -n` for repository shell scripts, plus `shellcheck` when installed;
* GitHub Actions workflow validation through `scripts/validate-ci.sh` when `.github/workflows/` exists;
* secret scanning across tracked files and untracked non-ignored files;
* generated/private/runtime-file scanning across tracked files and untracked
  non-ignored files;
* `cargo check --workspace --all-targets` for default, all-features, and
  no-default-features builds;
* `cargo fmt --all -- --check`;
* `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
* workspace tests and doc tests with all features;
* `RUSTDOCFLAGS=-Dwarnings cargo doc --workspace --all-features --no-deps`;
* debug and release workspace builds, including the release `missive` binary;
* optional `cargo-machete`, `cargo-audit`, and `cargo-deny` when the relevant
  tools/configuration are present.

Optional tools are handled as follows:

* if an optional tool is missing, the gate emits a warning and skips that check;
* if an optional tool is present, the gate runs the matching check;
* `scripts/validate-ci.sh` uses `actionlint` when available, otherwise it falls
  back to basic YAML syntax validation with Ruby or PyYAML when present;
* `cargo-deny` is skipped until a deny configuration exists, because the policy
  itself is introduced by a later ticket.

Run the default gate for every ticket:

```bash
scripts/quality-gate.sh
```

Run the same gate inside the development container when Docker validation is relevant:

```bash
docker build --pull=false --tag missive-dev:local .
scripts/docker-integration.sh
```

The container workflow is documented in [`container.md`](container.md). It keeps `MISSIVE_HOME`, Cargo caches, and build output out of tracked repository files.

Run deeper validation when feasible:

```bash
MISSIVE_AGGRESSIVE_TESTS=1 scripts/quality-gate.sh
```

Aggressive mode adds optional paths for `cargo-nextest`, `cargo-llvm-cov`,
`cargo-deny`/`cargo-audit`/`cargo-machete`, `miri`, a bounded `cargo-mutants`
compile smoke through `scripts/mutation-smoke.sh`, `cargo-fuzz` smoke runs for
the parser/replay targets under `fuzz/`, Criterion benchmark compilation when
`*/benches/*.rs` sources exist, and Docker or devcontainer checks when those
inputs exist. Full benchmark measurement remains a manual `cargo bench` workflow
documented in [`performance.md`](performance.md). Release packaging dry runs are
manual or run by `.github/workflows/release.yml`; see [`release.md`](release.md)
for `scripts/release-package.sh`, checksums, and install/update validation.
Mutation and fuzz smoke bounds can
be adjusted with `MISSIVE_MUTANTS_MODE`, `MISSIVE_MUTANTS_FILES`,
`MISSIVE_MUTANTS_RE`, `MISSIVE_MUTANTS_SHARD`, `MISSIVE_MUTANTS_TIMEOUT`,
`MISSIVE_MUTANTS_JOBS`, `MISSIVE_MUTANTS_BASELINE`, `MISSIVE_FUZZ_SECONDS`, and
`MISSIVE_FUZZ_SANITIZER`. The mutation smoke defaults to a `check`-mode shard of
critical store, router, auth/redaction, task, and collective command files. The
quality gate defaults fuzz smoke runs to `MISSIVE_FUZZ_SANITIZER=none` so they
compile on the stable toolchain; set `MISSIVE_FUZZ_SANITIZER=address` (and use a
compatible nightly setup) for sanitizer-backed fuzz campaigns.

## Manual package examples

The bootstrap script is the preferred entry point, but manual installation is
allowed when a ticket needs it. Examples:

```bash
rustup toolchain install stable --profile default --component rustfmt --component clippy
cargo install --locked cargo-audit
cargo install --locked cargo-machete
go install github.com/rhysd/actionlint/cmd/actionlint@v1.7.7
sudo apt-get update
sudo apt-get install -y jq shellcheck protobuf-compiler sqlite3 pkg-config
```

Keep runtime state, generated reports, credentials, database files, and local
machine configuration out of version control.
