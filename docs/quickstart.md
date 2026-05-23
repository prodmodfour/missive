# Quickstart

This guide gets a local `missive` checkout talking to the repository's mock A2A
agent without contacting third-party services. It is the fastest way to verify the
current user-facing workflow.

## 1. Build or install the CLI

From the repository root, either run through Cargo:

```bash
cargo run -p missive-cli --bin missive -- --help
```

or install the development binary into your Cargo bin directory:

```bash
cargo install --path crates/missive-cli --locked
missive --help
```

For isolated experiments, keep runtime state outside the repository:

```bash
export MISSIVE_HOME="$(mktemp -d /tmp/missive-quickstart.XXXXXX)"
```

`MISSIVE_HOME` contains profile databases, locks, logs, and caches. Delete it
when the quickstart is finished.

## 2. Run the smoke-tested examples

The simplest fully validated path is the bundled smoke runner:

```bash
examples/run-smoke.sh
```

For faster repeated runs, build the binary once and reuse it:

```bash
cargo build -p missive-cli --bin missive
MISSIVE_BIN="$PWD/target/debug/missive" examples/run-smoke.sh
```

The smoke runner starts the local mock A2A server, creates temporary
`MISSIVE_HOME` state, and executes the example scripts described in
[`examples.md`](examples.md). These scripts are also exercised by the normal Rust
workspace tests, so they are covered by `scripts/quality-gate.sh`.

## 3. Run a manual local session

Start the mock A2A server in one terminal:

```bash
ready_file="$(mktemp /tmp/missive-a2a-url.XXXXXX)"
cargo run -p missive-test-support --example mock_a2a_server -- --ready-file "$ready_file" &
mock_pid=$!
until test -s "$ready_file"; do sleep 0.05; done
export MISSIVE_EXAMPLE_A2A_BASE_URL="$(cat "$ready_file")"
```

Then use `missive` against that local server:

```bash
missive agent add echo "$MISSIVE_EXAMPLE_A2A_BASE_URL" --tag local --json
missive agent inspect echo --refresh --json
missive send echo "Hello from missive" --context ctx-quickstart --json
missive stream echo "Show progress" --ndjson
missive task list --agent echo --remote --json
missive events list --json
```

Stop the mock server when finished:

```bash
kill "$mock_pid"
```

## 4. Try local state and coordination commands

Contexts, groups, routing, and collectives use the same local profile database:

```bash
missive context create --id ctx-quickstart --name quickstart --agent echo --json
missive group create demo-team --routing-policy weighted --json
missive group add demo-team echo --rank rank-0 --tag local --weight 2 --json
missive route explain --group demo-team --policy capability-match --capability echo \
  --input-mode text/plain --streaming --refresh-capabilities --json
```

The collective commands are implemented, but the current smoke scripts exercise
only group and route setup. For full collective behavior, run a broadcast first,
then use the returned shared context id with `barrier`, `gather`, and `reduce`:

```bash
missive bcast demo-team "Summarize your status" --context ctx-quickstart --failure-policy continue --json
missive barrier demo-team --context ctx-quickstart --timeout 30s --json
missive gather demo-team --context ctx-quickstart --json
missive reduce demo-team --context ctx-quickstart --strategy summarise --json
```

## 5. Inspect diagnostics

Use local diagnostics before assuming a remote agent is broken:

```bash
missive doctor --json
missive logs --json
missive events tail --limit 10 --ndjson
```

`doctor` is a health-check surface. `logs` reads local profile log files when they
exist. `events` reads the SQLite event journal for the selected profile.

## 6. Where to go next

* [`agent-registry.md`](agent-registry.md) — register and inspect A2A agents.
* [`messaging.md`](messaging.md) — send text, JSON, and file parts.
* [`streaming.md`](streaming.md) — consume A2A streaming updates.
* [`tasks.md`](tasks.md) — inspect, wait for, cancel, and export task artifacts.
* [`contexts.md`](contexts.md) — manage context continuity.
* [`groups.md`](groups.md) and [`collectives.md`](collectives.md) — coordinate groups.
* [`gateway.md`](gateway.md) — run the local daemon, background jobs, and service helpers.
* [`adapters.md`](adapters.md) — use stdio, file-drop, and HTTP adapter surfaces.
* [`push-webhooks.md`](push-webhooks.md) — configure A2A push callbacks.
* [`troubleshooting.md`](troubleshooting.md) — diagnose common setup and runtime issues.
