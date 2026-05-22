# Gateway daemon

`missive gateway run` starts the local gateway daemon. The daemon owns the long-running runtime contract, process lock, store initialization, event bus, supervisor, health/readiness/status HTTP endpoints, lifecycle events, graceful shutdown, A2A task subscription/resume, gateway-managed background communication jobs, and the adapter event-bus bridge. `missive gateway install/start/stop/status/uninstall` manages optional OS service supervision on Linux systemd and macOS launchd.

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

## Service installation

Gateway service commands are intentionally thin wrappers around the host's normal supervisor. They do not create a privileged control API; they generate service files and call `systemctl` or `launchctl`.

Supported managers:

* Linux: systemd user units by default, or system units with `--system`.
* macOS: launchd LaunchAgents by default, or LaunchDaemons with `--system`.
* Other platforms: service commands fail clearly and recommend running `missive gateway run` under an external supervisor.

Always inspect the generated file first:

```bash
MISSIVE_HOME=/var/lib/missive \
  missive gateway install --dry-run --json --bin "$(command -v missive)"
```

Then install and control the user service:

```bash
missive gateway install --bin "$(command -v missive)"
missive gateway start
missive gateway status
missive gateway stop
missive gateway uninstall
```

The generated service captures the absolute missive binary path, the selected config path when one was loaded, `--profile <selected-profile>`, a `PATH` value, and allowlisted non-secret environment such as `MISSIVE_HOME`, `HOME`, XDG state/config roots, and `RUST_LOG`. Use `--path <PATH>` to override the captured PATH and `--env NAME=VALUE` to add extra non-secret environment. The installer refuses secret-looking environment names such as token, cookie, password, credential, or API-key variables because credentials should continue to come from config auth refs backed by env/keyring at runtime.

System service installation is opt-in:

```bash
sudo missive gateway install --system \
  --bin /usr/local/bin/missive \
  --env MISSIVE_HOME=/var/lib/missive
sudo missive gateway start --system
```

For safety, `gateway install --system` requires an absolute `MISSIVE_HOME` in the generated environment so a root/system service does not accidentally write missive profile state into a login user's home directory or `/root`. Linux system units are written to `/etc/systemd/system/missive-gateway.service`; macOS system LaunchDaemons are written to `/Library/LaunchDaemons/works.earendil.missive.gateway.plist`. User units are written under `$XDG_CONFIG_HOME/systemd/user/` or `~/.config/systemd/user/` on Linux and `~/Library/LaunchAgents/` on macOS.

### Logs and supervisor inspection

Linux user service:

```bash
systemctl --user status missive-gateway.service --no-pager
journalctl --user -u missive-gateway.service -f
```

Linux system service:

```bash
sudo systemctl status missive-gateway.service --no-pager
sudo journalctl -u missive-gateway.service -f
```

macOS user LaunchAgent:

```bash
launchctl print gui/$(id -u)/works.earendil.missive.gateway
log stream --predicate 'process == "missive"' --style compact
```

macOS system LaunchDaemon:

```bash
sudo launchctl print system/works.earendil.missive.gateway
log stream --predicate 'process == "missive"' --style compact
```

The generated macOS plist also sends stdout/stderr to `~/Library/Logs/missive/` for user services or `/var/log/missive/` for system services.

Machine-readable service command output uses `gateway_service_install`, `gateway_service_start`, `gateway_service_stop`, `gateway_service_status`, and `gateway_service_uninstall` envelopes. Dry-run output includes the generated service file in `data.service_file` and planned supervisor commands in `data.planned_commands`.

## Endpoints

The daemon exposes unauthenticated local JSON endpoints:

* `GET /healthz` — liveness
* `GET /readyz` — readiness
* `GET /status` — detailed component status

The paths can be changed with `--health-path`, `--ready-path`, and `--status-path`; they must be distinct non-root HTTP paths.

A status response includes the selected profile, bound address, uptime, configured `job_concurrency`, local event-bus count, and supervised components. Current component states include `supervisor`, `event_bus`, `store`, `sessions`, `health_http`, active `subscriptions`, active or idle `background_jobs`, plus an idle `webhook_receiver` placeholder and an `adapters` component whose event sink is ready for future adapter workers.

## Task subscriptions and resume

On startup and then periodically while running, the `subscriptions` component scans the profile SQLite store for in-flight A2A tasks in `submitted`, `working`, or `unknown` state. For each task whose registered agent has a cached Agent Card advertising `capabilities.streaming = true`, the gateway:

1. creates or resumes a durable `gateway_jobs` row with kind `task_subscription`;
2. negotiates the cached Agent Card interface using the agent's binding preference;
3. calls A2A `SubscribeToTask` over the selected HTTP+JSON or JSON-RPC binding;
4. persists each `StreamResponse` as an `a2a.subscription.*` event and updates local task state for `task` and `statusUpdate` events;
5. deletes the subscription job when the task reaches `completed`, `failed`, or `cancelled`.

If a subscription stream fails or closes before the task reaches a terminal state, the gateway leaves the `task_subscription` job in `retrying` state, increments `retry_count`, records `gateway.subscription.backoff_ms`, stores `next_run_at`, and appends `missive.gateway.subscription.retrying`. Backoff is bounded between 1s and 30s and is visible in `/status`, NDJSON `gateway_component` output, the `gateway_jobs` row, and the event journal.

This provides restart resume: after a daemon restart, any still-in-flight task and persisted subscription job are discovered from SQLite and monitored again once their backoff permits.

## Background communication jobs

`missive job start` enqueues durable rows in `gateway_jobs` with kind `send`,
`stream`, `wait`, or `reduce`. While the daemon is running, the
`background_jobs` component periodically scans queued/retrying rows, claims due
jobs with a short worker lock, executes the operation, stores redacted
`result_json`, sets terminal state (`succeeded`, `failed`, or `cancelled`) or a
bounded retry backoff, and appends `missive.gateway.job.*` lifecycle events.
Running jobs with expired locks are eligible for pickup by a later gateway
process, so queued/retrying work and crash-stale running rows survive daemon
restart where the operation is idempotent enough to retry.

Typical non-interactive flow:

```bash
job_id=$(missive job start send echo "background work" --json \
  | jq -r '.data.job.job_id')
missive gateway run --timeout 30s --ndjson
missive job show "$job_id" --json
```

Supported operations:

* `send` — sends a stored A2A `SendMessageRequest` and records direct Message or
  Task result metadata. Task responses update the local task row.
* `stream` — sends a stored A2A `SendStreamingMessage` request, records each SSE
  event as `a2a.job.stream.*`, and updates local task state for task/status
  updates.
* `wait` — polls local or remote task state until `completed`, `failed`,
  `cancelled`, `input_required`, or timeout. Timeout marks the job failed;
  decisive states mark the wait operation succeeded with the observed task
  state in result JSON.
* `reduce` — performs a deterministic local reduction over already persisted
  group outputs for a context. It does not call reducer agents or shell
  pipelines in the gateway worker.

`missive job cancel <job-id>` always marks the local job cancelled. With
`--remote`, or when the job was started with `--cancel-remote-on-cancel`, the CLI
also requests A2A `CancelTask` if the job has a known agent/task id. Remote
cancellation is done by the foreground CLI using the usual auth resolver so raw
secrets are not persisted in the job row.

Current gateway workers use configured A2A service parameters but do not resolve
outbound auth refs, keyring values, `--bearer-token-env`, or one-shot `--header`
values. Use foreground commands for authenticated job-like operations until a
later hardening ticket wires gateway-safe auth resolution.

## Sessions and reset policies

The `sessions` component represents the persistent gateway session store added for communication continuity. A gateway session is keyed by source kind, source identity, target agent, and a resume name, and points at the current A2A `contextId`. This lets future adapters and gateway job workers resume a named source/agent conversation after process restart without relying on in-memory state.

Session rows store reset policy metadata:

* `none` — keep using the linked context until an explicit reset or relink.
* `daily` — rotate when the configured UTC reset hour boundary has passed.
* `idle` — rotate when `last_active_at` is older than the configured idle timeout.
* `both` — rotate when either the daily boundary or idle timeout is reached.

The reset evaluator lives in `crates/missive-gateway::session` and accepts an injectable clock; tests use a fixed clock so daily and idle boundary behavior is deterministic. The current daemon initializes/migrates the store and reports the `sessions` component as ready, but it does not yet expose user-facing session commands or adapter-driven session rotation. Busy-input, job, and adapter workers should use the typed `missive-store` gateway-session repository APIs instead of storing session state in memory.

Sessions are not long-term memory. They do not contain learned facts, summaries for model recall, vector indexes, prompts, or tool state. They only record communication routing state: source identity, target agent, resume name, linked context id, reset policy fields, timestamps, reset count, and non-secret metadata.

## Adapter event bridge

The gateway now depends on the shared `missive-adapters` trait crate. Adapter implementations emit `AdapterEvent` values through an `AdapterEventSink` instead of depending directly on gateway internals. The daemon wraps its local event bus with that sink, updates the `adapters` component when an adapter event is received, and forwards serialized adapter runtime events as `gateway_adapter_event` NDJSON items.

No configured adapter is started by `gateway run` yet; the bridge is tested with a fake adapter event and reserved for daemon-managed adapter workers in later tickets. The concrete stdio adapter is available today as the foreground `missive adapter stdio` subprocess loop. See [`adapters.md`](adapters.md) for lifecycle and frame details.

## Busy input modes

Gateway/adapters use the shared busy-input policy evaluator in
`crates/missive-gateway::busy` when a source sends new input while an operation
is already in flight for that source.

Supported modes:

* `queue` — keep the active operation running and queue the new input up to
  `max_queue_depth`.
* `interrupt` — mark the active operation as `interrupting`, queue the new input
  behind cancellation, and return actions for workers to cancel local waits,
  cancel local subscription jobs, and request remote A2A `CancelTask` when a
  cancellable task id is known and `interrupt_remote_cancel = true`.
* `steer` — append the new input to the active task/context when the active
  protocol state is marked steerable and has a context id or task id.

If `steer` is configured but the active operation cannot accept follow-up input,
the evaluator applies `unsupported_steer_fallback` (`queue` or `interrupt`) and
records an explicit fallback action. The effective policy comes from the selected
profile's `[gateway.busy_input]` block; a configured adapter/source can override
it with `[adapters.<name>.busy_input]`. The current daemon exposes the policy
library and config validation, while later background-job and adapter workers
will execute the returned actions as they add user-visible inbound sources. The
current job cancellation path marks local job rows and can request remote task
cancellation from the foreground CLI, but there is not yet an adapter-driven busy
input source that invokes this evaluator automatically.

## State and locking

`gateway run` resolves the selected profile's state paths, creates required directories, acquires the profile `gateway.lock`, opens/migrates the SQLite store, appends redacted `missive.gateway.started` and `missive.gateway.stopped` event-journal rows, and uses short state-mutation locks from blocking subscription/job/lifecycle tasks. Only one gateway or standalone webhook receiver can hold the profile gateway lock at a time.

## Output

Human mode prints lifecycle lines. `--ndjson` emits `gateway_started`, `gateway_component`, optional `gateway_adapter_event`, and `gateway_stopped` envelopes as the runtime progresses. Subscription progress and retry/backoff details are reported as `gateway_component` updates for the `subscriptions` component; background job queue, success, failure, retry, and cancellation summaries are reported through the `background_jobs` component. Future adapter workers can emit serialized adapter events through `gateway_adapter_event`. `--json` emits one final `gateway_stopped` summary after shutdown. `--quiet` suppresses non-error output.

## Current limitations

The subscription worker uses cached Agent Cards already stored in SQLite; run `missive agent inspect <alias>` or use an implemented send/stream/task command first if an agent row has no card cache. It currently sends configured A2A service parameters but does not resolve outbound auth refs, keyring entries, `--bearer-token-env`, or `--header` values for subscription calls, so authenticated remote subscriptions remain a later hardening item. It updates task state and event journal rows but does not yet persist subscribed messages or artifacts as dedicated message/artifact rows.

The daemon still does not embed `missive webhook run`, start configured adapter workers, or expose an authenticated control API. The adapter trait, registry, event-bus bridge, and foreground stdio adapter exist, but daemon-managed stdio/file/HTTP/external adapters are later tickets. Background jobs execute send/stream/wait/local-reduce work but do not yet persist every streamed message/artifact as dedicated rows, call reducer agents or command pipelines for reduce, expose a remote job control socket, or resolve gateway-safe outbound auth refs. Busy-input queue/interrupt/steer semantics are implemented as a deterministic policy evaluator plus configuration schema, but no current adapter path invokes it automatically. Service installation is limited to Linux systemd and macOS launchd and does not create package-manager integration, privilege escalation, log rotation, or a remote control socket. Keep the listener bound to loopback unless you intentionally put it behind trusted local infrastructure.
