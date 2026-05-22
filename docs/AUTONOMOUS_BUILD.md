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

If the agent command fails, leaves a dirty working tree, or returns without a new
commit, the loop treats that attempt as failed. By default it resets the checkout
back to the pre-attempt `HEAD`, removes untracked non-ignored files with
`git clean -fd`, waits 600 seconds, and retries the same cycle. Use
`--failure-retry-sleep SECONDS` or `MISSIVE_BUILD_LOOP_FAILURE_RETRY_SLEEP` to
change the delay, and `--no-failure-retry` to clean once and stop.

Token/context-length failures get an extra recovery step before the retry. When
the failed attempt log looks like a model context or token limit was exceeded,
the loop first cleans back to the pre-attempt `HEAD`, then launches a splitter
agent with a reduced prompt context. That splitter must only divide the current
lowest TODO/IN_PROGRESS ticket into two smaller sequential tickets, update notes,
commit the split, and leave the tree clean. The loop then waits the same
failed-cycle retry delay and retries against the newly split queue. Set
`MISSIVE_SPLIT_AGENT_CONTEXT_FILES` to override the splitter prompt files when a
custom agent needs a different minimal context.

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

Use `scripts/bootstrap-tools.sh --check` to inspect local tooling and `scripts/bootstrap-tools.sh` to install Rust components and optional cargo tools. Use `--system-deps` only when OS packages are needed. See [`tooling.md`](tooling.md) for the maintained tool inventory and quality-gate behavior.
