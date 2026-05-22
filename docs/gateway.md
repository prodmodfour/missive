# Gateway daemon

`missive gateway run` starts the local gateway daemon. The daemon owns the long-running runtime contract, process lock, store initialization, event bus, supervisor, health/readiness/status HTTP endpoints, lifecycle events, graceful shutdown, and the first A2A task subscription/resume worker. `missive gateway install/start/stop/status/uninstall` manages optional OS service supervision on Linux systemd and macOS launchd.

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

A status response includes the selected profile, bound address, uptime, configured `job_concurrency`, local event-bus count, and supervised components. Current component states include `supervisor`, `event_bus`, `store`, `sessions`, `health_http`, active `subscriptions`, plus idle placeholders for `webhook_receiver`, `background_jobs`, and `adapters`.

## Task subscriptions and resume

On startup and then periodically while running, the `subscriptions` component scans the profile SQLite store for in-flight A2A tasks in `submitted`, `working`, or `unknown` state. For each task whose registered agent has a cached Agent Card advertising `capabilities.streaming = true`, the gateway:

1. creates or resumes a durable `gateway_jobs` row with kind `task_subscription`;
2. negotiates the cached Agent Card interface using the agent's binding preference;
3. calls A2A `SubscribeToTask` over the selected HTTP+JSON or JSON-RPC binding;
4. persists each `StreamResponse` as an `a2a.subscription.*` event and updates local task state for `task` and `statusUpdate` events;
5. deletes the subscription job when the task reaches `completed`, `failed`, or `cancelled`.

If a subscription stream fails or closes before the task reaches a terminal state, the gateway leaves the `task_subscription` job in `retrying` state, increments `retry_count`, records `gateway.subscription.backoff_ms`, stores `next_run_at`, and appends `missive.gateway.subscription.retrying`. Backoff is bounded between 1s and 30s and is visible in `/status`, NDJSON `gateway_component` output, the `gateway_jobs` row, and the event journal.

This provides restart resume: after a daemon restart, any still-in-flight task and persisted subscription job are discovered from SQLite and monitored again once their backoff permits.

## Sessions and reset policies

The `sessions` component represents the persistent gateway session store added for communication continuity. A gateway session is keyed by source kind, source identity, target agent, and a resume name, and points at the current A2A `contextId`. This lets future adapters and gateway job workers resume a named source/agent conversation after process restart without relying on in-memory state.

Session rows store reset policy metadata:

* `none` — keep using the linked context until an explicit reset or relink.
* `daily` — rotate when the configured UTC reset hour boundary has passed.
* `idle` — rotate when `last_active_at` is older than the configured idle timeout.
* `both` — rotate when either the daily boundary or idle timeout is reached.

The reset evaluator lives in `crates/missive-gateway::session` and accepts an injectable clock; tests use a fixed clock so daily and idle boundary behavior is deterministic. The current daemon initializes/migrates the store and reports the `sessions` component as ready, but it does not yet expose user-facing session commands or adapter-driven session rotation. Later busy-input, job, and adapter tickets should use the typed `missive-store` gateway-session repository APIs instead of storing session state in memory.

Sessions are not long-term memory. They do not contain learned facts, summaries for model recall, vector indexes, prompts, or tool state. They only record communication routing state: source identity, target agent, resume name, linked context id, reset policy fields, timestamps, reset count, and non-secret metadata.

## State and locking

`gateway run` resolves the selected profile's state paths, creates required directories, acquires the profile `gateway.lock`, opens/migrates the SQLite store, appends redacted `missive.gateway.started` and `missive.gateway.stopped` event-journal rows, and uses short state-mutation locks from blocking subscription/lifecycle tasks. Only one gateway or standalone webhook receiver can hold the profile gateway lock at a time.

## Output

Human mode prints lifecycle lines. `--ndjson` emits `gateway_started`, `gateway_component`, and `gateway_stopped` envelopes as the runtime progresses. Subscription progress and retry/backoff details are reported as `gateway_component` updates for the `subscriptions` component. `--json` emits one final `gateway_stopped` summary after shutdown. `--quiet` suppresses non-error output.

## Current limitations

The subscription worker uses cached Agent Cards already stored in SQLite; run `missive agent inspect <alias>` or use an implemented send/stream/task command first if an agent row has no card cache. It currently sends configured A2A service parameters but does not resolve outbound auth refs, keyring entries, `--bearer-token-env`, or `--header` values for subscription calls, so authenticated remote subscriptions remain a later hardening item. It updates task state and event journal rows but does not yet persist subscribed messages or artifacts as dedicated message/artifact rows.

The daemon still does not embed `missive webhook run`, execute user-visible background jobs, run adapters, or expose an authenticated control API. Service installation is limited to Linux systemd and macOS launchd and does not create package-manager integration, privilege escalation, log rotation, or a remote control socket. Keep the listener bound to loopback unless you intentionally put it behind trusted local infrastructure.
