# Guardrail, duplicate-body, and import-ablation contracts

This cohort moves 16 named Python tests to Rust-owned PyO3 contracts:

| Original Python module | Rust contract | Cases |
| --- | --- | ---: |
| `src/conductor/test_guardrail_ast_native.py` | `native/conductor-native/tests/python_contracts_guardrail_ast_native.rs` | 4 |
| `src/conductor/test_duplicate_bodies_native.py` | `native/conductor-native/tests/python_contracts_duplicate_bodies_native.rs` | 4 |
| `src/conductor/test_import_ablation.py` | `native/conductor-native/tests/python_contracts_import_ablation.rs` | 8 |

The guardrail contract computes its own AST nesting, branch, route-registration, and hot-loop metrics in Rust over Python standard-library AST input. It also exercises threshold, marker, allowlist, order, syntax, and file-size policy through the production Python API. The duplicate-body contract computes independent candidate and standalone CPython AST/SHA-256 reference digests and compares equivalence partitions, thresholds, nested order, parse failures, and unknown-policy errors. `reuse_ast_support.rs` supplies only standard-library AST/hash primitives; its reviewed native57 source remains unchanged.

The import-ablation contract keeps the exact module fixture as source input and checks silenced imports, whole multiline ablation, immutable source files, one-module finder scope, both classify oracles, and missing driver evidence. Its `consumers` case initializes and stages only a disposable synthetic Git repository under the case temp directory. The classifier's subprocess is replaced by a Rust callback with the original `*args, **kwargs` and `CompletedProcess` return shape; no driver command is launched.

The original Python tests passed 16/16 in `/tmp/forge-native58-guard-duplicate-import-baseline.log`. Independent case-by-case parity review passed after the import-ablation contract restored the default `import_sites(Path)` call. Exactly the three original modules were retired. The Rust targets then passed 4+4+8 using the fresh isolated native58 extension in `/tmp/forge-native58-post-retirement-tests.log`, and scoped Clippy passed with `-D warnings` in `/tmp/forge-native58-post-retirement-clippy.log`. Registry discovery passed 24/24 in `/tmp/forge-native58-discovery-final-tests.log`. Historical jscpd baseline JSON references to old test paths remain as provenance data and are not imports.
