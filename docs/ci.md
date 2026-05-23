# Continuous integration

missive uses GitHub Actions to mirror the local `scripts/quality-gate.sh` validation path on Linux and to compile/test the Rust workspace on Linux, macOS, and Windows.

## Workflow

The workflow lives at `.github/workflows/ci.yml` and runs on:

* `push`
* `pull_request`
* manual `workflow_dispatch`

The default CI path has three validation jobs plus optional coverage:

1. `workflow-lint` installs a pinned `actionlint` release and validates GitHub Actions workflow syntax.
2. `quality-gate` runs on Linux, installs the stable Rust toolchain with `rustfmt` and `clippy`, installs small Linux system tools used by the local gate, restores a Cargo cache, and runs `scripts/quality-gate.sh`.
3. `workspace-matrix` runs on `ubuntu-latest`, `macos-latest`, and `windows-latest` with `fail-fast: false`. Each matrix leg runs:

   ```bash
   cargo check --workspace --all-targets --all-features
   cargo test --workspace --all-targets --all-features
   cargo build --workspace --all-features
   cargo build -p missive-cli --bin missive --all-features
   ```

Because CI invokes the same quality gate used by autonomous local tickets on Linux, it covers shell syntax checks, workflow validation, secret scanning, generated/private-file guardrails, Rust feature checks, formatting, clippy with warnings denied, workspace tests, doc tests, docs with warnings denied, debug/release builds, and the release `missive` binary build. The matrix job keeps the core Rust CLI/library workspace compiling and testing on macOS and Windows without requiring platform-specific service managers or Bash-based repository scripts there.

## Platform-specific coverage

The cross-platform matrix validates portable missive functionality: protocol/client code, CLI command models and output rendering, SQLite-backed store APIs, router/collective logic, gateway daemon code that is not tied to a host service manager, adapter frame parsing, and local mock A2A integration tests.

Platform-specific features are handled as follows:

| Area | Linux CI | macOS CI | Windows CI |
| --- | --- | --- | --- |
| Local quality gate (`scripts/quality-gate.sh`) | Runs fully | Not run by default | Not run by default |
| Rust workspace check/test/build | Runs | Runs | Runs |
| Shell example smoke scripts | Run through the Linux quality gate and Linux matrix leg | Run when Bash/POSIX tools are available | Ignored because the examples are Bash/POSIX script demos rather than Windows-native workflows |
| `missive gateway install/start/stop/status/uninstall` service files | systemd user/system plans are supported and tested | launchd plans are supported and tested in platform-independent unit coverage | Unsupported; commands return a clear diagnostic and operators should run `missive gateway run` under an external supervisor |
| Gateway daemon, HTTP adapter, webhook receiver | Portable Rust code is built/tested | Portable Rust code is built/tested | Portable Rust code is built/tested |

Windows support currently means the Rust CLI, library crates, local SQLite state, HTTP clients/servers, output formats, and non-service gateway operation should compile and pass tests. Windows service installation is intentionally unsupported until a later ticket explicitly adds a Windows service manager integration.

## Cache and secret posture

The workflow uses `permissions: contents: read` and checks out with `persist-credentials: false`. It does not require repository secrets for the default gate or matrix.

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
