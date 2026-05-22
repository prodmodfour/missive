#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

bad_patterns=(
  '^target/'
  '^\.agent/'
  '^\.missive/'
  '^coverage/'
  '^dist/'
  '^mutants\.out/'
  '^\.env$'
  '^\.env\.'
  '(^|/)\.DS_Store$'
  '(^|/)Thumbs\.db$'
  '\.sqlite$'
  '\.sqlite3$'
  '\.db$'
  '\.pid$'
  '\.sock$'
  '\.log$'
  '(^|/)id_rsa$'
  '(^|/)id_ed25519$'
  '\.pem$'
  '\.key$'
  'fuzz/artifacts/'
  'fuzz/crashes/'
)

failed=0
check_path() {
  local file="$1"
  local source="$2"
  local pattern

  for pattern in "${bad_patterns[@]}"; do
    if [[ "$file" =~ $pattern ]]; then
      echo "Generated/private/runtime file is present in $source: $file" >&2
      failed=1
    fi
  done
}

while IFS= read -r -d '' file; do
  check_path "$file" "tracked files"
done < <(git ls-files -z)

while IFS= read -r -d '' file; do
  check_path "$file" "untracked non-ignored files"
done < <(git ls-files --others --exclude-standard -z)

exit "$failed"
