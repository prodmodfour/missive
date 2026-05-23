# Security policy

`missive` is an early-stage A2A communication CLI and local control plane, not a hardened production service. Treat protocol inputs, adapter inputs, webhook payloads, configuration files, generated diagnostics, logs, event journals, and local SQLite state as sensitive and untrusted unless a deployment-specific review says otherwise.

## Supported versions

No public stable release is supported yet. Security work is handled on a best-effort basis for active development snapshots until maintainers publish a release support policy.

| Version or branch | Security support |
| --- | --- |
| `main` | Best-effort triage for development snapshots; not a production support commitment. |
| Tags or artifacts before an announced support policy | Unsupported unless maintainers explicitly say otherwise. |

## Reporting a vulnerability

Do not include real secrets, credentials, tokens, private keys, private URLs, internal hostnames, customer data, or sensitive payloads in public reports.

Preferred reporting path:

1. Use GitHub private vulnerability reporting for this repository: <https://github.com/prodmodfour/missive/security/advisories/new>.
2. If private reporting is unavailable to you, open a public issue that only asks for a secure contact path. Do not include vulnerability details in that issue.

Helpful details for a private report include:

* affected command, crate, adapter, gateway endpoint, storage path, or protocol flow;
* commit SHA, tag, platform, Rust version, and relevant feature flags;
* a minimal redacted reproduction using local fixtures whenever possible;
* expected impact, affected trust boundary, and any known workaround;
* confirmation that no third-party service was scanned, attacked, fuzzed, or load-tested without authorization.

Maintainers will triage reports on a best-effort basis. Public disclosure timing should be coordinated with maintainers after a fix, mitigation, or explicit non-issue determination.

## Current safeguards

The repository includes guardrails and implementation boundaries for:

* obvious secret-pattern scans and generated/private/runtime file scans in `scripts/quality-gate.sh`;
* default runtime state outside the source tree through XDG and `MISSIVE_HOME` state paths;
* non-secret auth refs for environment variables and platform keyrings instead of raw-token SQLite storage;
* CLI, log, event, doctor, context-export, and diagnostic output redaction for authorization headers, cookies, tokens, API keys, passwords, and secret-like fields;
* A2A service-parameter validation, protocol-version handling, interface negotiation, and local mock/conformance tests;
* webhook and HTTP-adapter optional header-token validation, local bind defaults, body/rate limits where implemented, and documented trust boundaries;
* SQLite migrations and typed repository APIs for local communication state;
* cargo-deny advisory/license/source policy plus metadata-derived SBOM generation.

These safeguards do not replace a deployment security review, transport/TLS review, operating-system hardening, retention policy, or external adapter platform review. Current limitations and trust boundaries are documented in [`docs/security.md`](docs/security.md), [`docs/gateway.md`](docs/gateway.md), [`docs/adapters.md`](docs/adapters.md), and [`docs/runbook.md`](docs/runbook.md).

## Local development expectations

* Keep runtime state, local configs, logs, databases, release artifacts, coverage, fuzz artifacts, generated reports, and generated SBOMs out of version control.
* Keep real `missive.toml`, `.missive.toml`, `.env*`, tokens, keys, private endpoint details, and internal hostnames out of commits.
* Use local mock services for protocol tests by default.
* Do not attack, scan, fuzz, or load-test third-party services.
* Redact authentication material in logs, traces, fixtures, documentation, issue reports, and escalation bundles.
* Run `scripts/quality-gate.sh` before commits when feasible; use `MISSIVE_AGGRESSIVE_TESTS=1 scripts/quality-gate.sh` when a deeper local validation pass is appropriate.

## Known limitations

`missive` is still evolving. Known security limitations include incomplete production hardening for gateway/adapters, no broad external-platform adapter implementation, limited inbound authentication primitives, no built-in TLS termination, no complete retention policy, and known gaps called out in [`docs/security.md`](docs/security.md). Future work that expands network exposure, persistent secrets, platform adapters, or daemon-managed outbound authentication must update the security docs and reuse the existing redaction/auth-reference patterns.
