# Supply-chain policy

missive uses local, reproducible Rust dependency checks before release. The
policy is intentionally conservative for advisories, crate sources, and duplicate
versions, while allowing the permissive licenses currently present in the Cargo
workspace graph.

## Default checks

The policy file is [`../deny.toml`](../deny.toml). When `cargo-deny` is installed,
`scripts/quality-gate.sh` runs:

```bash
cargo deny --locked check
```

The GitHub Actions Linux quality-gate job installs a pinned `cargo-deny` version
so CI enforces the same policy. Local developers can install or verify optional
tools with:

```bash
scripts/bootstrap-tools.sh --check
scripts/bootstrap-tools.sh
```

Useful direct commands:

```bash
cargo deny --locked check
cargo audit
cargo machete
cargo tree -d --all-features
```

`cargo-audit` and `cargo-machete` remain optional local gate checks: they are run
when installed and skipped with a warning when absent. `cargo-deny` is the source
of truth for the reviewed license/source/advisory/duplicate-version policy.

## License policy

Allowed dependency licenses are listed in `deny.toml` as SPDX identifiers:

* `Apache-2.0` and `Apache-2.0 WITH LLVM-exception`
* `MIT`, `ISC`, `BSD-2-Clause`, `BSD-3-Clause`, `BSL-1.0`, `Zlib`, and
  `Unlicense`
* `Unicode-3.0` for Unicode/ICU data crates used by URL/IDNA handling
* `CDLA-Permissive-2.0` for trusted root data crates

Workspace crates are not ignored by the license check; their license comes from
`workspace.package.license` and should remain `Apache-2.0` unless an ADR and
maintainer review change the project license.

When a new dependency introduces a new license:

1. prefer a dependency with an already allowed permissive license;
2. review the license text and obligations;
3. document the reason in this file before adding it to `deny.toml`;
4. rerun `cargo deny --locked check` and the full quality gate.

Do not add strong copyleft, network-copyleft, commercial, source-available, or
unknown license terms without an explicit project decision.

## Advisory and source policy

`cargo-deny` uses the RustSec advisory database and denies yanked crates. The
current policy has no advisory ignores. If an advisory is ignored in the future,
the `ignore` entry in `deny.toml` must include a reason, a risk assessment, and a
planned removal condition in this document or a linked issue.

Allowed crate sources are limited to the crates.io registry. Unknown registries
and unknown Git dependencies are denied. If a future change pins a Git dependency
(such as an upstream A2A SDK revision), add the exact repository URL to
`allow-git`, document the pin/update process, and keep credentials out of Cargo
configuration and CI.

## Duplicate dependency policy

`deny.toml` denies duplicate crate versions by default. Existing duplicate-version
exceptions are reviewed transitive dependencies where missive cannot force a
single version without forking or prematurely replacing upstream crates:

| Crate line | Reason |
| --- | --- |
| `getrandom 0.2` | Pulled by `ring`/`rustls` while newer `getrandom` is used by `tempfile` and `a2a-lf`. |
| `hashbrown 0.14` | Pulled by `rusqlite`/`hashlink` while `toml`/`indexmap` use a newer line. |
| `unicode-width 0.1` | Pulled by `miette` while `textwrap` uses `unicode-width 0.2`. |
| `windows-sys 0.52`/`0.60` and `windows-targets 0.52` target crates | Transitive Windows support split across `ring`, `keyring`, `clap`, and Tokio-era crates. |
| `wit-bindgen 0.46` | Transitive WASI support through `getrandom`/`proptest` while a newer line is also present. |

When updating dependencies, run both `cargo deny --locked check` and
`cargo tree -d --all-features`. Remove a skip from `deny.toml` as soon as the
older transitive line disappears. New duplicate skips require a reason in
`deny.toml` and a short note in this document.

## Dependency update workflow

For routine dependency updates:

```bash
cargo update
cargo deny --locked check
cargo audit
cargo machete
scripts/quality-gate.sh
```

For a targeted update:

```bash
cargo update -p crate-name
cargo tree -i crate-name --all-features
cargo deny --locked check
scripts/quality-gate.sh
```

Before committing dependency changes:

* inspect `Cargo.lock` for unexpected large transitive changes;
* confirm no new registries, Git URLs, or duplicate skips were introduced without
  documentation;
* confirm no advisories are ignored without a removal plan;
* update release, security, or protocol docs if a dependency materially changes
  TLS, storage, auth, protocol, or packaging behaviour.

## SBOM generation

A metadata-derived CycloneDX JSON SBOM can be generated locally without contacting
third-party services:

```bash
scripts/generate-sbom.sh --output dist/missive-sbom.cdx.json
```

The script uses `cargo metadata --all-features --locked`, records workspace and
transitive Cargo packages, includes license expressions where available, and
writes dependency relationships. `dist/`, `sbom/`, `*.cdx.json`, `*.spdx.json`,
and default `bom.json`/`bom.xml` outputs are ignored or guarded as generated
artifacts; do not commit generated SBOMs unless a future release process
explicitly asks for reviewed artifacts.

Current SBOM limitations:

* it describes the Cargo workspace dependency graph, not OS packages, container
  base images, release workflow actions, or binary provenance;
* it is not a signed attestation;
* release archives do not yet bundle SBOMs by default.

For release hardening, generate an SBOM alongside checksums, inspect it with a
CycloneDX-compatible tool if available, and attach it as a release artifact rather
than committing it to the source tree.
