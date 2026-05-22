# CLI reference

`missive` currently exposes a clap-based command tree with shared flags,
configuration discovery, profile validation, and stable human/JSON/NDJSON/quiet
rendering. Implemented operational commands are the SQLite-backed agent registry
commands, public A2A Agent Card inspection/refresh, and non-streaming
`missive send`; other top-level commands still emit skeletal parsed status until
their ordered tickets land.

Run help with:

```bash
missive --help
missive agent --help
missive send --help
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
Agent Card HTTP requests and are shared with future A2A send/stream/task/push
clients. Timeout enforcement, tracing, streaming, task polling, and broader
command-specific semantics are intentionally left to their ordered implementation
tickets.

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
command-status record through the selected renderer.

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
MISSIVE_HOME=/tmp/missive-demo missive send echo --file ./prompt.txt --accepted-output-mode text/plain
MISSIVE_HOME=/tmp/missive-demo missive send echo --part text='first part' --part text='second part'
```

Supported inputs for this ticket are text-only:

* positional `[MESSAGE]` creates one text part
* `--stdin` reads one UTF-8 text part from standard input
* repeatable `--file PATH` reads each file as one UTF-8 text part
* repeatable `--part text=VALUE` adds explicit text parts
* repeatable `--metadata KEY=VALUE` adds non-secret A2A request metadata, parsing
  `VALUE` as JSON when possible and otherwise treating it as a string
* `--context CONTEXT_ID` and `--task TASK_ID` set A2A continuity fields on the
  outbound message and link persisted local rows
* repeatable `--accepted-output-mode MIME` populates
  `configuration.acceptedOutputModes`

Binary file bytes, MIME-specific file parts, JSON structured-data parts,
streaming, remote task polling, and artifact export are intentionally deferred to
later ordered tickets.

`send` resolves auth the same way as Agent Card commands: agent auth refs,
`--bearer-token-env`, and repeatable `--header Name:Value` are applied to both
the Agent Card fetch (when needed) and the send request, while raw secret values
are never persisted or printed. A2A service parameters (`A2A-Version`,
`A2A-Extensions`, and `--service-param`) are also sent on the outbound request
and recorded in local message/task metadata.

Machine-readable output uses `kind: "send_result"` and includes the selected
interface, outbound request summary, response shape (`message` or `task`), raw
redacted response JSON, and local persistence ids. Task responses include
`task_id`, `context_id`, and mapped state when the remote server returns them.
Human output is a concise one-line send summary. Request and response rows are
stored in SQLite `messages`; returned task responses are stored or updated in
`tasks` and linked to the messages.

## Output contract

The current renderer supports four modes:

* default human output — command-specific redacted text for implemented commands
  or one redacted status line for an unimplemented parsed command
* config `output.format = "json"` or `--json` — one JSON document using stable
  top-level fields
* config `output.format = "ndjson"` or `--ndjson` — one JSON object per line,
  currently one command-specific event for implemented agent registry/send
  commands or one command-status event for skeletal commands, and reserved for
  future event streams
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
{"schema_version":"missive.output.v1","ok":true,"kind":"command_status","sequence":0,"data":{"command":"events","status":"parsed","implemented":false,"config":{"source":"built_in_default","profile":"default","output_format":"human","agent_count":0,"auth_ref_count":0},"message":"missive: 'events' command parsed; implementation lands in a later ticket"}}
```

Structured errors use `ok: false`, `kind: "error"`, and the shared
`missive-core` error-report fields under `data`: `code`, `category`, `message`,
optional `help`, optional `sources`, and `exit_code`.

Before JSON/NDJSON values are written, the renderer recursively redacts
secret-like keys and HTTP-style auth headers. Authorization values preserve only
the auth scheme, for example `Bearer [REDACTED]`; raw tokens, API keys,
passwords, cookies, client secrets, and similar fields are not printed by normal
renderers.
