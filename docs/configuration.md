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
missive agent --config examples/config/minimal.toml --json
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
* `agents` — config-seeded agent aliases, base URLs, explicit interface URLs,
  binding preference, auth ref, tags, notes, and metadata.
* `auth_refs` — references to secrets in environment variables or platform
  keyrings. Raw token values are not accepted by the schema.
* `storage` — storage backend defaults. Only `sqlite` is currently defined;
  concrete state-path behavior is implemented by later storage tickets.
* `output` — default output format (`human`, `json`, `ndjson`, or `quiet`), color
  mode (`auto`, `always`, `never`), and mandatory secret redaction.
* `gateway` — gateway enablement, bind address, optional public base URL, and job
  concurrency defaults.
* `adapters` — adapter kind, enablement, profile mapping, and non-secret settings
  for later adapter tickets.
* `qos` — timeout, connect timeout, retry attempts/backoff, maximum request
  bytes, and concurrency defaults.

Unknown fields are rejected so configuration typos fail early.

## Validation and redaction

Config validation checks:

* schema version is `missive.config.v1`
* profile, agent, auth-ref, adapter, tag, and binding names use stable
  lowercase CLI-safe forms
* referenced profiles, agents, and auth refs exist
* HTTP(S) URLs are absolute, include a host, and do not embed credentials
* durations use `ms`, `s`, `m`, or `h` units
* gateway bind address is an IP socket address such as `127.0.0.1:7347`
* raw secret storage is not part of the auth-ref schema

Structured config rendering uses redaction before printing JSON. Secret-like keys
such as `token`, `password`, `authorization`, `client_secret`, and cookies are
rendered as `[REDACTED]`; auth-scheme strings such as `Bearer value` preserve only
the scheme.
