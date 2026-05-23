# Agent registry

`missive agent` stores profile-scoped aliases for A2A agents in the local SQLite
state database. Aliases let humans and automation refer to agents with stable
names instead of full URLs.

## Add and inspect an agent

```bash
export MISSIVE_HOME="$(mktemp -d /tmp/missive-agents.XXXXXX)"
missive agent add echo http://127.0.0.1:8080 --tag local --metadata owner=demo --json
missive agent list --json
missive agent show echo --json
```

`agent add` supports:

* `<alias>` — lowercase CLI-safe name.
* `<base-url>` — the agent base URL used for public Agent Card discovery.
* `--interface BINDING=URL` — explicit interface URL such as
  `http+json=http://127.0.0.1:8080/a2a`; repeatable.
* `--binding-preference BINDING` — prefer `http+json` or `json-rpc` when the
  Agent Card advertises multiple interfaces.
* `--auth-ref NAME` — link to a config auth reference.
* `--tag TAG`, `--notes TEXT`, and `--metadata KEY=VALUE` — non-secret local
  routing and operator context.

Local mutations are journaled as redacted `missive.agent.*` events.

## Agent Card discovery and cache

`agent inspect` fetches `/.well-known/agent-card.json`, caches the raw and parsed
card, negotiates a locally supported interface, and prints provider, capability,
skill, version, and interface details:

```bash
missive agent inspect echo --refresh --json
missive agent refresh echo --json
missive agent inspect echo --binding http+json --json
```

Use `--refresh` when you need to bypass or revalidate the local cache. Use
`--binding` only when you want to force a supported binding for tests or advanced
routing; unsupported bindings fail with diagnostics that list local support.

## Capability summaries

Capability summaries combine local tags and cached/fetched Agent Card data:

```bash
missive agent capabilities echo --json
missive agent capabilities --refresh --json
```

These summaries are used by `missive route explain`, `missive group
capabilities`, and capability-aware selection. Missing Agent Cards are reported
as unknown rather than invented.

## Config-seeded agents

Agents can also be declared in TOML configuration. Config-seeded agents are
synced into the selected profile database as read-only rows before registry and
messaging commands run:

```toml
[agents.echo]
base_url = "http://127.0.0.1:8080"
tags = ["local", "mock"]
auth_ref = "echo-token"

[auth_refs.echo-token]
kind = "env"
env = "MISSIVE_ECHO_TOKEN"
header = "Authorization"
scheme = "Bearer"
```

Then run:

```bash
MISSIVE_ECHO_TOKEN=example missive agent list --config examples/config/minimal.toml --json
```

Do not put raw tokens in the config file. Auth refs name environment variables
or keyring coordinates only.

## Rename and remove

```bash
missive agent rename echo echo-local --json
missive agent remove echo-local --json
```

Config-seeded read-only agents cannot be renamed or removed with local registry
commands; edit the config instead.

## Smoke coverage

The `examples/demo-agent-registry.sh` script covers `agent add`, `list`, `show`,
`inspect`, and `capabilities` against the local mock A2A server. It is included
in `examples/run-smoke.sh` and the default quality gate.
