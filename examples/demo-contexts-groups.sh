#!/usr/bin/env bash
set -euo pipefail

# Demonstrates local context lifecycle and group/routing inspection commands.
# Context and group IDs are explicit so the script remains deterministic.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=examples/lib/demo-common.sh
source "$SCRIPT_DIR/lib/demo-common.sh"

example_init "contexts-groups"
example_add_echo_agent

run_missive context create --id ctx-example-local --name example-round --agent echo \
  --summary "Example planning context" --metadata example=context --json
run_missive context show ctx-example-local --json
run_missive context fork ctx-example-local --id ctx-example-child --name example-child \
  --summary "Forked example context" --json
run_missive context list --json
run_missive context export ctx-example-local --json

run_missive group create demo-team --routing-policy weighted --metadata example=group --json
run_missive group add demo-team echo --rank rank-0 --tag local --weight 2 \
  --routing-metadata lane=primary --json
run_missive group show demo-team --json
run_missive group capabilities demo-team --refresh --json
run_missive route explain --group demo-team --policy capability-match --capability echo \
  --input-mode text/plain --streaming --refresh-capabilities --json
