#!/usr/bin/env bash
set -euo pipefail

# Demonstrates local agent registry commands plus public Agent Card discovery.
# Runtime state is created under a temporary MISSIVE_HOME unless overridden.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=examples/lib/demo-common.sh
source "$SCRIPT_DIR/lib/demo-common.sh"

example_init "agent-registry"
example_add_echo_agent

run_missive agent list --json
run_missive agent show echo --json
run_missive agent inspect echo --refresh --json
run_missive agent capabilities echo --json
