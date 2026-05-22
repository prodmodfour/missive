# Collectives

`missive` collective commands coordinate groups of registered A2A agents. The
currently implemented collectives are broadcast, barrier, and gather; reduce
remains a future ordered ticket and is not available yet.

## Broadcast: `missive bcast`

`missive bcast <group> <message>` sends the same non-streaming A2A
`SendMessage` content to every member of a local group.

```bash
MISSIVE_HOME=/tmp/missive-demo missive group show team --json
MISSIVE_HOME=/tmp/missive-demo missive bcast team "Draft a plan" --json
MISSIVE_HOME=/tmp/missive-demo missive bcast team "Draft a plan" \
  --execution concurrent \
  --failure-policy continue \
  --context ctx-planning-round \
  --timeout 30s \
  --json
```

Behavior:

* group membership is read from the selected profile SQLite store
* member order follows deterministic group rank order
* one shared context id is generated unless `--context CONTEXT_ID` is provided
* the context id is persisted locally and sent on every outbound A2A message
* message input flags match `missive send` for text, stdin, file references,
  file bytes, JSON data parts, MIME values, metadata, and accepted output modes
* each successful member uses normal Agent Card discovery/cache, interface
  negotiation, auth headers, A2A service parameters, and send persistence
* successful task responses create/update local task rows and message rows
* broadcast lifecycle events are appended as `missive.bcast.*`

Machine output uses `kind: "bcast_result"`. The `data.members[]` entries include
agent alias, rank, status, request message id, selected interface, response
shape, task id, context id, mapped task state, and structured errors where a
member failed.

## Execution and failure policy

`--execution sequential` is the default and sends to one member at a time.
`--execution concurrent` resolves members first, then performs outbound A2A
sends in worker threads and persists results in rank order.

`--failure-policy stop` stops after the first sequential failure and exits
non-zero after printing a summary. `--failure-policy continue` sends remaining
members and returns success when at least one member succeeded, while marking the
summary `status` as `partial_failure`.

Global `--timeout` bounds Agent Card fetches and member sends for this command.
A timeout prints the same `bcast_result` summary and exits with code `82`.

## Barrier: `missive barrier`

`missive barrier <group> --context <id>` waits for tasks belonging to each group
member in one shared A2A context. It is intended to follow `bcast`, but can also
operate on any locally known member tasks in the context.

```bash
MISSIVE_HOME=/tmp/missive-demo missive bcast team "Draft a plan" \
  --context ctx-planning-round \
  --json > bcast.json
MISSIVE_HOME=/tmp/missive-demo missive barrier team --context ctx-planning-round --json
MISSIVE_HOME=/tmp/missive-demo missive barrier team --from-bcast bcast.json --json
MISSIVE_HOME=/tmp/missive-demo missive barrier team \
  --context ctx-planning-round \
  --required 2 \
  --failure-policy continue \
  --timeout 2m \
  --interval 2s \
  --json
```

Behavior:

* member order follows deterministic group rank order
* task ids come from `--from-bcast` member output when supplied, otherwise from
  the latest local task row for each member in the selected context
* remote polling is the default and refreshes known task ids with A2A `GetTask`
* `--local` polls only SQLite rows, which is useful with gateway-updated state
  or deterministic tests
* when `--state` is omitted, the command waits for terminal states but only
  `completed` counts toward successful quorum
* repeatable `--state STATE` makes the requested states the satisfying states
* `--required N` sets quorum and defaults to all members
* `--failure-policy stop` exits on the first non-requested failure/cancellation;
  `--failure-policy continue` allows partial failure while quorum remains
  possible
* lifecycle and member events are appended as `missive.barrier.*`, and remote
  task refreshes append normal `a2a.task.updated` events when state changes

Machine output uses `kind: "barrier_result"`. The summary includes status,
quorum, target/success states, counts, attempts, timeout/interval, and one member
row per rank. Member rows include agent alias, rank, status, task id, context id,
state, source, updated timestamp, selected interface for remote polls, and
structured errors.

Barrier exits are deterministic: success `0`, failed or impossible quorum `80`,
cancelled `81`, and timeout `82`.

## Gather: `missive gather`

`missive gather <group> --context <id>` collects the latest locally known task
output and persisted artifacts for every group member in deterministic rank
order. It is intended to follow `bcast` and `barrier`, or any workflow that has
already populated local task/message/artifact rows.

```bash
MISSIVE_HOME=/tmp/missive-demo missive gather team --context ctx-planning-round
MISSIVE_HOME=/tmp/missive-demo missive gather team --context ctx-planning-round --json
MISSIVE_HOME=/tmp/missive-demo missive gather team --context ctx-planning-round --ndjson
MISSIVE_HOME=/tmp/missive-demo missive gather team \
  --context ctx-planning-round \
  --output-dir ./gathered-artifacts \
  --json
```

Behavior:

* member order follows deterministic group rank order
* the latest local task row for each member/context is selected by updated time,
  with task id as a deterministic tie-breaker
* output text is chosen from the latest persisted non-request message for the
  task, then the task status message, then the first text artifact preview
* missing tasks are represented as member rows with `status: "missing_task"`
  rather than causing the whole command to fail
* known tasks with no text, messages, or artifacts are represented with
  `status: "empty_output"`
* human output is markdown; machine output uses `kind: "gather_result"` for both
  `--json` and single-line `--ndjson`
* `--output-dir DIR` writes gathered artifacts with sanitized, rank-prefixed,
  deterministic filenames and refuses to overwrite existing files unless
  `--force` is supplied
* lifecycle and member events are appended as `missive.gather.*`

Machine output includes the operation id, group, context id, status, member
counts, message/artifact/export counts, and one member row per rank. Member rows
include agent alias, rank, status, task summary, selected text, output message
summaries, artifact summaries, and artifact export records when used.

## Current limitations

`bcast` does not stream responses. `barrier` synchronizes task state but does not
collect outputs or artifacts by itself; run `gather` afterward to collect the
latest local outputs. `gather` is local-only and does not refresh remote task
state, subscribe to tasks, or reduce/summarize member outputs. Run `barrier` or
`task get --remote` first when fresh remote state is required. Reduce is not
implemented yet. Concurrent broadcast mode does not cancel already-started worker
threads when another member fails; it reports all completed outcomes in the
summary.
