# Troubleshooting

Start with local diagnostics and then narrow the problem to config, state, the
remote A2A endpoint, or the gateway/adapters.

## Run doctor first

```bash
missive doctor --json
```

`doctor` checks binary metadata, config discovery/validation, selected profile
state paths, existing SQLite migration state, configured A2A Agent Card
reachability, selected local gateway `/status`, and useful local tool
availability. If no agents or gateway are configured, remote/gateway checks are
reported as skipped or not applicable.

Common nonzero exit categories:

| Exit | Meaning |
| ---: | --- |
| `69` | unavailable endpoint or gateway status |
| `75` | storage/schema issue |
| `76` | protocol error, including unsupported A2A version |
| `77` | auth material could not be resolved |
| `78` | configuration validation failed |
| `80`-`83` | task wait/barrier state outcomes |

## Inspect logs and events

```bash
missive logs --json
missive events list --limit 20 --json
missive events tail --limit 10 --ndjson
missive events replay --context ctx-example-1 --json
```

`logs` reads local profile log files under the selected profile state directory
when they exist. Foreground command diagnostics normally go to stderr unless you
redirect them. `events` reads the profile SQLite event journal.

Enable more diagnostics for one command:

```bash
RUST_LOG=missive=debug missive agent inspect echo --refresh --json
MISSIVE_LOG_FORMAT=json RUST_LOG=missive=debug missive send echo "hello" --json
missive --trace stream echo "debug stream" --ndjson
```

Logs and command output use redaction helpers, but runtime databases and adapter
inbox/outbox files can contain message content. Keep them outside the repository.

## Config was not loaded

Check discovery order:

```bash
missive doctor --json
MISSIVE_CONFIG=/path/to/config.toml missive agent list --json
missive --config examples/config/minimal.toml agent list --json
MISSIVE_REPO_CONFIG=1 missive agent list --json
```

Repository-local `missive.toml` and `.missive.toml` are ignored unless
`MISSIVE_REPO_CONFIG=1` is set. Unknown TOML fields are rejected so typos fail
early.

## Agent Card or protocol errors

```bash
missive agent inspect echo --refresh --json
missive agent inspect echo --binding http+json --json
missive agent capabilities echo --refresh --json
```

If discovery fails, verify the base URL serves `/.well-known/agent-card.json` and
that any required auth headers are available. If a version error occurs, set the
profile `protocol.protocol_version` or pass `--protocol-version` for the command.

## Auth material is missing

Use environment-backed auth for quick checks:

```bash
export MISSIVE_ECHO_TOKEN=example
missive send echo "hello" --bearer-token-env MISSIVE_ECHO_TOKEN --json
```

For config auth refs, store only env var names or keyring coordinates. missive
does not provide keyring-management commands yet, so provision keyring entries
with OS tooling or another keyring client.

## State, locks, or database issues

Isolate state with a temporary home:

```bash
export MISSIVE_HOME="$(mktemp -d /tmp/missive-debug.XXXXXX)"
missive agent list --json
```

Profile databases and locks live under the resolved state paths described in
[`configuration.md`](configuration.md). Lock files can remain after a crash, but
OS locks are released when the owning process exits. If a command reports an
active gateway lock, stop the running gateway or use a different profile/home.

## Task wait timed out or returned a nonzero state

```bash
missive task get task-123 --agent echo --remote --json
missive task wait task-123 --agent echo --timeout 30s --interval 1s --json
missive task cancel task-123 --agent echo --json
```

Remember that `task wait` is a foreground polling command. It returns deterministic
exit codes for completed, failed, cancelled, timeout, and input-required states;
see [`tasks.md`](tasks.md).

## Stream produced no events

```bash
missive agent inspect echo --refresh --json
missive stream echo "Show progress" --ndjson
missive stream echo "Try despite missing card capability" --force --ndjson
```

A missing or stale Agent Card can make missive reject streaming before the
request is sent. Use `--force` only for compatibility tests against trusted local
or known endpoints.

## Gateway or HTTP adapter is unavailable

```bash
missive gateway run --bind-address 127.0.0.1 --port 7347 --timeout 30s --ndjson
missive doctor --json
curl -fsS http://127.0.0.1:7347/status
```

When using the HTTP adapter, start the gateway with `--http-adapter` and match
the auth header/token settings in the client request. The HTTP adapter currently
journals accepted frames and emits gateway adapter events; it does not execute
submitted send/stream/task commands automatically.

## Example scripts fail

```bash
cargo build -p missive-cli --bin missive
MISSIVE_BIN="$PWD/target/debug/missive" examples/run-smoke.sh
MISSIVE_EXAMPLE_KEEP_WORKDIR=1 examples/run-smoke.sh
```

When `MISSIVE_EXAMPLE_KEEP_WORKDIR=1` is set, inspect the printed temporary
workdir for mock server logs, command output, and `MISSIVE_HOME` state. Do not
commit those runtime files.
