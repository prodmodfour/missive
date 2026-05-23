# Push notifications and webhooks

`missive push` configures A2A task push notification callbacks on a remote agent.
`missive webhook run` starts a local receiver for those callbacks. Both surfaces
use redacted output and local event journaling.

## Start a local receiver

```bash
export MISSIVE_WEBHOOK_TOKEN=change-me
missive webhook run \
  --bind-address 127.0.0.1 \
  --port 7347 \
  --path /a2a/push \
  --auth-token-env MISSIVE_WEBHOOK_TOKEN \
  --max-events 1 \
  --ndjson
```

The receiver exposes:

* `POST /a2a/push` by default for A2A `StreamResponse` callback payloads.
* `/healthz` and `/readyz` for local health checks.

Use a trusted local tunnel or reverse proxy if a remote agent needs to reach the
receiver from outside the host. Configure TLS and public exposure outside
missive; the built-in receiver is a local development/control-plane component.

## Create a push config

```bash
export MISSIVE_PUSH_CALLBACK_SECRET=change-me
missive push create echo task-123 http://127.0.0.1:7347/a2a/push \
  --config-id local-webhook \
  --auth-scheme Bearer \
  --auth-credentials-env MISSIVE_PUSH_CALLBACK_SECRET \
  --metadata purpose=demo \
  --json
```

The callback URL must be an absolute HTTP or HTTPS URL. Callback auth
credentials are read from the named environment variable and redacted from output
and persisted local records.

## Inspect, list, and delete configs

```bash
missive push get echo task-123 local-webhook --json
missive push list echo task-123 --json
missive push delete echo task-123 local-webhook --json
```

`push list` is scoped to one response page and reports `nextPageToken` when the
remote agent returns one.

## Events and diagnostics

Accepted callbacks are journaled as redacted `a2a.push.*` events; rejected
callbacks are journaled as `a2a.push.rejected` when they reach the handler:

```bash
missive events list --type a2a.push.status_update --json
missive events list --type a2a.push.rejected --json
missive events tail --limit 10 --ndjson
```

## Current limitations

* missive does not rotate callback credentials, verify signatures/JWTs, or manage
  TLS termination.
* Push callbacks are persisted as events; they do not automatically refresh task
  rows yet.
* The gateway daemon does not embed this standalone webhook receiver yet; run
  `missive webhook run` separately when you need a local receiver.

## Example coverage

Push/webhook command integration tests run in the Rust test suite. The top-level
smoke examples do not create push configs because a realistic callback flow
requires coordinating a long-running receiver with a remote or mock agent; use
the local commands above for manual end-to-end checks.
