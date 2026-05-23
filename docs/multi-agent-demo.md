# End-to-end multi-agent demo

`examples/demo-multi-agent.sh` is a local-only end-to-end demo for the
collective workflow. It starts three compatible mock A2A agents, registers them
as `scout`, `analyst`, and `reviewer`, creates a `demo-squad` group, then runs:

1. `missive bcast` with one shared context id.
2. `missive barrier` against the broadcast result.
3. `missive gather` with safe artifact export.
4. `missive reduce` with the deterministic local `summarise` strategy.
5. `missive events list` for the shared context so the collective events are
   visible as machine-readable JSON.

Run it from a clean checkout with:

```bash
examples/demo-multi-agent.sh
```

Or run it as part of every example smoke test:

```bash
examples/run-smoke.sh
```

The script creates temporary runtime state under `MISSIVE_HOME` and writes JSON
outputs under a temporary `multi-agent-output/` directory. Use this when you want
to inspect the results after the script exits:

```bash
MISSIVE_EXAMPLE_KEEP_WORKDIR=1 examples/demo-multi-agent.sh
```

The generated files are:

| File | Contents |
| --- | --- |
| `bcast.json` | Stable `kind: "bcast_result"` envelope with one member row per agent. |
| `barrier.json` | Stable `kind: "barrier_result"` envelope showing quorum and task states. |
| `gather.json` | Stable `kind: "gather_result"` envelope with gathered text/artifact summaries. |
| `reduce.json` | Stable `kind: "reduce_result"` envelope with source-attributed reduced text. |
| `events.json` | Stable `kind: "events_list"` envelope containing `missive.bcast.*`, `missive.barrier.*`, `missive.gather.*`, and `missive.reduce.*` events for the shared context. |
| `artifacts/` | Safely named artifact exports from `missive gather --output-dir`. |

By default the demo starts three local mock servers. Advanced users can point it
at three already-running compatible A2A endpoints with a comma-separated list:

```bash
MISSIVE_EXAMPLE_MULTI_AGENT_URLS="http://127.0.0.1:3101,http://127.0.0.1:3102,http://127.0.0.1:3103" \
  examples/demo-multi-agent.sh
```

Only use that override with endpoints you control. The default path contacts no
third-party services and keeps runtime databases, logs, mock output, and artifact
exports outside the repository.
