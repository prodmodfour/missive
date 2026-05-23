#!/usr/bin/env bash
set -Eeuo pipefail

# Run missive against the pinned upstream a2a-rs helloworld example agent.
# The upstream example binds fixed local ports 3000 (HTTP) and 50051 (gRPC).

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)

A2A_RS_REPO_URL=${A2A_RS_REPO_URL:-https://github.com/a2aproject/a2a-rs.git}
A2A_RS_REV=${A2A_RS_REV:-a32ef57182dd0ecd1d3c04f338778f1974494905}
A2A_RS_HTTP_PORT=${A2A_RS_HTTP_PORT:-3000}
A2A_RS_GRPC_PORT=${A2A_RS_GRPC_PORT:-50051}
A2A_RS_BASE_URL=${MISSIVE_A2A_RS_BASE_URL:-http://127.0.0.1:${A2A_RS_HTTP_PORT}}
MISSIVE_BIN=${MISSIVE_BIN:-$REPO_ROOT/target/debug/missive}
KEEP_WORKDIR=${MISSIVE_A2A_RS_KEEP_WORKDIR:-0}
USER_WORKDIR=${MISSIVE_A2A_RS_WORKDIR:-}

if [[ -n "$USER_WORKDIR" ]]; then
  WORKDIR=$USER_WORKDIR
  mkdir -p "$WORKDIR"
else
  WORKDIR=$(mktemp -d "${TMPDIR:-/tmp}/missive-a2a-rs-interop.XXXXXX")
fi

RESULTS_DIR=${MISSIVE_A2A_RS_RESULTS_DIR:-$WORKDIR/results}
RESULTS_NDJSON=$RESULTS_DIR/results.ndjson
SUMMARY_JSON=$RESULTS_DIR/summary.json
SERVER_LOG=$RESULTS_DIR/a2a-rs-helloworld.log
MISSIVE_HOME_DIR=$WORKDIR/missive-home
CHECKOUT_DIR=$WORKDIR/a2a-rs
SERVER_PGID=""
PASSES=0
FAILURES=0
SKIPS=0

mkdir -p "$RESULTS_DIR" "$MISSIVE_HOME_DIR"
: >"$RESULTS_NDJSON"

log() {
  printf '[interop:a2a-rs] %s\n' "$*"
}

record() {
  local check=$1
  local status=$2
  local classification=$3
  local message=$4

  python3 - "$RESULTS_NDJSON" "$check" "$status" "$classification" "$message" <<'PY'
import datetime as dt
import json
import sys

path, check, status, classification, message = sys.argv[1:]
record = {
    "timestamp": dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z"),
    "check": check,
    "status": status,
    "classification": classification,
    "message": message,
}
with open(path, "a", encoding="utf-8") as handle:
    handle.write(json.dumps(record, sort_keys=True) + "\n")
PY

  case "$status" in
    pass) PASSES=$((PASSES + 1)) ;;
    fail) FAILURES=$((FAILURES + 1)) ;;
    skip) SKIPS=$((SKIPS + 1)) ;;
    *)
      printf 'unknown status %s for %s\n' "$status" "$check" >&2
      exit 2
      ;;
  esac
  printf '%-24s %-5s %-36s %s\n' "$check" "$status" "$classification" "$message"
}

cleanup() {
  local status=$?
  if [[ -n "$SERVER_PGID" ]]; then
    kill -TERM "-$SERVER_PGID" 2>/dev/null || true
    sleep 0.5
    kill -KILL "-$SERVER_PGID" 2>/dev/null || true
    wait "$SERVER_PGID" 2>/dev/null || true
  fi
  if [[ "$KEEP_WORKDIR" != "1" && -z "$USER_WORKDIR" ]]; then
    rm -rf "$WORKDIR"
  else
    log "kept workdir: $WORKDIR"
  fi
  exit "$status"
}
trap cleanup EXIT

require_command() {
  local name=$1
  if ! command -v "$name" >/dev/null 2>&1; then
    record prerequisites fail environment "required command '$name' is missing"
    return 1
  fi
}

port_available() {
  local port=$1
  python3 - "$port" <<'PY'
import socket
import sys

port = int(sys.argv[1])
with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        sock.bind(("127.0.0.1", port))
    except OSError:
        sys.exit(1)
PY
}

validate_json_file() {
  local check=$1
  local path=$2
  local validator=$3
  if python3 - "$path" "$validator" <<'PY'
import json
import sys

path, validator = sys.argv[1:]
with open(path, "r", encoding="utf-8") as handle:
    value = json.load(handle)

if validator == "agent_add":
    assert value["ok"] is True
    assert value["kind"] == "agent_add"
    assert value["data"]["agent"]["alias"] == "upstream"
elif validator == "agent_inspect":
    assert value["ok"] is True
    assert value["kind"] == "agent_inspect"
    assert value["data"]["card"]["name"] == "Hello World Agent"
    assert value["data"]["card"]["capabilities"].get("streaming") is True
    assert value["data"]["selected_interface"]["binding"] in {"http+json", "json-rpc"}
elif validator == "send":
    assert value["ok"] is True
    assert value["kind"] == "send_result"
    assert value["data"]["response"]["shape"] == "task"
    assert value["data"]["response"]["state"] == "completed"
    assert "Echo: hello from missive" in value["data"]["response"].get("text", "")
elif validator == "task_list":
    assert value["ok"] is True
    assert value["kind"] == "task_list"
    assert value["data"]["source"] == "remote"
    assert value["data"]["count"] >= 1
    assert any("Echo:" in task.get("text", "") for task in value["data"]["tasks"])
elif validator == "push":
    assert value["ok"] is True
    assert value["kind"] in {"push_create", "push_get", "push_list", "push_delete"}
else:
    raise AssertionError(f"unknown validator: {validator}")
PY
  then
    record "$check" pass missive_verified "validated $path"
  else
    record "$check" fail missive_bug_or_protocol_regression "unexpected $validator output in $path"
  fi
}

validate_stream_file() {
  local path=$1
  if python3 - "$path" <<'PY'
import json
import sys

path = sys.argv[1]
records = []
with open(path, "r", encoding="utf-8") as handle:
    for line in handle:
        if line.strip():
            records.append(json.loads(line))
assert records, "stream emitted no records"
assert records[-1]["kind"] == "stream_result"
assert records[-1]["ok"] is True
assert records[-1]["data"]["final_state"] == "completed"
assert any(record["kind"] == "stream_event" for record in records[:-1])
assert any("Echo: stream from missive" in json.dumps(record) for record in records)
PY
  then
    record stream pass missive_verified "validated streaming NDJSON output"
  else
    record stream fail missive_bug_or_protocol_regression "stream output was not valid completed missive NDJSON"
  fi
}

push_supported_from_card() {
  local path=$1
  python3 - "$path" <<'PY'
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as handle:
    value = json.load(handle)
if value["data"]["card"]["capabilities"].get("pushNotifications") is True:
    sys.exit(0)
sys.exit(1)
PY
}

task_id_from_send() {
  local path=$1
  python3 - "$path" <<'PY'
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as handle:
    value = json.load(handle)
print(value["data"]["response"].get("task_id") or value["data"]["persistence"]["task_id"])
PY
}

write_summary() {
  python3 - "$RESULTS_NDJSON" "$SUMMARY_JSON" "$A2A_RS_REPO_URL" "$A2A_RS_REV" "$A2A_RS_BASE_URL" <<'PY'
import json
import sys
from collections import Counter

results_path, summary_path, repo_url, revision, base_url = sys.argv[1:]
records = []
with open(results_path, "r", encoding="utf-8") as handle:
    for line in handle:
        if line.strip():
            records.append(json.loads(line))
counts = Counter(record["status"] for record in records)
summary = {
    "schema_version": "missive.interop.a2a-rs.v1",
    "repo_url": repo_url,
    "revision": revision,
    "base_url": base_url,
    "counts": dict(sorted(counts.items())),
    "ok": counts.get("fail", 0) == 0,
    "results": records,
}
with open(summary_path, "w", encoding="utf-8") as handle:
    json.dump(summary, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
  log "summary: $SUMMARY_JSON"
}

main() {
  require_command python3 || return 1
  require_command cargo || return 1
  require_command curl || return 1

  if [[ ! -x "$MISSIVE_BIN" ]]; then
    log "building missive binary at $MISSIVE_BIN"
    (cd "$REPO_ROOT" && cargo build -p missive-cli --bin missive)
  fi
  if [[ ! -x "$MISSIVE_BIN" ]]; then
    record missive_binary fail environment "missive binary is not executable at $MISSIVE_BIN"
    return 1
  fi
  record missive_binary pass environment "using $MISSIVE_BIN"

  if [[ -z "${MISSIVE_A2A_RS_BASE_URL:-}" ]]; then
    require_command git || return 1
    require_command setsid || return 1
    if ! port_available "$A2A_RS_HTTP_PORT"; then
      record upstream_server_start fail environment "local HTTP port $A2A_RS_HTTP_PORT is already in use; upstream helloworld uses fixed ports"
      return 1
    fi
    if ! port_available "$A2A_RS_GRPC_PORT"; then
      record upstream_server_start fail environment "local gRPC port $A2A_RS_GRPC_PORT is already in use; upstream helloworld uses fixed ports"
      return 1
    fi

    log "checking out $A2A_RS_REPO_URL at $A2A_RS_REV"
    git init -q "$CHECKOUT_DIR"
    git -C "$CHECKOUT_DIR" remote add origin "$A2A_RS_REPO_URL"
    git -C "$CHECKOUT_DIR" fetch --depth 1 origin "$A2A_RS_REV"
    git -C "$CHECKOUT_DIR" checkout -q --detach FETCH_HEAD
    local actual_rev
    actual_rev=$(git -C "$CHECKOUT_DIR" rev-parse HEAD)
    if [[ "$actual_rev" != "$A2A_RS_REV" ]]; then
      record upstream_checkout fail upstream_dependency "checked out $actual_rev instead of pinned $A2A_RS_REV"
      return 1
    fi
    record upstream_checkout pass upstream_dependency "checked out pinned a2a-rs revision $A2A_RS_REV"

    log "starting upstream helloworld-server; log: $SERVER_LOG"
    # shellcheck disable=SC2016 # $1 is expanded by the child bash invoked below.
    setsid bash -c 'cd "$1" && exec cargo run --quiet --package examples --bin helloworld-server' bash "$CHECKOUT_DIR" >"$SERVER_LOG" 2>&1 &
    SERVER_PGID=$!
    for _ in $(seq 1 120); do
      if curl -fsS "$A2A_RS_BASE_URL/.well-known/agent-card.json" >"$RESULTS_DIR/upstream-card.json" 2>/dev/null; then
        record upstream_server_start pass upstream_dependency "upstream helloworld responded at $A2A_RS_BASE_URL"
        break
      fi
      if ! kill -0 "$SERVER_PGID" 2>/dev/null; then
        record upstream_server_start fail upstream_dependency "upstream helloworld exited before Agent Card was reachable; see $SERVER_LOG"
        return 1
      fi
      sleep 0.5
    done
    if [[ ! -s "$RESULTS_DIR/upstream-card.json" ]]; then
      record upstream_server_start fail upstream_dependency "timed out waiting for upstream Agent Card at $A2A_RS_BASE_URL"
      return 1
    fi
  else
    record upstream_checkout skip external_agent "using already-running agent at $A2A_RS_BASE_URL"
    if curl -fsS "$A2A_RS_BASE_URL/.well-known/agent-card.json" >"$RESULTS_DIR/upstream-card.json"; then
      record upstream_server_start pass external_agent "external agent responded at $A2A_RS_BASE_URL"
    else
      record upstream_server_start fail external_agent "external agent did not return an Agent Card at $A2A_RS_BASE_URL"
      return 1
    fi
  fi

  export MISSIVE_HOME=$MISSIVE_HOME_DIR
  local base_url
  base_url=${A2A_RS_BASE_URL%/}

  if "$MISSIVE_BIN" agent add upstream "$base_url" \
    --interface "http+json=$base_url/rest" \
    --interface "json-rpc=$base_url/jsonrpc" \
    --json >"$RESULTS_DIR/agent-add.json" 2>"$RESULTS_DIR/agent-add.stderr"; then
    validate_json_file agent_registration "$RESULTS_DIR/agent-add.json" agent_add
  else
    record agent_registration fail missive_bug_or_protocol_regression "missive agent add failed; see $RESULTS_DIR/agent-add.stderr"
  fi

  if "$MISSIVE_BIN" agent inspect upstream --refresh --json >"$RESULTS_DIR/agent-inspect.json" 2>"$RESULTS_DIR/agent-inspect.stderr"; then
    validate_json_file card_discovery "$RESULTS_DIR/agent-inspect.json" agent_inspect
  else
    record card_discovery fail missive_bug_or_protocol_regression "missive agent inspect failed; see $RESULTS_DIR/agent-inspect.stderr"
  fi

  if "$MISSIVE_BIN" send upstream "hello from missive" --json >"$RESULTS_DIR/send.json" 2>"$RESULTS_DIR/send.stderr"; then
    validate_json_file send "$RESULTS_DIR/send.json" send
  else
    record send fail missive_bug_or_protocol_regression "missive send failed; see $RESULTS_DIR/send.stderr"
  fi

  if "$MISSIVE_BIN" stream upstream "stream from missive" --ndjson >"$RESULTS_DIR/stream.ndjson" 2>"$RESULTS_DIR/stream.stderr"; then
    validate_stream_file "$RESULTS_DIR/stream.ndjson"
  else
    record stream fail missive_bug_or_protocol_regression "missive stream failed; see $RESULTS_DIR/stream.stderr"
  fi

  if "$MISSIVE_BIN" task list --remote --agent upstream --json >"$RESULTS_DIR/task-list.json" 2>"$RESULTS_DIR/task-list.stderr"; then
    validate_json_file task_list "$RESULTS_DIR/task-list.json" task_list
  else
    record task_list fail missive_bug_or_protocol_regression "missive remote task list failed; see $RESULTS_DIR/task-list.stderr"
  fi

  if [[ -s "$RESULTS_DIR/agent-inspect.json" ]] && push_supported_from_card "$RESULTS_DIR/agent-inspect.json"; then
    local task_id
    if [[ -s "$RESULTS_DIR/send.json" ]] && task_id=$(task_id_from_send "$RESULTS_DIR/send.json"); then
      if "$MISSIVE_BIN" push create upstream "$task_id" "http://127.0.0.1:9/a2a/push" \
        --config-id missive-interop --json >"$RESULTS_DIR/push-create.json" 2>"$RESULTS_DIR/push-create.stderr"; then
        validate_json_file push_config "$RESULTS_DIR/push-create.json" push
      else
        record push_config fail missive_bug_or_protocol_regression "upstream card advertises push, but missive push create failed; see $RESULTS_DIR/push-create.stderr"
      fi
    else
      record push_config fail missive_bug_or_protocol_regression "upstream card advertises push, but no task id was available from send.json"
    fi
  else
    record push_config skip upstream_example_limitation "a2a-rs helloworld Agent Card advertises pushNotifications=false"
  fi

  write_summary
  printf '\nSummary: %s pass, %s fail, %s skip\n' "$PASSES" "$FAILURES" "$SKIPS"
  if [[ "$FAILURES" -gt 0 ]]; then
    return 1
  fi
}

main "$@"
