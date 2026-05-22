#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=scripts/lib/pretty-print.sh
source "$SCRIPT_DIR/lib/pretty-print.sh"

have() { command -v "$1" >/dev/null 2>&1; }

CHECK_ONLY=0
INSTALL_RUSTUP="${MISSIVE_BOOTSTRAP_RUSTUP:-1}"
INSTALL_CARGO_TOOLS="${MISSIVE_BOOTSTRAP_CARGO_TOOLS:-1}"
INSTALL_SYSTEM_DEPS="${MISSIVE_BOOTSTRAP_SYSTEM_DEPS:-0}"
INSTALL_DOCKER="${MISSIVE_BOOTSTRAP_DOCKER:-0}"
CARGO_INSTALL_LOCKED="${MISSIVE_BOOTSTRAP_CARGO_INSTALL_LOCKED:-1}"
APT_UPDATED=0

usage() {
  cat <<'USAGE'
Usage: scripts/bootstrap-tools.sh [OPTIONS]

Install or verify local tooling used by the missive autonomous build loop.
The script is best-effort and idempotent: it skips tools already available and
continues after optional-tool installation failures with a warning.

Options:
  --check              Report missing tools without installing anything
  --no-rustup          Skip rustup toolchain/component installation
  --no-cargo-tools     Skip optional cargo tool installation
  --system-deps        Install supported OS packages such as jq/protoc/sqlite
  --docker             Include best-effort Docker installation where supported
  -h, --help           Show this help

Environment toggles:
  MISSIVE_BOOTSTRAP_RUSTUP=0              same as --no-rustup
  MISSIVE_BOOTSTRAP_CARGO_TOOLS=0         same as --no-cargo-tools
  MISSIVE_BOOTSTRAP_SYSTEM_DEPS=1         same as --system-deps
  MISSIVE_BOOTSTRAP_DOCKER=1              same as --docker
  MISSIVE_BOOTSTRAP_CARGO_INSTALL_LOCKED=0  omit cargo install --locked
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --check)
      CHECK_ONLY=1
      ;;
    --no-rustup)
      INSTALL_RUSTUP=0
      ;;
    --no-cargo-tools)
      INSTALL_CARGO_TOOLS=0
      ;;
    --system-deps)
      INSTALL_SYSTEM_DEPS=1
      ;;
    --docker)
      INSTALL_DOCKER=1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      pp_error "Unknown option: $1"
      usage >&2
      exit 2
      ;;
  esac
  shift
done

try_run() {
  if [[ "$CHECK_ONLY" == "1" ]]; then
    pp_cmd "[check] $*"
    return 0
  fi

  pp_cmd "$*"
  "$@"
}

ensure_rustup_component() {
  local component="$1"
  if rustup component list --installed 2>/dev/null | grep -q "^${component}\\b"; then
    pp_info "rustup component $component already installed"
    return 0
  fi

  if ! try_run rustup component add "$component"; then
    pp_warn "Could not install rustup component $component; continuing"
  fi
}

install_rust_toolchain() {
  pp_section "Rust toolchain"

  if ! have rustup; then
    pp_warn "rustup is not installed; install Rust from https://rustup.rs/ or with a package manager"
    return 0
  fi

  if ! try_run rustup toolchain install stable --profile default --component rustfmt --component clippy; then
    pp_warn "Could not install or update the stable Rust toolchain; continuing with the current toolchain"
  fi
  ensure_rustup_component rustfmt
  ensure_rustup_component clippy
  ensure_rustup_component llvm-tools-preview

  # Miri is useful for aggressive validation but is not always published for the
  # active stable toolchain. Keep it best-effort and never fail bootstrap.
  ensure_rustup_component miri
}

cargo_install_args() {
  if [[ "$CARGO_INSTALL_LOCKED" == "1" ]]; then
    printf '%s\0' --locked
  fi
}

ensure_cargo_tool() {
  local command_name="$1"
  local package_name="$2"
  local extra_args="$3"

  if have "$command_name"; then
    pp_info "$command_name already installed"
    return 0
  fi

  if ! have cargo; then
    pp_warn "cargo is not installed; cannot install $package_name"
    return 0
  fi

  if [[ "$CHECK_ONLY" == "1" ]]; then
    if [[ -n "$extra_args" ]]; then
      pp_warn "$command_name missing (install with: cargo install $package_name $extra_args)"
    else
      pp_warn "$command_name missing (install with: cargo install $package_name)"
    fi
    return 0
  fi

  local args=()
  while IFS= read -r -d '' arg; do
    args+=("$arg")
  done < <(cargo_install_args)

  if [[ -n "$extra_args" ]]; then
    # extra_args is maintained by this script and intentionally split into cargo
    # CLI words such as: --no-default-features --features rustls,sqlite
    read -r -a split_extra_args <<< "$extra_args"
    args+=("${split_extra_args[@]}")
  fi

  if ! try_run cargo install "${args[@]}" "$package_name"; then
    pp_warn "Failed to install $package_name; quality gate will skip it unless required later"
  fi
}

install_cargo_tools() {
  pp_section "Cargo tools"

  local tools=(
    "cargo-nextest|cargo-nextest|"
    "cargo-llvm-cov|cargo-llvm-cov|"
    "cargo-deny|cargo-deny|"
    "cargo-audit|cargo-audit|"
    "cargo-machete|cargo-machete|"
    "cargo-mutants|cargo-mutants|"
    "cargo-fuzz|cargo-fuzz|"
    "sqlx|sqlx-cli|--no-default-features --features rustls,sqlite"
    "just|just|"
    "cargo-dist|cargo-dist|"
  )

  local spec command_name package_name extra_args
  for spec in "${tools[@]}"; do
    IFS='|' read -r command_name package_name extra_args <<< "$spec"
    ensure_cargo_tool "$command_name" "$package_name" "$extra_args"
  done
}

apt_install() {
  local package_name="$1"

  if [[ "$CHECK_ONLY" == "1" ]]; then
    pp_cmd "[check] sudo apt-get install -y $package_name"
    return 0
  fi

  if [[ "$APT_UPDATED" == "0" ]]; then
    try_run sudo apt-get update || {
      pp_warn "apt-get update failed; cannot install $package_name"
      return 1
    }
    APT_UPDATED=1
  fi

  try_run sudo apt-get install -y "$package_name"
}

brew_install() {
  local package_name="$1"
  try_run brew install "$package_name"
}

install_system_package() {
  local command_name="$1"
  local apt_package="$2"
  local brew_package="$3"

  if have "$command_name"; then
    pp_info "$command_name already installed"
    return 0
  fi

  if [[ "$INSTALL_SYSTEM_DEPS" != "1" ]]; then
    pp_warn "$command_name missing; rerun with --system-deps to install it where supported"
    return 0
  fi

  if have apt-get && [[ -n "$apt_package" ]]; then
    apt_install "$apt_package" || pp_warn "Failed to install $apt_package"
  elif have brew && [[ -n "$brew_package" ]]; then
    brew_install "$brew_package" || pp_warn "Failed to install $brew_package"
  else
    pp_warn "No supported package manager recipe for $command_name"
  fi
}

install_system_deps() {
  pp_section "System tools"
  install_system_package jq jq jq
  install_system_package shellcheck shellcheck shellcheck
  install_system_package protoc protobuf-compiler protobuf
  install_system_package sqlite3 sqlite3 sqlite
  install_system_package pkg-config pkg-config pkg-config
  install_system_package gh "" gh

  if [[ "$INSTALL_DOCKER" == "1" ]]; then
    install_system_package docker docker.io docker
  elif have docker; then
    pp_info "docker already installed"
  else
    pp_warn "docker missing; install manually or rerun with --docker if local Docker validation is needed"
  fi
}

pp_banner "missive build-tool bootstrap"
pp_kv "repository" "$REPO_ROOT"
pp_kv "check only" "$CHECK_ONLY"
pp_kv "rustup setup" "$INSTALL_RUSTUP"
pp_kv "cargo tools" "$INSTALL_CARGO_TOOLS"
pp_kv "system deps" "$INSTALL_SYSTEM_DEPS"
pp_kv "docker install" "$INSTALL_DOCKER"

if [[ "$INSTALL_RUSTUP" == "1" ]]; then
  install_rust_toolchain
else
  pp_info "Skipping rustup setup"
fi

if [[ "$INSTALL_CARGO_TOOLS" == "1" ]]; then
  install_cargo_tools
else
  pp_info "Skipping cargo tool installation"
fi

install_system_deps

pp_section "Summary"
if [[ "$CHECK_ONLY" == "1" ]]; then
  pp_success "Bootstrap check completed; missing optional tools were reported without installing anything."
else
  pp_success "Bootstrap completed best-effort; rerun safely at any time."
fi
