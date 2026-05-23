#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

usage() {
  cat <<'USAGE'
Usage: scripts/release-package.sh [OPTIONS]

Build and package the missive release binary for one or more Rust targets.
Artifacts are archives plus SHA-256 checksum files written under dist/ by
default. The script is local and deterministic; it does not publish anything.

Options:
  --dry-run             Label the run as a release dry run; artifacts are still written
  --dist-dir DIR        Write archives/checksums to DIR (default: ./dist)
  --target TRIPLE       Rust target triple to package (repeatable; default: host)
  --profile PROFILE     Cargo profile to build (default: dist)
  --version VERSION     Override the package version used in artifact names
  --no-locked           Omit cargo --locked during the release build
  -h, --help            Show this help

Examples:
  scripts/release-package.sh --dry-run
  scripts/release-package.sh --target x86_64-unknown-linux-gnu
  scripts/release-package.sh --dist-dir /tmp/missive-dist --target "$(rustc -vV | awk '/^host:/ {print $2}')"
USAGE
}

log() { printf 'release-package: %s\n' "$*" >&2; }
die() {
  printf 'release-package: error: %s\n' "$*" >&2
  exit 2
}
have() { command -v "$1" >/dev/null 2>&1; }

DRY_RUN=0
DIST_DIR="$REPO_ROOT/dist"
PROFILE="dist"
VERSION=""
LOCKED=1
TARGETS=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run)
      DRY_RUN=1
      ;;
    --dist-dir)
      [[ $# -ge 2 ]] || die "--dist-dir requires a directory"
      DIST_DIR="$2"
      shift
      ;;
    --target)
      [[ $# -ge 2 ]] || die "--target requires a Rust target triple"
      TARGETS+=("$2")
      shift
      ;;
    --profile)
      [[ $# -ge 2 ]] || die "--profile requires a Cargo profile name"
      PROFILE="$2"
      shift
      ;;
    --version)
      [[ $# -ge 2 ]] || die "--version requires a version value"
      VERSION="$2"
      shift
      ;;
    --no-locked)
      LOCKED=0
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

cd "$REPO_ROOT"

have cargo || die "cargo is required"
have rustc || die "rustc is required"
have tar || die "tar is required to create release archives"
if ! have sha256sum && ! have shasum; then
  die "sha256sum or shasum is required to generate checksums"
fi

if ((${#TARGETS[@]} == 0)); then
  host_target="$(rustc -vV | awk '/^host:/ {print $2}')"
  [[ -n "$host_target" ]] || die "could not determine the host Rust target"
  TARGETS+=("$host_target")
fi

if [[ -z "$VERSION" ]]; then
  VERSION="$(cargo pkgid -p missive-cli | sed 's/^.*#//')"
  [[ -n "$VERSION" ]] || die "could not determine missive-cli version"
fi

mkdir -p "$DIST_DIR"
DIST_DIR="$(cd "$DIST_DIR" && pwd)"
STAGING_DIR="$DIST_DIR/staging"
CHECKSUMS_FILE="$DIST_DIR/SHA256SUMS"
rm -rf "$STAGING_DIR"
rm -f "$CHECKSUMS_FILE"
mkdir -p "$STAGING_DIR"

if ((DRY_RUN == 1)); then
  log "running release dry run for version $VERSION"
else
  log "packaging release artifacts for version $VERSION"
fi
log "output directory: $DIST_DIR"

checksum_file() {
  local filename="$1"
  if have sha256sum; then
    sha256sum "$filename"
  else
    shasum -a 256 "$filename"
  fi
}

package_target() {
  local target="$1"
  local exe_suffix=""
  if [[ "$target" == *windows* ]]; then
    exe_suffix=".exe"
  fi

  local cargo_args=(build -p missive-cli --bin missive --profile "$PROFILE" --target "$target" --all-features)
  if ((LOCKED == 1)); then
    cargo_args+=(--locked)
  fi

  log "building missive for $target with cargo profile $PROFILE"
  cargo "${cargo_args[@]}"

  local binary_path="$REPO_ROOT/target/$target/$PROFILE/missive$exe_suffix"
  [[ -x "$binary_path" || -f "$binary_path" ]] || die "expected release binary is missing: $binary_path"

  local archive_base="missive-v${VERSION}-${target}"
  local stage_path="$STAGING_DIR/$archive_base"
  local archive_name="${archive_base}.tar.gz"
  local archive_path="$DIST_DIR/$archive_name"

  rm -rf "$stage_path"
  mkdir -p "$stage_path"
  cp "$binary_path" "$stage_path/missive$exe_suffix"
  chmod 0755 "$stage_path/missive$exe_suffix" 2>/dev/null || true
  cp README.md LICENSE CHANGELOG.md "$stage_path/"
  cat >"$stage_path/INSTALL.md" <<EOF
# Installing missive $VERSION for $target

This archive was produced by scripts/release-package.sh.

1. Verify the archive with the adjacent .sha256 file or SHA256SUMS manifest.
2. Copy missive$exe_suffix to a directory on PATH, for example ~/.local/bin on Unix-like systems.
3. Run \`missive --version\` and \`missive doctor\` after installation.

Runtime state is created outside the repository by default. Set MISSIVE_HOME only
when you intentionally want an isolated state directory.
EOF

  tar -czf "$archive_path" -C "$STAGING_DIR" "$archive_base"
  (
    cd "$DIST_DIR"
    checksum_file "$archive_name" | tee "$archive_name.sha256" >>"$CHECKSUMS_FILE"
  ) >/dev/null
  log "created $archive_path"
  log "created $archive_path.sha256"
}

for target in "${TARGETS[@]}"; do
  package_target "$target"
done

rm -rf "$STAGING_DIR"
log "created $CHECKSUMS_FILE"
log "release packaging complete"
