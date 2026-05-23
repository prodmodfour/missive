#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

if [[ ! -d .github/workflows ]]; then
  echo "No GitHub Actions workflow directory found; skipping CI workflow validation."
  exit 0
fi

workflows=()
while IFS= read -r -d '' workflow; do
  workflows+=("$workflow")
done < <(find .github/workflows -type f \( -name '*.yml' -o -name '*.yaml' \) -print0 | sort -z)

if ((${#workflows[@]} == 0)); then
  echo "No GitHub Actions workflow files found; skipping CI workflow validation."
  exit 0
fi

if command -v actionlint >/dev/null 2>&1; then
  actionlint -color=false "${workflows[@]}"
  exit 0
fi

if command -v ruby >/dev/null 2>&1; then
  ruby -e '
    require "yaml"

    ARGV.each do |path|
      begin
        content = File.read(path)
        parsed = YAML.load(content, aliases: true)
        unless parsed.is_a?(Hash)
          warn "#{path}: expected a mapping at the top level"
          exit 1
        end
      rescue Psych::SyntaxError => error
        warn "#{path}: #{error.message}"
        exit 1
      end
    end
  ' "${workflows[@]}"
  echo "actionlint is not installed; validated workflow YAML syntax with Ruby only."
  exit 0
fi

if command -v python3 >/dev/null 2>&1; then
  python3 - "${workflows[@]}" <<'PY'
import sys

try:
    import yaml
except Exception:
    print(
        "actionlint, Ruby, and PyYAML are unavailable; skipping CI workflow validation.",
        file=sys.stderr,
    )
    sys.exit(0)

for path in sys.argv[1:]:
    with open(path, "r", encoding="utf-8") as handle:
        parsed = yaml.safe_load(handle)
    if not isinstance(parsed, dict):
        print(f"{path}: expected a mapping at the top level", file=sys.stderr)
        sys.exit(1)
PY
  echo "actionlint is not installed; validated workflow YAML syntax with PyYAML only."
  exit 0
fi

echo "actionlint, Ruby, and Python are unavailable; skipping CI workflow validation." >&2
