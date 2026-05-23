# Contributing to missive

Human and agent contributors should keep changes small, reviewable, and aligned with the documented project goals.

## Workflow

1. Start from a clean working tree and create a focused branch when useful.
2. Implement one coherent change at a time.
3. Update tests and documentation when behavior, setup, architecture, operations, security posture, limitations, or public usage changes.
4. Run `scripts/quality-gate.sh` before submitting or committing.
5. Commit with a conventional commit message.

Avoid broad, unrelated rewrites. If a change needs a larger design discussion, document the proposal in an issue, ADR, or pull request before implementation.

## Quality gate

Run before every commit:

```bash
scripts/quality-gate.sh
```

The gate checks shell script syntax, CI workflow syntax where applicable, repository hygiene, Rust feature combinations, formatting, Clippy, workspace tests, doc tests, documentation builds, debug/release builds, and installed dependency-policy tools. Optional aggressive checks are enabled with the `MISSIVE_AGGRESSIVE_TESTS=1` environment variable.

## Repository hygiene

Never commit real secrets, credentials, private keys, tokens, local runtime databases, logs, sockets, PID files, target directories, coverage reports, or other machine-specific state.

Use temporary directories, local mock services, or isolated containers for destructive or stateful tests.

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
