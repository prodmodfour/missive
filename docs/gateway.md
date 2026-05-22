# Gateway daemon

`missive gateway run` starts the current local gateway daemon skeleton. The daemon is intentionally small in this ticket: it owns the long-running runtime contract, process lock, store initialization, event bus, supervisor, health/readiness/status HTTP endpoints, lifecycle events, and graceful shutdown. Later gateway tickets will attach real remote subscriptions, embedded webhook handling, retrying background jobs, and adapter workers to this supervisor.

## Run

Use an isolated `MISSIVE_HOME` for demos and tests so runtime state stays outside the repository:

```bash
MISSIVE_HOME=/tmp/missive-demo missive gateway run --timeout 30s --ndjson
```

By default the daemon binds to the selected profile's `gateway.bind_address`, which defaults to `127.0.0.1:7347`. Override the local bind address and port for one invocation:

```bash
MISSIVE_HOME=/tmp/missive-demo missive gateway run \
  --bind-address 127.0.0.1 \
  --port 7347 \
  --status-path /status \
  --ndjson
```

`--timeout <DURATION>` is currently the non-interactive graceful shutdown budget for `gateway run`; without it, the process runs until Ctrl-C or process supervision stops it.

## Endpoints

The daemon exposes unauthenticated local JSON endpoints:

* `GET /healthz` — liveness
* `GET /readyz` — readiness
* `GET /status` — detailed component status

The paths can be changed with `--health-path`, `--ready-path`, and `--status-path`; they must be distinct non-root HTTP paths.

A status response includes the selected profile, bound address, uptime, configured `job_concurrency`, local event-bus count, and supervised components. Current component states include running `supervisor`, `event_bus`, `store`, and `health_http` plus idle placeholders for `subscriptions`, `webhook_receiver`, `background_jobs`, and `adapters`.

## State and locking

`gateway run` resolves the selected profile's state paths, creates required directories, acquires the profile `gateway.lock`, opens/migrates the SQLite store, and appends redacted `missive.gateway.started` and `missive.gateway.stopped` event-journal rows. Only one gateway or standalone webhook receiver can hold the profile gateway lock at a time.

## Output

Human mode prints lifecycle lines. `--ndjson` emits `gateway_started`, `gateway_component`, and `gateway_stopped` envelopes as the runtime progresses. `--json` emits one final `gateway_stopped` summary after shutdown. `--quiet` suppresses non-error output.

## Current limitations

The daemon does not yet subscribe to remote task updates, embed `missive webhook run`, execute background jobs, run adapters, resume work after restart, or expose an authenticated control API. Keep the listener bound to loopback unless you intentionally put it behind trusted local infrastructure.
