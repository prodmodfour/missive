# Contributing to missive

`missive` is currently built through an autonomous, ticket-driven workflow. Human and agent contributors should keep changes small, reviewable, and aligned with the active ticket.

## Workflow

1. Read `AGENTS.md`, `PROJECT_BRIEF.md`, `BUILD_TICKETS.md`, and `BUILD_NOTES.md`.
2. Select the lowest-numbered `TODO` or `IN_PROGRESS` ticket.
3. Implement only that ticket.
4. Run `scripts/quality-gate.sh`.
5. Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
6. Commit with a conventional commit message.

Do not start future tickets or broaden scope without updating the ticket queue first.

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
