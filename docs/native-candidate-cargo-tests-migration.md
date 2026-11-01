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

Native Cargo lint now has shared Rust roster, owner-selection, workspace and
borrowed-target planning, a Forge CLI, and an external-stage integration path.
The focused native filter is `candidate_cargo_lint` in
`native/conductor-native` and the targets are `native_candidate_cargo_lint_{cli,stage,seed}` in
`native/forge`; they cover plan/selection, scope expansion, tool grants,
sequential fmt/clippy execution, private runtime cleanup, and result codes.
Focused release validation passed all 49 planner/runtime/CLI/stage/seed tests.

The Python `cargo_lint_files.py` wrapper, `python_contracts_candidate_cargo_lint`
target, and ambient `candidate_cargo_toolchain` compatibility shim are retired.
Candidate policy keeps the `cargo-fmt` and `cargo-clippy` IDs but routes their
commands and version probes through the uncached native Forge CLI stage. The
remaining `python_contracts_candidate_policy` target covers the separate Python
audit-dispatch seam; audit and lint are distinct paths.

Run all lint integration cases, including the six explicitly provisioned tests:

```sh
FORGE_TEST_BIN=/absolute/installed/bin/forge \
FORGE_TEST_CARGO_BIN=/absolute/native/toolchain/bin \
FORGE_TEST_CARGO_HOME_SEED=/absolute/cargo/cache-home \
FORGE_TEST_CGROUP_PARENT=/sys/fs/cgroup/delegated-parent \
RUST_TEST_THREADS=2 CARGO_BUILD_JOBS=2 PYO3_PYTHON=/nonexistent/forbidden-python \
  cargo +1.98.0 test --release --offline --locked \
  --manifest-path native/forge/Cargo.toml \
  --test native_candidate_cargo_lint_cli \
  --test native_candidate_cargo_lint_stage \
  --test native_candidate_cargo_lint_seed -- --include-ignored
```

The seed fixture requires cached crates.io `version_check` 0.9.5, its matching
sparse index entry and registry config. It copies only those files into an owned
minimal seed, verifies the pinned archive checksum, tests failure without that
seed and success with it, and checks source/cache identity and cleanup. Missing
tools, cgroup delegation or cache prerequisites fail explicitly when requested;
the test does not fetch dependencies. The core planner and eight runtime unit
tests do not need those external inputs.
