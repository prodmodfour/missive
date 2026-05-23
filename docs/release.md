# Release packaging

missive uses a small local packaging script as the cargo-dist-equivalent release
path for the current phase. It builds the `missive` binary from the
`missive-cli` package with the workspace `dist` profile, creates one archive per
Rust target, and writes SHA-256 checksum files next to the archives. The script
never publishes artifacts by itself.

## Release profile

The workspace defines two optimized profiles in `Cargo.toml`:

* `release` — optimized workspace release builds used by the quality gate.
* `dist` — the packaging profile used by `scripts/release-package.sh`; it
  inherits from `release`, enables thin LTO, uses one codegen unit, and strips
  symbols where the platform supports it.

The binary inside every archive is named `missive` (`missive.exe` on Windows).

## Target plan

The release metadata and GitHub Actions packaging workflow cover these common
end-user targets:

| Target | Runner used by `.github/workflows/release.yml` |
| --- | --- |
| `x86_64-unknown-linux-gnu` | `ubuntu-latest` |
| `x86_64-apple-darwin` | `macos-latest` |
| `aarch64-apple-darwin` | `macos-latest` |
| `x86_64-pc-windows-msvc` | `windows-latest` |

Linux arm64 and musl/static builds are intentionally deferred until a later
packaging hardening pass can add matching runners/linkers and compatibility
checks.

## Local dry run

Run a host-target packaging dry run from the repository root:

```bash
scripts/release-package.sh --dry-run
```

To choose a target explicitly:

```bash
scripts/release-package.sh --dry-run --target x86_64-unknown-linux-gnu
```

The script writes artifacts to `dist/` by default. That directory is ignored and
must not be committed. A successful dry run produces files like:

```text
dist/missive-v0.1.0-x86_64-unknown-linux-gnu.tar.gz
dist/missive-v0.1.0-x86_64-unknown-linux-gnu.tar.gz.sha256
dist/SHA256SUMS
```

Each archive contains:

* the `missive` binary for the target;
* `README.md`, `LICENSE`, and `CHANGELOG.md`;
* a generated `INSTALL.md` reminder with checksum and PATH guidance.

## Checksum verification

Verify an archive with its adjacent checksum file:

```bash
sha256sum --check dist/missive-v0.1.0-x86_64-unknown-linux-gnu.tar.gz.sha256
```

On platforms without `sha256sum`, use:

```bash
shasum -a 256 dist/missive-v0.1.0-x86_64-unknown-linux-gnu.tar.gz
```

Compare the printed hash with the matching line in `SHA256SUMS`.

## Installing from an archive

The repository includes a local installer helper for reviewed release archives:

```bash
scripts/install-release.sh \
  --artifact dist/missive-v0.1.0-x86_64-unknown-linux-gnu.tar.gz \
  --checksum dist/missive-v0.1.0-x86_64-unknown-linux-gnu.tar.gz.sha256 \
  --bin-dir ~/.local/bin
```

The helper verifies the checksum when provided, extracts exactly one
`missive`/`missive.exe` binary, and copies it to the chosen bin directory. It
does not download files, modify shell startup files, or edit `PATH`.

Confirm the installed binary:

```bash
missive --version
missive doctor
```

To update, download or build the newer archive, verify its checksum, and rerun
`scripts/install-release.sh` with the new artifact. Runtime state lives outside
the installed binary and is not migrated by the installer.

## CI dry-run workflow

`.github/workflows/release.yml` runs on release tags matching `v*.*.*` and on
manual `workflow_dispatch`. It builds the common target matrix, runs
`scripts/release-package.sh --dry-run`, and uploads the archives plus checksum
files as workflow artifacts. It uses read-only repository permissions and does
not create GitHub Releases or require repository secrets.

Publishing a public release remains a manual maintainer step for now:

1. run `scripts/quality-gate.sh` on the release commit;
2. run the release packaging workflow or local target builds;
3. inspect checksums and archive contents;
4. attach the archives, `.sha256` files, and `SHA256SUMS` to the release notes.

## Limitations

This is packaging, not a production deployment system. It does not yet create
Homebrew formulas, Debian/RPM packages, MSIs, signed/notarized macOS artifacts,
Windows code-signed binaries, SBOMs, or provenance attestations. Supply-chain
policy and SBOM work are tracked by the next ticket.
