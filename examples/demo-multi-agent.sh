#!/usr/bin/env bash
set -euo pipefail

# End-to-end local multi-agent collective demo.
# It registers three compatible mock A2A agents, broadcasts one message to a
# group, waits at a barrier, gathers local outputs/artifacts, reduces the
# gathered state, and lists the resulting collective operation events.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=examples/lib/demo-common.sh
source "$SCRIPT_DIR/lib/demo-common.sh"

CONTEXT_ID="ctx-multi-agent-demo"
GROUP_NAME="demo-squad"
OUTPUT_DIR="${MISSIVE_EXAMPLE_MULTI_AGENT_OUTPUT_DIR:-}"

example_prepare_environment "multi-agent"
example_missive_cmd

if [[ -z "$OUTPUT_DIR" ]]; then
  OUTPUT_DIR="$MISSIVE_EXAMPLE_WORKDIR/multi-agent-output"
fi
mkdir -p "$OUTPUT_DIR/artifacts"

run_missive_json_to_file() {
  local output_file="$1"
  shift
  printf '\n$ missive'
  printf ' %q' "$@"
  printf ' --json > %q\n' "$output_file"
  missive "$@" --json >"$output_file"
  printf 'wrote %s\n' "$output_file"
}

read_configured_agent_urls() {
  if [[ -z "${MISSIVE_EXAMPLE_MULTI_AGENT_URLS:-}" ]]; then
    return 1
  fi
  IFS=',' read -r -a AGENT_URLS <<<"$MISSIVE_EXAMPLE_MULTI_AGENT_URLS"
  if [[ "${#AGENT_URLS[@]}" -ne 3 ]]; then
    printf 'MISSIVE_EXAMPLE_MULTI_AGENT_URLS must contain exactly three comma-separated A2A base URLs.\n' >&2
    return 64
  fi
  return 0
}

start_local_agent_urls() {
  AGENT_URLS=()
  example_start_mock_a2a_instance scout \
    --task-id task-demo-scout \
    --context-id "$CONTEXT_ID" \
    --task-text 'scout agent mapped the local demo constraints' >/dev/null
  AGENT_URLS+=("$MISSIVE_EXAMPLE_LAST_A2A_BASE_URL")

  example_start_mock_a2a_instance analyst \
    --task-id task-demo-analyst \
    --context-id "$CONTEXT_ID" \
    --task-text 'analyst agent checked the collective workflow state' >/dev/null
  AGENT_URLS+=("$MISSIVE_EXAMPLE_LAST_A2A_BASE_URL")

  example_start_mock_a2a_instance reviewer \
    --task-id task-demo-reviewer \
    --context-id "$CONTEXT_ID" \
    --task-text 'reviewer agent confirmed the final handoff' >/dev/null
  AGENT_URLS+=("$MISSIVE_EXAMPLE_LAST_A2A_BASE_URL")
}

if [[ -n "${MISSIVE_EXAMPLE_MULTI_AGENT_URLS:-}" ]]; then
  read_configured_agent_urls
else
  start_local_agent_urls
fi

printf 'Using MISSIVE_HOME=%s\n' "$MISSIVE_HOME"
printf 'Writing machine-readable demo output under %s\n' "$OUTPUT_DIR"
printf 'Using mock A2A agents:\n'
printf '  scout=%s\n' "${AGENT_URLS[0]}"
printf '  analyst=%s\n' "${AGENT_URLS[1]}"
printf '  reviewer=%s\n' "${AGENT_URLS[2]}"

run_missive_json_to_file "$OUTPUT_DIR/agent-scout.json" \
  agent add scout "${AGENT_URLS[0]}" --tag local --tag scout \
  --metadata demo=multi-agent
run_missive_json_to_file "$OUTPUT_DIR/agent-analyst.json" \
  agent add analyst "${AGENT_URLS[1]}" --tag local --tag analyst \
  --metadata demo=multi-agent
run_missive_json_to_file "$OUTPUT_DIR/agent-reviewer.json" \
  agent add reviewer "${AGENT_URLS[2]}" --tag local --tag reviewer \
  --metadata demo=multi-agent

run_missive_json_to_file "$OUTPUT_DIR/group-create.json" \
  group create "$GROUP_NAME" --routing-policy broadcast \
  --metadata demo=multi-agent
run_missive_json_to_file "$OUTPUT_DIR/group-add-scout.json" \
  group add "$GROUP_NAME" scout --rank rank-0 --tag scout --weight 1
run_missive_json_to_file "$OUTPUT_DIR/group-add-analyst.json" \
  group add "$GROUP_NAME" analyst --rank rank-1 --tag analyst --weight 1
run_missive_json_to_file "$OUTPUT_DIR/group-add-reviewer.json" \
  group add "$GROUP_NAME" reviewer --rank rank-2 --tag reviewer --weight 1
run_missive_json_to_file "$OUTPUT_DIR/group-show.json" \
  group show "$GROUP_NAME"

run_missive_json_to_file "$OUTPUT_DIR/bcast.json" \
  --timeout 5s \
  bcast "$GROUP_NAME" "Inspect the local multi-agent demo and report readiness." \
  --context "$CONTEXT_ID" \
  --execution sequential \
  --failure-policy continue \
  --metadata demo=multi-agent \
  --accepted-output-mode text/plain

run_missive_json_to_file "$OUTPUT_DIR/barrier.json" \
  --timeout 5s \
  barrier "$GROUP_NAME" \
  --context "$CONTEXT_ID" \
  --from-bcast "$OUTPUT_DIR/bcast.json" \
  --interval 100ms

run_missive_json_to_file "$OUTPUT_DIR/gather.json" \
  gather "$GROUP_NAME" \
  --context "$CONTEXT_ID" \
  --output-dir "$OUTPUT_DIR/artifacts" \
  --force

run_missive_json_to_file "$OUTPUT_DIR/reduce.json" \
  reduce "$GROUP_NAME" \
  --context "$CONTEXT_ID" \
  --strategy summarise

run_missive_json_to_file "$OUTPUT_DIR/events.json" \
  events list \
  --context "$CONTEXT_ID" \
  --source cli \
  --limit 100

for output in bcast barrier gather reduce events; do
  if [[ ! -s "$OUTPUT_DIR/$output.json" ]]; then
    printf 'expected machine-readable output file %s to exist and be non-empty\n' "$OUTPUT_DIR/$output.json" >&2
    exit 1
  fi
done

for marker in bcast_result barrier_result gather_result reduce_result; do
  grep -F "$marker" "$OUTPUT_DIR/${marker%_result}.json" >/dev/null
  printf 'validated %s in %s\n' "$marker" "$OUTPUT_DIR/${marker%_result}.json"
done

for event_type in \
  missive.bcast.completed \
  missive.barrier.completed \
  missive.gather.completed \
  missive.reduce.completed; do
  grep -F "$event_type" "$OUTPUT_DIR/events.json" >/dev/null
  printf 'observed collective event %s\n' "$event_type"
done

printf '\nMulti-agent collective demo completed. JSON outputs are under %s\n' "$OUTPUT_DIR"
