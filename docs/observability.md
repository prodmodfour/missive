# Observability

missive initializes a shared tracing/logging subscriber through `crates/missive-observe`. It configures log filters, stderr rendering, JSON log formatting, log redaction, and operation-level spans across representative CLI, A2A protocol, store, gateway, adapter, and collective paths.

## Enabling logs

Logs are written to standard error by foreground commands. This keeps command output on stdout stable for human, JSON, and NDJSON consumers.

Default behavior is quiet except for warnings and errors:

```bash
missive agent list --json
```

Increase missive diagnostics with the global flags:

```bash
missive -v agent list
missive -vv agent list
missive --trace agent list
```

The derived filters are:

| Input | Effective filter when `RUST_LOG` is unset |
| --- | --- |
| no diagnostic flag | `warn` |
| `-v` / `--verbose` | `info` |
| `-vv` | `debug` |
| `-vvv` or `--trace` | `trace` |

`RUST_LOG` uses standard `tracing-subscriber` EnvFilter syntax and takes precedence over `--verbose` and `--trace`:

```bash
RUST_LOG=missive_cli=debug,missive_a2a=trace missive agent inspect echo --json
```

If `RUST_LOG` is invalid, missive fails early with a configuration diagnostic before running the command.

## JSON log mode

Human log lines are the default. Set `MISSIVE_LOG_FORMAT=json` for one JSON log object per line:

```bash
MISSIVE_LOG_FORMAT=json RUST_LOG=info missive agent list --json
```

`MISSIVE_LOG_JSON=1` is also accepted as a boolean shortcut. JSON logs still go to stderr and do not change stdout command schemas such as `missive.output.v1`.

`--no-color` and `NO_COLOR` disable ANSI coloring for log formatting. The current formatter is plain text, but the setting is honored for future formatters.

## Span and field conventions

Structured spans use stable, dotted names and safe identifiers rather than raw payloads:

| Span name | Emitted by | Representative fields |
| --- | --- | --- |
| `cli.command` | CLI dispatch | `command`, `selected_profile`, `output_mode` |
| `a2a.request` | A2A clients | `operation`, `binding`, `protocol_version`, redacted `endpoint`, `message_id`, `task_id`, `auth_configured` |
| `store.operation` | SQLite repository | `db_system=sqlite`, `operation`, `entity`, `identifier` |
| `gateway.daemon` / `gateway.job_manager` / `gateway.job` | Gateway runtime and background jobs | `profile`, `job_concurrency`, `job_id`, `kind`, `state`, `retry_count` |
| `gateway.http_adapter.request` / `gateway.adapter_event` / `adapter.event` | HTTP and shared adapter ingress | `adapter_kind`, `adapter_name`, `event_type`, `request_id`, `command`, `auth_validated` |
| `collective.operation` | `bcast`, `barrier`, `gather`, and `reduce` | `collective`, `group`, `context_id`, `operation_id`, `status`, counts |

Operation logs are intentionally metadata-first. A2A request spans include transport binding, selected protocol version, request/task/message identifiers, response status, stream event counts, and errors, but they do not include message parts, artifact bytes, auth headers, or callback credentials. URL fields are stripped of userinfo, query strings, and fragments before logging.

## Troubleshooting examples

Inspect a failing send without changing stdout JSON:

```bash
MISSIVE_LOG_FORMAT=json \
RUST_LOG=missive_cli=debug,missive_a2a=debug,missive_store=debug \
missive --json send echo "hello"
```

Inspect gateway background job state transitions:

```bash
MISSIVE_LOG_FORMAT=json RUST_LOG=missive_gateway=debug missive gateway run --timeout 30s
```

Inspect collective routing/persistence metadata without printing prompt bodies or gathered content in logs:

```bash
RUST_LOG=missive_cli=debug,missive_store=debug missive bcast reviewers "check status"
```

## Redaction boundaries

The logging layer redacts:

* fields with secret-like names such as `authorization`, `cookie`, `token`, `password`, `client_secret`, and `api_key`;
* text after common auth schemes such as `Bearer`, `Basic`, `Token`, `ApiKey`, and `Api-Key`;
* secret-like `key=value` and JSON-style `"key":"value"` fragments in debug text.

Redaction is best-effort at the logging boundary. Do not put raw credentials in command arguments, shell history, config files, event payloads, adapter inboxes, or local SQLite state. Logs should contain identifiers, states, and redacted metadata rather than full message bodies or authentication material.

## Gateway and services

`missive gateway run` uses the same logging foundation as foreground CLI commands. Foreground logs go to stderr. Installed Linux systemd services capture stderr in the journal, and macOS launchd plists route stdout/stderr as documented in [`gateway.md`](gateway.md).

Examples:

```bash
RUST_LOG=info missive gateway run --timeout 30s
MISSIVE_LOG_FORMAT=json RUST_LOG=missive_gateway=debug missive gateway run --timeout 30s
```

## Inspecting diagnostics

Use `missive logs` to inventory local diagnostic sources for the selected profile
and read bounded records from the profile file-log directory when a supervisor or
operator redirects logs there:

```bash
missive logs
missive logs --json
missive logs --source profile-files --limit 20 --ndjson
```

The command reports these sources:

* `profile-files` — files under `<profile state dir>/logs/` with `.log`, `.txt`,
  `.ndjson`, `.jsonl`, or `.json` extensions;
* `event-journal` — the SQLite event journal path and matching `missive events`
  command hints;
* `gateway-service` — platform service-manager hints for the installed gateway
  service, when supported.

`missive logs --json` emits one stable `logs` document. `missive logs --ndjson`
emits `log_source` lines followed by `log_record` lines. All log messages and
JSON fields pass through the CLI redaction helpers, including auth schemes and
secret-like `key=value` fragments. `missive logs` is an inspection surface, not a
health checker. Use `missive doctor` for local binary/config/state-path, SQLite
migration-state, and tool-availability checks; endpoint reachability and gateway
liveness are reserved for the follow-up remote/gateway doctor checks.

Use `missive events tail` when you need structured control-plane event
diagnostics rather than stderr logs:

```bash
missive events tail --from-sequence 100 --limit 10 --ndjson
missive --timeout 5s events tail --type a2a.task.updated --ndjson
```
