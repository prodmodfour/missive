#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# Lightweight guardrail. This does not replace a dedicated scanner such as
# gitleaks, but it intentionally fails fast on common committed or untracked
# secret forms before they enter a repository commit.
patterns=(
  'AWS access key::AKIA[0-9A-Z]{16}'
  'Private key block::-----BEGIN (RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----'
  'Slack token::xox[baprs]-[0-9A-Za-z-]+'
  'GitHub token::gh[pousr]_[0-9A-Za-z_]{30,}'
  'OpenAI-style token::sk-[A-Za-z0-9]{20,}'
  'OpenAI API key assignment::OPENAI_API_KEY[[:space:]]*='
  'Anthropic API key assignment::ANTHROPIC_API_KEY[[:space:]]*='
  'Google API key assignment::GOOGLE_API_KEY[[:space:]]*='
)

pathspec_excludes=(
  ':(exclude).git/*'
  ':(exclude)target/*'
  ':(exclude).agent/*'
  ':(exclude).missive/*'
  ':(exclude)coverage/*'
  ':(exclude)dist/*'
  ':(exclude)fuzz/artifacts/*'
  ':(exclude)fuzz/crashes/*'
)

tmp_file="$(mktemp "${TMPDIR:-/tmp}/missive-secret-scan.XXXXXX")"
trap 'rm -f "$tmp_file"' EXIT

failed=0
scan_tracked() {
  local label="$1"
  local pattern="$2"

  if git grep -n -I -E "$pattern" -- . "${pathspec_excludes[@]}" >"$tmp_file" 2>/dev/null; then
    echo "Potential secret pattern found in tracked files: $label" >&2
    cat "$tmp_file" >&2
    failed=1
  fi
}

scan_untracked() {
  local label="$1"
  local pattern="$2"
  local file
  local matches=0

  : >"$tmp_file"
  while IFS= read -r -d '' file; do
    if grep -n -I -E "$pattern" -- "$file" >>"$tmp_file" 2>/dev/null; then
      matches=1
    fi
  done < <(git ls-files --others --exclude-standard -z)

  if [[ "$matches" == "1" ]]; then
    echo "Potential secret pattern found in untracked files: $label" >&2
    cat "$tmp_file" >&2
    failed=1
  fi
}

for entry in "${patterns[@]}"; do
  label="${entry%%::*}"
  pattern="${entry#*::}"
  scan_tracked "$label" "$pattern"
  scan_untracked "$label" "$pattern"
done

exit "$failed"
