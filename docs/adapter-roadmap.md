# External adapter roadmap

This roadmap covers the feature-gated placeholder adapters for Discord, Slack,
Telegram, Matrix, and Email. They are compileable stubs in
`crates/missive-adapters`; they do not connect to any third-party service, do
not require platform SDK dependencies, and do not contain credentials.

The current external adapter boundary is intentionally conservative:

* A2A remains the canonical agent protocol. Chat/email platforms are ingress and
  egress sources for missive gateway events, not agent runtimes.
* Stubs define adapter kinds, identity mapping, registry factories, and static
  roadmap metadata only.
* Starting a stub as a live adapter returns a configuration error that explains
  the platform integration is not implemented yet.
* Future adapters must use auth refs, environment variables, keyrings, or
  platform secret stores. Raw platform secrets must not be stored in adapter
  settings, SQLite rows, docs, tests, or examples.
* Source ids, channel ids, room ids, chat ids, message ids, and email addresses
  can be operationally sensitive. Treat adapter runtime state as private local
  data even when it contains no credentials.

## Feature flags

The `missive-adapters` crate has no external adapter stubs enabled by default.
Library consumers can enable individual placeholder factories or the umbrella
feature:

```bash
cargo test -p missive-adapters --no-default-features
cargo test -p missive-adapters --features external-chat-stubs
cargo test -p missive-adapters --features adapter-slack
```

Available adapter kinds and feature flags:

| Adapter kind | Feature flag | Current status |
| --- | --- | --- |
| `discord` | `adapter-discord` | registry/identity stub only |
| `slack` | `adapter-slack` | registry/identity stub only |
| `telegram` | `adapter-telegram` | registry/identity stub only |
| `matrix` | `adapter-matrix` | registry/identity stub only |
| `email` | `adapter-email` | registry/identity stub only |

`external-chat-stubs` enables all five placeholder factories. Enabling these
features does not add network clients or authenticate to any platform.

## Configuration shape

External adapters use the same `[adapters.<name>]` schema as local adapters.
Until live implementations land, keep them disabled or use them only in
crate-level tests:

```toml
[adapters.team-chat]
kind = "slack"
enabled = false
session_profile = "default"

[adapters.team-chat.settings]
auth_ref = "slack-bot"
signing_secret_ref = "slack-signing"
workspace = "example-workspace"
allowed_channels = ["engineering"]
```

The `settings` table is for non-secret values and references to secret storage.
It must not contain token values, passwords, private keys, webhook secrets, OAuth
refresh values, or mailbox credentials.

## Platform roadmap

| Platform | Expected secret references | Minimum permissions/scopes | Platform behaviours to handle |
| --- | --- | --- | --- |
| Discord | bot-token auth ref; interaction public key reference | selected message/mention or command intake; send messages; optional thread support | gateway events and interaction callbacks have different acknowledgement deadlines; guild/channel/thread ids must map to sessions; route-level and global rate limits apply |
| Slack | bot-token auth ref; signing-secret auth ref | app mentions or commands/events for ingress; `chat:write`; history scopes only when explicitly needed | event retries require idempotent acknowledgements; workspace/team/channel ids become source-session keys; response URLs and platform tokens must never be persisted raw |
| Telegram | bot-token auth ref; optional webhook-secret auth ref | bot command/message receipt for selected chats; outbound send permissions | privacy mode changes group visibility; long polling and webhooks use different offset/retry semantics; chat/message ids are session state, not agent memory |
| Matrix | access-token auth ref; homeserver URL; optional device id | join/read selected rooms; send room messages; maintain sync tokens as runtime state | federation can delay or reorder events; encrypted rooms need an explicit future crypto design; room/user/event ids are sensitive local state |
| Email | SMTP auth ref; IMAP/Graph/provider auth ref; optional OAuth refresh auth ref | read an explicitly configured mailbox/folder; send through an explicit relay/provider | polling is slower than chat; MIME bodies and attachments need size limits/sanitization; reply threading, bounces, and spam filtering affect acknowledgement state |

## Hermes-inspired boundaries

missive borrows Hermes-style communication boundaries, not unrelated agent
behaviour:

* **Source identity mapping:** map platform user/channel/room/mailbox ids into
  stable missive source ids used by sessions and busy-input policy.
* **Session continuity:** link each source/resume name to an A2A context. Reset
  policies should rotate context links, not create long-term memory.
* **Busy-input modes:** queue, interrupt, and steer should apply to active
  gateway work when future workers execute adapter events.
* **Acknowledgements:** distinguish source delivery acknowledgement from A2A task
  completion. Platform retries must be idempotent.
* **Adapter trust boundary:** treat every platform payload as untrusted input;
  validate size, content type, signatures, and identity before emitting gateway
  events.

Non-goals for these stubs and their first live implementations:

* no agent cognition, memory, skill learning, or tool execution in adapters;
* no broad platform SDK dependency until platform-specific work needs it;
* no production chat-bot feature set before the local gateway adapter lifecycle
  is proven;
* no committed real credentials, private workspace names, private email
  addresses, internal-only hostnames, or machine-specific runtime state.

## Future implementation checklist

A platform-specific implementation should add, at minimum:

1. narrow platform SDK or HTTP client dependencies with documented supply-chain
   review;
2. adapter-specific configuration validation for secret references, allowed
   channels/rooms/mailboxes, size limits, and rate limits;
3. signature/webhook verification or polling checkpoint persistence as the
   platform requires;
4. inbound message normalization into `AdapterEvent::inbound_message` with
   deterministic source ids and redacted metadata;
5. outbound update rendering for status, message, artifact, completed, and error
   updates;
6. acknowledgement/idempotency handling for platform retries;
7. tests using local fixtures or recorded synthetic payloads, never live
   third-party services by default;
8. documentation for required permissions, secret provisioning, local gateway
   operation, troubleshooting, and limitations.
