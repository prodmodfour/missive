#!/usr/bin/env bash
set -euo pipefail

# Runs every top-level command demo against one local mock A2A server. This is
# the same entry point exercised by the Rust smoke test in crates/missive-cli.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=examples/lib/demo-common.sh
source "$SCRIPT_DIR/lib/demo-common.sh"

example_init "smoke"

for demo in \
  demo-agent-registry.sh \
  demo-send.sh \
  demo-stream-tasks.sh \
  demo-contexts-groups.sh \
  demo-gateway.sh
  do
    printf '\n== Running examples/%s ==\n' "$demo"
    MISSIVE_EXAMPLE_A2A_BASE_URL="$MISSIVE_EXAMPLE_A2A_BASE_URL" \
      MISSIVE_EXAMPLE_WORKDIR="$MISSIVE_EXAMPLE_WORKDIR" \
      MISSIVE_BIN="${MISSIVE_BIN:-}" \
      bash "$SCRIPT_DIR/$demo"
  done
