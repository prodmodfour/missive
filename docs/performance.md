# Performance benchmarks

missive uses Criterion benchmarks as local, non-blocking performance probes. They
are intended to make regressions inspectable while the project is still adding
features; the initial budgets below are soft guidance, not quality-gate failures.

## Running benchmarks

Compile every benchmark target without measuring:

```bash
cargo bench --workspace --all-features --no-run
```

Run the current benchmark suite:

```bash
cargo bench -p missive-cli --bench performance --all-features
```

Run the same benchmark target in Criterion's fast smoke-test mode:

```bash
cargo bench -p missive-cli --bench performance --all-features -- --test
```

Criterion stores local reports and baselines under `target/criterion/`, which is
build output and must not be committed. To inspect a local regression, save a
baseline before the change and compare after the change:

```bash
cargo bench -p missive-cli --bench performance --all-features -- --save-baseline before
# make the candidate change
cargo bench -p missive-cli --bench performance --all-features -- --baseline before
```

Aggressive quality-gate mode compiles benchmarks with `cargo bench --no-run`
when benchmark sources exist, but it does not run full measurements by default.

## Current benchmark coverage

The `performance` Criterion target currently lives at
[`crates/missive-cli/benches/performance.rs`](../crates/missive-cli/benches/performance.rs)
and covers:

* configuration loading and redacted rendering for a multi-agent TOML config;
* SQLite repository CRUD batches for agents, contexts, tasks, messages,
  artifacts, and events;
* router planning for capability-match, weighted, broadcast, and quorum
  policies;
* event-journal replay summary reconstruction;
* A2A streaming event JSON parsing, including JSON-RPC unwrapping;
* local group collective command paths for gather and deterministic summarise
  reduce over seeded profile state.

All benchmark fixtures use temporary in-memory stores or temporary `MISSIVE_HOME`
directories and local loopback URLs only. They do not contact third-party
agents.

## Initial soft budgets

These budgets are deliberately loose so early development is not blocked by
machine noise, debug packages, or still-evolving data models. Treat them as
investigation thresholds on a typical developer laptop after a release build has
already warmed dependencies.

| Area | Benchmark signal | Soft investigation budget |
| --- | --- | --- |
| Config load | parse and validate a 32-agent TOML config | Criterion upper estimate under 25 ms |
| Config redaction | render the same config as redacted JSON | Criterion upper estimate under 25 ms |
| Store operations | one CRUD batch for 32 task/message/artifact/event rows | Criterion upper estimate under 75 ms |
| Routing | 128-candidate policy planning | Criterion upper estimate under 20 ms per policy |
| Event replay | replay 512 event records into summaries | Criterion upper estimate under 50 ms |
| Stream parsing | parse 256 A2A stream events | Criterion upper estimate under 50 ms |
| Local collectives | gather or local summarise reduce over 16 members | Criterion upper estimate under 200 ms |

When a result exceeds a soft budget, first rerun with a saved Criterion baseline
and check whether the regression is repeatable. Only then decide whether to
optimize, adjust the budget with a documented reason, or add a future automated
performance gate.
