# missive autonomous build system

This directory is a drop-in autonomous build system for `missive`, a lowercase-m Rust CLI for A2A-native agent communication management.

It is based on the ticket-driven pattern from the autonomous build template:

1. read `AGENTS.md`, `PROJECT_BRIEF.md`, `BUILD_TICKETS.md`, and `BUILD_NOTES.md`
2. select the lowest-numbered `TODO` or `IN_PROGRESS` ticket
3. implement only that ticket
4. run `scripts/quality-gate.sh`
5. update tickets and notes
6. commit
7. leave the working tree clean

## Files

```text
AGENTS.md                         Agent rules for autonomous work
PROJECT_BRIEF.md                  missive-specific project brief
BUILD_TICKETS.md                  66 ordered autonomous build tickets
BUILD_NOTES.md                    Current state and cycle notes
tickets.json                      Machine-readable ticket export
tickets/github-issue-bodies.md    Copy/paste GitHub issue bodies
scripts/build-loop.sh             Autonomous loop
scripts/run-agent.sh              Agent wrapper; defaults to pi
scripts/quality-gate.sh           Rust-aware quality gate
scripts/bootstrap-tools.sh        Optional tool installer
scripts/check-no-secrets.sh       Secret guardrail
scripts/check-no-generated-private-files.sh
scripts/create-github-issues.sh   Optional gh-based issue creator
docs/USAGE.md                     How to use this build system
docs/AUTONOMOUS_BUILD.md          Operational model
```

## Quick start

Copy these files into the root of the future/existing `missive` repo, commit them, then run:

```bash
scripts/bootstrap-tools.sh
scripts/build-loop.sh --create-branch feature/autonomous-build --max-cycles 70
```

Run without pushing:

```bash
scripts/build-loop.sh --create-branch feature/autonomous-build --max-cycles 70 --no-push
```

The final ticket sets:

```text
AUTOMATION_STATUS: DONE
```

## Autonomy

The agent may use sudo, install build/test dependencies, run Docker, run local mock servers, run fuzzers, run benchmarks, and use aggressive validation. The only retained limits are repository hygiene and not attacking third-party systems.
