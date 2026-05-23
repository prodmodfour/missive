# Interoperability checks

`missive` includes an opt-in compatibility script for running the CLI against the
upstream `a2aproject/a2a-rs` hello-world example agent. This is separate from the
normal mock-server and fixture tests: it downloads/builds an external upstream
example in a temporary directory and therefore is not part of the default quality
gate.

## Upstream source

Default source and pinned revision:

```text
repository: https://github.com/a2aproject/a2a-rs.git
revision:   a32ef57182dd0ecd1d3c04f338778f1974494905
example:    examples package, helloworld-server binary
```

The upstream example exposes:

* Agent Card: `http://127.0.0.1:3000/.well-known/agent-card.json`
* JSON-RPC endpoint: `http://localhost:3000/jsonrpc`
* HTTP+JSON endpoint: `http://localhost:3000/rest`
* gRPC endpoint: `localhost:50051` (recognized by missive, not exercised yet)

## Running the check

From a clean checkout:

```bash
scripts/interop-a2a-rs.sh
```

The script builds `target/debug/missive` if needed, clones the pinned upstream
revision into a temporary directory, starts the hello-world server, creates an
isolated `MISSIVE_HOME`, and runs these missive commands:

1. `agent add` for the upstream server.
2. `agent inspect --refresh --json` for Agent Card discovery and interface
   negotiation.
3. `send --json` and response validation.
4. `stream --ndjson` and streaming event validation.
5. `task list --remote --agent upstream --json` and remote task persistence
   validation.
6. `push create` only when the Agent Card advertises push notification support.

The hello-world Agent Card currently advertises `pushNotifications=false`, so the
push-config check is recorded as an upstream example limitation and skipped; this
is a successful result for this particular pinned example.

## Outputs and failure classification

The script prints a table and writes machine-readable results under its temporary
results directory:

```text
results.ndjson
summary.json
a2a-rs-helloworld.log
```

Set `MISSIVE_A2A_RS_KEEP_WORKDIR=1` to keep the temporary checkout, missive home,
logs, and result files for inspection.

Each result includes a classification:

* `missive_verified` — the command passed and output matched the expected
  machine-readable contract.
* `missive_bug_or_protocol_regression` — a missive command failed or emitted an
  unexpected JSON/NDJSON shape for a required upstream operation.
* `upstream_dependency` — cloning, building, or starting the pinned upstream
  example succeeded or failed.
* `upstream_example_limitation` — the upstream example does not advertise the
  optional feature, currently push notification configs.
* `environment` — a local prerequisite is missing, the missive binary is not
  executable, or the fixed upstream ports are unavailable.

The script exits nonzero for `fail` records and exits zero when all required
checks pass even if optional push support is skipped.

## Configuration knobs

| Variable | Default | Purpose |
| --- | --- | --- |
| `MISSIVE_BIN` | `target/debug/missive` | Use an existing missive binary instead of building the default. |
| `A2A_RS_REPO_URL` | `https://github.com/a2aproject/a2a-rs.git` | Override the upstream repository URL. |
| `A2A_RS_REV` | `a32ef57182dd0ecd1d3c04f338778f1974494905` | Override the pinned upstream revision. |
| `MISSIVE_A2A_RS_WORKDIR` | temporary directory | Reuse a specific working directory. |
| `MISSIVE_A2A_RS_RESULTS_DIR` | `$WORKDIR/results` | Write result files somewhere specific. |
| `MISSIVE_A2A_RS_KEEP_WORKDIR` | `0` | Keep temporary files when set to `1`. |
| `MISSIVE_A2A_RS_BASE_URL` | unset | Use an already-running compatible agent instead of cloning/starting upstream. |
| `A2A_RS_HTTP_PORT` | `3000` | Fixed HTTP port expected by the upstream hello-world example. |
| `A2A_RS_GRPC_PORT` | `50051` | Fixed gRPC port bound by the upstream hello-world example. |

If `MISSIVE_A2A_RS_BASE_URL` is set, the script skips the clone/start step and
uses the supplied server. That mode is useful when the upstream example is
already running under a local supervisor, but the default pinned mode is the
canonical reproducible check.

## Latest ticket validation result

Ticket 066 ran:

```bash
scripts/interop-a2a-rs.sh
```

Result summary:

```text
8 pass, 0 fail, 1 skip
```

The skipped check was `push_config` because the pinned hello-world example's
Agent Card advertises `pushNotifications=false`. Required checks for Agent Card
discovery, send, streaming, and remote task listing passed.
