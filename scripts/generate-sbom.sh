#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
OUTPUT="$REPO_ROOT/dist/missive-sbom.cdx.json"
METADATA_PATH=""
TEMP_DIR=""

usage() {
  cat <<'USAGE'
Usage: scripts/generate-sbom.sh [OPTIONS]

Generate a CycloneDX JSON software bill of materials for the missive Cargo
workspace from `cargo metadata`. The generated SBOM is a release/build artifact
and should be written under dist/ or another ignored directory.

Options:
  --output PATH       Write the SBOM JSON to PATH (default: dist/missive-sbom.cdx.json)
  --metadata PATH     Reuse an existing `cargo metadata --format-version 1` JSON file
  -h, --help          Show this help
USAGE
}

cleanup() {
  if [[ -n "$TEMP_DIR" && -d "$TEMP_DIR" ]]; then
    rm -rf "$TEMP_DIR"
  fi
}
trap cleanup EXIT

while [[ $# -gt 0 ]]; do
  case "$1" in
    --output)
      if [[ $# -lt 2 ]]; then
        echo "--output requires a path" >&2
        exit 2
      fi
      OUTPUT="$2"
      shift
      ;;
    --metadata)
      if [[ $# -lt 2 ]]; then
        echo "--metadata requires a path" >&2
        exit 2
      fi
      METADATA_PATH="$2"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required to generate the metadata-derived SBOM" >&2
  exit 127
fi

cd "$REPO_ROOT"

if [[ -z "$METADATA_PATH" ]]; then
  if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo is required when --metadata is not provided" >&2
    exit 127
  fi
  TEMP_DIR="$(mktemp -d /tmp/missive-sbom.XXXXXX)"
  METADATA_PATH="$TEMP_DIR/cargo-metadata.json"
  cargo metadata --format-version 1 --all-features --locked > "$METADATA_PATH"
fi

mkdir -p "$(dirname "$OUTPUT")"

python3 - "$METADATA_PATH" "$OUTPUT" <<'PY'
from __future__ import annotations

import datetime as _dt
import json
import sys
import urllib.parse
import uuid
from pathlib import Path

metadata_path = Path(sys.argv[1])
output_path = Path(sys.argv[2])
metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
packages = {package["id"]: package for package in metadata.get("packages", [])}
workspace_members = set(metadata.get("workspace_members", []))


def package_ref(package: dict) -> str:
    name = urllib.parse.quote(package["name"], safe="._-")
    version = urllib.parse.quote(package["version"], safe="._-+")
    return f"pkg:cargo/{name}@{version}"


def normalize_license_expression(expression: str) -> str:
    # Some older crates still use Cargo's deprecated slash separator. CycloneDX
    # consumers expect SPDX expressions, so normalize the common Cargo form.
    return expression.replace(" / ", " OR ").replace("/", " OR ")


def package_component(package: dict) -> dict:
    component = {
        "type": "application" if package["id"] in workspace_members else "library",
        "bom-ref": package_ref(package),
        "name": package["name"],
        "version": package["version"],
    }
    if package.get("source"):
        component["purl"] = package_ref(package)
    if package.get("license"):
        component["licenses"] = [
            {"expression": normalize_license_expression(package["license"])}
        ]
    elif package.get("license_file"):
        component["licenses"] = [
            {"license": {"name": f"file:{package['license_file']}"}}
        ]
    if package.get("description"):
        component["description"] = package["description"]
    return component


components = [
    package_component(package)
    for package in sorted(
        packages.values(), key=lambda package: (package["name"], package["version"])
    )
]

resolve = metadata.get("resolve") or {"nodes": []}
dependencies = []
for node in sorted(resolve.get("nodes", []), key=lambda item: package_ref(packages[item["id"]])):
    package = packages.get(node["id"])
    if not package:
        continue
    depends_on = sorted(
        {
            package_ref(packages[dep_id])
            for dep_id in node.get("dependencies", [])
            if dep_id in packages
        }
    )
    dependencies.append({"ref": package_ref(package), "dependsOn": depends_on})

root_package = next(
    (packages[pkg_id] for pkg_id in workspace_members if packages[pkg_id]["name"] == "missive-cli"),
    next((packages[pkg_id] for pkg_id in workspace_members), None),
)
metadata_component = {
    "type": "application",
    "name": "missive",
    "version": root_package["version"] if root_package else metadata.get("version", "0"),
}

timestamp = _dt.datetime.now(tz=_dt.timezone.utc).replace(microsecond=0).isoformat()
if timestamp.endswith("+00:00"):
    timestamp = timestamp[:-6] + "Z"

bom = {
    "bomFormat": "CycloneDX",
    "specVersion": "1.5",
    "serialNumber": f"urn:uuid:{uuid.uuid4()}",
    "version": 1,
    "metadata": {
        "timestamp": timestamp,
        "tools": {
            "components": [
                {
                    "type": "application",
                    "name": "scripts/generate-sbom.sh",
                    "version": "0.1.0",
                    "description": "missive metadata-derived SBOM generator",
                }
            ]
        },
        "component": metadata_component,
    },
    "components": components,
    "dependencies": dependencies,
}

output_path.write_text(json.dumps(bom, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY

echo "Wrote CycloneDX SBOM to $OUTPUT"
