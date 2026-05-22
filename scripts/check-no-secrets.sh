#!/usr/bin/env bash
set -euo pipefail

# Lightweight guardrail. This does not replace a dedicated scanner such as gitleaks.
patterns=(
  'AKIA[0-9A-Z]{16}'
  '-----BEGIN (RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----'
  'xox[baprs]-[0-9A-Za-z-]+'
  'gh[pousr]_[0-9A-Za-z_]{30,}'
  'sk-[A-Za-z0-9]{20,}'
  'OPENAI_API_KEY[[:space:]]*='
  'ANTHROPIC_API_KEY[[:space:]]*='
  'GOOGLE_API_KEY[[:space:]]*='
)

exclude=':(exclude).git/* :(exclude)target/* :(exclude).agent/* :(exclude).missive/* :(exclude)coverage/* :(exclude)dist/*'

failed=0
for pattern in "${patterns[@]}"; do
  if git grep -n -E "$pattern" -- . $exclude >/tmp/missive-secret-scan.$$ 2>/dev/null; then
    echo "Potential secret pattern found: $pattern" >&2
    cat /tmp/missive-secret-scan.$$ >&2
    failed=1
  fi
done
rm -f /tmp/missive-secret-scan.$$

if [[ $failed -ne 0 ]]; then
  exit 1
fi
