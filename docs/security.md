# Security

`missive` is still early-stage. The current security implementation focuses on
safe authentication inputs for implemented outbound Agent Card, send, stream,
broadcast, barrier polling, reduce reducer-agent, task, and push requests, redaction at output
boundaries, and keeping runtime state out of the repository.

## Authentication inputs

Implemented outbound A2A HTTP requests (`agent inspect` when it fetches,
`agent refresh`, `send`, `stream`, `bcast`, `barrier`, `reduce --reducer-agent`,
remote `task`, and `push` operations) can receive auth material from three sources:

1. Config auth refs linked from an agent with `auth_ref = "name"`.
2. `--bearer-token-env ENV`, which reads `ENV` and sends
   `Authorization: Bearer <value>`.
3. Repeatable `--header Name:Value`, which sends an explicit HTTP header for
   one invocation.

Config auth refs never contain the token itself:

```toml
[agents.echo]
base_url = "https://agent.example"
auth_ref = "echo-token"

[auth_refs.echo-token]
kind = "env"
env = "MISSIVE_ECHO_TOKEN"
header = "Authorization"
scheme = "Bearer"
```

Keyring-backed auth refs are also accepted:

```toml
[auth_refs.echo-keyring]
kind = "keyring"
keyring_service = "missive"
keyring_account = "echo"
header = "Authorization"
scheme = "Bearer"
```

Default builds enable the `native-keyring` Cargo feature. On Linux this uses the
kernel keyutils backend through the Rust `keyring` crate; on macOS and Windows it
uses the platform-native credential store. Builds without that feature still
parse keyring refs but fail clearly if a keyring-backed token is needed.

Precedence for a single request is: config auth ref first,
`--bearer-token-env` second, and repeated `--header` values last. Later values
replace earlier values with the same HTTP header name. `missive send`,
`missive stream`, `missive bcast`, remote `missive barrier` polling,
`missive reduce --reducer-agent`, remote `missive task`, and `missive push`
operations apply the resolved headers to the optional Agent Card fetch and to the
A2A protocol request.

## Storage tradeoffs

SQLite stores only non-secret auth reference metadata: auth-ref name, kind,
header name, scheme, environment variable name, or keyring service/account. Raw
tokens are not stored in SQLite by the current implementation. There is no
local-only insecure token storage mode yet; if such a mode is ever added it must
be explicit, documented, redacted from output, and unsuitable for shared systems.

Environment-variable tokens are simple and automation-friendly, but any child
process or process inspector with sufficient local privileges may be able to see
the environment. Prefer short-lived tokens and shell scopes that do not leak into
logs or history.

Keyring tokens avoid putting the secret in config, command history, or SQLite,
but availability depends on the local OS/session keyring. Headless Linux systems
may need keyutils/keyring setup or an env-backed fallback.

Gateway session rows store source kind/source identity, target agent alias,
resume name, linked context id, reset policy fields, timestamps, reset count,
and non-secret metadata. They are communication continuity records only, not
agent memory. The adapter trait normalizes external platform identities into the
same source kind/source id model before gateway/session/busy-input handling.
Busy-input state uses that source identity when deciding queue, interrupt, or
steer behavior, and queued inputs may later be persisted by adapter/job workers
as ordinary gateway state. Source identities such as adapter user/channel
identifiers can be operationally sensitive, so keep profile SQLite files outside
the repository and prune them according to future adapter retention policy.

`--header Name:Value` is useful for one-off auth headers such as `X-Api-Key`,
but the full value can be visible in shell history and process listings. Prefer
config auth refs or `--bearer-token-env` for repeated use.

## Redaction

Normal human, JSON, and NDJSON output recursively redacts secret-like JSON keys
and HTTP auth headers before printing. Authorization values preserve only the
scheme, for example `Bearer [REDACTED]`. Auth headers passed to the A2A request
builder are marked sensitive, and their debug representation is redacted.
`missive context export` and `missive events list/tail/replay/export` apply the
same output redaction and also redact raw message, task, and event payload JSON
before including those records in stdout. Event producers created in current CLI
paths store redacted event payloads for agent registry changes, send/stream/bcast
requests, send responses, streaming updates, changed remote task records,
broadcast, barrier, gather, and reduce lifecycle/member/provenance results, and
push notification config create/get/list/delete operations. `missive push` also redacts
`authentication.credentials` before persisting local `push_configs.remote_config_json` rows.

Redaction is a guardrail, not a substitute for secret hygiene. Do not place real
tokens in config files, command examples, tests, fixtures, docs, event payloads,
metadata, context names/summaries, notes, or committed runtime files. Local
SQLite state can contain raw remote protocol payloads from tasks/messages and
raw A2A artifact JSON before export-time redaction; event rows are intended to be
redacted but should still be treated as operational history. Keep profile state
directories outside the repository and protect them like local application data.

## Local file inputs and artifact exports

`missive send`, `missive stream`, and `missive bcast` validate `--file` and
`--file-bytes` paths by canonicalizing them and requiring regular local files
with safe UTF-8 filenames.
`--file` sends a canonical `file://` reference, so it can reveal the local path to
the remote A2A agent and in local SQLite request-message rows. Use
`--file-bytes` when the remote agent needs the content embedded instead, and only
attach files you are willing to send to that agent. Both forms are bounded by the
selected profile's `qos.max_request_bytes`; streaming/chunked upload is not
implemented yet.

`missive gather --output-dir` exports already persisted artifacts using sanitized,
rank-prefixed filenames and refuses to overwrite existing files unless `--force`
is supplied. Remote URL/file-reference artifacts are exported as JSON manifests
rather than fetched from untrusted locations. `missive reduce --command` executes
a user-supplied local shell command with the generated prompt on stdin; missive
does not sandbox that command, so use only trusted local reducers and keep their
stdout/stderr free of secrets. `missive reduce --reducer-agent` sends gathered
member text/provenance to the selected remote agent as ordinary A2A message
content, so only use reducer agents that are allowed to see those gathered
outputs.

## Push callback authentication

`missive push create --auth-scheme SCHEME --auth-credentials-env ENV` sends the
value of `ENV` to the remote A2A agent as callback authentication information.
That value is needed so the remote agent can authenticate when it later calls the
configured webhook URL, but it is treated as secret material: stdout/stderr,
structured output, event payloads, and the local redacted push config record do
not print the raw credential. The environment variable name and non-secret local
metadata may still appear in shell history or automation logs if users include
them there; do not pass real credentials in examples or committed scripts.

`missive webhook run --auth-token-env ENV` reads the expected inbound callback
token from the local environment and compares it to the configured request
header. By default the receiver expects `Authorization: Bearer <token>`; use
`--auth-header HEADER` and `--auth-scheme none` for a raw custom-header token.
The token is kept in process memory only, is never written to SQLite, and is
redacted from CLI startup/NDJSON output. Missing or mismatched auth returns
`401` and records only a redacted rejection event.

The webhook receiver is local HTTP only. For remote agents, terminate HTTPS in a
trusted tunnel, reverse proxy, or local ingress and forward to
`http://127.0.0.1:<port>/a2a/push`; no specific vendor is required. Treat public
callback URLs and tunnel configuration as sensitive operational data and avoid
committing them. The current receiver validates JSON shape and optional header
tokens, but it does not yet implement JWT/signature verification, replay
protection, rate limits, or credential rotation.

`missive gateway run` also serves local HTTP only. Its `/healthz`, `/readyz`,
and `/status` endpoints are unauthenticated and intended for loopback process
supervision. Keep the listener bound to `127.0.0.1` unless you intentionally
place it behind trusted local infrastructure; do not expose it as a public
control API. The subscription worker makes outbound `SubscribeToTask` calls only
for cached streaming-capable agents and redacts subscription event payloads
before journal insertion. The background job worker executes queued send,
stream, wait, and local-reduce jobs from local SQLite state; job command output
shows only summarized request fields rather than raw A2A message bodies, and
job lifecycle events are redacted before insertion. The durable `gateway_jobs.request_json` row must still contain the full protocol request so the daemon can execute it later; if a job is started with `--file-bytes` or sensitive message text, that content is present in local SQLite runtime state even though normal CLI output summarizes it.

`missive gateway install` writes local service-manager files. Dry-run mode is
available and should be used before installation to inspect generated systemd
units or launchd plists. The generated service captures only an allowlisted
non-secret runtime environment (`PATH`, `HOME`, `MISSIVE_HOME`, XDG roots, and
`RUST_LOG` by default) plus explicit `--env NAME=VALUE` entries. Secret-looking
environment names such as token, cookie, password, credential, authorization, or
API-key variables are refused so service files do not become credential stores.
`--system` installation is opt-in, may require elevated privileges, and requires
an absolute `MISSIVE_HOME` to keep root/system runtime state explicit.

## Artifact exports

`missive task artifact save` and `missive task artifact export` write only
artifacts already persisted in the selected local profile. Remote artifact names
and file-part filenames are treated as untrusted: missive strips path separators,
normalizes unsafe characters, and ensures directory exports remain under the
chosen `--output-dir`. Existing files are not overwritten unless `--force` is
supplied. URL/file-reference artifacts are exported as JSON manifests instead of
fetching or dereferencing remote/local URLs.

## Current limitations

Authentication is wired into implemented Agent Card fetch/refresh,
non-streaming send, broadcast send, streaming send, reduce reducer-agent send,
task get/list/wait/cancel, push config requests, `missive job cancel --remote`,
and the inbound webhook header-token hook. Gateway subscriptions and background
job workers currently send A2A service parameters but do not yet resolve
outbound auth refs, keyring entries, `--bearer-token-env`, or `--header` values,
so authenticated remote subscription resume and authenticated gateway-executed
jobs remain known limitations. Future gateway worker and adapter tickets must
reuse the same resolution and redaction path when they add broader outbound
requests.

Adapter workers are not live yet, but the trait and registry treat every
external source as untrusted input. Future adapters must validate framing,
message size, identity mapping, acknowledgements, and outbound rendering before
emitting gateway events or displaying updates. Adapter `settings` metadata in
config must remain non-secret; credentials should stay in auth refs, env vars,
keyrings, or future explicit secret references.

Webhook signature/JWT verification, live adapter trust-boundary enforcement,
trace/log sinks, rate limits beyond busy-input `max_queue_depth`, gateway
subscription/job auth resolution, user-facing session management commands, and
insecure local token storage policy are not implemented yet.
