#!/usr/bin/env bash
set -euo pipefail

bad_patterns=(
  '^target/'
  '^\.agent/'
  '^\.missive/'
  '^coverage/'
  '^dist/'
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
  'fuzz/artifacts/'
  'fuzz/crashes/'
)

failed=0
while IFS= read -r file; do
  for pattern in "${bad_patterns[@]}"; do
    if [[ "$file" =~ $pattern ]]; then
      echo "Generated/private/runtime file is tracked: $file" >&2
      failed=1
    fi
  done
done < <(git ls-files)

exit "$failed"
