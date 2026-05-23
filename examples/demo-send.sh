#!/usr/bin/env bash
set -euo pipefail

# Demonstrates one non-streaming A2A SendMessage call against the local mock agent.
# The mock returns a deterministic completed task so later task examples can use
# the same fixture IDs.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=examples/lib/demo-common.sh
source "$SCRIPT_DIR/lib/demo-common.sh"

example_init "send"
example_add_echo_agent

run_missive send echo "Hello from the missive send example" \
  --context ctx-example-1 \
  --metadata example=send \
  --accepted-output-mode text/plain \
  --json
run_missive events list --type a2a.send.response --json
