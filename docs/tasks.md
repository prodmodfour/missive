# Tasks and artifacts

`missive task` inspects local A2A task state, refreshes task state from remote
agents, polls for terminal states, requests remote cancellation, and exports
artifacts that missive has already observed.

## List and refresh tasks

Local list:

```bash
missive task list --agent echo --json
missive task list --context ctx-example-1 --state completed --json
```

Remote list for one agent:

```bash
missive task list --agent echo --remote --include-artifacts --page-size 20 --json
```

Remote listing is scoped to one page. The returned `nextPageToken`, when present,
is rendered for callers that want to request the next page.

## Get one task

```bash
missive task get task-example-1 --json
missive task get task-example-1 --agent echo --remote --history-length 5 --json
```

Without `--remote`, `task get` reads the local SQLite row. With `--remote`, it
calls A2A `GetTask`, persists changes, and records redacted task-update events.
When a local task row already records the agent alias, `--agent` can be omitted;
otherwise provide it.

## Wait for completion

```bash
missive task wait task-example-1 --agent echo --timeout 30s --interval 500ms --json
missive task wait task-example-1 --local --timeout 10s --json
```

Deterministic exit codes for automation:

| State | Exit code |
| --- | ---: |
| completed | `0` |
| failed | `80` |
| cancelled | `81` |
| timeout | `82` |
| input-required | `83` |

Non-success waits render the latest task output first, then return a structured
error envelope on stderr in machine-readable modes.

## Cancel a remote task

```bash
missive task cancel task-example-1 --agent echo --json
```

Cancellation calls remote A2A `CancelTask` and persists the returned task. There
is no local-only mark-cancelled mode today.

## Persisted artifacts

Artifacts are available after `send`, `stream`, `task get --remote`, or
`task list --remote --include-artifacts` has observed them:

```bash
missive task artifact list task-example-1 --json
missive task artifact show task-example-1 artifact-stream-example-1 --json
missive task artifact save task-example-1 artifact-stream-example-1 --output ./artifact.txt
missive task artifact export task-example-1 --output-dir ./artifacts
```

Exports sanitize remote filenames to avoid path traversal and refuse to overwrite
existing files unless `--force` is supplied. URL/file-reference artifacts are
exported as JSON manifests rather than dereferenced.

## Current limitations

* `task wait` is a foreground polling loop; it does not attach to gateway jobs.
* Remote `task list --remote` does not auto-page through every remote task.
* Raw remote task JSON is stored in local runtime SQLite. Keep `MISSIVE_HOME` and
  profile state directories outside the repository and treat them as sensitive.

## Smoke coverage

`examples/demo-stream-tasks.sh` covers remote `task list`, `task get`,
`task wait`, and `task artifact list` against the local mock A2A server. It is
run by `examples/run-smoke.sh` and the default quality gate.
