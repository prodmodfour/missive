# Security policy

`missive` is at bootstrap stage and is not yet production-ready. Treat all protocol inputs, adapter inputs, webhooks, configuration files, and persisted state as untrusted until the relevant tickets add hardened implementations.

## Supported versions

No released versions are supported yet. Security posture will be updated when release packaging is added.

## Reporting issues

Do not include real secrets, credentials, tokens, private keys, private URLs, or private data in public reports. Use the repository's configured private advisory process when available; otherwise, share only a minimal redacted reproduction with maintainers.

## Current safeguards

The repository includes lightweight guardrails for:

* obvious secret patterns
* generated/private/runtime files
* avoiding committed local state such as databases, logs, sockets, and PID files

These guardrails are not a substitute for full security review. Later tickets add authentication handling, redaction, storage hardening, webhook validation, adapter trust boundaries, and supply-chain checks.

## Local development expectations

* Keep runtime state outside the repository by default.
* Use local mock services for protocol tests.
* Do not attack, scan, fuzz, or load-test third-party services.
* Redact authentication material in logs, traces, fixtures, and documentation.
