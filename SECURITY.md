# Security policy

`missive` is an early-stage communication control plane rather than a hardened production service. Treat protocol inputs, adapter inputs, webhook payloads, configuration files, generated diagnostics, and local SQLite state as sensitive and untrusted unless a deployment-specific review says otherwise.

## Supported versions

No public stable release is supported yet. The repository contains release-packaging dry-run scripts and CI artifacts, but maintainers should publish a support policy when the first supported release is cut.

## Reporting issues

Do not include real secrets, credentials, tokens, private keys, private URLs, internal hostnames, or private data in public reports. Use the repository's configured private advisory process when available; otherwise, share only a minimal redacted reproduction with maintainers.

## Current safeguards

The repository includes guardrails and implementation boundaries for:

* obvious secret-pattern scans and generated/private/runtime file scans in `scripts/quality-gate.sh`;
* keeping default runtime state outside the source tree through XDG/MISSIVE_HOME state paths;
* non-secret auth refs for environment variables and platform keyrings instead of raw-token SQLite storage;
* CLI, log, event, doctor, context-export, and diagnostic output redaction for authorization headers, cookies, tokens, API keys, passwords, and secret-like fields;
* A2A service-parameter validation, protocol-version handling, interface negotiation, and local mock/conformance tests;
* webhook and HTTP-adapter optional header-token validation, local bind defaults, body/rate limits where implemented, and clear trust-boundary documentation;
* SQLite migrations and typed repository APIs for local communication state;
* cargo-deny advisory/license/source policy plus metadata-derived SBOM generation.

These safeguards do not replace a deployment security review, transport/TLS review, operating-system hardening, retention policy, or external adapter platform review. Current limitations and trust boundaries are documented in [`docs/security.md`](docs/security.md), [`docs/gateway.md`](docs/gateway.md), [`docs/adapters.md`](docs/adapters.md), and [`docs/runbook.md`](docs/runbook.md).

## Local development expectations

* Keep runtime state, local configs, logs, databases, release artifacts, coverage, fuzz artifacts, and generated reports out of version control.
* Keep real `missive.toml`, `.missive.toml`, `.env*`, tokens, keys, and private endpoint details out of commits.
* Use local mock services for protocol tests by default.
* Do not attack, scan, fuzz, or load-test third-party services.
* Redact authentication material in logs, traces, fixtures, documentation, issue reports, and escalation bundles.
* Run `scripts/quality-gate.sh` before commits; use `MISSIVE_AGGRESSIVE_TESTS=1 scripts/quality-gate.sh` when a deeper local validation pass is feasible.
