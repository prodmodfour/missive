# Collectives

`missive` collective commands coordinate groups of registered A2A agents. The
current implemented collective is broadcast; barrier, gather, and reduce remain
future ordered tickets and are not available yet.

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

## Current limitations

`bcast` does not stream responses and does not wait for returned tasks to reach a
terminal state. Use `missive task wait` for returned task ids until the barrier
collective is implemented. Gather and reduce are not implemented yet. Concurrent
mode does not cancel already-started worker threads when another member fails;
it reports all completed outcomes in the summary.
