# Groups and routing

`missive group` stores local agent groups for routing and collective operations.
Group membership is profile-scoped SQLite state and references registered agent
aliases.

## Create a group and add members

```bash
missive group create demo-team --routing-policy weighted --metadata purpose=demo --json
missive group add demo-team echo --rank rank-0 --tag local --weight 2 \
  --routing-metadata lane=primary --json
missive group show demo-team --json
```

Each member has:

* `agent_alias` — a registered agent.
* `rank` — a unique rank name within the group.
* `tags` — local routing labels.
* `weight` — positive integer used by weighted policies.
* `routing_metadata` — non-secret key/value context for routing decisions.

## Capability summaries

```bash
missive group capabilities demo-team --refresh --json
```

Capability summaries combine membership data with cached/fetched Agent Cards for
members. They are useful before route explanations and broadcasts.

## Dry-run routing

```bash
missive route explain --group demo-team --policy capability-match \
  --capability echo --input-mode text/plain --streaming --refresh-capabilities --json
```

`route explain` never sends messages. It supports `direct`, `capability-match`,
`tag-match`, `round-robin`, `weighted`, `broadcast`, `first-success`, `quorum`,
and `fallback` policy labels. Capability-match uses known Agent Card skills,
tags, modes, streaming support, push support, and deterministic tie-breaking.

## Collective workflow

After group setup, use the implemented MPI-inspired collective commands:

```bash
missive bcast demo-team "Report status" --context ctx-shared --failure-policy continue --json
missive barrier demo-team --context ctx-shared --timeout 30s --json
missive gather demo-team --context ctx-shared --output-dir ./gathered-artifacts --json
missive reduce demo-team --context ctx-shared --strategy summarise --json
```

See [`collectives.md`](collectives.md) for detailed behavior, failure policies,
quorum handling, artifact export, and reducer strategies.

## Maintenance

```bash
missive group rename demo-team demo-team-2 --json
missive group remove demo-team-2 echo --json
missive group delete demo-team-2 --json
```

Group metadata and routing metadata should remain non-secret because they are
stored in local runtime SQLite.

## Current limitations

* Groups are local control-plane state; they do not synchronize with remote
  agents.
* `route explain` is a planner only. It does not persist route decisions or send
  A2A requests.
* Broadcast uses non-streaming sends only; use separate `stream` commands for
  foreground streaming behavior.

## Smoke coverage

`examples/demo-contexts-groups.sh` covers group create/add/show, group capability
summary, and route explanation against the local mock A2A server. It is run by
`examples/run-smoke.sh` and the default quality gate.
