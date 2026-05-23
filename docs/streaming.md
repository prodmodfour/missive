# Streaming

`missive stream` starts one A2A `SendStreamingMessage` request, renders incoming
SSE updates, and persists stream events as they arrive.

## Basic stream

```bash
missive stream echo "Show progress" --ndjson
```

Use `--ndjson` for automation: each line is one JSON object, typically a
`stream_event` followed by a final `stream_result`. Human output is suitable for
watching progress interactively. `--json` returns the final summary as one JSON
document after the stream completes.

## Inputs match `send`

Streaming uses the same message-part parser as `missive send`:

```bash
missive stream echo --part text="analyze" --json-part '{"priority":"low"}' --ndjson
missive stream echo --file ./notes.txt --mime text/plain --ndjson
missive stream echo --file-bytes ./image.png --mime image/png --ndjson
```

The same request-size, local-file, MIME, metadata, context, task, auth, and A2A
service-parameter notes from [`messaging.md`](messaging.md) apply.

## Capability validation

By default, missive checks the cached or freshly fetched Agent Card and refuses
streaming when `capabilities.streaming` is not advertised:

```bash
missive agent inspect echo --refresh --json
missive stream echo "Show progress" --ndjson
```

For compatibility testing against an endpoint you know supports streaming even
when its card is incomplete, pass `--force`:

```bash
missive stream echo "Try anyway" --force --ndjson
```

## Persistence

Stream status, task, message, and artifact updates are appended to the local
journal as `a2a.stream.*` events. Task and artifact updates also refresh local
task/artifact rows where IDs are present:

```bash
missive events list --type a2a.stream.status --json
missive task list --agent echo --json
missive task artifact list task-stream-example-1 --json
```

## Current limitations

* The foreground stream command does not resume after the process exits.
* Gateway task subscription/resume exists for cached streaming-capable tasks, but
  the foreground stream command does not attach to a gateway-managed stream.
* Local cancellation of an active foreground stream is by process control; use
  `task cancel` after a task id is known.

## Smoke coverage

`examples/demo-stream-tasks.sh` covers `stream --ndjson`, remote task list/get,
`task wait`, and artifact listing against the local mock A2A server. It is run by
`examples/run-smoke.sh` and the default quality gate.
