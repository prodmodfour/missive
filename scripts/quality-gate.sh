#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/pretty-print.sh
source "$SCRIPT_DIR/lib/pretty-print.sh"

have() { command -v "$1" >/dev/null 2>&1; }
run_cmd() { pp_cmd "$*"; "$@"; }
warn() { pp_warn "$*"; }

pp_banner "missive quality gate"

pp_section "Shell syntax checks"
while IFS= read -r -d '' script; do
  pp_step "bash -n $script"
  bash -n "$script"
done < <(find scripts -type f -name '*.sh' -print0 | sort -z)
pp_success "Shell syntax checks passed."

if [[ -f scripts/check-no-secrets.sh ]]; then
  pp_section "Secret guardrail"
  run_cmd bash scripts/check-no-secrets.sh
fi

if [[ -f scripts/check-no-generated-private-files.sh ]]; then
  pp_section "Generated/private-file guardrail"
  run_cmd bash scripts/check-no-generated-private-files.sh
fi

if [[ -f Cargo.toml ]]; then
  pp_section "Rust workspace"
  if ! have cargo; then
    pp_error "cargo not installed. Install Rust with rustup or run scripts/bootstrap-tools.sh."
    exit 127
  fi

  if have rustup; then
    rustup component add rustfmt clippy >/dev/null 2>&1 || warn "Could not ensure rustfmt/clippy components via rustup"
  fi

  run_cmd cargo fmt --all -- --check
  run_cmd cargo clippy --workspace --all-targets --all-features -- -D warnings
  run_cmd cargo test --workspace --all-features
  run_cmd cargo test --workspace --doc --all-features
  run_cmd cargo build --workspace --all-features
  run_cmd cargo build --workspace --all-features --release

  if have cargo-machete; then
    pp_section "Unused dependency check"
    run_cmd cargo machete
  else
    warn "cargo-machete not installed; skipping unused dependency check"
  fi

  if have cargo-audit; then
    pp_section "Advisory audit"
    run_cmd cargo audit
  else
    warn "cargo-audit not installed; skipping advisory audit"
  fi

  if [[ -f deny.toml ]] || [[ -f cargo-deny.toml ]]; then
    if have cargo-deny; then
      pp_section "Dependency policy"
      run_cmd cargo deny check
    else
      warn "cargo-deny config exists but cargo-deny is not installed"
    fi
  fi

  if [[ "${MISSIVE_AGGRESSIVE_TESTS:-0}" == "1" ]]; then
    pp_section "Aggressive Rust checks"

    if have cargo-nextest; then
      run_cmd cargo nextest run --workspace --all-features
    else
      warn "cargo-nextest not installed; skipping nextest"
    fi

    if have cargo-llvm-cov; then
      run_cmd cargo llvm-cov --workspace --all-features --no-report
    else
      warn "cargo-llvm-cov not installed; skipping coverage smoke"
    fi

    if have cargo-mutants; then
      run_cmd cargo mutants --workspace --timeout 60 --jobs 1 --no-shuffle || warn "cargo-mutants found surviving mutants or timed out; inspect output"
    else
      warn "cargo-mutants not installed; skipping mutation smoke"
    fi

    if have cargo-fuzz && [[ -d fuzz ]]; then
      while IFS= read -r target; do
        [[ -n "$target" ]] || continue
        run_cmd cargo fuzz run "$target" -- -max_total_time=15
      done < <(cargo fuzz list 2>/dev/null || true)
    else
      warn "cargo-fuzz/fuzz targets not available; skipping fuzz smoke"
    fi

    if have cargo && rustup component list --installed 2>/dev/null | grep -q '^miri'; then
      run_cmd cargo miri test --workspace || warn "miri failed; inspect unsupported dependencies or UB findings"
    else
      warn "miri not installed; skipping miri"
    fi
  fi
else
  pp_info "No Cargo.toml yet; skipping Rust checks until ticket 000/001 creates the workspace."
fi

if [[ -f justfile ]] && have just; then
  pp_section "just ci"
  if grep -Eq '^[[:space:]]*ci:' justfile; then
    run_cmd just ci
  fi
fi

if [[ -f docker-compose.yml ]] || [[ -f compose.yml ]]; then
  pp_section "Docker Compose validation"
  if have docker; then
    [[ ! -f docker-compose.yml ]] || run_cmd docker compose -f docker-compose.yml config >/dev/null
    [[ ! -f compose.yml ]] || run_cmd docker compose -f compose.yml config >/dev/null
  else
    warn "docker not installed; skipping Docker Compose validation"
  fi
fi

if [[ -f package.json ]]; then
  pp_section "Node project adjunct"
  if have npm; then
    if [[ -f package-lock.json ]]; then run_cmd npm ci; else run_cmd npm install; fi
    run_cmd npm run lint --if-present
    run_cmd npm run typecheck --if-present
    run_cmd npm test --if-present
    run_cmd npm run build --if-present
  else
    warn "npm not installed; skipping Node checks"
  fi
fi

pp_section "Summary"
pp_success "Quality gate passed."
