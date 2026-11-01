# Native candidate Cargo tests

The Python audit wrapper and its compatibility test target are deleted. Native
Rust tests now exercise lock ownership, RustSec matching, registry/checksum
coverage, bounded evidence loading, freshness, cancellation and result mapping.
The Forge CLI tests execute vulnerable, yanked and clean fixtures with an empty
`PATH`, no analyzer program grants and no cgroup delegation. They verify staged
source selection, ignored candidate audit configuration, evidence refusals,
no-file skips and owned cleanup.

The [audit contract](native-cargo-audit.md) defines the operator-supplied evidence
format and its trust boundary. Fixture digests and timestamps do not constitute
upstream authenticity or completeness evidence.

Run the focused native tests after installing the CLI:

```sh
cargo +1.98.0 test --release --locked --manifest-path native/conductor-native/Cargo.toml \
  --features bundled-sqlite candidate_cargo_
FORGE_TEST_BIN=/absolute/installed/bin/forge cargo +1.98.0 test --release --locked \
  --manifest-path native/forge/Cargo.toml --test native_candidate_cargo_audit_cli
```

The remaining cargo-lint adapter still handles roster and crate selection and
external fmt/clippy execution. Its environment recovery is now a metadata-only
call to native code. `python_contracts_candidate_cargo_lint` retains roster,
prerequisite, selection and execution coverage and adds a real native recovery
contract. `python_contracts_candidate_policy` exercises the remaining audit
dispatch seam against the actual native evaluator and verifies digest refusal.
These explicit compatibility targets require a freshly built Python binding;
they are separate from the native gate and remain until their callers retire.
