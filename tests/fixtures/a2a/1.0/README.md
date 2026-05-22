# A2A 1.0 conformance fixtures

These fixtures are deterministic JSON examples for missive's A2A compatibility tests.
They are versioned by directory name: `tests/fixtures/a2a/1.0/` corresponds to the
A2A protocol major/minor version `1.0` (`A2A-Version: 1.0`).

Sources and adaptation notes:

* The fixture shapes are based on the A2A 1.0 specification examples from
  <https://a2a-protocol.org/v1.0.0/specification> and the normative data model
  described there.
* Where prose examples omit required IDs or include placeholder comments, these
  files replace them with deterministic fixture IDs and valid JSON.
* Push notification credentials are deliberately redacted fixture strings. Do not
  add real tokens, private URLs, or captured external traffic.
* `cli/*.json` files are normalized golden outputs from the missive CLI. Dynamic
  local ports, generated message IDs, and timestamps are replaced with stable
  placeholders before comparison.

Update process for future protocol versions:

1. Create a new sibling directory named for the A2A major/minor version, for
   example `tests/fixtures/a2a/1.1/`.
2. Copy the closest previous fixture set and update examples from the released
   specification and upstream SDK wire format.
3. Add or adjust `crates/missive-a2a/tests/protocol_fixtures.rs` cases so every
   new fixture either round-trips through an official protocol type or is
   intentionally validated as a binding-specific JSON value.
4. Update the CLI golden output tests when public output changes, keeping
   generated values normalized rather than committed literally.
5. Run `cargo test -p missive-a2a --test protocol_fixtures --all-features`,
   `cargo test -p missive-cli --test a2a_conformance_fixtures --all-features`,
   and then `scripts/quality-gate.sh`.
