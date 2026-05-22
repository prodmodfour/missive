# Usage

## 1. Put these files in the missive repo

Copy the contents of this directory into the root of the `missive` repository.

The project brief is already customised:

```text
TEMPLATE_CUSTOMISED: true
```

## 2. Optional: inspect or install tools

```bash
scripts/bootstrap-tools.sh --check
scripts/bootstrap-tools.sh
```

The script is executable, idempotent, and intentionally best-effort. It skips installed tools and warns when optional tools cannot be installed. Use `scripts/bootstrap-tools.sh --system-deps` when a ticket needs supported OS packages such as `jq`, `protoc`, `sqlite3`, or `pkg-config`.

The agent may also install tools manually with `sudo`, package managers, `rustup`, or `cargo install` as needed for build/test dependencies. Record notable installation commands in `BUILD_NOTES.md`. See [`tooling.md`](tooling.md) for details.

## 3. Run one cycle

```bash
scripts/build-loop.sh
```

## 4. Run many cycles

The queue has 66 tickets. Use a slightly higher max cycle count to allow blocked/retry cycles:

```bash
scripts/build-loop.sh --create-branch feature/autonomous-build --max-cycles 70
```

## 5. Run without pushing

```bash
scripts/build-loop.sh --create-branch feature/autonomous-build --max-cycles 70 --no-push
```

## 6. Failed-cycle cleanup and retry

By default, if an agent cycle fails, leaves the working tree dirty, or returns
without creating a commit, `scripts/build-loop.sh` resets back to the cycle's
starting `HEAD`, removes untracked non-ignored files with `git clean -fd`, waits
600 seconds, and retries the same cycle.

```bash
scripts/build-loop.sh --failure-retry-sleep 600
MISSIVE_BUILD_LOOP_FAILURE_RETRY_SLEEP=600 scripts/build-loop.sh
scripts/build-loop.sh --no-failure-retry
```

## 7. Use a different agent

Edit only:

```text
scripts/run-agent.sh
```

The default wrapper expects `pi`:

```bash
pi --no-session -p @AGENTS.md @PROJECT_BRIEF.md @BUILD_TICKETS.md @BUILD_NOTES.md "$PROMPT"
```

## 8. Create GitHub issues from tickets

After installing `gh` and `jq`, run:

```bash
scripts/create-github-issues.sh --repo OWNER/missive
```

This is optional. The autonomous loop uses `BUILD_TICKETS.md` as the source of truth.

## 9. Aggressive testing

```bash
MISSIVE_AGGRESSIVE_TESTS=1 scripts/quality-gate.sh
```

The agent is encouraged to run aggressive checks where feasible and record them in `BUILD_NOTES.md`.
