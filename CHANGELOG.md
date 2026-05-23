# Changelog

All notable changes to `missive` will be documented in this file.

Keep entries concise when changes affect user-visible behavior, setup, architecture, operations, security posture, or release contents.

## [Unreleased]

### Added

* A `missive` binary with human, JSON, NDJSON, and quiet output modes plus implemented agent registry, Agent Card discovery, send, stream, task, artifact, context, group, route, collective, push, webhook, gateway, job, doctor, logs, events, completion, manpage, stdio adapter, file-drop adapter, and HTTP-adapter surfaces.
* Workspace crates for core types/config/errors, A2A client/protocol compatibility, store migrations/repositories, routing, gateway, adapters, observability, CLI, and reusable local A2A test fixtures.
* Local mock A2A fixtures, A2A conformance fixtures, CLI smoke examples, property tests, fuzz smoke targets, mutation/failure-injection smoke, Criterion benchmarks, and multi-agent demos.
* GitHub Actions CI/release dry-run workflows, Docker/devcontainer support, cargo-deny supply-chain policy, metadata-derived CycloneDX SBOM generation, and local release archive/checksum/install scripts.
* Repository hygiene files, licensing, contribution, security, changelog, architecture decision records, user guides, operations runbook, and troubleshooting documentation.

### Removed

* Removed the retired ticket-loop artifacts, issue-seeding files, and build-agent wrapper scripts from the repository.
