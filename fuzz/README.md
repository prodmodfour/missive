# missive fuzz targets

This directory contains bounded [`cargo-fuzz`](https://rust-fuzz.github.io/book/cargo-fuzz.html) smoke targets for parser and replay surfaces that accept untrusted local or protocol input.

Targets:

* `config_parse` — TOML configuration parsing, validation, and redacted rendering.
* `a2a_json_parse` — A2A Agent Card, message, task, stream, push-config, JSON-RPC, and error JSON deserialization/serialization through `missive-a2a`'s official protocol type boundary.
* `event_replay` — event-journal replay summary reconstruction from generated event records.
* `cli_frame_parse` — stdio, HTTP, and file-drop adapter frame parsing plus NDJSON stdio frame handling.

Run a stable-toolchain compile check:

```bash
cargo fuzz build --sanitizer none
```

Run short smoke fuzzing locally:

```bash
MISSIVE_FUZZ_SECONDS=3 cargo fuzz run config_parse --sanitizer none -- -max_total_time=3
cargo fuzz run a2a_json_parse --sanitizer none -- -runs=100
cargo fuzz run event_replay --sanitizer none -- -runs=100
cargo fuzz run cli_frame_parse --sanitizer none -- -runs=100
```

The aggressive quality gate automatically runs every listed target when `cargo-fuzz` is installed. It defaults to `MISSIVE_FUZZ_SANITIZER=none`; set `MISSIVE_FUZZ_SANITIZER=address` when using a compatible nightly setup for sanitizer-backed campaigns:

```bash
MISSIVE_AGGRESSIVE_TESTS=1 MISSIVE_FUZZ_SECONDS=3 scripts/quality-gate.sh
```

Generated corpora, coverage output, and crash artifacts are ignored and guarded by repository hygiene checks. Commit only intentional, reviewed regression seeds if a future bug fix explicitly needs a stable corpus file.
