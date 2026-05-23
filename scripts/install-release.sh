#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/install-release.sh --artifact PATH [OPTIONS]

Install a missive release archive produced by scripts/release-package.sh.
The script can verify the archive checksum before copying the missive binary to
an installation directory on PATH. It does not download artifacts or modify PATH.

Options:
  --artifact PATH       Release .tar.gz archive to install (required)
  --checksum PATH       Adjacent .sha256 or SHA256SUMS file to verify first
  --bin-dir DIR         Installation directory (default: ~/.local/bin)
  --verify-only         Verify checksum and archive contents without installing
  -h, --help            Show this help

Examples:
  scripts/install-release.sh --artifact dist/missive-v0.1.0-x86_64-unknown-linux-gnu.tar.gz --checksum dist/missive-v0.1.0-x86_64-unknown-linux-gnu.tar.gz.sha256
  scripts/install-release.sh --artifact ./missive-v0.1.0-x86_64-unknown-linux-gnu.tar.gz --bin-dir /usr/local/bin
USAGE
}

log() { printf 'install-release: %s\n' "$*" >&2; }
die() {
  printf 'install-release: error: %s\n' "$*" >&2
  exit 2
}
have() { command -v "$1" >/dev/null 2>&1; }

ARTIFACT=""
CHECKSUM=""
BIN_DIR="${HOME:-}/.local/bin"
VERIFY_ONLY=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --artifact)
      [[ $# -ge 2 ]] || die "--artifact requires a path"
      ARTIFACT="$2"
      shift
      ;;
    --checksum)
      [[ $# -ge 2 ]] || die "--checksum requires a path"
      CHECKSUM="$2"
      shift
      ;;
    --bin-dir)
      [[ $# -ge 2 ]] || die "--bin-dir requires a directory"
      BIN_DIR="$2"
      shift
      ;;
    --verify-only)
      VERIFY_ONLY=1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "unknown option: $1"
      ;;
  esac
  shift
done

[[ -n "$ARTIFACT" ]] || die "--artifact is required"
[[ -f "$ARTIFACT" ]] || die "artifact not found: $ARTIFACT"
have tar || die "tar is required to inspect/install release archives"

artifact_dir="$(cd "$(dirname "$ARTIFACT")" && pwd)"
artifact_name="$(basename "$ARTIFACT")"
artifact_path="$artifact_dir/$artifact_name"

verify_checksum() {
  local checksum_path="$1"
  local expected actual
  [[ -f "$checksum_path" ]] || die "checksum file not found: $checksum_path"

  expected="$(awk -v name="$artifact_name" '$2 == name {print $1; found=1; exit} END {if (!found) exit 1}' "$checksum_path")" \
    || die "checksum file does not contain an entry for $artifact_name"
  if have sha256sum; then
    actual="$(sha256sum "$artifact_path" | awk '{print $1}')"
  elif have shasum; then
    actual="$(shasum -a 256 "$artifact_path" | awk '{print $1}')"
  else
    die "sha256sum or shasum is required to verify checksums"
  fi

  [[ "$expected" == "$actual" ]] || die "checksum mismatch for $artifact_name"
  log "checksum verified for $artifact_name"
}

if [[ -n "$CHECKSUM" ]]; then
  verify_checksum "$CHECKSUM"
fi

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/missive-install.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT

tar -xzf "$artifact_path" -C "$tmp_dir"
binaries=()
while IFS= read -r binary; do
  binaries+=("$binary")
done < <(find "$tmp_dir" -type f \( -name missive -o -name missive.exe \) | sort)
((${#binaries[@]} == 1)) || die "expected exactly one missive binary in archive; found ${#binaries[@]}"

if ((VERIFY_ONLY == 1)); then
  log "archive contents verified: ${binaries[0]}"
  exit 0
fi

[[ -n "$BIN_DIR" ]] || die "installation directory is empty"
mkdir -p "$BIN_DIR"
install_name="missive"
if [[ "${binaries[0]}" == *.exe ]]; then
  install_name="missive.exe"
fi
cp "${binaries[0]}" "$BIN_DIR/$install_name"
chmod 0755 "$BIN_DIR/$install_name" 2>/dev/null || true
log "installed $install_name to $BIN_DIR"
log "run '$BIN_DIR/$install_name --version' to verify the installed binary"
