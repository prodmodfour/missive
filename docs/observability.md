# Observability

missive now initializes a shared tracing/logging subscriber through `crates/missive-observe`. The foundation is intentionally small: it configures log filters, stderr rendering, JSON log formatting, and log redaction. Operation-wide spans and detailed per-command instrumentation are added by a later ticket.

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

The `missive logs` command remains a placeholder until the diagnostics-command ticket lands; use stderr, `journalctl`, or macOS `log stream` for now.
