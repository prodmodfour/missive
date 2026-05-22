# Security

`missive` is still early-stage. The current security implementation focuses on
safe authentication inputs for implemented outbound Agent Card and send
requests, redaction at output boundaries, and keeping runtime state out of the
repository.

## Authentication inputs

Implemented outbound A2A HTTP requests (`agent inspect` when it fetches,
`agent refresh`, and `send`) can receive auth material from three sources:

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
replace earlier values with the same HTTP header name. `missive send` applies the
resolved headers to the optional Agent Card fetch and to the A2A `SendMessage`
request.

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

Redaction is a guardrail, not a substitute for secret hygiene. Do not place real
tokens in config files, command examples, tests, fixtures, docs, event payloads,
metadata, notes, or committed runtime files.

## Current limitations

Authentication is wired into implemented Agent Card fetch/refresh and
non-streaming send requests. Future stream, task, push, gateway, and adapter
tickets must reuse the same resolution and redaction path when they add outbound
requests.

Webhook verification, adapter trust boundaries, trace/log sinks, rate limits, and
insecure local token storage policy are not implemented yet.
