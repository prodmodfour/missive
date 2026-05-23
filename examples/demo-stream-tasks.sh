#!/usr/bin/env bash
set -euo pipefail

# Demonstrates streaming updates and foreground task inspection/waiting against
# the local mock A2A server. The server seeds task-example-1 and emits a small
# deterministic SSE stream for task-stream-example-1.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=examples/lib/demo-common.sh
source "$SCRIPT_DIR/lib/demo-common.sh"

example_init "stream-tasks"
example_add_echo_agent

run_missive stream echo "Show progress from the stream example" --ndjson
run_missive task list --agent echo --remote --include-artifacts --json
run_missive task get task-example-1 --agent echo --remote --json
run_missive task wait task-example-1 --agent echo --timeout 3s --interval 100ms --json
run_missive task artifact list task-example-1 --json
