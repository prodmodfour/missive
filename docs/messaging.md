# Messaging

`missive send` performs one non-streaming A2A `SendMessage` request against a
registered agent. It persists the outbound request, the direct response message
or returned task, task linkage, artifacts observed in task responses, and
redacted event-journal entries.

## Basic send

```bash
missive agent add echo http://127.0.0.1:8080 --tag local
missive send echo "Say hello" --json
```

Machine output uses a stable `send_result` envelope. Human output is intended for
interactive terminals; use `--json` or `--ndjson` for automation.

## Message input sources

`send` accepts one or more message parts:

```bash
printf 'hello from stdin' | missive send echo --stdin --json
missive send echo --part text="first part" --part text="second part" --json
missive send echo --json-part '{"kind":"status","ok":true}' --json
missive send echo --file ./notes.txt --mime text/plain --json
missive send echo --file-bytes ./image.png --mime image/png --json
```

Part behavior:

* positional `[MESSAGE]`, `--stdin`, and `--part text=...` create text parts.
* `--json-part` creates an A2A structured data part and defaults to
  `application/json` metadata.
* `--file` sends a canonical local `file://` reference. This can reveal the local
  path to the remote agent and in local runtime state.
* `--file-bytes` embeds raw bytes in the request. Use it only for content you are
  willing to store in the local SQLite request row and send to the remote agent.
* `--mime` applies media type metadata. When several non-text parts are present,
  provide one MIME value for all non-text parts or one value per part.

The selected profile's `qos.max_request_bytes` bounds local text, JSON, file
reference, byte attachments, and the serialized A2A request. Oversized requests
fail locally; missive does not chunk large uploads yet.

## Context, task, output mode, and metadata

```bash
missive send echo "Continue the plan" \
  --context ctx-planning \
  --task task-123 \
  --accepted-output-mode text/plain \
  --metadata workflow=planning \
  --json
```

Use `--context` for A2A context continuity. Use `--task` only when the remote
protocol state allows follow-up input on that task. Metadata should be non-secret;
normal command output is redacted, but local runtime state can contain request
content.

## Auth and service parameters

Outbound auth and A2A service parameters can come from config or global flags:

```bash
MISSIVE_ECHO_TOKEN=example \
  missive send echo "Authenticated hello" \
    --bearer-token-env MISSIVE_ECHO_TOKEN \
    --protocol-version 1.0 \
    --a2a-extension demo-extension \
    --service-param A2A-Tenant=local-demo \
    --json
```

CLI auth headers are used for the current invocation and are not persisted as
raw secrets. Gateway-executed background jobs currently do not resolve CLI auth
flags; see [`gateway.md`](gateway.md) for that limitation.

## Persistence and follow-up commands

After a send returns a task, use the task and event commands:

```bash
missive task get task-123 --agent echo --remote --json
missive task artifact list task-123 --json
missive events list --type a2a.send.response --json
```

`send` is non-streaming. Use [`streaming.md`](streaming.md) for incremental
A2A updates.

## Smoke coverage

`examples/demo-send.sh` covers a non-streaming send with context, metadata, an
accepted output mode, and event inspection against the local mock A2A server. It
is run by `examples/run-smoke.sh` and the default quality gate.
