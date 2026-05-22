# CLI reference

`missive` currently exposes a clap-based command skeleton. The command tree,
shared flags, and output rendering contract are stable enough for help and
machine-output tests, but operational behaviour is implemented by later tickets.

Run help with:

```bash
missive --help
missive agent --help
missive send --help
```

## Global flags

The following flags are accepted at the top level and after subcommands:

* `--json` — request single-document JSON output when a command supports it.
* `--ndjson` — request newline-delimited JSON for event streams when supported.
* `--quiet` / `-q` — suppress non-error output.
* `--no-color` — disable colored terminal output and diagnostics.
* `--config <PATH>` — select an explicit configuration file path.
* `--profile <PROFILE>` — select a named profile.
* `--timeout <DURATION>` — set an overall timeout string such as `30s` or `2m`.
* `--trace` — request trace-oriented diagnostics.
* `--verbose` / `-v` — increase human diagnostic verbosity; repeat as needed.

The flags are parsed now so scripts can converge on a stable invocation shape.
Configuration discovery, timeout enforcement, tracing, and command-specific
semantics are intentionally left to their ordered implementation tickets.

## Top-level commands

The current skeleton includes these top-level commands:

```text
agent       Manage configured A2A agents and cached Agent Cards
send        Send one message to an A2A agent
stream      Stream message updates from an A2A agent
task        Inspect, list, wait for, or cancel A2A tasks
context     Manage conversation contexts and session continuity
group       Manage groups of agents for collective operations
gateway     Run and manage the local missive gateway daemon
webhook     Receive A2A push notification callbacks locally
push        Manage A2A push notification configurations
doctor      Diagnose local configuration, storage, gateway, and endpoint health
logs        Inspect local missive logs
events      Inspect, tail, replay, or export the local event journal
completion  Generate shell completion scripts
manpage     Generate manual pages
```

Each command has a help page. Running a command other than help currently emits
a command-status record through the selected renderer.

## Output contract

The current renderer supports four modes:

* default human output — one redacted status line for a parsed command
* `--json` — one JSON document using stable top-level fields
* `--ndjson` — one JSON object per line, currently one command-status event for
  skeletal commands and reserved for future event streams
* `--quiet` / `-q` — no non-error output

`--json` and `--ndjson` are mutually exclusive for command execution. If both are
provided, `missive` returns usage exit code `64` and renders a structured error;
when `--ndjson` is present, that error is one JSON object on stderr.

Current successful machine-readable output has this envelope:

```json
{
  "schema_version": "missive.output.v1",
  "ok": true,
  "kind": "command_status",
  "data": {
    "command": "agent",
    "status": "parsed",
    "implemented": false,
    "message": "missive: 'agent' command parsed; implementation lands in a later ticket"
  }
}
```

NDJSON uses the same envelope and adds a numeric `sequence` field:

```json
{"schema_version":"missive.output.v1","ok":true,"kind":"command_status","sequence":0,"data":{"command":"events","status":"parsed","implemented":false,"message":"missive: 'events' command parsed; implementation lands in a later ticket"}}
```

Structured errors use `ok: false`, `kind: "error"`, and the shared
`missive-core` error-report fields under `data`: `code`, `category`, `message`,
optional `help`, optional `sources`, and `exit_code`.

Before JSON/NDJSON values are written, the renderer recursively redacts
secret-like keys and HTTP-style auth headers. Authorization values preserve only
the auth scheme, for example `Bearer [REDACTED]`; raw tokens, API keys,
passwords, cookies, client secrets, and similar fields are not printed by normal
renderers.
