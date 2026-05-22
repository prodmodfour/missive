#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/pretty-print.sh
source "$SCRIPT_DIR/lib/pretty-print.sh"

if [[ $# -ne 1 ]]; then
  pp_error "Usage: scripts/run-agent.sh '<prompt>'"
  exit 2
fi

PROMPT="$1"

if [[ -n "${MISSIVE_AGENT_COMMAND:-}" ]]; then
  pp_step "Launching configured agent command from MISSIVE_AGENT_COMMAND."
  pp_cmd "$MISSIVE_AGENT_COMMAND '<prompt>'"
  exec bash -lc "$MISSIVE_AGENT_COMMAND \"\$0\"" "$PROMPT"
fi

if ! command -v pi >/dev/null 2>&1; then
  pp_error "Required command not found: pi"
  pp_hint "Install pi or set MISSIVE_AGENT_COMMAND to another one-argument agent command."
  exit 127
fi

pp_step "Launching Pi agent."
pp_cmd "pi --no-session -p @AGENTS.md @PROJECT_BRIEF.md @BUILD_TICKETS.md @BUILD_NOTES.md '<prompt>'"

pi --no-session -p @AGENTS.md @PROJECT_BRIEF.md @BUILD_TICKETS.md @BUILD_NOTES.md "$PROMPT"
