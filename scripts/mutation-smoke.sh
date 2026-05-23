#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=scripts/lib/pretty-print.sh
source "$SCRIPT_DIR/lib/pretty-print.sh"

cd "$REPO_ROOT"

have() { command -v "$1" >/dev/null 2>&1; }

if ! have cargo-mutants; then
  pp_warn "cargo-mutants not installed; skipping mutation smoke"
  exit 0
fi

mode="${MISSIVE_MUTANTS_MODE:-check}"
shard="${MISSIVE_MUTANTS_SHARD:-1/12}"
timeout="${MISSIVE_MUTANTS_TIMEOUT:-30}"
jobs="${MISSIVE_MUTANTS_JOBS:-1}"
baseline="${MISSIVE_MUTANTS_BASELINE:-skip}"
output_parent="${MISSIVE_MUTANTS_OUTPUT:-}"
keep_output="${MISSIVE_MUTANTS_KEEP_OUTPUT:-0}"
filter_re="${MISSIVE_MUTANTS_RE:-}"

case "$mode" in
  check|run|list) ;;
  *)
    pp_error "MISSIVE_MUTANTS_MODE must be one of: check, run, list"
    exit 2
    ;;
esac

if [[ -z "$output_parent" ]]; then
  output_parent="$(mktemp -d "${TMPDIR:-/tmp}/missive-mutants.XXXXXX")"
  cleanup_output=1
else
  mkdir -p "$output_parent"
  cleanup_output=0
fi

cleanup() {
  local status=$?
  if [[ "$cleanup_output" == "1" && "$keep_output" != "1" ]]; then
    rm -rf "$output_parent"
  else
    pp_info "Mutation output retained at $output_parent"
  fi
  exit "$status"
}
trap cleanup EXIT

if [[ -n "${MISSIVE_MUTANTS_FILES:-}" ]]; then
  read -r -a files <<<"$MISSIVE_MUTANTS_FILES"
else
  files=(
    "crates/missive-store/src/repository.rs"
    "crates/missive-router/src/lib.rs"
    "crates/missive-cli/src/auth.rs"
    "crates/missive-cli/src/output.rs"
    "crates/missive-cli/src/task.rs"
    "crates/missive-cli/src/bcast.rs"
    "crates/missive-cli/src/barrier.rs"
    "crates/missive-cli/src/gather.rs"
    "crates/missive-cli/src/reduce.rs"
  )
fi

args=(
  mutants
  --workspace
  --all-features
  --shard "$shard"
  --timeout "$timeout"
  --jobs "$jobs"
  --no-shuffle
)

if [[ "$mode" != "list" ]]; then
  args+=(--baseline "$baseline" --output "$output_parent")
fi
if [[ "$mode" == "check" ]]; then
  args+=(--check)
elif [[ "$mode" == "list" ]]; then
  args+=(--list)
fi

for file in "${files[@]}"; do
  [[ -n "$file" ]] || continue
  args+=(--file "$file")
done

if [[ -n "$filter_re" ]]; then
  args+=(--re "$filter_re")
fi

pp_section "Mutation smoke"
pp_kv "mode" "$mode"
pp_kv "shard" "$shard"
pp_kv "timeout" "$timeout"
pp_kv "jobs" "$jobs"
pp_kv "files" "${files[*]}"

pp_cmd cargo "${args[@]}"
cargo "${args[@]}"
