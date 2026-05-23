# Collectives

`missive` collective commands coordinate groups of registered A2A agents. The
currently implemented collectives are broadcast, barrier, gather, and reduce.
For a runnable local workflow with three mock A2A agents and machine-readable
outputs, run [`examples/demo-multi-agent.sh`](../examples/demo-multi-agent.sh) or
see [`multi-agent-demo.md`](multi-agent-demo.md).

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

## Reduce: `missive reduce`

`missive reduce <group> --context <id>` consumes the local gathered state for a
group/context and produces one final reduced result with source references.

```bash
MISSIVE_HOME=/tmp/missive-demo missive reduce team --context ctx-planning-round --json
MISSIVE_HOME=/tmp/missive-demo missive reduce team --context ctx-planning-round --strategy vote --json
MISSIVE_HOME=/tmp/missive-demo missive reduce team \
  --context ctx-planning-round \
  --strategy custom \
  --template 'Combine {{input_count}} inputs from {{group}}:\n{{inputs}}' \
  --json
MISSIVE_HOME=/tmp/missive-demo missive reduce team \
  --context ctx-planning-round \
  --reducer-agent editor \
  --strategy summarise \
  --json
MISSIVE_HOME=/tmp/missive-demo missive reduce team \
  --context ctx-planning-round \
  --command 'python3 ./scripts/reducer.py' \
  --json
```

Behavior:

* member order and latest-task selection match `gather`
* the command is local-first and does not refresh remote tasks before reducing
* local deterministic strategies are `summarise`/`summarize`, `vote`, `merge`,
  `rank`, and `custom`
* `custom` requires `--template`; other strategies can optionally use a template
  to shape local output or the prompt sent to an external reducer
* templates support `{{group}}`, `{{context_id}}`, `{{strategy}}`,
  `{{input_count}}`, `{{inputs}}`, and `{{default_reduction}}`
* `--reducer-agent ALIAS` sends the generated prompt as an A2A `SendMessage` to
  a registered agent and persists the normal send request/response rows
* `--command COMMAND` writes the generated prompt to a local shell command's
  stdin and treats UTF-8 stdout as the reduced result
* one local `messages` row with direction `local` records the final reduced
  output plus provenance metadata
* lifecycle and provenance events are appended as `missive.reduce.*`

Machine output uses `kind: "reduce_result"`. The summary includes operation id,
strategy, reducer method, final `reduced_text`, persisted reduced-message id,
member/input counts, and a `provenance` array. Provenance rows include agent,
rank, status, task summary, selected text when available, source message ids, and
source artifact ids/kinds/versions.

Failure modes:

* missing group, empty group, invalid context id, or missing reducer agent fail
  validation before reducing
* `--strategy custom` without `--template` fails validation
* `--reducer-agent` and `--command` are mutually exclusive
* no gathered outputs in the selected context fails with a usage error instead
  of producing an empty reduction
* reducer-agent transport/protocol failures use the same A2A diagnostics as
  `send`
* command reducers fail when the shell command exits nonzero, writes invalid
  UTF-8, or writes empty stdout

## Current limitations

`bcast` does not stream responses. `barrier` synchronizes task state but does not
collect outputs or artifacts by itself; run `gather` afterward to collect the
latest local outputs. `gather` and `reduce` are local-only and do not refresh
remote task state or subscribe to tasks. Run `barrier` or `task get --remote`
first when fresh remote state is required. Local `summarise` is an attributed
summary template rather than an LLM semantic summary; use `--reducer-agent` or
`--command` for richer reduction. Command reducers execute user-supplied local
commands and are not sandboxed by missive. Concurrent broadcast mode does not
cancel already-started worker threads when another member fails; it reports all
completed outcomes in the summary.
