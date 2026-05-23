#!/usr/bin/env bash
set -euo pipefail

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  printf 'examples/lib/demo-common.sh is a shared library; source a demo script instead.\n' >&2
  exit 64
fi

if [[ -n "${MISSIVE_EXAMPLE_COMMON_LOADED:-}" ]]; then
  return 0
fi
MISSIVE_EXAMPLE_COMMON_LOADED=1

MISSIVE_EXAMPLES_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MISSIVE_REPO_ROOT="$(cd "$MISSIVE_EXAMPLES_DIR/.." && pwd)"
MISSIVE_EXAMPLE_OWNS_WORKDIR=0
MISSIVE_EXAMPLE_MOCK_PID=""
MISSIVE_EXAMPLE_MOCK_PIDS=()
MISSIVE_EXAMPLE_LAST_A2A_BASE_URL=""

example_usage_hint() {
  cat <<'EOF'
Example environment overrides:
  MISSIVE_BIN=/path/to/missive               Use an already-built missive binary.
  MISSIVE_EXAMPLE_A2A_BASE_URL=http://...    Reuse an existing local mock A2A server.
  MISSIVE_EXAMPLE_WORKDIR=/tmp/missive-demo  Keep all demo runtime state under this directory.
  MISSIVE_EXAMPLE_KEEP_WORKDIR=1             Do not delete the generated temporary workdir.
EOF
}

example_cleanup() {
  local status=$?
  local mock_pid
  for mock_pid in "${MISSIVE_EXAMPLE_MOCK_PIDS[@]}"; do
    kill "$mock_pid" >/dev/null 2>&1 || true
    wait "$mock_pid" >/dev/null 2>&1 || true
  done
  if [[ "$MISSIVE_EXAMPLE_OWNS_WORKDIR" == "1" && "${MISSIVE_EXAMPLE_KEEP_WORKDIR:-0}" != "1" ]]; then
    rm -rf "$MISSIVE_EXAMPLE_WORKDIR"
  elif [[ "$MISSIVE_EXAMPLE_OWNS_WORKDIR" == "1" ]]; then
    printf 'Example workdir kept at %s\n' "$MISSIVE_EXAMPLE_WORKDIR"
  fi
  exit "$status"
}
trap example_cleanup EXIT

example_prepare_environment() {
  local name="$1"

  if [[ -z "${MISSIVE_EXAMPLE_WORKDIR:-}" ]]; then
    MISSIVE_EXAMPLE_WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/missive-example-${name}.XXXXXX")"
    MISSIVE_EXAMPLE_OWNS_WORKDIR=1
  else
    mkdir -p "$MISSIVE_EXAMPLE_WORKDIR"
  fi
  export MISSIVE_EXAMPLE_WORKDIR

  if [[ "${MISSIVE_EXAMPLE_USE_EXISTING_HOME:-0}" == "1" && -n "${MISSIVE_HOME:-}" ]]; then
    mkdir -p "$MISSIVE_HOME"
  else
    export MISSIVE_HOME="$MISSIVE_EXAMPLE_WORKDIR/$name/missive-home"
    mkdir -p "$MISSIVE_HOME"
  fi

  if [[ "${MISSIVE_EXAMPLE_ALLOW_CONFIG:-0}" != "1" ]]; then
    unset MISSIVE_CONFIG MISSIVE_REPO_CONFIG
  fi
  if [[ "${MISSIVE_EXAMPLE_ALLOW_LOG_ENV:-0}" != "1" ]]; then
    unset RUST_LOG MISSIVE_LOG_FORMAT MISSIVE_LOG_JSON
  fi
}

example_missive_cmd() {
  if [[ -n "${MISSIVE_BIN:-}" ]]; then
    MISSIVE_CMD=("$MISSIVE_BIN")
  else
    MISSIVE_CMD=(cargo run --quiet -p missive-cli --bin missive --)
  fi
}

missive() {
  "${MISSIVE_CMD[@]}" "$@"
}

run_missive() {
  printf '\n$ missive'
  printf ' %q' "$@"
  printf '\n'
  missive "$@"
}

example_mock_a2a_binary() {
  local mock_bin="${MISSIVE_EXAMPLE_MOCK_BIN:-}"

  if [[ -z "$mock_bin" ]]; then
    if ! command -v cargo >/dev/null 2>&1; then
      printf 'cargo is required to build the local mock A2A server when MISSIVE_EXAMPLE_A2A_BASE_URL is unset.\n' >&2
      example_usage_hint >&2
      return 127
    fi
    (cd "$MISSIVE_REPO_ROOT" && cargo build --quiet -p missive-test-support --example mock_a2a_server)
    local cargo_target_dir="${CARGO_TARGET_DIR:-$MISSIVE_REPO_ROOT/target}"
    if [[ "$cargo_target_dir" != /* ]]; then
      cargo_target_dir="$MISSIVE_REPO_ROOT/$cargo_target_dir"
    fi
    mock_bin="$cargo_target_dir/debug/examples/mock_a2a_server"
  fi

  printf '%s\n' "$mock_bin"
}

example_start_mock_a2a_instance() {
  local name="$1"
  shift
  local ready_file="$MISSIVE_EXAMPLE_WORKDIR/mock-a2a-${name}-base-url"
  local stdout_file="$MISSIVE_EXAMPLE_WORKDIR/mock-a2a-${name}.stdout"
  local stderr_file="$MISSIVE_EXAMPLE_WORKDIR/mock-a2a-${name}.stderr"
  local mock_bin
  mock_bin="$(example_mock_a2a_binary)"

  "$mock_bin" --ready-file "$ready_file" "$@" >"$stdout_file" 2>"$stderr_file" &
  local mock_pid=$!
  MISSIVE_EXAMPLE_MOCK_PIDS+=("$mock_pid")
  if [[ -z "$MISSIVE_EXAMPLE_MOCK_PID" ]]; then
    MISSIVE_EXAMPLE_MOCK_PID="$mock_pid"
  fi

  for _ in {1..200}; do
    if [[ -s "$ready_file" ]]; then
      MISSIVE_EXAMPLE_LAST_A2A_BASE_URL="$(<"$ready_file")"
      printf '%s\n' "$MISSIVE_EXAMPLE_LAST_A2A_BASE_URL"
      return 0
    fi
    if ! kill -0 "$mock_pid" >/dev/null 2>&1; then
      printf 'mock A2A server %s exited before writing %s\n' "$name" "$ready_file" >&2
      sed -n '1,120p' "$stderr_file" >&2 || true
      return 1
    fi
    sleep 0.05
  done

  printf 'timed out waiting for mock A2A server %s ready file %s\n' "$name" "$ready_file" >&2
  sed -n '1,120p' "$stderr_file" >&2 || true
  return 1
}

example_start_mock_a2a() {
  if [[ -n "${MISSIVE_EXAMPLE_A2A_BASE_URL:-}" ]]; then
    return 0
  fi

  example_start_mock_a2a_instance default >/dev/null
  MISSIVE_EXAMPLE_A2A_BASE_URL="$MISSIVE_EXAMPLE_LAST_A2A_BASE_URL"
  export MISSIVE_EXAMPLE_A2A_BASE_URL
}

example_add_echo_agent() {
  run_missive agent add echo "$MISSIVE_EXAMPLE_A2A_BASE_URL" --tag local --metadata example=true --json
}

example_init() {
  local name="$1"
  example_prepare_environment "$name"
  example_missive_cmd
  example_start_mock_a2a
  printf 'Using MISSIVE_HOME=%s\n' "$MISSIVE_HOME"
  printf 'Using mock A2A server=%s\n' "$MISSIVE_EXAMPLE_A2A_BASE_URL"
}
