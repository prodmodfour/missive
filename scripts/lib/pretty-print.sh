#!/usr/bin/env bash

if [[ -t 1 && "${NO_COLOR:-}" == "" ]]; then
  PP_BOLD=$'\033[1m'
  PP_DIM=$'\033[2m'
  PP_RESET=$'\033[0m'
else
  PP_BOLD=""
  PP_DIM=""
  PP_RESET=""
fi

pp_banner() {
  if [[ $# -gt 1 ]]; then
    printf '\n%s==> %s %s%s\n' "$PP_BOLD" "$1" "$2" "$PP_RESET"
  else
    printf '\n%s==> %s%s\n' "$PP_BOLD" "$1" "$PP_RESET"
  fi
}

pp_section() { printf '\n%s-- %s%s\n' "$PP_BOLD" "$*" "$PP_RESET"; }
pp_step() { printf ' • %s\n' "$*"; }
pp_info() { printf ' info: %s\n' "$*"; }
pp_warn() { printf ' warn: %s\n' "$*" >&2; }
pp_error() { printf ' error: %s\n' "$*" >&2; }
pp_hint() { printf ' hint: %s\n' "$*" >&2; }
pp_success() { printf ' ok: %s\n' "$*"; }
pp_cmd() { printf '%s$ %s%s\n' "$PP_DIM" "$*" "$PP_RESET"; }
pp_kv() { printf ' %-22s %s\n' "$1:" "$2"; }
pp_on_off() { if [[ "$1" == "1" ]]; then printf 'on'; else printf 'off'; fi; }
