#!/usr/bin/env bash
set -euo pipefail

# Demonstrates the local gateway daemon lifecycle with a short timeout. The mock
# A2A server is still started so this demo uses the same fully local fixture
# environment as the other command examples.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=examples/lib/demo-common.sh
source "$SCRIPT_DIR/lib/demo-common.sh"

example_init "gateway"
example_add_echo_agent

run_missive gateway run --bind-address 127.0.0.1 --port 0 --timeout 500ms --ndjson
run_missive events list --type missive.gateway.started --json
