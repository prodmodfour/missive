# BUILD_TICKETS.md
AUTOMATION_STATUS: IN_PROGRESS
Ticket statuses:

* TODO
* IN_PROGRESS
* DONE
* BLOCKED
The build loop must select the lowest-numbered TODO or IN_PROGRESS ticket.
This queue contains 66 project-specific tickets for building `missive`. Each ticket is intended to produce one autonomous commit.
---
## 000 — Bootstrap repository skeleton

Status: DONE

Phase: Foundation

Objective:

Implement bootstrap repository skeleton for `missive` without starting later tickets.

Required:

* Create or normalise the Rust workspace root for missive.
* Add README.md, LICENSE, CONTRIBUTING.md, SECURITY.md, CHANGELOG.md, .gitignore, rust-toolchain.toml, and initial docs directories.
* Preserve this autonomous build system at repository root.

Acceptance criteria:

* Repository has a coherent Rust CLI workspace skeleton.
* No generated/private/runtime files are committed.
* scripts/quality-gate.sh passes from a clean checkout.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 001 — Define Cargo workspace and crate layout

Status: DONE

Phase: Foundation

Objective:

Implement define cargo workspace and crate layout for `missive` without starting later tickets.

Required:

* Create a Cargo workspace with crates/missive-cli, crates/missive-core, crates/missive-a2a, crates/missive-store, crates/missive-router, crates/missive-gateway, crates/missive-adapters, and crates/missive-observe.
* The published package may be named missive-cli, but the binary must be named missive.
* Add minimal compileable lib/bin targets and shared workspace dependency versions.

Acceptance criteria:

* cargo metadata succeeds.
* cargo build --workspace succeeds.
* The binary name missive is present in the CLI crate manifest.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 002 — Install and document autonomous build tooling

Status: DONE

Phase: Foundation

Objective:

Implement install and document autonomous build tooling for `missive` without starting later tickets.

Required:

* Add docs and optional scripts for installing rustup components and cargo tools useful for exhaustive validation.
* Tools may include clippy, rustfmt, cargo-nextest, cargo-llvm-cov, cargo-deny, cargo-audit, cargo-machete, cargo-mutants, cargo-fuzz, sqlx-cli, just, jq, gh, docker, and protoc as appropriate.
* Record commands used in BUILD_NOTES.md when installing tools.

Acceptance criteria:

* scripts/bootstrap-tools.sh is executable and idempotent.
* Documentation explains the agent may use sudo/package managers for build/test dependencies.
* quality gate passes even when optional tools are absent, while using them when present.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 003 — Harden Rust quality gate

Status: DONE

Phase: Foundation

Objective:

Implement harden rust quality gate for `missive` without starting later tickets.

Required:

* Extend scripts/quality-gate.sh for Rust fmt, clippy, workspace tests, doc tests, release build, cargo check across features, secret scanning, and generated/private-file scanning.
* Add optional aggressive validation paths for nextest, llvm-cov, audit, deny, machete, miri, mutants, fuzz smoke tests, Docker integration tests, and benchmarks.

Acceptance criteria:

* scripts/quality-gate.sh exits nonzero on format, clippy, test, secret, or generated-file failures.
* Default gate is suitable for every autonomous cycle.
* MISSIVE_AGGRESSIVE_TESTS=1 enables deeper checks without changing script source.

Validation:

* Run `scripts/quality-gate.sh`.
* Also run aggressive or targeted checks relevant to this ticket when feasible.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 004 — Create architecture decision records scaffold

Status: DONE

Phase: Foundation

Objective:

Implement create architecture decision records scaffold for `missive` without starting later tickets.

Required:

* Add docs/adr/ with ADR template and initial ADRs for Rust workspace structure, A2A-first protocol strategy, SQLite local state, and CLI-first UX.
* Document alternatives considered, especially wrapping a2a-rs versus hand-rolling protocol models.

Acceptance criteria:

* At least four ADRs exist and are linked from docs/architecture.md.
* ADR status values are consistent.
* docs validation passes.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 005 — Implement core error and result types

Status: DONE

Phase: Core

Objective:

Implement implement core error and result types for `missive` without starting later tickets.

Required:

* Add missive-core error taxonomy using thiserror/miette or equivalent.
* Cover IO, config, protocol, transport, storage, auth, validation, and orchestration errors.
* Ensure errors render well for both humans and JSON output.

Acceptance criteria:

* Unit tests cover representative error rendering.
* No panic-based error handling in public APIs.
* CLI can map errors to deterministic exit codes in later tickets.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 006 — Implement IDs, timestamps, metadata, and envelope primitives

Status: DONE

Phase: Core

Objective:

Implement implement ids, timestamps, metadata, and envelope primitives for `missive` without starting later tickets.

Required:

* Create strongly typed AgentAlias, ContextId, TaskId, MessageId, GroupName, RankName, EventId, and TransportName wrappers.
* Add metadata map helpers and serde support.
* Add deterministic display/parse tests.

Acceptance criteria:

* All ID types have FromStr/Display/Serialize/Deserialize coverage.
* Invalid aliases/group names are rejected with clear diagnostics.
* Property tests cover valid/invalid identifier round trips where useful.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 007 — Implement CLI skeleton and global flags

Status: DONE

Phase: CLI

Objective:

Implement implement cli skeleton and global flags for `missive` without starting later tickets.

Required:

* Use clap derive to create missive with subcommands for agent, send, stream, task, context, group, gateway, webhook, push, doctor, logs, events, completion, and manpage.
* Add global --json, --ndjson, --quiet, --no-color, --config, --profile, --timeout, --trace, and --verbose flags.

Acceptance criteria:

* missive --help is useful and stable.
* Every subcommand has a help page.
* Snapshot tests cover help output for key commands.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 008 — Implement output rendering contract

Status: DONE

Phase: CLI

Objective:

Implement implement output rendering contract for `missive` without starting later tickets.

Required:

* Create human, JSON, NDJSON, and quiet output renderers.
* Ensure agent-callable mode has machine-readable output and stable field names.
* Add redaction helpers for tokens, headers, and secrets.

Acceptance criteria:

* --json output parses as JSON for every implemented command.
* --ndjson emits one JSON object per line for event streams.
* No secret-like values are printed in normal logs.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 009 — Implement configuration discovery and profiles

Status: DONE

Phase: Core

Objective:

Implement implement configuration discovery and profiles for `missive` without starting later tickets.

Required:

* Support config discovery via --config, MISSIVE_CONFIG, XDG config dirs, and repository-local config when explicitly requested.
* Define config schema for profiles, agents, auth refs, storage, output defaults, gateway, adapters, and quality of service.
* Add config validation and redacted config rendering.

Acceptance criteria:

* Config examples load successfully.
* Invalid configs fail with actionable diagnostics.
* Config secrets are never displayed raw.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 010 — Implement local state paths and lock handling

Status: DONE

Phase: Store

Objective:

Implement implement local state paths and lock handling for `missive` without starting later tickets.

Required:

* Resolve XDG-compatible data/state/cache paths for missive.
* Support MISSIVE_HOME and profile-specific state directories.
* Add process locks for state mutation and gateway operation.

Acceptance criteria:

* Path resolution tests cover Linux/macOS fallback behaviour.
* Concurrent lock acquisition is tested.
* No runtime state is written into source tree by default.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 011 — Design SQLite schema and migrations

Status: DONE

Phase: Store

Objective:

Implement design sqlite schema and migrations for `missive` without starting later tickets.

Required:

* Create migrations for agents, contexts, tasks, messages, artifacts, events, groups, group_members, auth_refs, push_configs, gateway_jobs, and adapter_bindings.
* Use sqlx or rusqlite with a clear migration strategy.
* Include schema docs.

Acceptance criteria:

* Fresh database migration succeeds.
* Migration tests run against temporary databases.
* Schema docs include table purpose and retention notes.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 012 — Implement store repository APIs

Status: DONE

Phase: Store

Objective:

Implement implement store repository apis for `missive` without starting later tickets.

Required:

* Add typed repository methods for agents, contexts, tasks, events, groups, and gateway jobs.
* Ensure store APIs are async-aware or blocking-safe depending on chosen database crate.
* Add transactional update helpers.

Acceptance criteria:

* Unit/integration tests cover CRUD paths.
* Transactions roll back on failure.
* Store APIs do not leak SQL strings into CLI code.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 013 — Implement agent registry commands

Status: DONE

Phase: Agent Registry

Objective:

Implement implement agent registry commands for `missive` without starting later tickets.

Required:

* Add missive agent add/remove/list/show/rename commands.
* Support aliases, base URLs, explicit interface URLs, auth refs, tags, notes, and metadata.
* Persist registry entries in SQLite and allow config-seeded read-only entries.

Acceptance criteria:

* agent add/list/show/remove work in human and JSON modes.
* Alias validation is enforced.
* Tests cover duplicate aliases and missing agents.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 014 — Implement public Agent Card discovery

Status: DONE

Phase: A2A

Objective:

Implement implement public agent card discovery for `missive` without starting later tickets.

Required:

* Fetch /.well-known/agent-card.json from a base URL.
* Cache raw and parsed Agent Cards with timestamps and ETags/Last-Modified where available.
* Expose missive agent inspect and missive agent refresh.

Acceptance criteria:

* Mock HTTP tests cover successful fetch, 404, malformed JSON, TLS/HTTP errors, and cache refresh.
* agent inspect prints capabilities, skills, provider, versions, and supported interfaces.
* Card cache can be bypassed with --refresh.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 015 — Implement A2A interface negotiation

Status: DONE

Phase: A2A

Objective:

Implement implement a2a interface negotiation for `missive` without starting later tickets.

Required:

* Select the first supported interface from Agent Card supportedInterfaces, respecting ordered preference.
* Support HTTP+JSON and JSON-RPC first; prepare extension points for gRPC.
* Allow --binding override for tests and advanced users.

Acceptance criteria:

* Negotiation tests cover preference order, unsupported bindings, explicit override, and missing supportedInterfaces fallback.
* Selected binding is visible in --json inspect output.
* Errors explain which bindings are supported locally.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 016 — Integrate official or vendored A2A Rust types

Status: DONE

Phase: A2A

Objective:

Implement integrate official or vendored a2a rust types for `missive` without starting later tickets.

Required:

* Evaluate a2a-rs dependencies and wire missive-a2a to use official protocol types where practical.
* If crates are Git dependencies, pin revisions and document update process.
* If wrapping is not viable, create a compatibility layer with conformance fixtures.

Acceptance criteria:

* Protocol type strategy is documented in an ADR.
* missive-a2a compiles without duplicating unnecessary protocol structs.
* A2A fixtures round-trip through serde.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 017 — Implement A2A service parameter handling

Status: DONE

Phase: A2A

Objective:

Implement implement a2a service parameter handling for `missive` without starting later tickets.

Required:

* Send A2A-Version on every request by default.
* Support A2A-Extensions and arbitrary service parameters where applicable.
* Add explicit protocol-version config and CLI override.

Acceptance criteria:

* HTTP mock tests assert A2A-Version is sent.
* Unsupported version responses map to a specific error and exit code.
* Version used is recorded in event/task metadata.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 018 — Implement authentication inputs and redaction

Status: DONE

Phase: Security

Objective:

Implement implement authentication inputs and redaction for `missive` without starting later tickets.

Required:

* Support --bearer-token-env, --header Name:Value, config auth refs, and keyring-backed tokens where available.
* Do not store raw tokens in SQLite unless explicitly configured for local-only insecure mode.
* Redact tokens in logs, events, errors, and debug output.

Acceptance criteria:

* Tests prove auth headers are sent but redacted from output.
* Missing env vars fail clearly.
* Security docs explain auth storage tradeoffs.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 019 — Implement send message command

Status: DONE

Phase: Messaging

Objective:

Implement implement send message command for `missive` without starting later tickets.

Required:

* Add missive send <agent> <message>, --stdin, --file, --part text=, --metadata, --context, --task, --accepted-output-mode, and --json.
* Persist request/response messages and task linkage.
* Support direct response Message and Task response shapes.

Acceptance criteria:

* Mock A2A server tests cover direct message and task response.
* CLI examples work with stdin and file input.
* Output includes task id/context id when provided by server.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 020 — Implement streaming message command

Status: TODO

Phase: Messaging

Objective:

Implement implement streaming message command for `missive` without starting later tickets.

Required:

* Add missive stream <agent> ... using A2A streaming where capability allows.
* Render human streaming updates and NDJSON events.
* Persist streaming status/artifact events as they arrive.

Acceptance criteria:

* SSE/mock streaming tests cover status updates, artifact updates, completion, cancellation, and malformed events.
* Capability validation prevents stream attempts when unsupported unless --force is used.
* NDJSON stream is machine readable.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 021 — Implement task get/list/wait/cancel

Status: TODO

Phase: Tasks

Objective:

Implement implement task get/list/wait/cancel for `missive` without starting later tickets.

Required:

* Add missive task get/list/wait/cancel commands.
* Support filtering by agent, context, task state, updated-after, and local/remote source.
* Implement polling wait with configurable timeout and interval.

Acceptance criteria:

* Mock tests cover task state transitions and cancellation.
* wait exits with deterministic codes for complete, failed, cancelled, timeout, and input-required.
* Task output is available in human and JSON modes.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 022 — Implement context/session commands

Status: TODO

Phase: Contexts

Objective:

Implement implement context/session commands for `missive` without starting later tickets.

Required:

* Add missive context create/list/show/fork/close/export commands.
* Track A2A contextId continuity across messages and tasks.
* Allow human-friendly context names.

Acceptance criteria:

* Context IDs are persisted and reused correctly.
* Export includes messages/tasks/events without raw secrets.
* Forking records parent context metadata.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 023 — Implement message parts for text, files, and structured data

Status: TODO

Phase: Messaging

Objective:

Implement implement message parts for text, files, and structured data for `missive` without starting later tickets.

Required:

* Support text parts, file references/bytes, MIME types, and JSON structured-data parts according to A2A compatibility.
* Add CLI parsing for --file, --file-bytes, --json-part, and --mime.
* Implement safe local file path handling and size limits configurable by profile.

Acceptance criteria:

* Tests cover text, JSON, and file inputs.
* Large files fail or stream according to documented limits.
* MIME metadata is preserved.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 024 — Implement artifact handling and export

Status: TODO

Phase: Artifacts

Objective:

Implement implement artifact handling and export for `missive` without starting later tickets.

Required:

* Persist and render returned artifacts from A2A tasks.
* Support missive artifact list/show/save/export or equivalent task artifact commands.
* Handle text, JSON, and file artifacts.

Acceptance criteria:

* Artifacts can be saved to disk safely without path traversal.
* Artifact metadata is visible in task show output.
* Tests cover multiple artifacts and incremental artifact updates.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 025 — Implement event journal and replay

Status: TODO

Phase: Events

Objective:

Implement implement event journal and replay for `missive` without starting later tickets.

Required:

* Store local event records for agent registry changes, requests, responses, streaming updates, task changes, group operations, and gateway callbacks.
* Add missive events tail/list/replay/export.
* Support NDJSON export for agent use.

Acceptance criteria:

* Event records include sequence, timestamp, source, type, task/context IDs, and redacted payload.
* events tail follows new events.
* Replay reconstructs task/context summaries in tests.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 026 — Build local mock A2A server fixtures

Status: TODO

Phase: Testing

Objective:

Implement build local mock a2a server fixtures for `missive` without starting later tickets.

Required:

* Create a mock A2A server for integration tests with REST/HTTP+JSON and JSON-RPC behaviours.
* Support controllable task states, streaming events, push config endpoints, auth requirements, version errors, and malformed responses.

Acceptance criteria:

* Integration tests can run fully locally without external agents.
* Server fixtures are reusable by later tickets.
* Test docs explain how to run and extend fixtures.

Validation:

* Run `scripts/quality-gate.sh`.
* Also run aggressive or targeted checks relevant to this ticket when feasible.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 027 — Add A2A conformance fixture suite

Status: TODO

Phase: Testing

Objective:

Implement add a2a conformance fixture suite for `missive` without starting later tickets.

Required:

* Create fixtures from the current A2A spec examples for Agent Cards, messages, tasks, artifacts, push configs, and errors.
* Add serde round-trip tests and CLI golden outputs.
* Track protocol version in fixture names.

Acceptance criteria:

* Fixtures live under tests/fixtures/a2a/<version>.
* Round-trip tests pass.
* Future protocol updates have a documented fixture update process.

Validation:

* Run `scripts/quality-gate.sh`.
* Also run aggressive or targeted checks relevant to this ticket when feasible.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 028 — Implement push notification config commands

Status: TODO

Phase: Push

Objective:

Implement implement push notification config commands for `missive` without starting later tickets.

Required:

* Add missive push create/get/list/delete for A2A task push notification configs.
* Support URL, auth info, config id, metadata, and JSON output.
* Persist local records of configured push endpoints.

Acceptance criteria:

* Mock tests cover all push config endpoints.
* push create validates callback URL shape.
* Auth info is redacted in output.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 029 — Implement webhook receiver

Status: TODO

Phase: Gateway

Objective:

Implement implement webhook receiver for `missive` without starting later tickets.

Required:

* Add missive webhook run to receive A2A push notifications locally.
* Validate payloads, persist events, print NDJSON optionally, and expose health endpoint.
* Support configurable bind address, port, TLS termination note, and auth validation hooks.

Acceptance criteria:

* Webhook integration tests post valid and invalid payloads.
* Receiver shuts down gracefully.
* Docs include local tunneling examples without requiring a specific vendor.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 030 — Implement gateway daemon skeleton

Status: TODO

Phase: Gateway

Objective:

Implement implement gateway daemon skeleton for `missive` without starting later tickets.

Required:

* Add missive gateway run with async task supervisor, event bus, store access, and graceful shutdown.
* Gateway should manage subscriptions, webhook receiver, retries, background jobs, and adapter tasks in later tickets.

Acceptance criteria:

* gateway run starts and stops cleanly.
* Health/status endpoint or command reports running components.
* No orphan processes remain after tests.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 031 — Implement gateway subscriptions and resume

Status: TODO

Phase: Gateway

Objective:

Implement implement gateway subscriptions and resume for `missive` without starting later tickets.

Required:

* Let gateway subscribe to remote task updates where supported and resume local monitoring after restart.
* Persist subscription jobs and backoff state.
* Handle terminal tasks by stopping subscription loops.

Acceptance criteria:

* Tests simulate restart with in-flight tasks.
* Backoff is bounded and observable.
* Terminal task subscriptions are cleaned up.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 032 — Implement gateway service installation

Status: TODO

Phase: Gateway

Objective:

Implement implement gateway service installation for `missive` without starting later tickets.

Required:

* Add missive gateway install/start/stop/status/uninstall for Linux systemd user service and macOS launchd where feasible.
* Include optional --system support only when safely documented.
* Capture PATH and environment requirements for installed services.

Acceptance criteria:

* Generated service files are testable with dry-run mode.
* Commands explain unsupported platforms clearly.
* Docs include journal/log inspection commands.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 033 — Implement group model and commands

Status: TODO

Phase: Groups

Objective:

Implement implement group model and commands for `missive` without starting later tickets.

Required:

* Add missive group create/list/show/add/remove/rename/delete.
* Represent group members with alias, rank name, tags, weight, and routing metadata.
* Persist group membership and validate references.

Acceptance criteria:

* Group CRUD tests pass.
* Duplicate rank names are handled.
* group show displays members and routing policy.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 034 — Implement broadcast collective

Status: TODO

Phase: Collectives

Objective:

Implement implement broadcast collective for `missive` without starting later tickets.

Required:

* Add missive bcast <group> <message> to send the same message to all group members.
* Support sequential and concurrent execution, context creation, failure policy, and JSON summary.
* Persist group operation events.

Acceptance criteria:

* Tests cover successful broadcast, partial failure, and timeout.
* Each member gets a task/message record.
* Output includes per-agent task ids and states.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 035 — Implement barrier collective

Status: TODO

Phase: Collectives

Objective:

Implement implement barrier collective for `missive` without starting later tickets.

Required:

* Add missive barrier <group> --context <id> to wait for member tasks to reach terminal or requested states.
* Support timeout, required quorum, and failure policy.

Acceptance criteria:

* Barrier exits deterministically for success, timeout, failure, and cancellation.
* Barrier can consume previous bcast operation output.
* Tests cover terminal-state detection.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 036 — Implement gather collective

Status: TODO

Phase: Collectives

Objective:

Implement implement gather collective for `missive` without starting later tickets.

Required:

* Add missive gather <group> --context <id> to collect latest outputs/artifacts from group member tasks.
* Support markdown, JSON, NDJSON, and file export.
* Preserve rank/member ordering.

Acceptance criteria:

* Gather output is deterministic.
* Missing outputs are represented clearly.
* Artifacts can be exported without overwriting unsafe paths.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 037 — Implement reduce collective

Status: TODO

Phase: Collectives

Objective:

Implement implement reduce collective for `missive` without starting later tickets.

Required:

* Add missive reduce <group> using a reducer agent, local reducer template, or command pipeline.
* Support summarise, vote, merge, rank, and custom prompt strategies.
* Record provenance from gathered inputs to final reduced output.

Acceptance criteria:

* Tests cover local deterministic reducer and mocked reducer agent.
* Reduced output includes source references/provenance.
* Failure modes are documented.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 038 — Implement routing policies

Status: TODO

Phase: Router

Objective:

Implement implement routing policies for `missive` without starting later tickets.

Required:

* Add router policies for direct, capability-match, tag-match, round-robin, weighted, broadcast, first-success, quorum, and fallback.
* Expose missive route explain for dry-run routing decisions.

Acceptance criteria:

* Policy unit tests cover deterministic decisions.
* Route explain output is available in human and JSON modes.
* Invalid policies fail config validation.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 039 — Implement capability-aware agent selection

Status: TODO

Phase: Router

Objective:

Implement implement capability-aware agent selection for `missive` without starting later tickets.

Required:

* Use Agent Card skills, input/output modes, streaming support, push support, and tags to choose agents.
* Add missive agent capabilities and group capability summaries.

Acceptance criteria:

* Selection tests cover matching and tie-breaking.
* Missing capabilities produce actionable messages.
* Capabilities are refreshed/cached correctly.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 040 — Implement gateway session store inspired by Hermes

Status: TODO

Phase: Sessions

Objective:

Implement implement gateway session store inspired by hermes for `missive` without starting later tickets.

Required:

* Add persistent per-source/per-agent sessions with reset policies, context linking, and named resume.
* Support daily, idle, and combined reset modes.
* Keep this as communication/session state, not agent memory.

Acceptance criteria:

* Session persistence survives process restart.
* Reset policy tests use controllable clocks.
* Docs distinguish sessions from long-term memory.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 041 — Implement busy input modes

Status: TODO

Phase: Sessions

Objective:

Implement implement busy input modes for `missive` without starting later tickets.

Required:

* Implement queue, interrupt, and steer semantics for gateway/adapters where possible.
* For interrupt, cancel local waits/subscriptions and request remote task cancellation when appropriate.
* For steer, append follow-up input to active task/context if protocol state allows.

Acceptance criteria:

* Tests cover queue, interrupt, steer, and unsupported steer fallback.
* In-flight operation state remains consistent.
* Behaviour is configurable per profile/source.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 042 — Implement background communication jobs

Status: TODO

Phase: Gateway

Objective:

Implement implement background communication jobs for `missive` without starting later tickets.

Required:

* Add missive job start/list/show/cancel for background send/stream/wait/reduce operations managed by gateway.
* Persist jobs and deliver results via events, stdout attach, or adapter callbacks.

Acceptance criteria:

* Background jobs survive gateway restart where possible.
* job cancel cancels local job and remote task when configured.
* Docs include examples for agents invoking jobs non-interactively.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 043 — Define adapter trait and registry

Status: TODO

Phase: Adapters

Objective:

Implement define adapter trait and registry for `missive` without starting later tickets.

Required:

* Create missive-adapters trait/interface for inbound messages, outbound updates, identity mapping, sessions, and acknowledgements.
* Add adapter registry and config schema.
* Do not implement external chat platforms yet unless needed for local tests.

Acceptance criteria:

* Adapter trait has unit tests with a fake adapter.
* Adapters can emit messages into gateway event bus.
* Docs explain adapter lifecycle.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 044 — Implement stdin/stdout adapter

Status: TODO

Phase: Adapters

Objective:

Implement implement stdin/stdout adapter for `missive` without starting later tickets.

Required:

* Add adapter for agent/human use via stdin/stdout with JSON/NDJSON framing.
* Support single-shot and long-running modes.
* Map input frames to send/stream/task commands.

Acceptance criteria:

* Tests cover valid frames, invalid frames, and streaming output.
* Adapter is useful for another agent invoking missive as a subprocess.
* Docs include shell examples.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 045 — Implement file drop adapter

Status: TODO

Phase: Adapters

Objective:

Implement implement file drop adapter for `missive` without starting later tickets.

Required:

* Add adapter that watches an inbox directory for message/job files and writes results to outbox.
* Support atomic file handoff and processed/error directories.
* Useful for simple agent-to-agent automation without network services.

Acceptance criteria:

* Temp-directory integration tests cover handoff and errors.
* No partial files are processed.
* Docs explain file naming and schemas.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 046 — Implement HTTP inbound adapter

Status: TODO

Phase: Adapters

Objective:

Implement implement http inbound adapter for `missive` without starting later tickets.

Required:

* Add local HTTP endpoint for inbound control messages to gateway, separate from A2A push webhook if useful.
* Support auth token, JSON schema validation, rate/size limits, and health endpoint.

Acceptance criteria:

* HTTP adapter tests cover auth, valid requests, invalid requests, and shutdown.
* Secrets are redacted.
* Docs include curl examples.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 047 — Add external chat adapter stubs and roadmap

Status: TODO

Phase: Adapters

Objective:

Implement add external chat adapter stubs and roadmap for `missive` without starting later tickets.

Required:

* Create compileable stubs or feature-gated placeholders for Discord, Slack, Telegram, Matrix, and Email adapters.
* Do not add real platform credentials or unnecessary heavy dependencies until a later explicit ticket.
* Document Hermes-inspired design boundaries.

Acceptance criteria:

* Feature flags compile with and without adapter stubs.
* Roadmap documents required secrets, permissions, and platform behaviours.
* No real platform tokens are included.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 048 — Implement observability and tracing

Status: TODO

Phase: Observability

Objective:

Implement implement observability and tracing for `missive` without starting later tickets.

Required:

* Add tracing spans for CLI commands, A2A requests, store operations, gateway jobs, adapter events, and collectives.
* Support RUST_LOG/env filters and optional JSON logs.
* Redact secrets in traces.

Acceptance criteria:

* Tests or snapshots confirm redaction.
* Debug logs help diagnose failed protocol calls.
* Docs explain log levels and destinations.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 049 — Implement logs and diagnostics commands

Status: TODO

Phase: Observability

Objective:

Implement implement logs and diagnostics commands for `missive` without starting later tickets.

Required:

* Add missive logs, missive events tail, and missive doctor.
* doctor should check Rust binary version, config validity, database migrations, tool availability, A2A endpoint reachability, and gateway status.

Acceptance criteria:

* doctor works with no config and with a populated config.
* doctor --json emits actionable status objects.
* logs/events commands do not expose secrets.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 050 — Implement shell completions and manpages

Status: TODO

Phase: CLI Polish

Objective:

Implement implement shell completions and manpages for `missive` without starting later tickets.

Required:

* Add missive completion <shell> and missive manpage generation.
* Document installation locations for bash, zsh, fish, powershell, and manpages.

Acceptance criteria:

* Generated completions are tested/snapshotted where feasible.
* Manpage generation does not require network.
* README links to completion docs.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 051 — Implement command examples and smoke tests

Status: TODO

Phase: Testing

Objective:

Implement implement command examples and smoke tests for `missive` without starting later tickets.

Required:

* Add examples/ with scripted demos for agent registry, send, stream, tasks, contexts, groups, and gateway.
* Use mock A2A server so examples can run in CI.
* Add smoke tests that execute examples.

Acceptance criteria:

* Examples run from a clean checkout.
* Examples are included in docs.
* Smoke tests are part of quality gate or CI.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 052 — Add property tests for parsers and routing

Status: TODO

Phase: Testing

Objective:

Implement add property tests for parsers and routing for `missive` without starting later tickets.

Required:

* Use proptest/quickcheck to exercise IDs, config parsing, CLI value parsers, routing policy selection, and metadata merging.
* Keep generated cases reproducible on failure.

Acceptance criteria:

* Property tests run in normal or aggressive gate as appropriate.
* At least five core parser/router invariants are covered.
* Failing seeds are documented by test output.

Validation:

* Run `scripts/quality-gate.sh`.
* Also run aggressive or targeted checks relevant to this ticket when feasible.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 053 — Add fuzzing smoke tests

Status: TODO

Phase: Testing

Objective:

Implement add fuzzing smoke tests for `missive` without starting later tickets.

Required:

* Set up cargo-fuzz or equivalent for config parsing, A2A JSON parsing, event replay, and CLI frame parsing.
* Add short smoke fuzz runs suitable for aggressive gate.

Acceptance criteria:

* Fuzz targets compile.
* Short fuzz smoke run completes without crashes.
* Corpus and artifacts are not accidentally committed unless intentional seed corpus.

Validation:

* Run `scripts/quality-gate.sh`.
* Also run aggressive or targeted checks relevant to this ticket when feasible.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 054 — Add mutation and failure-injection tests

Status: TODO

Phase: Testing

Objective:

Implement add mutation and failure-injection tests for `missive` without starting later tickets.

Required:

* Use cargo-mutants or targeted failure-injection tests for critical modules: store, router, auth redaction, task wait/cancel, and collectives.
* Document how to run longer mutation campaigns.

Acceptance criteria:

* At least one mutation/failure-injection path is available.
* Aggressive gate can run a bounded mutation smoke test.
* Results are documented in BUILD_NOTES.md when run.

Validation:

* Run `scripts/quality-gate.sh`.
* Also run aggressive or targeted checks relevant to this ticket when feasible.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 055 — Add benchmarks and performance budgets

Status: TODO

Phase: Testing

Objective:

Implement add benchmarks and performance budgets for `missive` without starting later tickets.

Required:

* Add criterion or iai benchmarks for config load, store operations, routing, event replay, streaming event parsing, and group collectives.
* Define initial performance budgets without blocking early development excessively.

Acceptance criteria:

* cargo bench or equivalent works.
* Benchmarks are documented.
* Performance regressions can be inspected locally.

Validation:

* Run `scripts/quality-gate.sh`.
* Also run aggressive or targeted checks relevant to this ticket when feasible.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 056 — Add CI workflow

Status: TODO

Phase: CI/CD

Objective:

Implement add ci workflow for `missive` without starting later tickets.

Required:

* Add GitHub Actions workflow for fmt, clippy, tests, doc tests, build, secret scan, generated-file guardrail, and optional coverage.
* Use cache responsibly and avoid storing secrets.

Acceptance criteria:

* CI YAML validates.
* Workflow runs on push and pull_request.
* Local quality gate mirrors CI as closely as possible.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 057 — Add cross-platform build matrix

Status: TODO

Phase: CI/CD

Objective:

Implement add cross-platform build matrix for `missive` without starting later tickets.

Required:

* Extend CI to Linux, macOS, and Windows where Rust CLI functionality should work.
* Document any gateway/service features that are platform-specific.

Acceptance criteria:

* Matrix builds compile the workspace.
* Platform-specific tests are gated correctly.
* Docs are honest about unsupported functionality.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 058 — Add Docker and devcontainer support

Status: TODO

Phase: Developer Experience

Objective:

Implement add docker and devcontainer support for `missive` without starting later tickets.

Required:

* Add Dockerfile and optional devcontainer for reproducible builds/tests.
* Include Rust toolchain, protobuf tooling, SQLite, and useful cargo tools.
* Do not bake in secrets.

Acceptance criteria:

* docker build succeeds.
* Container can run scripts/quality-gate.sh.
* Docs include local and container workflows.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 059 — Add release packaging

Status: TODO

Phase: Release

Objective:

Implement add release packaging for `missive` without starting later tickets.

Required:

* Add release profile, cargo-dist or equivalent packaging plan, binaries for common targets, checksums, and install script docs.
* Ensure binary remains named missive.

Acceptance criteria:

* Release dry run succeeds locally or in CI.
* Checksums are generated for artifacts.
* README includes install/update instructions.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 060 — Add supply-chain checks

Status: TODO

Phase: Security

Objective:

Implement add supply-chain checks for `missive` without starting later tickets.

Required:

* Add cargo-deny/advisory configuration, license policy, duplicate dependency checks, and dependency update docs.
* Add SBOM generation if practical.

Acceptance criteria:

* cargo deny check passes or documented exceptions are justified.
* Dependency policy is documented.
* SBOM generation is available or consciously deferred.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 061 — Write user documentation

Status: TODO

Phase: Documentation

Objective:

Implement write user documentation for `missive` without starting later tickets.

Required:

* Create docs for quickstart, CLI reference, configuration, agent registry, messaging, streaming, tasks, contexts, groups, gateway, adapters, push/webhooks, troubleshooting, and examples.
* Keep docs aligned with implemented commands only; mark future features clearly until implemented.

Acceptance criteria:

* README links to all major docs.
* Examples in docs are covered by smoke tests where feasible.
* Docs avoid unsupported claims.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 062 — Write architecture and operations documentation

Status: TODO

Phase: Documentation

Objective:

Implement write architecture and operations documentation for `missive` without starting later tickets.

Required:

* Create docs/architecture.md, docs/storage.md, docs/protocol.md, docs/gateway.md, docs/security.md, docs/testing.md, and docs/runbook.md.
* Include diagrams in Mermaid where helpful.

Acceptance criteria:

* Architecture docs describe crate boundaries and data flow.
* Runbook includes diagnosis and recovery steps.
* Security docs cover auth, redaction, webhooks, and adapter trust boundaries.

Validation:

* Run `scripts/quality-gate.sh`.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 063 — Add compatibility tests against a2a-rs example agent

Status: TODO

Phase: Interoperability

Objective:

Implement add compatibility tests against a2a-rs example agent for `missive` without starting later tickets.

Required:

* Run missive against the a2a-rs helloworld/example agent or equivalent local compatible agent.
* Test card discovery, send, stream, list tasks, and push config where supported.
* Pin or document external dependency source.

Acceptance criteria:

* Interoperability script is reproducible.
* Results are documented.
* Failures distinguish missive bugs from upstream/example limitations.

Validation:

* Run `scripts/quality-gate.sh`.
* Also run aggressive or targeted checks relevant to this ticket when feasible.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 064 — Add end-to-end multi-agent demo

Status: TODO

Phase: Interoperability

Objective:

Implement add end-to-end multi-agent demo for `missive` without starting later tickets.

Required:

* Create a local demo with at least three mock/real compatible agents and a group workflow using bcast, barrier, gather, and reduce.
* Use only local services unless explicitly configured otherwise.

Acceptance criteria:

* Demo runs from a clean checkout.
* Demo output is documented and machine-readable.
* Collective operation events are visible via missive events.

Validation:

* Run `scripts/quality-gate.sh`.
* Also run aggressive or targeted checks relevant to this ticket when feasible.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

## 065 — Final autonomous review and completion marker

Status: TODO

Phase: Completion

Objective:

Implement final autonomous review and completion marker for `missive` without starting later tickets.

Required:

* Review every ticket, PROJECT_BRIEF.md, AGENTS.md, BUILD_NOTES.md, README, docs, code, tests, CI, release config, and safety posture.
* Run full quality gate plus aggressive tests where feasible.
* Ensure no secrets/private/generated files are committed.
* Set top-level AUTOMATION_STATUS: DONE only when genuinely complete.

Acceptance criteria:

* All prior tickets are DONE or explicitly justified if not applicable.
* scripts/quality-gate.sh passes.
* Repository is ready for intended open-source/internal audience.

Validation:

* Run `scripts/quality-gate.sh`.
* Also run aggressive or targeted checks relevant to this ticket when feasible.
* Update `BUILD_TICKETS.md` and `BUILD_NOTES.md`.
* Commit the completed ticket with a conventional commit message.

---

