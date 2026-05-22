# Security

`missive` is still early-stage. The current security implementation focuses on
safe authentication inputs for implemented outbound Agent Card, send, stream,
and task requests, redaction at output boundaries, and keeping runtime state out of the
repository.

## Authentication inputs

Implemented outbound A2A HTTP requests (`agent inspect` when it fetches,
`agent refresh`, `send`, `stream`, and remote `task` operations) can receive
auth material from three sources:

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
`missive stream`, and remote `missive task` operations apply the resolved headers
to the optional Agent Card fetch and to the A2A protocol request.

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
paths store redacted event payloads for agent registry changes, send/stream
requests, send responses, streaming updates, changed remote task records, and
push notification config create/get/list/delete operations. `missive push` also
redacts `authentication.credentials` before persisting local
`push_configs.remote_config_json` rows.

Redaction is a guardrail, not a substitute for secret hygiene. Do not place real
tokens in config files, command examples, tests, fixtures, docs, event payloads,
metadata, context names/summaries, notes, or committed runtime files. Local
SQLite state can contain raw remote protocol payloads from tasks/messages and
raw A2A artifact JSON before export-time redaction; event rows are intended to be
redacted but should still be treated as operational history. Keep profile state
directories outside the repository and protect them like local application data.

## Local file inputs

`missive send` and `missive stream` validate `--file` and `--file-bytes` paths by
canonicalizing them and requiring regular local files with safe UTF-8 filenames.
`--file` sends a canonical `file://` reference, so it can reveal the local path to
the remote A2A agent and in local SQLite request-message rows. Use
`--file-bytes` when the remote agent needs the content embedded instead, and only
attach files you are willing to send to that agent. Both forms are bounded by the
selected profile's `qos.max_request_bytes`; streaming/chunked upload is not
implemented yet.

## Push callback authentication

`missive push create --auth-scheme SCHEME --auth-credentials-env ENV` sends the
value of `ENV` to the remote A2A agent as callback authentication information.
That value is needed so the remote agent can authenticate when it later calls the
configured webhook URL, but it is treated as secret material: stdout/stderr,
structured output, event payloads, and the local redacted push config record do
not print the raw credential. The environment variable name and non-secret local
metadata may still appear in shell history or automation logs if users include
them there; do not pass real credentials in examples or committed scripts.

Webhook receiving and validation are implemented by later tickets. Until then,
only configure callback URLs that point at trusted local or test endpoints and do
not expose long-lived production webhook credentials through missive demos.

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
non-streaming send, streaming send, task get/list/wait/cancel, and push config
requests. Future gateway and adapter tickets must reuse the same resolution and
redaction path when they add outbound requests.

Webhook receiver verification, adapter trust boundaries, trace/log sinks, rate
limits, and insecure local token storage policy are not implemented yet.
