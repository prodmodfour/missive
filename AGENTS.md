# AGENTS.md

You are working in an autonomous, ticket-driven build system for `missive`.

This file contains general rules. Project-specific instructions live in `PROJECT_BRIEF.md`.

## Required reading

Before making changes, read:

* `AGENTS.md`
* `PROJECT_BRIEF.md`
* `BUILD_TICKETS.md`
* `BUILD_NOTES.md`

## Core workflow

When invoked by the build loop:

1. Select the lowest-numbered `TODO` or `IN_PROGRESS` ticket from `BUILD_TICKETS.md`.
2. Say what you are working on now, including the selected ticket and immediate action.
3. Implement only that ticket.
4. Do not start future tickets.
5. Do not broaden scope.
6. Add or update tests/validation where appropriate.
7. Add or update docs where appropriate.
8. Run `scripts/quality-gate.sh`.
9. Update `BUILD_TICKETS.md`.
10. Update `BUILD_NOTES.md`.
11. Commit the completed ticket with a conventional commit message.
12. Leave the working tree clean.

## Autonomy level

The build agent has broad local autonomy for this project.

Allowed when useful:

* installing Rust toolchains, components, and cargo subcommands
* installing OS packages with `sudo`, apt, brew, or equivalent
* installing Docker/devcontainer dependencies
* running local servers, mock A2A agents, webhook receivers, test databases, fuzzers, benchmarks, and integration suites
* using network access for official package registries, protocol documentation, upstream SDKs, and test dependencies
* running aggressive validation when feasible

Do not treat missing tools as a reason to skip important validation before attempting to install them or documenting why installation failed.

## Scope control

Do not:

* start future tickets
* silently change project goals
* rewrite unrelated code
* add dependencies that are unrelated to the selected ticket
* add speculative features beyond the ticket acceptance criteria
* bypass quality gates
* mark tickets done without working implementation and validation
* commit generated/private files unless explicitly required

## Minimal safety and repository hygiene

Never commit:

* real secrets
* credentials
* access tokens
* private keys
* real `.env` files
* private data
* internal hostnames or private URLs
* employer/client data
* Terraform state
* generated cloud plans
* machine-specific configuration
* runtime databases, logs, sockets, pid files, coverage HTML, fuzz crash artifacts, or target directories

You may run destructive tests only inside temporary directories, isolated containers, or clearly disposable local fixtures. Do not attack, scan, fuzz, or load-test third-party services.

## Documentation rules

Update docs when behaviour, setup, architecture, operations, security, limitations, or public-facing usage changes.

Prefer clear, honest limitations over pretending the project is production-ready.

## Testing and validation

Use `scripts/quality-gate.sh` for every ticket.

For important protocol, persistence, routing, CLI, and gateway behaviour, add tests rather than relying on manual inspection.

When feasible, run stronger checks than the minimum gate, for example:

```bash
MISSIVE_AGGRESSIVE_TESTS=1 scripts/quality-gate.sh
```

Record extra checks in `BUILD_NOTES.md`.

## Commit style

Use conventional commits:

```text
chore:
feat:
fix:
test:
docs:
refactor:
ci:
build:
```

Examples:

```text
chore: bootstrap missive workspace
feat: add a2a agent registry
feat: implement task wait command
test: add mock a2a streaming fixtures
docs: document gateway operation
ci: add rust validation workflow
```

## Completion

A project is complete only when:

* the final ticket is done
* quality gates pass
* docs match implementation
* minimal safety and repository hygiene constraints are respected
* the top-level `AUTOMATION_STATUS` in `BUILD_TICKETS.md` is set to `DONE`
