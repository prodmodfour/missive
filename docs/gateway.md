# Gateway daemon

`missive gateway run` starts the local gateway daemon. The daemon owns the long-running runtime contract, process lock, store initialization, event bus, supervisor, health/readiness/status HTTP endpoints, lifecycle events, graceful shutdown, and the first A2A task subscription/resume worker.

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

`--timeout <DURATION>` is the non-interactive graceful shutdown budget for `gateway run`; without it, the process runs until Ctrl-C or process supervision stops it.

## Endpoints

The daemon exposes unauthenticated local JSON endpoints:

* `GET /healthz` — liveness
* `GET /readyz` — readiness
* `GET /status` — detailed component status

The paths can be changed with `--health-path`, `--ready-path`, and `--status-path`; they must be distinct non-root HTTP paths.

A status response includes the selected profile, bound address, uptime, configured `job_concurrency`, local event-bus count, and supervised components. Current component states include `supervisor`, `event_bus`, `store`, `health_http`, active `subscriptions`, plus idle placeholders for `webhook_receiver`, `background_jobs`, and `adapters`.

## Task subscriptions and resume

On startup and then periodically while running, the `subscriptions` component scans the profile SQLite store for in-flight A2A tasks in `submitted`, `working`, or `unknown` state. For each task whose registered agent has a cached Agent Card advertising `capabilities.streaming = true`, the gateway:

1. creates or resumes a durable `gateway_jobs` row with kind `task_subscription`;
2. negotiates the cached Agent Card interface using the agent's binding preference;
3. calls A2A `SubscribeToTask` over the selected HTTP+JSON or JSON-RPC binding;
4. persists each `StreamResponse` as an `a2a.subscription.*` event and updates local task state for `task` and `statusUpdate` events;
5. deletes the subscription job when the task reaches `completed`, `failed`, or `cancelled`.

If a subscription stream fails or closes before the task reaches a terminal state, the gateway leaves the `task_subscription` job in `retrying` state, increments `retry_count`, records `gateway.subscription.backoff_ms`, stores `next_run_at`, and appends `missive.gateway.subscription.retrying`. Backoff is bounded between 1s and 30s and is visible in `/status`, NDJSON `gateway_component` output, the `gateway_jobs` row, and the event journal.

This provides restart resume: after a daemon restart, any still-in-flight task and persisted subscription job are discovered from SQLite and monitored again once their backoff permits.

## State and locking

`gateway run` resolves the selected profile's state paths, creates required directories, acquires the profile `gateway.lock`, opens/migrates the SQLite store, appends redacted `missive.gateway.started` and `missive.gateway.stopped` event-journal rows, and uses short state-mutation locks from blocking subscription/lifecycle tasks. Only one gateway or standalone webhook receiver can hold the profile gateway lock at a time.

## Output

Human mode prints lifecycle lines. `--ndjson` emits `gateway_started`, `gateway_component`, and `gateway_stopped` envelopes as the runtime progresses. Subscription progress and retry/backoff details are reported as `gateway_component` updates for the `subscriptions` component. `--json` emits one final `gateway_stopped` summary after shutdown. `--quiet` suppresses non-error output.

## Current limitations

The subscription worker uses cached Agent Cards already stored in SQLite; run `missive agent inspect <alias>` or use an implemented send/stream/task command first if an agent row has no card cache. It currently sends configured A2A service parameters but does not resolve outbound auth refs, keyring entries, `--bearer-token-env`, or `--header` values for subscription calls, so authenticated remote subscriptions remain a later hardening item. It updates task state and event journal rows but does not yet persist subscribed messages or artifacts as dedicated message/artifact rows.

The daemon still does not embed `missive webhook run`, execute user-visible background jobs, run adapters, expose an authenticated control API, or install itself as a system service. Keep the listener bound to loopback unless you intentionally put it behind trusted local infrastructure.
