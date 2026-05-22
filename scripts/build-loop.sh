#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/pretty-print.sh
source "$SCRIPT_DIR/lib/pretty-print.sh"
# shellcheck source=scripts/lib/git-branch.sh
source "$SCRIPT_DIR/lib/git-branch.sh"

usage() {
  cat <<'USAGE'
Usage: scripts/build-loop.sh [options]

Runs autonomous missive build cycles.

Options:
--max-cycles N       Number of cycles to run. Default: 1.
--sleep SECONDS      Pause between successful cycles. Default: 0.
--no-push            Do not push after successful cycles. Default pushes.
--push               Push after successful cycles.
--branch NAME        Select existing local or unique remote branch before running.
--create-branch NAME Create and select a new branch before running.
--branch-start REF   Start point for --create-branch. Default: HEAD.
--allow-template     Allow running even if PROJECT_BRIEF.md is marked uncustomised.
-h, --help           Show this help.

Environment:
MISSIVE_BUILD_LOOP_STATE_DIR     Override build-loop logs/lock state directory.
USAGE
}

MAX_CYCLES=1
SLEEP_SECONDS=0
PUSH_AFTER=1
SELECT_BRANCH=""
CREATE_BRANCH=""
BRANCH_START_POINT="HEAD"
BRANCH_START_SET=0
ALLOW_TEMPLATE=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --max-cycles) MAX_CYCLES="${2:?}"; shift 2 ;;
    --sleep) SLEEP_SECONDS="${2:?}"; shift 2 ;;
    --push) PUSH_AFTER=1; shift ;;
    --no-push) PUSH_AFTER=0; shift ;;
    --branch) SELECT_BRANCH="${2:?}"; shift 2 ;;
    --create-branch) CREATE_BRANCH="${2:?}"; shift 2 ;;
    --branch-start) BRANCH_START_POINT="${2:?}"; BRANCH_START_SET=1; shift 2 ;;
    --allow-template) ALLOW_TEMPLATE=1; shift ;;
    *) pp_error "Unknown argument: $1"; usage >&2; exit 2 ;;
  esac
done

if ! [[ "$MAX_CYCLES" =~ ^[0-9]+$ ]] || [[ "$MAX_CYCLES" -lt 1 ]]; then
  pp_error "--max-cycles must be a positive integer"
  exit 2
fi

if ! [[ "$SLEEP_SECONDS" =~ ^[0-9]+$ ]]; then
  pp_error "--sleep must be a non-negative integer"
  exit 2
fi

if [[ -n "$SELECT_BRANCH" && -n "$CREATE_BRANCH" ]]; then
  pp_error "--branch and --create-branch cannot be used together"
  exit 2
fi

if (( BRANCH_START_SET == 1 )) && [[ -z "$CREATE_BRANCH" ]]; then
  pp_error "--branch-start requires --create-branch"
  exit 2
fi

REQUIRED_FILES=(
  AGENTS.md
  PROJECT_BRIEF.md
  BUILD_TICKETS.md
  BUILD_NOTES.md
  scripts/quality-gate.sh
  scripts/run-agent.sh
  scripts/lib/pretty-print.sh
  scripts/lib/git-branch.sh
)

PROMPT=$(cat <<'PROMPT_EOF'
You are continuing the autonomous ticket-driven build for missive.

Read AGENTS.md, PROJECT_BRIEF.md, BUILD_TICKETS.md, and BUILD_NOTES.md.

Your task in this run:

* Select the lowest-numbered TODO or IN_PROGRESS ticket from BUILD_TICKETS.md.
* At the start of the run, print a short "Now working on ..." line naming the selected ticket and immediate action.
* Implement only that ticket.
* Do not start future tickets.
* Do not broaden scope.
* Respect all project-specific instructions in PROJECT_BRIEF.md.
* Respect all general instructions in AGENTS.md.
* You may install tools/dependencies, use sudo, run Docker/local services, and run aggressive tests when useful.
* Add or update tests/validation where appropriate.
* Update documentation if the ticket changes setup, architecture, behaviour, operations, security posture, limitations, or public-facing usage.
* Run scripts/quality-gate.sh.
* Update BUILD_TICKETS.md with ticket status.
* Update BUILD_NOTES.md with what changed, quality gates run, limitations, blockers if any, and next recommended ticket.
* Commit the completed ticket with a conventional commit message.
* Leave the working tree clean.

If you cannot safely complete the ticket:

* explain the blocker in BUILD_NOTES.md
* mark the ticket BLOCKED if appropriate
* do not mark it DONE
* do not commit partial broken work
* leave the working tree clean if possible
PROMPT_EOF
)

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    pp_error "Required command not found: $1"
    exit 127
  fi
}

require_customised_template() {
  if (( ALLOW_TEMPLATE == 1 )); then
    return 0
  fi

  if grep -Eq '^TEMPLATE_CUSTOMISED:[[:space:]]*false[[:space:]]*$' PROJECT_BRIEF.md; then
    pp_error "PROJECT_BRIEF.md is still marked TEMPLATE_CUSTOMISED: false."
    pp_hint "Edit PROJECT_BRIEF.md and set TEMPLATE_CUSTOMISED: true before running."
    exit 1
  fi
}

get_upstream_ref() {
  git rev-parse --abbrev-ref --symbolic-full-name '@{u}' 2>/dev/null || true
}

get_automation_status() {
  awk -F: '
    /^##[[:space:]]/ { exit }
    /^AUTOMATION_STATUS:/ {
      status=$2
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", status)
      print status
      exit
    }
  ' BUILD_TICKETS.md
}

get_next_ticket_summary() {
  awk '
    function trim(value) { gsub(/^[[:space:]]+|[[:space:]]+$/, "", value); return value }
    function record_if_current() {
      if (ticket_number != "" && (ticket_status == "TODO" || ticket_status == "IN_PROGRESS")) {
        ticket_value = ticket_number + 0
        if (!found || ticket_value < best_value) {
          found = 1
          best_value = ticket_value
          best_number = ticket_number
          best_title = ticket_title
          best_status = ticket_status
        }
      }
    }
    /^##[[:space:]]+[0-9]+([[:space:]]|$)/ {
      record_if_current()
      heading = $0
      sub(/^##[[:space:]]+/, "", heading)
      ticket_number = heading
      sub(/[[:space:]].*$/, "", ticket_number)
      ticket_title = heading
      sub(/^[0-9]+[[:space:]]+/, "", ticket_title)
      sub(/^[-—][[:space:]]*/, "", ticket_title)
      ticket_status = ""
      next
    }
    ticket_number != "" && /^Status:/ {
      ticket_status = $0
      sub(/^Status:[[:space:]]*/, "", ticket_status)
      ticket_status = trim(ticket_status)
    }
    END {
      record_if_current()
      if (found) printf "%s — %s (%s)\n", best_number, best_title, best_status
    }
  ' BUILD_TICKETS.md
}

configure_state_paths() {
  local repo_root repo_name repo_slug repo_hash state_home
  if [[ -n "${MISSIVE_BUILD_LOOP_STATE_DIR:-}" ]]; then
    BUILD_LOOP_STATE_DIR="$MISSIVE_BUILD_LOOP_STATE_DIR"
  else
    if [[ -n "${XDG_STATE_HOME:-}" ]]; then
      state_home="$XDG_STATE_HOME"
    else
      state_home="${HOME:?}/.local/state"
    fi
    repo_root="$(git rev-parse --show-toplevel)"
    repo_name="$(basename "$repo_root")"
    repo_slug="$(printf '%s' "$repo_name" | tr -c 'A-Za-z0-9._-' '-')"
    repo_hash="$(printf '%s' "$repo_root" | git hash-object --stdin | cut -c 1-12)"
    BUILD_LOOP_STATE_DIR="$state_home/missive/build-loop/${repo_slug}-${repo_hash}"
  fi
  LOG_DIR="$BUILD_LOOP_STATE_DIR/logs"
  LOCK_DIR="$BUILD_LOOP_STATE_DIR/lock"
}

acquire_lock() {
  mkdir -p "$BUILD_LOOP_STATE_DIR" "$LOG_DIR"
  if ! mkdir "$LOCK_DIR" 2>/dev/null; then
    pp_error "Another build loop appears to be running: $LOCK_DIR"
    exit 1
  fi
  echo "$$" > "$LOCK_DIR/pid"
  trap 'rm -rf "$LOCK_DIR"' EXIT
}

sync_before_cycle() {
  local upstream_ref counts behind_count ahead_count
  git_branch_require_clean_tree || exit 1
  upstream_ref="$(get_upstream_ref)"
  CYCLE_UPSTREAM_REF="$upstream_ref"
  CYCLE_UPSTREAM_HEAD=""
  if [[ -z "$upstream_ref" ]]; then
    pp_info "No upstream configured; skipping remote sync checks."
    pp_success "Pre-flight checks passed."
    return 0
  fi
  git fetch --quiet
  CYCLE_UPSTREAM_HEAD="$(git rev-parse "$upstream_ref")"
  counts="$(git rev-list --left-right --count "${upstream_ref}...HEAD")"
  read -r behind_count ahead_count <<< "$counts"
  if (( behind_count > 0 )); then
    pp_error "Branch is behind upstream by ${behind_count} commit(s); refusing to start."
    exit 1
  fi
  pp_kv "Upstream" "$upstream_ref"
  pp_kv "Behind" "$behind_count commit(s)"
  pp_kv "Ahead" "$ahead_count commit(s)"
  pp_success "Pre-flight checks passed."
}

refuse_if_remote_advanced() {
  local upstream_ref="$1"
  local expected_upstream_head="$2"
  local current_upstream_head
  if [[ -z "$upstream_ref" || -z "$expected_upstream_head" ]]; then
    return 0
  fi
  git fetch --quiet
  current_upstream_head="$(git rev-parse "$upstream_ref")"
  if [[ "$current_upstream_head" != "$expected_upstream_head" ]]; then
    pp_error "Upstream $upstream_ref advanced during the cycle; refusing to continue."
    exit 1
  fi
}

require_command git

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  pp_error "Not inside a git work tree."
  exit 1
fi

configure_state_paths
acquire_lock

pp_banner "missive autonomous build loop"
pp_kv "Max cycles" "$MAX_CYCLES"
pp_kv "Sleep" "${SLEEP_SECONDS}s"
pp_kv "Push after commit" "$(pp_on_off "$PUSH_AFTER")"
pp_kv "State dir" "$BUILD_LOOP_STATE_DIR"
pp_kv "Logs" "$LOG_DIR"

if [[ -n "$SELECT_BRANCH" || -n "$CREATE_BRANCH" ]]; then
  pp_section "Branch setup"
  git_branch_prepare "$SELECT_BRANCH" "$CREATE_BRANCH" "$BRANCH_START_POINT" || exit $?
fi

pp_kv "Current branch" "$(git_branch_current)"

for file in "${REQUIRED_FILES[@]}"; do
  if [[ ! -f "$file" ]]; then
    pp_error "Required file missing: $file"
    exit 1
  fi
done

require_customised_template

cycle=0
while (( cycle < MAX_CYCLES )); do
  automation_status="$(get_automation_status)"
  if [[ -z "$automation_status" ]]; then
    pp_error "Missing top-level AUTOMATION_STATUS line in BUILD_TICKETS.md."
    exit 1
  fi
  if [[ "$automation_status" == "DONE" ]]; then
    pp_success "Build tickets marked done."
    exit 0
  fi

  cycle=$((cycle + 1))
  pp_banner "Autonomous build cycle" "$cycle/$MAX_CYCLES"

  pp_section "Current work"
  next_ticket="$(get_next_ticket_summary)"
  if [[ -n "$next_ticket" ]]; then
    pp_info "Now working on: ticket $next_ticket"
  else
    pp_warn "No TODO or IN_PROGRESS ticket found; agent will inspect BUILD_TICKETS.md."
  fi

  pp_section "Pre-flight checks"
  sync_before_cycle

  before_head="$(git rev-parse HEAD)"
  mkdir -p "$LOG_DIR"
  log_file="$LOG_DIR/cycle-$(date +%Y%m%d-%H%M%S)-$cycle.log"
  pp_kv "Log file" "$log_file"

  pp_section "Agent run"
  if ! scripts/run-agent.sh "$PROMPT" 2>&1 | tee "$log_file"; then
    pp_error "Agent failed during cycle $cycle; stopping."
    pp_hint "See $log_file"
    exit 1
  fi

  if [[ -n "$(git status --porcelain)" ]]; then
    pp_error "Agent left a dirty working tree; stopping for manual review."
    git status --short >&2
    exit 1
  fi

  refuse_if_remote_advanced "$CYCLE_UPSTREAM_REF" "$CYCLE_UPSTREAM_HEAD"

  after_head="$(git rev-parse HEAD)"
  if [[ "$after_head" == "$before_head" ]]; then
    pp_error "Cycle completed without a new commit; stopping."
    exit 1
  fi

  pp_success "Cycle committed $(git rev-parse --short HEAD)"

  if (( PUSH_AFTER == 1 )); then
    pp_section "Push"
    git_branch_push_current origin
  fi

  automation_status="$(get_automation_status)"
  if [[ "$automation_status" == "DONE" ]]; then
    pp_success "Build tickets marked done."
    exit 0
  fi

  if (( SLEEP_SECONDS > 0 )); then
    pp_info "Sleeping ${SLEEP_SECONDS}s before next cycle."
    sleep "$SLEEP_SECONDS"
  fi
done
