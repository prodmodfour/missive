# Continuous integration

missive uses GitHub Actions to mirror the local `scripts/quality-gate.sh` validation path on Linux.

## Workflow

The workflow lives at `.github/workflows/ci.yml` and runs on:

* `push`
* `pull_request`
* manual `workflow_dispatch`

The default CI path has two jobs:

1. `workflow-lint` installs a pinned `actionlint` release and validates GitHub Actions workflow syntax.
2. `quality-gate` installs the stable Rust toolchain with `rustfmt` and `clippy`, installs small system tools used by the local gate, restores a Cargo cache, and runs `scripts/quality-gate.sh`.

Because CI invokes the same quality gate used by autonomous local tickets, it covers shell syntax checks, workflow validation, secret scanning, generated/private-file guardrails, Rust feature checks, formatting, clippy with warnings denied, workspace tests, doc tests, docs with warnings denied, debug/release builds, and the release `missive` binary build.

## Cache and secret posture

The workflow uses `permissions: contents: read` and checks out with `persist-credentials: false`. It does not require repository secrets for the default gate.

The cache is limited to Cargo registry/git data and `target/` build outputs keyed by the operating system, stable Rust toolchain, lockfile, and Cargo manifests. Runtime state, credentials, local config, coverage reports, databases, logs, and other generated private files must remain outside version control and should not be uploaded as artifacts.

## Optional coverage

Coverage is intentionally optional so normal pull requests stay close to the local default quality gate. Run the manual workflow with the `coverage` input enabled, or set the non-secret repository variable `MISSIVE_CI_COVERAGE=1`, to run:

```bash
cargo llvm-cov --workspace --all-features --no-report
```

The coverage job is a smoke check only and does not publish coverage artifacts by default.

## Local validation

Run the same default checks locally before pushing:

```bash
scripts/quality-gate.sh
```

If `actionlint` is installed, the quality gate uses it through `scripts/validate-ci.sh`. Without `actionlint`, the script falls back to YAML syntax validation when Ruby or PyYAML is available and prints a clear warning otherwise.

Cross-platform CI is intentionally deferred to the next CI/CD ticket.
