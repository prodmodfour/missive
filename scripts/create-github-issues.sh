#!/usr/bin/env bash
set -euo pipefail

REPO=""
LABELS="autonomous-build"
DRY_RUN=0

usage() {
  cat <<'USAGE'
Usage: scripts/create-github-issues.sh --repo OWNER/REPO [--dry-run]

Creates GitHub issues from tickets.json using gh and jq.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo) REPO="${2:?}"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if [[ -z "$REPO" ]]; then
  echo "--repo is required" >&2
  exit 2
fi

command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 127; }
command -v gh >/dev/null 2>&1 || { echo "gh is required" >&2; exit 127; }

count="$(jq length tickets.json)"
for i in $(seq 0 $((count - 1))); do
  id="$(jq -r ".[$i].id" tickets.json)"
  title="$(jq -r ".[$i].title" tickets.json)"
  phase="$(jq -r ".[$i].phase" tickets.json)"
  body="$(mktemp)"
  {
    echo "Phase: $phase"
    echo
    echo "### Required"
    jq -r ".[$i].required[] | \"* \" + ." tickets.json
    echo
    echo "### Acceptance criteria"
    jq -r ".[$i].acceptance_criteria[] | \"* \" + ." tickets.json
    echo
    echo "### Validation"
    jq -r ".[$i].validation[] | \"* \" + ." tickets.json
  } > "$body"

  full_title="$id — $title"
  labels="$LABELS,phase:$(printf '%s' "$phase" | tr '[:upper:] ' '[:lower:]-')"

  if [[ "$DRY_RUN" == "1" ]]; then
    echo "Would create: $full_title [$labels]"
  else
    gh issue create --repo "$REPO" --title "$full_title" --body-file "$body" --label "$labels"
  fi
  rm -f "$body"
done
