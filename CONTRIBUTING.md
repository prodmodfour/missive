# Contributing to missive

Thanks for helping improve `missive`. Human and agent contributors should keep changes small, reviewable, and aligned with the project goal: a boring, scriptable Rust control plane for A2A-native agent communication.

`missive` is early-stage. Prefer clear contracts, deterministic tests, documented limitations, and safe defaults over broad rewrites or speculative abstractions.

## Ways to contribute

Useful contributions include:

* bug fixes with a regression test or a documented reproduction;
* CLI, protocol, routing, store, gateway, adapter, or observability improvements;
* documentation updates for setup, operations, security, troubleshooting, or examples;
* test fixtures, conformance coverage, fuzz/property tests, and smoke tests;
* supply-chain, packaging, release, CI, and repository-hygiene improvements.

For larger design changes, open an issue, discussion, ADR draft, or pull request outline before implementing the full change.

## Development setup

Required basics:

* Rust stable matching the workspace `rust-version` in `Cargo.toml`;
* `cargo`, `rustfmt`, and `clippy`;
* Bash for repository scripts.

Optional local tooling can be checked or installed with:

```bash
scripts/bootstrap-tools.sh --check
scripts/bootstrap-tools.sh
```

See [`docs/tooling.md`](docs/tooling.md), [`docs/testing.md`](docs/testing.md), and [`docs/container.md`](docs/container.md) for tool inventory, validation modes, and container workflows.

## Workflow

1. Start from a clean working tree and create a focused branch when useful.
2. Make one coherent change at a time.
3. Add or update tests when behavior changes.
4. Update documentation when setup, usage, architecture, operations, security posture, public output, limitations, or troubleshooting guidance changes.
5. Run the relevant validation commands before committing or submitting.
6. Commit with a conventional commit message.

Avoid broad, unrelated rewrites. If you need to move code around, keep mechanical refactors separate from behavior changes.

## Validation

Run the default quality gate before commits and pull requests when feasible:

```bash
scripts/quality-gate.sh
```

The gate checks shell script syntax, CI workflow syntax where applicable, repository hygiene, Rust feature combinations, formatting, Clippy, workspace tests, doc tests, documentation builds, debug/release builds, and installed dependency-policy tools.

For quicker local loops, use targeted commands such as:

```bash
cargo check --workspace --all-targets --all-features
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
```

Optional deeper checks are enabled with:

```bash
MISSIVE_AGGRESSIVE_TESTS=1 scripts/quality-gate.sh
```

Document any skipped validation in the pull request, including the reason and any follow-up needed.

## Documentation expectations

Keep user-facing behavior discoverable from `README.md`, `docs/`, command help, or examples. When adding or changing commands, update the relevant docs and any smoke-tested examples. When changing storage, protocol, gateway, adapter, security, or release behavior, update the corresponding reference docs and consider whether an ADR is warranted.

## Security and data hygiene

Never commit real secrets, credentials, private keys, tokens, private URLs, internal hostnames, local runtime databases, logs, sockets, PID files, target directories, coverage reports, generated SBOMs, release artifacts, fuzz artifacts, or other machine-specific state.

Use temporary directories, local mock services, or isolated containers for destructive or stateful tests. Do not attack, scan, fuzz, or load-test third-party services. Redact authentication material in logs, traces, fixtures, documentation, issue reports, and escalation bundles. Follow [`SECURITY.md`](SECURITY.md) and [`docs/security.md`](docs/security.md) for vulnerability reporting and implementation-specific trust boundaries.

## Pull request checklist

Before asking for review, confirm that:

* the change has a narrow scope and a clear rationale;
* tests or fixtures cover behavior changes, or the PR explains why not;
* documentation and examples are updated where needed;
* generated/runtime/private files are not included;
* `scripts/quality-gate.sh` or appropriate targeted checks have been run and summarized;
* known limitations, follow-up work, or compatibility risks are called out.

## Commit style

Use conventional commit prefixes such as:

* `chore:`
* `feat:`
* `fix:`
* `test:`
* `docs:`
* `refactor:`
* `ci:`
* `build:`
