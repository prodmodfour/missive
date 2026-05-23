#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=scripts/lib/pretty-print.sh
source "$SCRIPT_DIR/lib/pretty-print.sh"

cd "$REPO_ROOT"

have() { command -v "$1" >/dev/null 2>&1; }
warn() { pp_warn "$*"; }

run_cmd() {
  pp_cmd "$*"
  "$@"
}

run_cmd_with_env() {
  pp_cmd "$*"
  env "$@"
}

env_flag_enabled() {
  local value="${1:-0}"
  case "$value" in
    1|true|TRUE|yes|YES|on|ON) return 0 ;;
    *) return 1 ;;
  esac
}

rustup_component_installed() {
  local component="$1"
  have rustup && rustup component list --installed 2>/dev/null | grep -q "^${component}\\b"
}

ensure_rustup_component_best_effort() {
  local component="$1"
  if ! have rustup; then
    return 0
  fi
  if rustup_component_installed "$component"; then
    return 0
  fi
  rustup component add "$component" >/dev/null 2>&1 || warn "Could not ensure rustup component $component via rustup"
}

has_benchmark_sources() {
  find crates -path '*/benches/*.rs' -type f -print -quit 2>/dev/null | grep -q . && return 0
  if [[ -d benches ]]; then
    find benches -name '*.rs' -type f -print -quit 2>/dev/null | grep -q . && return 0
  fi
  return 1
}

has_docker_inputs() {
  [[ -f Dockerfile || -f docker-compose.yml || -f compose.yml || -x scripts/docker-integration.sh || -f .devcontainer/devcontainer.json ]]
}

run_shell_checks() {
  local scripts_to_check=()
  local script

  pp_section "Shell checks"
  while IFS= read -r -d '' script; do
    scripts_to_check+=("$script")
    pp_step "bash -n $script"
    bash -n "$script"
  done < <(find scripts -type f -name '*.sh' -print0 | sort -z)
  pp_success "Shell syntax checks passed."

  if have shellcheck; then
    if ((${#scripts_to_check[@]} > 0)); then
      run_cmd shellcheck "${scripts_to_check[@]}"
    fi
  else
    warn "shellcheck not installed; skipping shell lint check"
  fi
}

run_ci_config_checks() {
  if [[ ! -d .github/workflows ]]; then
    return 0
  fi

  pp_section "CI workflow validation"
  if [[ ! -x scripts/validate-ci.sh ]]; then
    pp_error "Missing required CI workflow validator: scripts/validate-ci.sh"
    exit 1
  fi
  run_cmd bash scripts/validate-ci.sh
}

run_guardrails() {
  pp_section "Secret guardrail"
  if [[ ! -f scripts/check-no-secrets.sh ]]; then
    pp_error "Missing required secret guardrail: scripts/check-no-secrets.sh"
    exit 1
  fi
  run_cmd bash scripts/check-no-secrets.sh

  pp_section "Generated/private-file guardrail"
  if [[ ! -f scripts/check-no-generated-private-files.sh ]]; then
    pp_error "Missing required generated/private-file guardrail: scripts/check-no-generated-private-files.sh"
    exit 1
  fi
  run_cmd bash scripts/check-no-generated-private-files.sh
}

run_rust_required_checks() {
  pp_section "Rust workspace"
  if ! have cargo; then
    pp_error "cargo not installed. Install Rust with rustup or run scripts/bootstrap-tools.sh."
    exit 127
  fi

  ensure_rustup_component_best_effort rustfmt
  ensure_rustup_component_best_effort clippy

  if ! cargo fmt --version >/dev/null 2>&1; then
    pp_error "cargo fmt/rustfmt is not available. Run scripts/bootstrap-tools.sh or rustup component add rustfmt."
    exit 127
  fi
  if ! cargo clippy --version >/dev/null 2>&1; then
    pp_error "cargo clippy is not available. Run scripts/bootstrap-tools.sh or rustup component add clippy."
    exit 127
  fi

  pp_section "Rust feature checks"
  run_cmd cargo check --workspace --all-targets
  run_cmd cargo check --workspace --all-targets --all-features
  run_cmd cargo check --workspace --all-targets --no-default-features

  pp_section "Rust formatting and lints"
  run_cmd cargo fmt --all -- --check
  run_cmd cargo clippy --workspace --all-targets --all-features -- -D warnings

  pp_section "Rust tests"
  run_cmd cargo test --workspace --all-targets --all-features
  run_cmd cargo test --workspace --doc --all-features

  pp_section "Rust documentation"
  run_cmd_with_env RUSTDOCFLAGS=-Dwarnings cargo doc --workspace --all-features --no-deps

  pp_section "Rust builds"
  run_cmd cargo build --workspace --all-features
  run_cmd cargo build --workspace --all-features --release
  run_cmd cargo build -p missive-cli --bin missive --release
}

run_dependency_checks() {
  pp_section "Dependency and advisory checks"

  if have cargo-machete; then
    run_cmd cargo machete
  else
    warn "cargo-machete not installed; skipping unused dependency check"
  fi

  if have cargo-audit; then
    run_cmd cargo audit
  else
    warn "cargo-audit not installed; skipping advisory audit"
  fi

  if [[ -f deny.toml || -f cargo-deny.toml ]]; then
    if have cargo-deny; then
      run_cmd cargo deny check
    else
      warn "cargo-deny config exists but cargo-deny is not installed"
    fi
  else
    pp_info "No cargo-deny config found; dependency policy check is deferred until a policy file exists."
  fi
}

run_fuzz_smoke() {
  local fuzz_seconds="${MISSIVE_FUZZ_SECONDS:-15}"
  local fuzz_sanitizer="${MISSIVE_FUZZ_SANITIZER:-none}"
  local fuzz_targets=()
  local target

  if ! have cargo-fuzz; then
    warn "cargo-fuzz not installed; skipping fuzz smoke"
    return 0
  fi
  if [[ ! -d fuzz ]]; then
    warn "No fuzz/ directory found; skipping fuzz smoke"
    return 0
  fi

  while IFS= read -r target; do
    [[ -n "$target" ]] || continue
    fuzz_targets+=("$target")
  done < <(cargo fuzz list 2>/dev/null || true)

  if ((${#fuzz_targets[@]} == 0)); then
    warn "cargo-fuzz found no fuzz targets; skipping fuzz smoke"
    return 0
  fi

  for target in "${fuzz_targets[@]}"; do
    run_cmd cargo fuzz run "$target" --sanitizer "$fuzz_sanitizer" -- -max_total_time="$fuzz_seconds"
  done
}

run_miri_smoke() {
  ensure_rustup_component_best_effort miri

  if cargo miri --version >/dev/null 2>&1; then
    run_cmd cargo miri test --workspace --all-features
  else
    warn "miri component is not available for the active toolchain; skipping miri"
  fi
}

run_mutation_smoke() {
  if [[ -x scripts/mutation-smoke.sh ]]; then
    run_cmd scripts/mutation-smoke.sh
  elif have cargo-mutants; then
    run_cmd cargo mutants --workspace --check --shard "${MISSIVE_MUTANTS_SHARD:-1/12}" --timeout "${MISSIVE_MUTANTS_TIMEOUT:-30}" --jobs "${MISSIVE_MUTANTS_JOBS:-1}" --no-shuffle
  else
    warn "cargo-mutants not installed; skipping mutation smoke"
  fi
}

run_coverage_smoke() {
  if have cargo-llvm-cov; then
    ensure_rustup_component_best_effort llvm-tools-preview
    run_cmd cargo llvm-cov --workspace --all-features --no-report
  else
    warn "cargo-llvm-cov not installed; skipping coverage smoke"
  fi
}

run_nextest_smoke() {
  if have cargo-nextest; then
    run_cmd cargo nextest run --workspace --all-features
  else
    warn "cargo-nextest not installed; skipping nextest"
  fi
}

run_benchmark_builds() {
  if has_benchmark_sources; then
    run_cmd cargo bench --workspace --all-features --no-run
  else
    warn "No Rust benchmark sources found; skipping benchmark compile smoke"
  fi
}

run_docker_aggressive_checks() {
  pp_section "Aggressive Docker/container checks"

  if ! has_docker_inputs; then
    pp_info "No Dockerfile, Compose file, devcontainer, or docker integration script found; skipping Docker checks."
    return 0
  fi

  if ! have docker; then
    warn "docker not installed; skipping Docker/container checks"
    return 0
  fi

  if [[ -f docker-compose.yml ]]; then
    run_cmd docker compose -f docker-compose.yml config
  fi
  if [[ -f compose.yml ]]; then
    run_cmd docker compose -f compose.yml config
  fi
  if [[ -f Dockerfile ]]; then
    run_cmd docker build --pull=false --tag "${MISSIVE_DOCKER_TEST_TAG:-missive-quality-gate:local}" .
  fi
  if [[ -x scripts/docker-integration.sh ]]; then
    run_cmd scripts/docker-integration.sh
  fi
  if [[ -f .devcontainer/devcontainer.json ]]; then
    if have devcontainer; then
      run_cmd devcontainer read-configuration --workspace-folder .
    else
      warn "devcontainer CLI not installed; skipping devcontainer validation"
    fi
  fi
}

run_aggressive_checks() {
  pp_section "Aggressive Rust checks"
  run_nextest_smoke
  run_coverage_smoke
  run_dependency_checks
  run_miri_smoke
  run_mutation_smoke
  run_fuzz_smoke
  run_benchmark_builds
  run_docker_aggressive_checks
}

run_just_ci() {
  if [[ -f justfile ]] && have just && grep -Eq '^[[:space:]]*ci:' justfile; then
    pp_section "just ci"
    run_cmd just ci
  fi
}

run_default_compose_validation() {
  if [[ -f docker-compose.yml || -f compose.yml ]]; then
    pp_section "Docker Compose validation"
    if have docker; then
      [[ ! -f docker-compose.yml ]] || run_cmd docker compose -f docker-compose.yml config
      [[ ! -f compose.yml ]] || run_cmd docker compose -f compose.yml config
    else
      warn "docker not installed; skipping Docker Compose validation"
    fi
  fi
}

run_node_adjunct() {
  if [[ -f package.json ]]; then
    pp_section "Node project adjunct"
    if have npm; then
      if [[ -f package-lock.json ]]; then
        run_cmd npm ci
      else
        run_cmd npm install
      fi
      run_cmd npm run lint --if-present
      run_cmd npm run typecheck --if-present
      run_cmd npm test --if-present
      run_cmd npm run build --if-present
    else
      warn "npm not installed; skipping Node checks"
    fi
  fi
}

AGGRESSIVE=0
if env_flag_enabled "${MISSIVE_AGGRESSIVE_TESTS:-0}"; then
  AGGRESSIVE=1
fi

pp_banner "missive quality gate"
pp_kv "repository" "$REPO_ROOT"
pp_kv "aggressive checks" "$(pp_on_off "$AGGRESSIVE")"

run_shell_checks
run_ci_config_checks
run_guardrails

if [[ -f Cargo.toml ]]; then
  run_rust_required_checks
  run_dependency_checks
  if ((AGGRESSIVE == 1)); then
    run_aggressive_checks
  fi
else
  pp_info "No Cargo.toml yet; skipping Rust checks until ticket 000/001 creates the workspace."
fi

run_just_ci
run_default_compose_validation
run_node_adjunct

pp_section "Summary"
pp_success "Quality gate passed."
