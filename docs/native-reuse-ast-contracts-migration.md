# Reuse AST contract migration

This cohort moves nine named Python tests to Rust-owned PyO3 contracts:

| Python original | Rust contract | Cases |
| --- | --- | ---: |
| `src/conductor/reuse/test_consolidation_collect_native.py` | `native/conductor-native/tests/python_contracts_reuse_consolidation_collect.rs` | 3 |
| `src/conductor/reuse/test_detector_scan_native.py` | `native/conductor-native/tests/python_contracts_reuse_detector_scan.rs` | 3 |
| `src/conductor/reuse/test_file_family_profiles_native.py` | `native/conductor-native/tests/python_contracts_reuse_file_family_profiles.rs` | 3 |

The shared `reuse_ast_support.rs` uses Python's standard-library parser, AST traversal, and hash functions as primitives. The independent reference algorithms are Rust-owned in dedicated helpers. Consolidation computes binding and rename order, docstring removal, normalized dumps, SHA-1, breadth-first collection, and failure counts. Detector scan computes guardrail thresholds, exemptions, and fallback classifications from source ASTs. File-family profiles compute statement and structural features, schemas, API signatures, imports, calls, and control counts. Like the original profile test, method hashes use `consolidation._normalize_hash`; the remaining expected profile fields are computed independently.

All fixture source text and invalid UTF-8 bytes are retained as input data. The tests run CPU-only against candidate-local `slop_core` and the production Python API. Registry mappings include the target/helper paths, direct Python providers, and slop-core source providers so candidate changes select the corresponding Rust contracts and rebuild the correct local native library.

The nine original Python cases passed before migration in `/tmp/forge-reuse-ast-three-baseline.log`. All three targets passed independent semantic review, and exactly their three originals were retired. Post-retirement Rust targets passed 3+3+3 using the isolated native57 extension in `/tmp/forge-reuse-ast-three-post-retirement-tests.log`; scoped Clippy passed with `-D warnings` in `/tmp/forge-reuse-ast-three-post-retirement-clippy.log`. Native57 discovery and focused candidate-local slop selection passed in `/tmp/forge-native57-discovery-tests.log` and `/tmp/forge-native57-runtime-selection-tests.log`.
