#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=scripts/lib/pretty-print.sh
source "$SCRIPT_DIR/lib/pretty-print.sh"

have() { command -v "$1" >/dev/null 2>&1; }

env_flag_enabled() {
  local value="${1:-0}"
  case "$value" in
    1|true|TRUE|yes|YES|on|ON) return 0 ;;
    *) return 1 ;;
  esac
}

if ! have docker; then
  pp_error "docker is not installed; install Docker or run scripts/bootstrap-tools.sh --docker where supported"
  exit 127
fi

IMAGE_TAG="${MISSIVE_DOCKER_TEST_TAG:-missive-quality-gate:local}"
CONTAINER_WORKDIR="${MISSIVE_DOCKER_WORKDIR:-/workspace/missive}"
CONTAINER_HOME="${MISSIVE_DOCKER_HOME:-/tmp/missive-docker-home}"
CONTAINER_CARGO_HOME="${MISSIVE_DOCKER_CARGO_HOME:-/tmp/missive-docker-cargo}"
CONTAINER_TARGET_DIR="${MISSIVE_DOCKER_TARGET_DIR:-/tmp/missive-docker-target}"
CONTAINER_MISSIVE_HOME="${MISSIVE_DOCKER_MISSIVE_HOME:-/tmp/missive-home}"
RUN_QUALITY_GATE="${MISSIVE_DOCKER_RUN_QUALITY_GATE:-1}"
FORCE_BUILD="${MISSIVE_DOCKER_FORCE_BUILD:-0}"
RUN_QUALITY_GATE_ENABLED=0
if env_flag_enabled "$RUN_QUALITY_GATE"; then
  RUN_QUALITY_GATE_ENABLED=1
fi
FORCE_BUILD_ENABLED=0
if env_flag_enabled "$FORCE_BUILD"; then
  FORCE_BUILD_ENABLED=1
fi

pp_banner "missive Docker integration"
pp_kv "repository" "$REPO_ROOT"
pp_kv "image" "$IMAGE_TAG"
pp_kv "container workdir" "$CONTAINER_WORKDIR"
pp_kv "run quality gate" "$(pp_on_off "$RUN_QUALITY_GATE_ENABLED")"

if ((FORCE_BUILD_ENABLED == 1)) || ! docker image inspect "$IMAGE_TAG" >/dev/null 2>&1; then
  pp_section "Docker image build"
  pp_cmd docker build --pull=false --tag "$IMAGE_TAG" --build-arg "MISSIVE_UID=$(id -u)" --build-arg "MISSIVE_GID=$(id -g)" "$REPO_ROOT"
  docker build \
    --pull=false \
    --tag "$IMAGE_TAG" \
    --build-arg "MISSIVE_UID=$(id -u)" \
    --build-arg "MISSIVE_GID=$(id -g)" \
    "$REPO_ROOT"
else
  pp_info "Docker image $IMAGE_TAG already exists; set MISSIVE_DOCKER_FORCE_BUILD=1 to rebuild."
fi

if ((RUN_QUALITY_GATE_ENABLED == 1)); then
  pp_section "Container quality gate"
  pp_cmd docker run --rm --workdir "$CONTAINER_WORKDIR" --mount "type=bind,source=$REPO_ROOT,target=$CONTAINER_WORKDIR" "$IMAGE_TAG" scripts/quality-gate.sh
  docker run --rm \
    --user "$(id -u):$(id -g)" \
    --workdir "$CONTAINER_WORKDIR" \
    --mount "type=bind,source=$REPO_ROOT,target=$CONTAINER_WORKDIR" \
    --env "HOME=$CONTAINER_HOME" \
    --env "CARGO_HOME=$CONTAINER_CARGO_HOME" \
    --env "CARGO_TARGET_DIR=$CONTAINER_TARGET_DIR" \
    --env "MISSIVE_HOME=$CONTAINER_MISSIVE_HOME" \
    --env CARGO_TERM_COLOR=never \
    "$IMAGE_TAG" \
    bash -c 'mkdir -p "$HOME" "$CARGO_HOME" "$CARGO_TARGET_DIR" "$MISSIVE_HOME" && scripts/quality-gate.sh'
fi

pp_section "Summary"
pp_success "Docker integration validation completed."
