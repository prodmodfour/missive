# Configuration

`missive` loads a TOML configuration file into the `missive.config.v1`
schema. Configuration is optional: when no file is found, missive uses built-in
safe defaults with a single `default` profile and no configured agents.

## Discovery order

Configuration discovery is deterministic:

1. `--config <PATH>` — explicit CLI path. Relative paths are resolved from the
   current working directory.
2. `MISSIVE_CONFIG=<PATH>` — explicit environment path.
3. Repository-local config only when explicitly requested with
   `MISSIVE_REPO_CONFIG=1`. missive looks for `missive.toml` and `.missive.toml`
   in the current directory and ancestors. Local config filenames are ignored by
   git in this repository so real secrets or machine-specific settings are not
   committed accidentally.
4. XDG config locations:
   * `$XDG_CONFIG_HOME/missive/config.toml`
   * `$XDG_CONFIG_HOME/missive.toml`
   * if `XDG_CONFIG_HOME` is unset, `$HOME/.config/missive/config.toml` and
     `$HOME/.config/missive.toml`
   * each `$XDG_CONFIG_DIRS` root with the same two relative paths, defaulting
     to `/etc/xdg`
5. Built-in defaults.

`--profile <PROFILE>` selects a profile from the loaded config. If omitted,
`default_profile` is used. Missing or invalid profiles fail with configuration
exit code `78`.

## Examples

Validated examples live under [`examples/config/`](../examples/config/):

```bash
MISSIVE_HOME=/tmp/missive-demo missive agent list --config examples/config/minimal.toml --json
MISSIVE_HOME=/tmp/missive-demo missive send echo "Say hello" --config examples/config/minimal.toml --json
missive doctor --config examples/config/full.toml --profile ci
```

The examples use environment-variable and keyring references for auth material;
they do not contain real tokens.

## Schema overview

Top-level fields:

```toml
schema_version = "missive.config.v1"
default_profile = "default"

[profiles.default]
default_agent = "echo"

[protocol]
protocol_version = "1.0"
extensions = []

[protocol.service_parameters]
A2A-Tenant = "local-demo"

[routing]
default_policy = "direct"

[agents.echo]
base_url = "http://127.0.0.1:8080"
auth_ref = "example-env"
tags = ["local", "mock"]

[agents.echo.interface_urls]
"http+json" = "http://127.0.0.1:8080/a2a"

[auth_refs.example-env]
kind = "env"
env = "MISSIVE_EXAMPLE_TOKEN"
header = "Authorization"
scheme = "Bearer"
```

Supported sections in this ticket:

* `profiles` — profile descriptions, default agent aliases, and optional
  profile-specific storage/output/gateway/qos overrides.
* `agents` — config-seeded read-only agent aliases, base URLs, explicit interface
  URLs, binding preference, auth ref, tags, notes, and metadata. Binding
  preference is used by Agent Card interface negotiation; `http+json` and
  `json-rpc` are locally supported today.
* `auth_refs` — references to secrets in environment variables or platform
  keyrings. Raw token values are not accepted by the schema.
* `storage` — storage backend defaults. Only `sqlite` is currently defined;
  state paths are resolved outside the repository by default as described below.
* `output` — default output format (`human`, `json`, `ndjson`, or `quiet`), color
  mode (`auto`, `always`, `never`), and mandatory secret redaction.
* `protocol` — default A2A service parameters: `protocol_version` for the
  `A2A-Version` header, optional `extensions` for `A2A-Extensions`, and
  additional non-auth `service_parameters` sent as headers where HTTP-based A2A
  requests are implemented. Profiles may override the full protocol block with
  `[profiles.<name>.protocol]`.
* `gateway` — gateway enablement, bind address, optional public base URL, job
  concurrency defaults, and profile-wide busy-input policy. `missive gateway run`
  uses the selected profile's bind address and job concurrency today; gateway and
  future adapter workers share the busy-input policy model.
* `routing` — default routing policy for `missive route explain` when a command
  does not provide `--policy` and no group policy applies. Profiles may override
  this block with `[profiles.<name>.routing]`. Valid built-in policies are
  `direct`, `capability-match`, `tag-match`, `round-robin`, `weighted`,
  `broadcast`, `first-success`, `quorum`, and `fallback`.
* `adapters` — adapter kind, enablement, profile mapping, optional source-level
  busy-input overrides, and non-secret settings for later adapter tickets.
* `qos` — timeout, connect timeout, retry attempts/backoff, maximum request
  bytes, and concurrency defaults. `qos.max_request_bytes` is currently enforced
  by `missive send` and `missive stream` while parsing local text, file-reference,
  file-byte, and JSON parts and while checking the serialized A2A request size.

Busy-input policy is configured under `[gateway.busy_input]` and may be
replaced for a configured source/adapter with `[adapters.<name>.busy_input]`:

```toml
[gateway.busy_input]
mode = "queue"                   # queue, interrupt, or steer
unsupported_steer_fallback = "queue" # queue or interrupt
interrupt_remote_cancel = true
max_queue_depth = 32

[adapters.stdio.busy_input]
mode = "steer"
unsupported_steer_fallback = "queue"
max_queue_depth = 16
```

`queue` preserves the active operation and stores follow-up input for later,
`interrupt` marks the active operation for cancellation and asks workers to
cancel local waits/subscriptions plus remote A2A tasks when a cancellable task id
is known, and `steer` appends follow-up input to the active task/context only
when protocol state allows it. If steering is unsupported, the configured
fallback is used; fallback cannot be `steer` to avoid recursive policy loops.

Unknown fields are rejected so configuration typos fail early.

For implemented A2A HTTP requests (Agent Card fetch/refresh, non-streaming
send, streaming send, remote task get/list/wait/cancel, and push config calls),
`--protocol-version <VERSION>` overrides the selected profile's
`protocol_version` for that invocation. The
`--a2a-extension <EXTENSION>` flag appends requested extensions, and
`--service-param NAME=VALUE` adds or overrides an extra service parameter for the
invocation. These flags are validated before an outbound request is sent.

Authentication can be supplied by linking an agent to an `auth_ref`, by passing
`--bearer-token-env ENV`, or by passing repeatable `--header Name:Value` values.
Config auth refs currently support `kind = "env"` and `kind = "keyring"`; they
store only the environment variable name or keyring service/account, never the
raw token. CLI-supplied auth headers are used for the current invocation only,
including `missive send`, `missive stream`, and remote `missive task`
operations, and are not written to SQLite.

## Local state paths

The store layer now resolves data, state, cache, database, and lock paths for the
selected profile without creating files during resolution.

Precedence for runtime state roots is:

1. `MISSIVE_HOME=<ABSOLUTE_DIR>` — all roots live under this directory:
   * `data/profiles/<profile>`
   * `state/profiles/<profile>`
   * `cache/profiles/<profile>`
2. XDG variables on Linux and other Unix-like platforms:
   * `${XDG_DATA_HOME:-$HOME/.local/share}/missive/profiles/<profile>`
   * `${XDG_STATE_HOME:-$HOME/.local/state}/missive/profiles/<profile>`
   * `${XDG_CACHE_HOME:-$HOME/.cache}/missive/profiles/<profile>`
3. macOS fallback when XDG variables are not set:
   * `$HOME/Library/Application Support/missive/data/profiles/<profile>`
   * `$HOME/Library/Application Support/missive/state/profiles/<profile>`
   * `$HOME/Library/Caches/missive/profiles/<profile>`

`storage.database_path` may be absolute, use `~/` for the current home
directory, or be relative to the selected profile's state directory. Relative
paths containing `..` are rejected so they cannot escape that profile directory.
If omitted, the default database path is `<state-dir>/missive.sqlite3`.

Process locks live in `<state-dir>/locks/`. `state.lock` coordinates current
agent registry mutations, config-agent sync, and future migrations;
`gateway.lock` coordinates one gateway or standalone webhook process per profile.
Lock files may remain after a process exits, but the OS-level lock is released
when the owning process or file descriptor closes.

The SQLite schema is managed by embedded migrations in `missive-store`; see
[`docs/storage.md`](storage.md) for the migration strategy, table purposes, and
retention notes. `missive agent list/show` include `[agents.<alias>]` entries by
syncing them into the selected profile database as `source = "config_seed"` and
read-only rows before each agent registry operation.

## Validation and redaction

Config validation checks:

* schema version is `missive.config.v1`
* profile, agent, auth-ref, adapter, tag, and binding names use stable
  lowercase CLI-safe forms
* referenced profiles, agents, and auth refs exist
* HTTP(S) URLs are absolute, include a host, and do not embed credentials
* durations use `ms`, `s`, `m`, or `h` units
* A2A protocol versions are short ASCII version tokens, extensions are compact
  comma-free identifiers, and arbitrary service-parameter names are valid HTTP
  header names
* `A2A-Version` and `A2A-Extensions` cannot be redefined inside
  `protocol.service_parameters`; use `protocol_version` and `extensions`
  instead
* gateway bind address is an IP socket address such as `127.0.0.1:7347`
* busy-input queue depth must be greater than zero, and unsupported steer
  fallback must be `queue` or `interrupt`
* routing policy names in `routing.default_policy`, profile routing overrides,
  and group creation must be one of the built-in missive policy names
* auth refs identify only environment variables or keyring coordinates, not raw
  token values
* raw secret storage is not part of the auth-ref schema

Structured config rendering uses redaction before printing JSON. Secret-like keys
such as `token`, `password`, `authorization`, `client_secret`, and cookies are
rendered as `[REDACTED]`; auth-scheme strings such as `Bearer value` preserve only
the scheme. See [`security.md`](security.md) for authentication storage tradeoffs
and keyring notes.
