# Contexts

`missive context` manages local A2A context rows used for conversation and task
continuity. Contexts are control-plane state in the selected profile database;
they are not long-term agent memory.

## Create and name a context

```bash
missive context create --id ctx-planning --name planning-round --agent echo \
  --summary "Planning context" --metadata workflow=planning --json
```

If `--id` is omitted, missive generates a local context id. Names are optional,
human-friendly selectors and must be unique when used as command arguments.

Use the context id with messaging commands:

```bash
missive send echo "Continue planning" --context ctx-planning --json
missive stream echo "Show planning progress" --context ctx-planning --ndjson
```

## Inspect, list, fork, and close

```bash
missive context list --json
missive context show ctx-planning --json
missive context fork ctx-planning --id ctx-planning-child --name planning-child --json
missive context close ctx-planning --json
```

Forking records the parent context id and metadata. Closing marks the local row
closed while retaining history; it does not call a remote close endpoint.

## Export a context

```bash
missive context export ctx-planning --json
missive context export planning-round --json
```

Exports include linked context, task, message, and event rows with normal output
redaction applied. Dedicated artifact files, push configs, gateway jobs, and
retention metadata are not part of context export yet.

## Current limitations

* `send`, `stream`, and `task` accept explicit A2A context ids, not context
  names. Resolve a name with `context show <name> --json` before automating.
* Contexts do not synchronize remote history or expose gateway session reset
  commands yet.
* Local SQLite state may contain raw protocol payloads before export-time
  redaction; keep state directories outside the repository.

## Smoke coverage

`examples/demo-contexts-groups.sh` covers `context create`, `show`, `fork`,
`list`, and `export` with deterministic local context ids. It is run by
`examples/run-smoke.sh` and the default quality gate.
