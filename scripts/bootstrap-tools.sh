#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/pretty-print.sh
source "$SCRIPT_DIR/lib/pretty-print.sh"

have() { command -v "$1" >/dev/null 2>&1; }
run() { pp_cmd "$*"; "$@"; }

pp_banner "missive build-tool bootstrap"

if ! have rustup; then
  pp_warn "rustup is not installed. Install Rust from https://rustup.rs/ or with your system package manager."
else
  run rustup toolchain install stable
  run rustup default stable
  run rustup component add rustfmt clippy
  run rustup component add miri || pp_warn "miri unavailable for this toolchain/target"
fi

if have cargo; then
  pp_section "Cargo tools"
  for tool in \
    cargo-nextest \
    cargo-llvm-cov \
    cargo-audit \
    cargo-deny \
    cargo-machete \
    cargo-mutants \
    cargo-fuzz \
    sqlx-cli \
    cargo-dist; do
    if ! have "$tool"; then
      run cargo install "$tool" || pp_warn "Failed to install $tool; continue and let quality gate skip or report it"
    else
      pp_info "$tool already installed"
    fi
  done
fi

if ! have jq; then
  if have apt-get; then
    run sudo apt-get update
    run sudo apt-get install -y jq
  elif have brew; then
    run brew install jq
  else
    pp_warn "jq not installed and no known package manager found"
  fi
fi

if ! have gh; then
  pp_warn "GitHub CLI gh is not installed. Install it if you want scripts/create-github-issues.sh."
fi

if ! have docker; then
  pp_warn "docker is not installed. Install it if Docker integration/devcontainer tests are needed."
fi

pp_success "Bootstrap completed best-effort."
