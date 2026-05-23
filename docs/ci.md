# Continuous integration

missive uses GitHub Actions to mirror the local `scripts/quality-gate.sh` validation path on Linux and to compile/test the Rust workspace on Linux and macOS. The default workspace test matrix intentionally does not run on Windows; Windows release packaging remains a separate dry-run workflow. The workflows use Node.js 24-compatible GitHub-maintained actions (`actions/checkout@v6` and `actions/cache@v5`).

## Workflow

The CI workflow lives at `.github/workflows/ci.yml` and runs on:

* `push`
* `pull_request`
* manual `workflow_dispatch`

The default CI path has three validation jobs plus optional coverage. Every CI job first tries `actions/checkout@v6` with read-only credentials and `persist-credentials: false`; if that checkout step fails before repository code is available, CI falls back to an unauthenticated public fetch of the exact `${{ github.sha }}` and verifies the checked-out commit before continuing.

The default CI path has these validation jobs:

1. `workflow-lint` installs a pinned `actionlint` release and validates GitHub Actions workflow syntax.
2. `quality-gate` runs on Linux, installs the stable Rust toolchain with `rustfmt` and `clippy`, installs small Linux system tools used by the local gate plus pinned `cargo-deny`, restores a Cargo cache, and runs `scripts/quality-gate.sh`.
3. `workspace-matrix` runs on `ubuntu-latest` and `macos-latest` with `fail-fast: false`. Each matrix leg runs:

   ```bash
   cargo check --workspace --all-targets --all-features
   cargo test --workspace --all-targets --all-features
   cargo build --workspace --all-features
   cargo build -p missive-cli --bin missive --all-features
   ```

Because CI invokes the same quality gate used by autonomous local tickets on Linux, it covers shell syntax checks, workflow validation, secret scanning, generated/private-file guardrails, Rust feature checks, formatting, clippy with warnings denied, workspace tests, doc tests, docs with warnings denied, debug/release builds, the release `missive` binary build, and the `cargo-deny` supply-chain policy in `deny.toml`. The matrix job keeps the core Rust CLI/library workspace compiling and testing on macOS without requiring platform-specific service managers or Bash-based repository scripts there.

## Platform-specific coverage

The Linux/macOS workspace matrix validates portable missive functionality: protocol/client code, CLI command models and output rendering, SQLite-backed store APIs, router/collective logic, gateway daemon code that is not tied to a host service manager, adapter frame parsing, and local mock A2A integration tests.

Platform-specific features are handled as follows:

| Area | Linux CI | macOS CI | Windows |
| --- | --- | --- | --- |
| Local quality gate (`scripts/quality-gate.sh`) | Runs fully | Not run by default | Not run by default |
| Rust workspace check/test/build | Runs | Runs | Not run by default |
| Shell example smoke scripts | Run through the Linux quality gate and Linux matrix leg | Run when Bash/POSIX tools are available | Ignored because the examples are Bash/POSIX script demos rather than Windows-native workflows |
| `missive gateway install/start/stop/status/uninstall` service files | systemd user/system plans are supported and tested | launchd plans are supported and tested in platform-independent unit coverage | Unsupported; commands return a clear diagnostic and operators should run `missive gateway run` under an external supervisor |
| Gateway daemon, HTTP adapter, webhook receiver | Portable Rust code is built/tested | Portable Rust code is built/tested | Not covered by the default workspace matrix |

Windows workspace testing is disabled in the default CI path after repeated hosted-runner path and locking incompatibilities. Re-enable a Windows workspace leg only after the state-path and lock test fixtures are made native Windows-safe. Windows service installation remains future work; commands return a clear unsupported-platform diagnostic today.

## Cache and secret posture

The workflows use `permissions: contents: read` and check out with `persist-credentials: false`. The CI fallback checkout path uses only the public repository URL and the exact workflow commit SHA; it does not consume repository secrets or persist credentials. The default gate and Linux/macOS matrix do not require repository secrets.

The cache is limited to Cargo registry/git data and `target/` build outputs keyed by the operating system, stable Rust toolchain, lockfile, and Cargo manifests. Runtime state, credentials, local config, coverage reports, databases, logs, and other generated private files must remain outside version control and should not be uploaded as artifacts.

## Supply-chain policy

The Linux quality-gate job installs pinned `cargo-deny` and runs
`cargo deny --locked check` through `scripts/quality-gate.sh`. The policy checks
RustSec advisories/yanked crates, approved licenses, allowed crate sources, and
reviewed duplicate-version exceptions. See [`supply-chain.md`](supply-chain.md)
for the policy and dependency-update workflow.

SBOM generation is available locally through:

```bash
scripts/generate-sbom.sh --output dist/missive-sbom.cdx.json
```

Generated SBOMs are release artifacts and are not uploaded by the default CI
workflow.

## Optional coverage

Coverage is intentionally optional so normal pull requests stay close to the local default quality gate. Run the manual workflow with the `coverage` input enabled, or set the non-secret repository variable `MISSIVE_CI_COVERAGE=1`, to run:

```bash
cargo llvm-cov --workspace --all-features --no-report
```

The coverage job is a smoke check only and does not publish coverage artifacts by default.

## Release packaging dry run

The release packaging workflow lives at `.github/workflows/release.yml`. It runs
on manual `workflow_dispatch` and release tags matching `v*.*.*`. The workflow
uses read-only repository permissions plus Node.js 24-compatible checkout/cache/artifact actions, builds the `missive` binary with the
workspace `dist` profile, packages common Linux/macOS/Windows target archives,
generates SHA-256 checksum files, and uploads the archives/checksums as workflow
artifacts. It does not create GitHub Releases or require repository secrets.

Run the same packaging path locally with:

```bash
scripts/release-package.sh --dry-run
```

See [`release.md`](release.md) for the target matrix, archive contents,
checksum verification, and install/update instructions.

## Container validation

Docker and devcontainer support are available for local reproducible validation, but the default GitHub Actions workflow does not yet build the Docker image on every push. Validate the container workflow locally when Docker-related files change:

```bash
docker build --pull=false --tag missive-dev:local .
scripts/docker-integration.sh
```

Aggressive local quality-gate mode also builds the Docker image and validates the devcontainer configuration when the relevant tools are installed. See [`container.md`](container.md) for image contents, build arguments, and runtime hygiene notes.

## Local validation

Run the same default checks locally before pushing:

```bash
scripts/quality-gate.sh
```

If `actionlint` is installed, the quality gate uses it through `scripts/validate-ci.sh`. Without `actionlint`, the script falls back to YAML syntax validation when Ruby or PyYAML is available and prints a clear warning otherwise.
