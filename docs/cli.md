# CLI reference

`missive` currently exposes a clap-based command tree with shared flags,
configuration discovery, profile validation, and stable human/JSON/NDJSON/quiet
rendering. Implemented operational commands are the SQLite-backed agent registry
commands, public A2A Agent Card inspection/refresh, non-streaming
`missive send`, streaming `missive stream`, `missive task get/list/wait/cancel`,
and `missive context create/list/show/fork/close/export`; other top-level
commands still emit skeletal parsed status until their ordered tickets land.

Run help with:

```bash
missive --help
missive agent --help
missive send --help
missive stream --help
missive task --help
missive context --help
```

## Global flags

The following flags are accepted at the top level and after subcommands:

* `--json` — request single-document JSON output when a command supports it.
* `--ndjson` — request newline-delimited JSON for event streams when supported.
* `--quiet` / `-q` — suppress non-error output.
* `--no-color` — disable colored terminal output and diagnostics.
* `--config <PATH>` — select an explicit configuration file path.
* `--profile <PROFILE>` — select a named profile.
* `--timeout <DURATION>` — set an overall timeout string such as `30s` or `2m`.
* `--protocol-version <VERSION>` — override the A2A protocol version sent as
  `A2A-Version` for implemented outbound A2A requests.
* `--a2a-extension <EXTENSION>` — append an A2A extension requested through
  `A2A-Extensions`; repeatable.
* `--service-param NAME=VALUE` — add or override an arbitrary non-auth A2A
  service parameter for implemented outbound A2A HTTP requests; repeatable.
* `--bearer-token-env ENV` — read `ENV` and send `Authorization: Bearer <value>`
  for implemented outbound A2A HTTP requests.
* `--header Name:Value` — add one outbound HTTP header for this invocation;
  repeatable and never persisted.
* `--trace` — request trace-oriented diagnostics.
* `--verbose` / `-v` — increase human diagnostic verbosity; repeat as needed.

Configuration discovery now supports `--config`, `MISSIVE_CONFIG`, XDG config
locations, and repository-local `missive.toml`/`.missive.toml` when explicitly
requested with `MISSIVE_REPO_CONFIG=1`. `--profile` selects and validates a named
profile. See [`configuration.md`](configuration.md) for the schema and discovery
order. Protocol service-parameter and auth header flags are currently applied to
Agent Card HTTP requests plus implemented send, stream, and remote task calls,
and are shared with future A2A push clients. Task wait uses global `--timeout`;
tracing and broader command-specific timeout semantics are intentionally left to
their ordered implementation tickets.

## Top-level commands

The current skeleton includes these top-level commands:

```text
agent       Manage configured A2A agents and cached Agent Cards
send        Send one message to an A2A agent
stream      Stream message updates from an A2A agent
task        Inspect, list, wait for, or cancel A2A tasks
context     Manage conversation contexts and session continuity
group       Manage groups of agents for collective operations
gateway     Run and manage the local missive gateway daemon
webhook     Receive A2A push notification callbacks locally
push        Manage A2A push notification configurations
doctor      Diagnose local configuration, storage, gateway, and endpoint health
logs        Inspect local missive logs
events      Inspect, tail, replay, or export the local event journal
completion  Generate shell completion scripts
manpage     Generate manual pages
```

Each top-level command has a help page. Running an unimplemented command other
than help currently loads and validates configuration, then emits a
command-status record through the selected renderer. Implemented command groups
with no selected subcommand, such as `missive context --json`, still emit that
parsed command-status record so automation can distinguish parser support from a
specific operation.

## Agent registry commands

`missive agent` manages profile-scoped A2A agent aliases in the local SQLite
store. Runtime state is resolved through the loaded profile and defaults outside
the repository; use `MISSIVE_HOME=<absolute-dir>` in disposable demos/tests when
you want an isolated registry.

Implemented commands:

```bash
missive agent add <alias> <base-url> \
  --interface http+json=http://127.0.0.1:8080/a2a \
  --binding-preference http+json \
  --auth-ref example-env \
  --tag local \
  --notes "Local mock agent" \
  --metadata role=echo
missive agent list
missive agent show <alias>
missive agent inspect <alias> [--refresh] [--binding http+json|json-rpc]
missive agent refresh <alias>
missive agent remove <alias>
missive agent rename <old-alias> <new-alias>
```

Agent aliases are validated with the shared lowercase CLI-safe identifier rules.
`agent add` refuses duplicate aliases instead of overwriting existing rows.
`agent show`, `agent remove`, and `agent rename` fail clearly when an alias is
missing. `agent remove` and `agent rename` refuse read-only config-seeded agents;
edit the config file instead.

Supported registry fields:

* alias — command/routing name such as `echo` or `planner-1`
* base URL — absolute `http` or `https` URL without embedded credentials
* explicit interface URLs — repeat `--interface BINDING=URL`
* binding preference — repeat `--binding-preference BINDING` to override the
  default `http+json`, `json-rpc` order
* auth refs — `--auth-ref NAME`, where `NAME` must exist in the loaded config
* tags — repeat `--tag TAG`
* notes — one short human notes string
* metadata — repeat `--metadata KEY=VALUE`; `VALUE` is parsed as JSON when
  possible and otherwise stored as a string

Config file `[agents.<alias>]` entries are synced into the profile database as
`source = "config_seed"` and `read_only = true` before agent registry commands
run. This makes config-seeded entries visible in `agent list`/`agent show` while
preserving the config file as their source of truth. Cached Agent Card fields are
preserved for config-seeded agents while their configured `base_url` remains the
same.

### Agent Card discovery

`missive agent inspect <alias>` resolves the registered agent base URL, fetches
`/.well-known/agent-card.json` when no cached card exists, parses the public A2A
Agent Card, stores the raw card JSON plus cache metadata in SQLite, and renders a
summary of provider, versions, capabilities, supported interfaces, the selected
interface, default media modes, and skills. Re-running `inspect` uses the local
cache by default.

Use `missive agent inspect <alias> --refresh` to bypass/revalidate the cache for
one inspection, or `missive agent refresh <alias>` to explicitly refresh the
cached public card. Use `missive agent inspect <alias> --binding json-rpc` to
require a specific locally supported binding for the inspection result. When a
cached ETag or Last-Modified value is available, missive sends conditional
request headers during refresh and keeps the cached card if the remote endpoint
replies `304 Not Modified`. Every Agent Card fetch also sends `A2A-Version`
(default `1.0` unless config or `--protocol-version` overrides it), plus any
configured/CLI `A2A-Extensions` and extra service parameters. If the agent row
has an `auth_ref`, or the invocation passes `--bearer-token-env`/`--header`, the
resolved auth headers are sent on the fetch/refresh request. Missing environment
variables fail with `missive::auth` before any HTTP request is attempted.

Interface negotiation uses the agent row's binding preference, which defaults to
`http+json`, then `json-rpc`. Agent Card values such as `HTTP+JSON` and
`JSONRPC` are normalized to those lowercase missive names for comparison. `gRPC`
is recognized for diagnostics but not implemented locally yet. If an Agent Card
omits `supportedInterfaces`, missive falls back to explicit registry/config
interface URLs and then to the registered `base_url` for `http+json` only.

Discovery and negotiation failures have deterministic categories: HTTP status
errors, TLS/network failures, and lack of a mutually supported interface are
transport errors, while invalid or schema-incompatible Agent Card JSON is a
protocol error. If the remote error body reports A2A `VERSION_NOT_SUPPORTED`,
missive returns a protocol error with exit code `76`.

Human output is concise text. Machine output uses command-specific envelope
kinds such as `agent_add`, `agent_list`, `agent_show`, `agent_inspect`,
`agent_refresh`, `agent_remove`, and `agent_rename`:

```bash
MISSIVE_HOME=/tmp/missive-demo missive agent add echo http://127.0.0.1:8080 --tag local
MISSIVE_HOME=/tmp/missive-demo missive agent inspect echo --json
MISSIVE_HOME=/tmp/missive-demo missive agent refresh echo --bearer-token-env MISSIVE_AGENT_TOKEN
MISSIVE_HOME=/tmp/missive-demo missive agent list --json
```

## Send command

`missive send` sends one non-streaming A2A `SendMessage` request to a registered
agent. The command uses the cached Agent Card when present, otherwise fetches and
caches the public card before selecting the first mutually supported interface
from the agent's binding preference. HTTP+JSON sends `POST /message:send` under
the selected interface URL; JSON-RPC sends method `SendMessage` to the selected
JSON-RPC URL.

Basic examples:

```bash
MISSIVE_HOME=/tmp/missive-demo missive agent add echo http://127.0.0.1:8080
MISSIVE_HOME=/tmp/missive-demo missive send echo "Say hello" --json
printf 'hello from stdin' | MISSIVE_HOME=/tmp/missive-demo missive send echo --stdin
MISSIVE_HOME=/tmp/missive-demo missive send echo --file ./prompt.txt --mime text/plain
MISSIVE_HOME=/tmp/missive-demo missive send echo --file-bytes ./image.png --mime image/png
MISSIVE_HOME=/tmp/missive-demo missive send echo --json-part '{"kind":"example","ok":true}'
MISSIVE_HOME=/tmp/missive-demo missive send echo --part text='first part' --part text='second part'
```

Supported inputs now cover A2A text, file, raw-byte, and structured-data parts:

* positional `[MESSAGE]` creates one A2A text part
* `--stdin` reads one UTF-8 text part from standard input
* repeatable `--part text=VALUE` adds explicit text parts
* repeatable `--file PATH` validates a safe local regular file and sends an A2A
  file-reference part using a canonical `file://` URL plus the safe filename;
  file bytes are not embedded
* repeatable `--file-bytes PATH` validates and embeds a safe local regular file
  as an A2A raw byte part; the official A2A JSON serialization base64-encodes
  the `raw` field and preserves the safe filename
* repeatable `--json-part JSON` parses inline JSON into an A2A structured
  `data` part; `mediaType` defaults to `application/json`
* repeatable `--mime MIME` assigns `mediaType` metadata. One value applies to
  all non-text file/JSON parts; multiple values map to non-text parts in the
  command's deterministic part-building order, or to every part when the count
  equals the total part count.
* repeatable `--metadata KEY=VALUE` adds non-secret A2A request metadata, parsing
  `VALUE` as JSON when possible and otherwise treating it as a string
* `--context CONTEXT_ID` and `--task TASK_ID` set A2A continuity fields on the
  outbound message and link persisted local rows
* repeatable `--accepted-output-mode MIME` populates
  `configuration.acceptedOutputModes`

Local file paths are canonicalized, must point at regular files, and contribute
to the selected profile's `qos.max_request_bytes` limit. The serialized A2A
`SendMessageRequest` is also checked against that limit. Oversized local inputs
fail locally with a validation error; streaming/chunked file upload is not
implemented yet.

`send` resolves auth the same way as Agent Card commands: agent auth refs,
`--bearer-token-env`, and repeatable `--header Name:Value` are applied to both
the Agent Card fetch (when needed) and the send request, while raw secret values
are never persisted or printed. A2A service parameters (`A2A-Version`,
`A2A-Extensions`, and `--service-param`) are also sent on the outbound request
and recorded in local message/task metadata.

Machine-readable output uses `kind: "send_result"` and includes the selected
interface, outbound request summary, part summaries (`kind`, source, filename,
media type, local byte count), response shape (`message` or `task`), raw redacted
response JSON, and local persistence ids. Task responses include `task_id`,
`context_id`, and mapped state when the remote server returns them.
Human output is a concise one-line send summary. Request and response rows are
stored in SQLite `messages`; returned task responses are stored or updated in
`tasks` and linked to the messages.

## Stream command

`missive stream` sends an A2A `SendStreamingMessage` request to a registered
agent and reads the server's Server-Sent Events (SSE) response. It shares the
same rich input parser as `send`: positional `[MESSAGE]`, `--stdin`, repeatable
`--part text=VALUE`, repeatable `--file PATH`, repeatable `--file-bytes PATH`,
repeatable `--json-part JSON`, repeatable `--mime MIME`, repeatable
`--metadata KEY=VALUE`, `--context`, `--task`, and repeatable
`--accepted-output-mode`.

Before opening the stream, missive fetches or uses the cached Agent Card and
requires `capabilities.streaming = true`. If an interoperability test endpoint
streams despite an incomplete card, pass `--force` to attempt the request anyway.
Without `--force`, missing or false streaming capability is a usage/validation
error and no `message:stream` request is sent.

Transport mapping follows the negotiated interface:

* `http+json` appends `message:stream` to the selected interface URL and sends
  `POST <interface>/message:stream` with `Content-Type: application/a2a+json`
  and `Accept: text/event-stream`.
* `json-rpc` posts method `SendStreamingMessage` to the selected JSON-RPC URL
  and expects SSE `data` fields containing JSON-RPC responses. The parser also
  accepts direct A2A stream-response objects for HTTP+JSON interoperability.

Examples:

```bash
MISSIVE_HOME=/tmp/missive-demo missive stream echo "Show progress"
MISSIVE_HOME=/tmp/missive-demo missive stream echo "Show progress" --ndjson
printf 'stream from stdin' | MISSIVE_HOME=/tmp/missive-demo missive stream echo --stdin --json
MISSIVE_HOME=/tmp/missive-demo missive stream echo --json-part '{"phase":"draft"}' --force --json
MISSIVE_HOME=/tmp/missive-demo missive stream echo --file-bytes ./frame.png --mime image/png --force --ndjson
```

Human output prints one redacted status line per stream event as it arrives, then
a final summary. `--ndjson` emits one `kind: "stream_event"` envelope per SSE
event with monotonically increasing `sequence` values and a final
`kind: "stream_result"` summary line. `--json` collects the parsed events and
prints one final `stream_result` document after the stream closes. `--quiet`
persists events but prints no non-error output.

Each stream event is appended to the SQLite `events` journal as
`a2a.stream.task`, `a2a.stream.message`, `a2a.stream.status_update`, or
`a2a.stream.artifact_update` with redacted payload JSON and A2A protocol-version
metadata. A `messages` row with direction `stream_event` is also written for each
parsed event, status updates update the local `tasks` row state, and task/artifact
updates are linked to their task/context IDs when the remote payload includes
them. Artifacts embedded in task payloads are persisted to dedicated `artifacts`
rows, and `artifactUpdate` chunks with `append=true` are merged into the same row
with an incremented local version.

Malformed SSE data, JSON-RPC stream errors, unsupported protocol-version
responses, and stream payloads that are not one of `task`, `message`,
`statusUpdate`, or `artifactUpdate` fail with deterministic protocol or transport
errors. If an error occurs mid-stream, earlier event rows and any earlier
human/NDJSON output remain as a truthful partial stream record.

## Task commands

`missive task` manages the local task view and the initial remote A2A task
operations:

```bash
MISSIVE_HOME=/tmp/missive-demo missive task list --json
MISSIVE_HOME=/tmp/missive-demo missive task list --agent echo --state working --source remote
MISSIVE_HOME=/tmp/missive-demo missive task list --agent echo --remote --context ctx-1 --updated-after 2026-05-21T00:00:00Z --json
MISSIVE_HOME=/tmp/missive-demo missive task get task-123 --json
MISSIVE_HOME=/tmp/missive-demo missive task get task-123 --agent echo --remote --history-length 10 --json
MISSIVE_HOME=/tmp/missive-demo missive task wait task-123 --agent echo --timeout 2m --interval 2s --json
MISSIVE_HOME=/tmp/missive-demo missive task cancel task-123 --agent echo
MISSIVE_HOME=/tmp/missive-demo missive task artifact list task-123 --json
MISSIVE_HOME=/tmp/missive-demo missive task artifact show task-123 artifact-1 --json
MISSIVE_HOME=/tmp/missive-demo missive task artifact save task-123 artifact-1 --output ./artifact.txt
MISSIVE_HOME=/tmp/missive-demo missive task artifact export task-123 --output-dir ./artifacts
```

Local `task list` filters SQLite rows by `--agent`, `--context`, `--state`,
`--updated-after`, and `--source remote|local|gateway`. `task get` reads the
local row by default and can enforce `--agent`/`--source` on that row.

Passing `--remote` to `task get` or `task list` negotiates the agent interface,
applies the same service-parameter and auth-header handling used by send/stream,
queries the remote A2A `GetTask` or `ListTasks` endpoint, persists returned tasks
back to SQLite, and then renders the updated local task view. Remote `task list`
is scoped to one agent, so `--agent` is required with `--remote`. Remote list
filters are also sent to A2A when supported: context, state, updated-after,
page size/token, history length, and include-artifacts.

`task wait` polls remote `GetTask` by default. It resolves the agent from
`--agent` or from an existing local task row, uses global `--timeout` for the wait
budget, and uses `--interval` for the poll cadence. `--local` polls only the
local SQLite row for future gateway-managed updates. Wait prints a final
`task_wait` document before returning the deterministic process code:

* `0` when the task reaches `completed`
* `80` when the task reaches `failed`
* `81` when the task reaches `cancelled`
* `82` when the wait times out
* `83` when the task reaches `input_required`

When a non-success wait state is reached in `--json`/`--ndjson` mode, stdout
contains the final task output and stderr contains the structured error envelope
with the same exit code. This keeps task state machine-readable while preserving
shell-friendly nonzero statuses.

`task cancel` sends A2A `CancelTask` to the resolved remote agent, persists the
returned task, and renders `task_cancel`. If no local task row exists yet, pass
`--agent` so missive knows which remote agent to call.

Task output includes the mapped local state, source, agent, context, protocol
version, status text when present, artifact/history counts, artifact metadata
summaries from dedicated rows, timestamps, metadata, and redacted raw remote task
JSON when available.

`missive task artifact` operates on artifacts already persisted in the selected
profile's SQLite store. Run `task get --remote`, `task list --remote
--include-artifacts`, `send`, or `stream` first when a remote task has not been
cached locally. `task artifact list` renders stored artifact ids, names, kinds,
MIME types, versions, metadata, and text previews. `task artifact show` adds a
part summary and raw A2A artifact JSON in machine-readable output. `task artifact
save` writes one artifact to a user-selected path or, when `--output` points at
an existing directory, to a sanitized filename derived from the remote artifact.
`task artifact export` writes every artifact for a task into `--output-dir` and
creates the directory when missing. Remote filenames and artifact names are
sanitized so `../` path traversal cannot escape the chosen directory; existing
files are not overwritten unless `--force` is supplied. Text parts are written as
UTF-8 text, JSON data parts as pretty JSON, inline raw parts as bytes, and URL
file-reference artifacts as JSON manifests rather than fetching remote or local
URLs.

## Context commands

`missive context` manages local A2A `contextId` continuity records. Contexts live
in the selected profile's SQLite store, can have human-friendly names, and are
linked automatically by existing send, stream, and task persistence whenever A2A
payloads include a context id.

Implemented commands:

```bash
MISSIVE_HOME=/tmp/missive-demo missive context create --name "Planning round" --agent echo --json
MISSIVE_HOME=/tmp/missive-demo missive context create --id ctx-1 --name "Planning round" --summary "Initial plan"
MISSIVE_HOME=/tmp/missive-demo missive context list --agent echo --state open
MISSIVE_HOME=/tmp/missive-demo missive context show "Planning round" --json
MISSIVE_HOME=/tmp/missive-demo missive context fork "Planning round" --name "Planning follow-up" --json
MISSIVE_HOME=/tmp/missive-demo missive context close "Planning follow-up" --summary "Closed after review"
MISSIVE_HOME=/tmp/missive-demo missive context export "Planning round" --json
```

`context create` accepts an explicit `--id CONTEXT_ID` or generates a local
UUIDv7-compatible A2A context id when omitted. Optional `--name`, `--agent`,
`--summary`, and repeatable `--metadata KEY=VALUE` fields are stored without raw
secret material. If `--agent` is supplied, it must reference a known agent alias.
Context names may contain spaces when quoted and must be unique among named
contexts so commands can resolve them safely.

`context list` filters local context rows by `--agent`, exact `--name`,
`--state open|closed|archived`, and `--parent CONTEXT_ID`. `context show`,
`context close`, `context fork`, and `context export` accept either an exact A2A
context id or a unique human-friendly name. If a selector is both an existing id
and another context's name, the id wins; ambiguous duplicate names fail with an
actionable error.

`context fork` creates a new open child context, records the parent id in both
the typed `parent_context_id` column and metadata (`missive.context.parent_id`,
plus parent name and fork timestamp when available), and inherits the parent
agent unless `--agent` is supplied. Forking does not copy messages, tasks,
events, or artifacts; it records continuity metadata for later workflows.

`context close` marks the context `closed`, records `closed_at` once, and retains
existing messages, tasks, events, metadata, and summaries for export. There is no
remote A2A close call in the current protocol mapping; closing is local state
only.

`context export` renders a redacted local export containing the context record and
all currently linked `tasks`, `messages`, and `events`. Use `--json` or
`--ndjson` for the full export payload; human mode prints a concise redacted
summary. Export payloads recursively redact secret-like keys and HTTP auth
headers before they reach stdout. For dedicated event filtering, tailing,
NDJSON export, and replay summaries, use `missive events`. Context export does
not include artifact rows/files; use `missive task artifact export` for task
artifacts.

`missive send --context CONTEXT_ID` and `missive stream --context CONTEXT_ID`
continue to take explicit A2A context ids. Use `missive context show <name>
--json` to resolve a human-friendly name to the durable id for shell automation.

Machine-readable context output uses `context_create`, `context_list`,
`context_show`, `context_fork`, `context_close`, and `context_export` envelope
kinds. Context views include the context id, optional name/agent/parent, state,
summary, metadata, timestamps, closed timestamp, and linked message/task/event
counts.

## Event commands

`missive events` exposes the selected profile's append-only SQLite event
journal. The current producers record local agent registry changes, A2A
send/stream request records, A2A send responses, streaming updates, and remote
task changes observed by send/task commands. Future group, gateway, webhook, and
adapter tickets will append the same event table through the existing typed store
API.

Implemented commands:

```bash
MISSIVE_HOME=/tmp/missive-demo missive events list --json
MISSIVE_HOME=/tmp/missive-demo missive events list --agent echo --type a2a.task.updated
MISSIVE_HOME=/tmp/missive-demo missive events tail --from-sequence 100 --limit 10 --ndjson
MISSIVE_HOME=/tmp/missive-demo missive events replay --context ctx-123 --json
MISSIVE_HOME=/tmp/missive-demo missive events export --task task-123 --ndjson
```

Event records include a monotonic `sequence`, stable `event_id`, RFC3339
`timestamp`, `source`, `event_type`, optional agent/context/task/group/gateway
job/adapter links, redacted `payload`, metadata, and a `redacted` flag. Filters
shared by list, tail, replay, and export include `--agent`, `--context`,
`--task`, `--source`, `--type`, and `--since`. List/replay/export also accept
`--after-sequence` and `--limit`; tail uses `--from-sequence`, `--limit`,
`--poll-interval`, and the global `--timeout` as a bounded follow budget.

`events list` renders one `events_list` document in JSON/NDJSON modes. `events
export --ndjson` emits one `event_record` envelope per line for agent/subprocess
consumers. `events tail --ndjson` also emits one `event_record` per newly matched
record as it follows the journal. `events replay` derives deterministic context
and task summaries from the matching events, including event counts, first/last
sequences and timestamps, task membership per context, latest task state when it
can be read from event payloads, and event type counts. Replay is a local
summary reconstruction; it does not call remote A2A agents or mutate tasks.

## Output contract

The current renderer supports four modes:

* default human output — command-specific redacted text for implemented commands
  or one redacted status line for an unimplemented parsed command
* config `output.format = "json"` or `--json` — one JSON document using stable
  top-level fields
* config `output.format = "ndjson"` or `--ndjson` — one JSON object per line;
  stream emits one `stream_event` line per SSE event plus a `stream_result`
  summary, `events export` and `events tail` emit one `event_record` line per
  event, while other implemented commands emit one command-specific envelope and
  skeletal commands emit one command-status event
* config `output.format = "quiet"` or `--quiet` / `-q` — no non-error output

`--json` and `--ndjson` are mutually exclusive for command execution. If both are
provided, `missive` returns usage exit code `64` and renders a structured error;
when `--ndjson` is present, that error is one JSON object on stderr.

Skeletal command machine-readable output has this envelope. The `config` summary
is secret-free and reports only discovery/source metadata and counts:

```json
{
  "schema_version": "missive.output.v1",
  "ok": true,
  "kind": "command_status",
  "data": {
    "command": "agent",
    "status": "parsed",
    "implemented": false,
    "config": {
      "source": "built_in_default",
      "profile": "default",
      "output_format": "human",
      "agent_count": 0,
      "auth_ref_count": 0
    },
    "message": "missive: 'agent' command parsed; implementation lands in a later ticket"
  }
}
```

NDJSON uses the same envelope and adds a numeric `sequence` field:

```json
{"schema_version":"missive.output.v1","ok":true,"kind":"event_record","sequence":42,"data":{"sequence":42,"event_id":"evt/a2a.task.updated/example","timestamp":"2026-05-22T00:00:00Z","source":"cli","event_type":"a2a.task.updated","task_id":"task-123","payload":{"state":"completed"},"metadata":{},"redacted":true}}
```

Structured errors use `ok: false`, `kind: "error"`, and the shared
`missive-core` error-report fields under `data`: `code`, `category`, `message`,
optional `help`, optional `sources`, and `exit_code`.

Before JSON/NDJSON values are written, the renderer recursively redacts
secret-like keys and HTTP-style auth headers. Authorization values preserve only
the auth scheme, for example `Bearer [REDACTED]`; raw tokens, API keys,
passwords, cookies, client secrets, and similar fields are not printed by normal
renderers.
