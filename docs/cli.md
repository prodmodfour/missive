# CLI reference

`missive` currently exposes a clap-based command tree with shared flags,
configuration discovery, profile validation, and stable human/JSON/NDJSON/quiet
rendering. Implemented operational commands are the SQLite-backed agent registry
commands, public A2A Agent Card inspection/refresh, non-streaming
`missive send`, streaming `missive stream`, `missive task get/list/wait/cancel`,
`missive context create/list/show/fork/close/export`, `missive group
create/list/show/add/remove/rename/delete`, `missive bcast`, `missive barrier`,
`missive gather`, `missive reduce`, `missive push create/get/list/delete`,
`missive webhook run`, `missive gateway run`, `missive gateway install/start/stop/status/uninstall`,
and `missive job start/list/show/cancel` for gateway-managed background work;
other top-level commands still emit skeletal parsed status until their ordered
tickets land.

Run help with:

```bash
missive --help
missive agent --help
missive send --help
missive stream --help
missive task --help
missive context --help
missive group --help
missive bcast --help
missive barrier --help
missive gather --help
missive reduce --help
missive push --help
missive job --help
missive job start send --help
missive gateway --help
missive gateway run --help
missive gateway install --help
missive gateway status --help
missive webhook --help
missive webhook run --help
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
Agent Card HTTP requests plus implemented send, stream, bcast, barrier remote
task polling, reduce reducer-agent calls, remote task, and push config calls. `missive webhook run` records
the effective protocol version on inbound callback events when the callback omits
`A2A-Version`. Task wait, bcast, barrier, webhook run, and gateway run use global
`--timeout` as bounded execution budgets; tracing and broader command-specific
timeout semantics are intentionally left to their ordered implementation tickets.

## Top-level commands

The current skeleton includes these top-level commands:

```text
agent       Manage configured A2A agents and cached Agent Cards
send        Send one message to an A2A agent
stream      Stream message updates from an A2A agent
task        Inspect, list, wait for, or cancel A2A tasks
context     Manage conversation contexts and session continuity
group       Manage groups of agents for collective operations
bcast       Broadcast one message to every member of a local group
barrier     Wait for group member tasks to reach terminal or requested states
gather      Gather latest local outputs and artifacts from group member tasks
reduce      Reduce gathered group outputs into one source-attributed result
gateway     Run and manage the local missive gateway daemon
webhook     Receive A2A push notification callbacks locally
push        Manage A2A push notification configurations
job         Enqueue, inspect, and cancel gateway-managed background communication jobs
doctor      Diagnose local configuration, storage, gateway, and endpoint health
logs        Inspect local missive logs
events      Inspect, tail, replay, or export the local event journal
completion  Generate shell completion scripts
manpage     Generate manual pages
```

Each top-level command has a help page. Running an unimplemented command other
than help currently loads and validates configuration, then emits a
command-status record through the selected renderer. Implemented command groups
with no selected subcommand, such as `missive context --json`, `missive gateway
--json`, or `missive webhook --json`, still emit that parsed command-status
record so automation can distinguish parser support from a specific operation.

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

### Agent capability summaries

`missive agent capabilities [alias]` renders the public selection capabilities
that missive can derive for one registered agent or, when `alias` is omitted,
for every registered agent. The command uses a cached Agent Card when one is
available, fetches and caches the public Agent Card when the cache is missing,
and revalidates the cache when `--refresh` is passed. The summary includes local
registry tags, Agent Card skill ids/names/tags, default and skill input/output
modes, `capabilities.streaming`, and `capabilities.pushNotifications`. These
fields are the same public/non-secret fields used by capability-aware
`missive route explain`.

Human output is concise text. Machine output uses command-specific envelope
kinds such as `agent_add`, `agent_list`, `agent_show`, `agent_inspect`,
`agent_refresh`, `agent_capabilities`, `agent_remove`, and `agent_rename`:

```bash
MISSIVE_HOME=/tmp/missive-demo missive agent add echo http://127.0.0.1:8080 --tag local
MISSIVE_HOME=/tmp/missive-demo missive agent inspect echo --json
MISSIVE_HOME=/tmp/missive-demo missive agent capabilities echo --json
MISSIVE_HOME=/tmp/missive-demo missive agent capabilities --refresh --json
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

## Group commands

`missive group` manages local groups of registered agent aliases for collective
and later routing commands. Groups live in the selected profile's SQLite store
and contain only local control-plane metadata: a group name, routing policy
label, notes, metadata, and member rows. Group CRUD commands do not make A2A
network calls; `group capabilities` may fetch or revalidate public Agent Cards
for member agents.

Implemented commands:

```bash
MISSIVE_HOME=/tmp/missive-demo missive group create team \
  --routing-policy weighted \
  --notes "Planner/writer/reviewer group" \
  --metadata purpose=demo \
  --json
MISSIVE_HOME=/tmp/missive-demo missive group add team planner --rank rank-0 --tag planner --weight 2
MISSIVE_HOME=/tmp/missive-demo missive group add team writer --rank rank-1 --tag writer --routing-metadata lane=blue
MISSIVE_HOME=/tmp/missive-demo missive group show team --json
MISSIVE_HOME=/tmp/missive-demo missive group capabilities team --json
MISSIVE_HOME=/tmp/missive-demo missive group list
MISSIVE_HOME=/tmp/missive-demo missive group remove team writer
MISSIVE_HOME=/tmp/missive-demo missive group rename team demo-team
MISSIVE_HOME=/tmp/missive-demo missive group delete demo-team
```

`group create` refuses duplicate group names. `--routing-policy` defaults to
`direct` and must be one of missive's built-in policy names: `direct`,
`capability-match`, `tag-match`, `round-robin`, `weighted`, `broadcast`,
`first-success`, `quorum`, or `fallback`. The group command stores the policy
label for router/collective commands; group CRUD commands do not execute routing
decisions themselves. `--metadata KEY=VALUE` stores non-secret group metadata
and parses `VALUE` as JSON when possible.

`group add` requires an existing group and an existing registered agent alias.
Each member stores the alias, required `--rank RANK`, repeatable `--tag TAG`, a
positive finite `--weight`, and repeatable `--routing-metadata KEY=VALUE` values.
Rank names must be unique within a group; attempting to assign one rank to two
agents fails before writing. Re-running `group add` for the same agent updates
that member when the requested rank is available.

`group show` displays the routing policy and members in deterministic rank/name
order. `group capabilities <group>` summarizes each member's local tags/routing
metadata plus cached/fetched public Agent Card skills, input/output modes,
streaming support, and push notification support; it fetches missing cards and
revalidates all member cards when `--refresh` is passed. `group rename` changes
the group primary key and preserves membership. `group remove` removes one member
by agent alias. `group delete` deletes the group and cascades its member rows.
Group mutation commands append redacted `missive.group.*` rows to the local event
journal for audit and later replay.

Machine-readable group output uses `group_create`, `group_list`, `group_show`,
`group_capabilities`, `group_add`, `group_remove`, `group_rename`, and
`group_delete` envelope kinds. Group views include the routing policy, notes,
metadata, member count, timestamps, and member rows with rank, tags, weight, and
routing metadata; capability summaries also include aggregate tags, labels,
media modes, cache status counts, and streaming/push support counts.

## Route explain

`missive route explain` dry-runs a routing decision without sending an A2A
message. It reads the selected profile's local agent registry and, when
`--group GROUP` is used, the stored group membership rows. For
`capability-match`, it reads cached Agent Cards by default and can revalidate or
fetch them with `--refresh-capabilities`. It then applies a built-in policy and
renders a deterministic explanation in human, JSON, or NDJSON mode.

Examples:

```bash
MISSIVE_HOME=/tmp/missive-demo missive route explain --group team --json
MISSIVE_HOME=/tmp/missive-demo missive route explain \
  --group team \
  --policy tag-match \
  --tag writer \
  --json
MISSIVE_HOME=/tmp/missive-demo missive route explain \
  --group team \
  --policy capability-match \
  --capability echo \
  --input-mode application/json \
  --output-mode text/plain \
  --streaming \
  --push-notifications \
  --refresh-capabilities \
  --json
MISSIVE_HOME=/tmp/missive-demo missive route explain \
  --agent planner \
  --agent writer \
  --policy round-robin \
  --cursor 3
```

Candidate sources are mutually exclusive: pass either `--group GROUP` or one or
more repeatable `--agent ALIAS` values. When no `--policy` override is provided,
route explain uses the group's stored routing policy; ad-hoc `--agent` routes
use the selected config's effective `routing.default_policy`. `--preferred-agent`
sets the primary/direct candidate for `direct` and `fallback` policies and must
refer to a candidate in the route set.

Policy behavior in this dry-run command:

* `direct` selects one preferred or first candidate.
* `capability-match` selects candidates whose local metadata and cached/fetched
  Agent Card capabilities satisfy all requested capability filters. Repeatable
  `--capability` labels match local metadata keys named `capability`,
  `capabilities`, `skill`, or `skills`, plus Agent Card skill ids, skill names,
  skill tags, and boolean labels such as `streaming` and `push-notifications`.
  Repeatable `--tag` labels match local agent/member tags plus Agent Card skill
  tags. Repeatable `--input-mode` and `--output-mode` values match Agent Card
  default and skill media modes. `--streaming` requires
  `capabilities.streaming = true`; `--push-notifications` (alias `--push`)
  requires `capabilities.pushNotifications = true`. Matching candidates are
  ordered by match score, member weight, then deterministic input/rank order.
  Unknown card-derived requirements appear as `*:unknown` in
  `missing_requirements`; run `missive agent capabilities <alias> --refresh` or
  rerun route explain with `--refresh-capabilities` to update the cache.
* `tag-match` selects candidates whose local agent/member tags and cached Agent
  Card skill tags satisfy all repeatable `--tag` labels.
* `round-robin` selects one candidate using `--cursor % candidate_count` and
  reports `next_round_robin_cursor` for callers that persist their own cursor.
* `weighted` selects the highest positive member weight, with deterministic
  candidate-order tie-breaking.
* `broadcast` selects every candidate.
* `first-success` returns the ordered attempt set a caller would try until one
  succeeds.
* `quorum` selects the attempt set and reports the requested `--quorum` or the
  default majority.
* `fallback` orders the primary candidate followed by fallback candidates.

Machine-readable output uses `kind: "route_explain"` and includes the profile,
source kind, policy source, selected aliases, per-candidate decision reasons,
matched tags/capabilities/input modes/output modes, `missing_requirements`,
optional quorum, and optional next round-robin cursor. The command is read-only
except for normal config-seeded agent sync and, when `--refresh-capabilities` is
used, Agent Card cache updates for the considered candidates.

## Broadcast collective

`missive bcast <group> <message>` sends the same non-streaming A2A
`SendMessage` content to every member of a local group in deterministic rank
order. It reuses the send command's message input parser, Agent Card discovery,
interface negotiation, service-parameter/auth handling, and persistence path.
Each successful member records request/response message rows, task rows when the
remote response is a `Task`, artifacts embedded in task responses, `a2a.send.*`
events, and broadcast-specific `missive.bcast.*` event rows.

Examples:

```bash
MISSIVE_HOME=/tmp/missive-demo missive bcast team "Draft a plan" --json
MISSIVE_HOME=/tmp/missive-demo missive bcast team --stdin --execution concurrent --json
MISSIVE_HOME=/tmp/missive-demo missive bcast team "Draft a plan" \
  --context ctx-planning-round \
  --failure-policy continue \
  --accepted-output-mode text/plain \
  --timeout 30s \
  --json
```

Supported message inputs match `send`: positional `[MESSAGE]`, `--stdin`,
repeatable `--part text=VALUE`, `--file`, `--file-bytes`, `--json-part`,
`--mime`, `--metadata KEY=VALUE`, and repeatable `--accepted-output-mode`. If
`--context CONTEXT_ID` is omitted, missive generates and persists one local
broadcast context id before sending; if it is provided and absent locally,
missive creates the local context row. The context id is placed on every outbound
member message.

`--execution sequential` sends one member at a time and is the default.
`--execution concurrent` resolves members first, then performs outbound A2A sends
in parallel worker threads while persisting results back to SQLite in rank order.
SQLite writes remain synchronous and protected by the normal profile state lock.

`--failure-policy stop` stops after the first sequential member failure and
returns a non-zero orchestration error after printing a `bcast_result` summary.
`--failure-policy continue` keeps sending later members and returns success when
at least one member succeeded, with `status: "partial_failure"` in the JSON
summary. Global `--timeout` bounds Agent Card fetches and per-member sends for
this command; a timeout prints the summary and exits with code `82`.

Machine-readable output uses `kind: "bcast_result"` and includes the operation
id, group, execution mode, failure policy, status, shared request/context
summary, success/failure counts, and one member result per group member. Member
results include agent alias, rank, status, request message id, selected
interface, response shape, response message id, task id, context id, mapped task
state, or a structured error report.

Current limitations: `bcast` is non-streaming and does not wait for returned
tasks to finish. Use `missive barrier` for group task synchronization and
`missive gather` for local output/artifact collection. Run `missive reduce` after gather when the collected member outputs should be
summarised, voted, merged, ranked, or sent to a reducer agent/command pipeline.
Concurrent mode does not cancel already-started worker threads after another
member fails; it reports all completed member outcomes in the summary.

## Barrier collective

`missive barrier <group> --context <id>` waits for group member tasks in one
shared A2A context to reach terminal states. By default it succeeds only when the
required members are `completed`; `failed` and `cancelled` terminal states produce
stable task exit codes after printing a `barrier_result` summary.

Examples:

```bash
MISSIVE_HOME=/tmp/missive-demo missive barrier team --context ctx-planning-round --json
MISSIVE_HOME=/tmp/missive-demo missive barrier team \
  --context ctx-planning-round \
  --required 2 \
  --failure-policy continue \
  --timeout 2m \
  --interval 2s \
  --json
MISSIVE_HOME=/tmp/missive-demo missive bcast team "Draft a plan" --json > bcast.json
MISSIVE_HOME=/tmp/missive-demo missive barrier team --from-bcast bcast.json --json
```

`--from-bcast PATH_OR_-` reads a previous `missive bcast --json` result and uses
its shared context id plus per-member task ids. Use `-` to read that JSON from
stdin. If `--context` is also supplied, it must match the broadcast result.

`--state STATE` is repeatable and changes the states that satisfy the barrier;
for example `--state working` can wait for in-progress tasks during tests or
custom workflows. `--required N` sets the quorum and defaults to all group
members. `--failure-policy stop` is the default and exits as soon as a
non-requested failure/cancellation is observed. `--failure-policy continue`
keeps polling until the quorum is reached, becomes impossible, or times out.
`--local` reads only the local SQLite task rows; otherwise the command refreshes
known task ids with A2A `GetTask` using the same auth, service-parameter, Agent
Card cache, and interface negotiation path as remote task commands.

Machine-readable output uses `kind: "barrier_result"` and includes the operation
id, group, context id, target/success states, quorum, counts, attempts, timeout
and interval, and one member row per group member. Member rows include agent,
rank, status, task id, state, selected interface when remote polling occurred,
and structured errors. Exit codes are `0` for success, `80` for failed member
tasks or impossible quorum, `81` for cancellation, and `82` for timeout.

## Gather collective

`missive gather <group> --context <id>` reads the selected profile's local
SQLite store and collects the latest known task output plus artifacts for each
group member in rank order. It does not call remote A2A endpoints; run
`missive barrier` or `missive task get --remote` first when the local store needs
to be refreshed.

Examples:

```bash
MISSIVE_HOME=/tmp/missive-demo missive gather team --context ctx-planning-round
MISSIVE_HOME=/tmp/missive-demo missive gather team --context ctx-planning-round --json
MISSIVE_HOME=/tmp/missive-demo missive gather team --context ctx-planning-round --ndjson
MISSIVE_HOME=/tmp/missive-demo missive gather team \
  --context ctx-planning-round \
  --output-dir ./gathered-artifacts \
  --json
```

Human output is markdown. Machine-readable output uses `kind: "gather_result"`
and includes the operation id, group, context id, status, member/message/artifact
counts, and one member row per group member. Member rows include agent, rank,
`status` (`gathered`, `missing_task`, or `empty_output`), task summary, selected
text, non-request output message summaries, artifact summaries, and exported
artifact records when file export is requested. Missing tasks are represented in
the summary instead of causing a command failure.

`--output-dir DIR` exports gathered artifacts to local files using sanitized,
rank-prefixed deterministic names. Existing files are not overwritten unless
`--force` is supplied. URL/file-reference artifacts are exported as JSON
manifests, matching `missive task artifact export` behavior.

## Reduce collective

`missive reduce <group> --context <id>` reads the same local rank-ordered
member outputs used by `gather` and produces one final source-attributed result.
It does not refresh remote tasks by itself; run `barrier`, `task get --remote`,
or `gather` first when the local store needs fresh task/output rows.

Examples:

```bash
MISSIVE_HOME=/tmp/missive-demo missive reduce team --context ctx-planning-round --json
MISSIVE_HOME=/tmp/missive-demo missive reduce team --context ctx-planning-round --strategy vote --json
MISSIVE_HOME=/tmp/missive-demo missive reduce team \
  --context ctx-planning-round \
  --strategy custom \
  --template 'Write a release note for {{group}} using:\n{{inputs}}' \
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

Built-in local strategies are deterministic and automation-friendly:

* `summarise` (default) emits one source-attributed bullet per gathered member.
  The alias `summarize` is accepted for CLI convenience.
* `vote` normalizes identical text answers, counts votes, and reports the
  deterministic winner plus tallies.
* `merge` concatenates member outputs under rank/agent headings.
* `rank` orders outputs by a simple deterministic local heuristic using text
  length plus message/artifact counts.
* `custom` requires `--template`; local mode renders the template directly.

`--template` can also override the prompt sent to `--reducer-agent` or
`--command`. Supported placeholders are `{{group}}`, `{{context_id}}`,
`{{strategy}}`, `{{input_count}}`, `{{inputs}}`, and
`{{default_reduction}}`. Without a template, agent/command modes receive a
strategy-specific prompt containing the provenance block and deterministic local
baseline.

`--reducer-agent ALIAS` sends the generated prompt through the normal A2A
`SendMessage` path to a registered agent, including Agent Card discovery,
interface negotiation, auth headers, service parameters, accepted output modes,
and send persistence. The reducer agent does not need to be a member of the
reduced group. `--command COMMAND` runs a local shell command, writes the prompt
to stdin, and uses UTF-8 stdout as the reduced output; nonzero exit, invalid
UTF-8, or empty stdout is an orchestration error.

Machine-readable output uses `kind: "reduce_result"` and includes the operation
id, group/context, strategy, reducer method, gathered/missing/empty counts,
final `reduced_text`, a persisted local reduced-message id, and a `provenance`
array. Each provenance row includes the source agent, rank, status, task summary,
selected text when available, source message references, and source artifact
references. The command appends `missive.reduce.started`,
`missive.reduce.input.*`, `missive.reduce.completed`, and failed-attempt events
to the local journal; reducer-agent mode also records the normal `a2a.send.*`
events.

Failure modes are intentional: missing group/member references fail validation;
`--strategy custom` without `--template` fails validation; combining
`--reducer-agent` and `--command` fails validation; no gathered member outputs in
the selected context fails with a usage error; reducer-agent transport/protocol
failures are reported through the A2A error taxonomy; command pipelines fail when
they exit nonzero or produce unusable stdout.

## Push notification config commands

`missive push` configures A2A task push notification callback endpoints on a
registered remote agent. Commands fetch or reuse the agent's cached Agent Card,
negotiate `http+json` or `json-rpc`, send the standard A2A service-parameter and
auth headers, and persist redacted local `push_configs` records plus redacted
journal events.

Implemented commands:

```bash
MISSIVE_PUSH_CALLBACK_SECRET=change-me \
  missive push create echo task-123 https://example.test/a2a/push \
    --config-id local-webhook \
    --auth-scheme Bearer \
    --auth-credentials-env MISSIVE_PUSH_CALLBACK_SECRET \
    --metadata purpose=demo \
    --json
missive push get echo task-123 local-webhook --json
missive push list echo task-123 --json
missive push delete echo task-123 local-webhook
```

`push create` requires a registered `<agent>`, a task id, and an absolute
`http`/`https` callback URL without embedded credentials. `--config-id` supplies
the optional remote config id. `--auth-scheme` may be paired with
`--auth-credentials-env ENV`; the environment variable value is sent to the
remote A2A agent in the `authentication.credentials` field but is redacted from
stdout, stderr, local event payloads, and the persisted `remote_config_json`.
`--metadata KEY=VALUE` stores non-secret local metadata on the `push_configs`
row; A2A v1.0 `TaskPushNotificationConfig` does not carry arbitrary metadata on
the wire.

`push list` accepts `--page-size` and `--page-token` for remote pagination and
persists each returned config. `push delete` calls the remote delete endpoint and
soft-deletes any active local row by setting `deleted_at`; the event journal
keeps the redacted delete response for audit. Machine-readable output uses
`push_create`, `push_get`, `push_list`, and `push_delete` envelope kinds.

## Background job commands

`missive job` manages durable gateway jobs for non-interactive communication.
`job start` enqueues work in SQLite; `missive gateway run` executes due
`send`, `stream`, `wait`, and local `reduce` jobs, records results on the
`gateway_jobs` row, and appends redacted `missive.gateway.job.*`/`a2a.job.*`
events.

Examples for agent/subprocess callers:

```bash
job_id=$(missive job start send echo "run this in the gateway" --json \
  | jq -r '.data.job.job_id')
missive gateway run --timeout 30s --ndjson
missive job show "$job_id" --json

missive job start wait task-123 --agent echo --timeout 2m --attach --json
missive job cancel "$job_id" --remote --json
missive job list --state succeeded --json
```

`job start send` and `job start stream` reuse the same text/stdin/file/JSON part
parser as the foreground `send`/`stream` commands, but they store only a redacted
summary in job output. `job start wait` stores the task id, optional agent, poll
interval, and timeout; `--attach` polls the job row until the gateway completes
it or the attach timeout expires. `job start reduce <group> --context <id>` runs
a deterministic local reduction over already persisted group outputs when the
gateway executes it. `--max-attempts` enables bounded retry for transient gateway
worker errors, and `--cancel-remote-on-cancel` lets `job cancel` request remote
A2A `CancelTask` when a task id is known.

Machine-readable output uses `job_start`, `job_list`, `job_show`, and
`job_cancel` envelope kinds. Job rows survive daemon restarts; running jobs whose
worker lock expires are eligible for pickup by the next gateway process. Gateway
workers currently execute outbound A2A job requests without resolving auth refs
or one-shot CLI auth flags, so authenticated agents should be used from
foreground commands until a later gateway-auth hardening ticket.

## Gateway daemon

`missive gateway run` starts the local gateway daemon. It creates the selected
profile state directories, acquires the profile `gateway.lock`, opens and
migrates the SQLite store, starts an async supervisor and local event bus, serves
health/readiness/status HTTP endpoints, records gateway lifecycle events, runs
the task subscription/resume worker, and shuts down gracefully on Ctrl-C or
global `--timeout`.

```bash
MISSIVE_HOME=/tmp/missive-demo \
  missive gateway run \
    --bind-address 127.0.0.1 \
    --port 7347 \
    --timeout 30s \
    --ndjson
```

By default the daemon uses the selected profile's `gateway.bind_address`. The
status endpoints are local unauthenticated JSON endpoints: `GET /healthz`,
`GET /readyz`, and `GET /status`. Override their paths with `--health-path`,
`--ready-path`, and `--status-path`; the paths must be distinct non-root HTTP
paths. Status output reports the selected profile, bound address, uptime,
configured `job_concurrency`, event-bus count, and components: running
`supervisor`, `event_bus`, `store`, `health_http`, an active or idle
`subscriptions` component, active or idle `background_jobs`, an idle embedded
`webhook_receiver` placeholder, and an `adapters` component with a ready event
sink for later adapter workers. The subscriptions component scans local
in-flight tasks, resumes persisted `task_subscription` gateway jobs, calls A2A
`SubscribeToTask` for cached streaming-capable agents, updates task state from
stream events, cleans up terminal jobs, and reports bounded retry/backoff
details through component output.

Machine-readable stream output uses `gateway_started`, `gateway_component`,
future `gateway_adapter_event`, and `gateway_stopped` envelope kinds. `--json`
emits only the final `gateway_stopped` summary after shutdown; use `--ndjson`
for one object per runtime event. The daemon appends redacted
`missive.gateway.started`,
`missive.gateway.stopped`, `missive.gateway.subscription.*`, and
`a2a.subscription.*` rows to the local event journal as applicable.

Gateway service management commands generate and call the local OS supervisor
without adding a second daemon protocol. Linux uses systemd, macOS uses launchd,
and other platforms fail with a clear unsupported-platform diagnostic. Use
`--dry-run --json` before installing to inspect the exact unit/plist, captured
non-secret environment, service path, and supervisor commands:

```bash
MISSIVE_HOME=/var/lib/missive \
  missive gateway install --dry-run --json --bin "$(command -v missive)"
missive gateway install --bin "$(command -v missive)"
missive gateway start
missive gateway status --json
missive gateway stop
missive gateway uninstall
```

The default install is a per-user service (`systemctl --user` on Linux or a
LaunchAgent on macOS). `--system` targets `/etc/systemd/system` or
`/Library/LaunchDaemons`; for safety, system installs require an explicit
absolute `MISSIVE_HOME` in the generated environment, for example
`--env MISSIVE_HOME=/var/lib/missive`, and may need sudo/root privileges. The
service file embeds an absolute missive binary path, the selected config path
when a config file was loaded, `--profile <selected-profile>`, a captured
`PATH`, and allowlisted non-secret runtime variables such as `MISSIVE_HOME` and
XDG state roots. `--env NAME=VALUE` can add additional non-secret variables;
secret-looking names such as token/password/API-key variables are refused.

Machine-readable output uses `gateway_service_install`,
`gateway_service_start`, `gateway_service_stop`, `gateway_service_status`, and
`gateway_service_uninstall` envelope kinds.

See [`gateway.md`](gateway.md) for operational details and current limitations.

## Webhook receiver

`missive webhook run` starts a local HTTP receiver for A2A push notification
callbacks. The receiver validates inbound request authentication when configured,
parses callback bodies as official A2A `StreamResponse` payloads, persists
redacted `a2a.push.*` event rows, prints streaming output in human or NDJSON
mode as callbacks arrive, and exposes `GET /healthz` and `GET /readyz`.

```bash
MISSIVE_WEBHOOK_TOKEN=change-me \
  MISSIVE_HOME=/tmp/missive-demo \
  missive webhook run \
    --bind-address 127.0.0.1 \
    --port 7347 \
    --path /a2a/push \
    --auth-token-env MISSIVE_WEBHOOK_TOKEN \
    --ndjson
```

Use `--auth-header`, `--auth-scheme`, and `--auth-token-env` to require a simple
header-token check. The default expects `Authorization: Bearer <token>`; use
`--auth-scheme none` to compare a raw header value. Missing or mismatched auth
returns `401` and records a redacted `a2a.push.rejected` event. Invalid JSON or
schema-incompatible A2A callback payloads return `400` and also record redacted
rejection events. Valid `task`, `message`, `statusUpdate`, and `artifactUpdate`
payloads return `202` after the event is persisted. If matching context/task rows
already exist, the event links to them; otherwise the raw redacted payload still
contains the A2A ids.

`--bind-address` and `--port` override the selected profile's
`gateway.bind_address`. `--path` changes the callback path. `--max-events N`
stops gracefully after N accepted callbacks, which is useful for CI and
one-shot demos. The global `--timeout 30s` also stops the receiver gracefully
after the duration elapses. `missive webhook run` serves local HTTP only; for
remote A2A agents, put any trusted HTTPS tunnel, reverse proxy, or local ingress
in front of it and forward the public callback URL to
`http://127.0.0.1:<port>/a2a/push`. No specific tunneling vendor is required.

Machine-readable stream output uses `webhook_started`, `webhook_event`,
`webhook_rejected`, and `webhook_stopped` envelope kinds. `--json` emits only the
final `webhook_stopped` summary after the receiver shuts down; use `--ndjson` for
one object per runtime event.

## Event commands

`missive events` exposes the selected profile's append-only SQLite event
journal. The current producers record local agent registry changes, group
membership changes, A2A send/stream request records, A2A send responses,
streaming updates, remote task changes observed by send/task commands,
push-config changes, gateway daemon lifecycle events, and webhook callback
acceptance/rejection events. Future gateway worker and adapter tickets will
append the same event table through the existing typed store API.

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
  summary, gateway run emits `gateway_started`/`gateway_component`/
  `gateway_stopped` lines, webhook run emits `webhook_started`/`webhook_event`/
  `webhook_rejected`/`webhook_stopped` lines, `events export` and `events tail`
  emit one `event_record` line per event, while other implemented commands emit
  one command-specific envelope and skeletal commands emit one command-status
  event
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
