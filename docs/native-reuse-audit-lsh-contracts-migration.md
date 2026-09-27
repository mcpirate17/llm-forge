# Reuse audit inventory and file-family LSH contract migration

This cohort moves nine named Python tests into Rust-owned PyO3 contracts:

| Retired Python module | Rust target | Cases |
| --- | --- | ---: |
| `src/conductor/reuse/test_audit_inventory_native.py` | `native/conductor-native/tests/python_contracts_reuse_audit_inventory.rs` | 6 |
| `src/conductor/reuse/test_file_family_lsh_native.py` | `native/conductor-native/tests/python_contracts_reuse_file_family_lsh.rs` | 3 |

The audit contract constructs the original category inputs in Rust, retains Python list and dict shape and object-identity checks, and calculates clone/family IDs independently with SHA-256. The LSH contract recreates the seeded 72-profile fixture with the same Python `random.Random.sample` stream; its signature, band buckets, pair combinations, and hash masking are independent Rust reference logic using standard-library BLAKE2b only as a primitive. It tests all four parameter rows, the 80/81-member boundary, zero and negative permutations, malformed arguments, and scan telemetry. No production method serves as its own oracle.

Both targets exercise the Python reuse APIs and the candidate-local `slop_core` extension. Their registry entries must include the Python provider paths, native slop-core providers, their direct Rust helpers, and the target files. The selected paths must trigger candidate-local slop-core build preparation, so ambient installed `slop_core` cannot silently determine contract results.

Independent case-by-case parity review passed, then exactly the two original Python modules were retired. The baseline was nine passed cases in `/tmp/forge-reuse-audit-lsh-baseline.log`. Post-retirement Rust contracts passed 6+3 in `/tmp/forge-reuse-audit-lsh-post-retirement-tests.log`, and scoped Clippy passed with `-D warnings` in `/tmp/forge-reuse-audit-lsh-post-retirement-clippy.log`. Native56 discovery and focused candidate-local slop selection passed in `/tmp/forge-native56-discovery-tests.log` and `/tmp/forge-native56-runtime-selection-tests.log`.
