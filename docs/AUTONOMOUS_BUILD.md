# Autonomous build model for missive

`missive` uses a strict ticket-driven build loop.

## Invariant

One autonomous cycle should complete at most one ticket and produce at most one ticket commit.

## Build-loop contract

Each cycle:

1. verifies required files exist
2. refuses an uncustomised project brief
3. refuses a dirty working tree
4. checks whether upstream advanced
5. identifies the lowest-numbered TODO/IN_PROGRESS ticket
6. invokes `scripts/run-agent.sh`
7. requires the agent to run `scripts/quality-gate.sh`
8. requires a clean working tree after the agent returns
9. requires a new commit
10. pushes unless `--no-push` is used

## missive-specific expectations

The agent should build a Rust CLI that is:

* A2A-native
* shell-friendly
* agent-callable
* persistent via SQLite
* observable through logs/events
* capable of MPI-like group collectives
* gateway/adapters-ready

## Testing latitude

The agent may install dependencies, use sudo, run local services, run Docker, run fuzzers, run benchmarks, and run long integration suites. Tests should stay local/controlled unless a ticket explicitly configures an external endpoint.
