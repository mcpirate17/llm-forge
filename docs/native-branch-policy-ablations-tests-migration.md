# Branch-policy and ablation Python-boundary contracts in Rust

Thirteen named cases from `src/conductor/test_branch_policy_native.py` move to
`native/conductor-native/tests/python_contracts_branch_policy_native.rs`. Nine
named cases from `src/conductor/test_native_ablations.py` move to
`python_contracts_native_ablations.rs`. Both original modules passed their
22-case pytest baseline in `/tmp/forge-branch-ablation-baseline.log`.

The branch-policy target calls the production adapter through PyO3. Rust owns
all temporary binding stores, time boundaries, exact returned dataclass and
tuple comparisons, and signature-bound substitutes for the native functions,
`git_common_dir`, and `_refs`. It checks exception classes and diagnostic text
at the Python boundary. Its dedicated helper is
`tests/python_contracts/branch_policy_native_support.rs`, with shared
`agent_comm_support.rs` and `support.rs`.

The ablation target calls the native `slop_core` engine through the
production `native_ablations.py` adapter. Rust supplies the source fixture at
`src/conductor/testdata/native_ablations/sample.py`, byte-identical to the
former `SAMPLE` literal, and checks every returned ablation's Python compile
result without executing it. Rust also owns the missing-extension simulation
and checks the import-time error, optional rules, multi-site edits, and native
engine identity. The native providers exercised are `slop-core/src/lib.rs`,
`engine.rs`, and `rules.rs`.

The Rust targets use production Python and native bindings without wrapping a
retired pytest test or adding Python test algorithms. Historical campaign or
lineage references to the former Python paths retain their original provenance
and are not executable imports.

Independent semantic review passed all 22 named cases after exact Python-list
input was restored for the unknown-rule calls. Under the reviewed native54
extension, the Rust targets passed 13/13 and 9/9 in
`/tmp/forge-branch-ablation-reviewed-tests.log`; scoped Clippy passed with
warnings denied in `/tmp/forge-branch-ablation-reviewed-clippy.log`.
`test_branch_policy_native.py` was retired and its Rust target passed 13/13
again in `/tmp/forge-branch-policy-native-post-retire-tests.log`.

The contract runtime now builds the candidate's own `slop_core` extension for
the ablation and candidate-style targets. Both runner paths passed real native
extension probes against a conflicting ambient module; all 11 runtime tests
and scoped Clippy passed in `/tmp/forge-slop-runtime-tests.log` and
`/tmp/forge-slop-runtime-clippy.log`.

After that provenance fix, `test_native_ablations.py` was retired. The real
checkout's `conductor_native` and `slop_core` libraries were rebuilt together
into `/tmp/forge-contract-native54-final` without a shared-environment install.
Post-retirement checks passed 9/9 ablation, 23/23 candidate-style, and 24/24
discovery cases in `/tmp/forge-native54-final-contract-tests.log`; scoped
Clippy passed in `/tmp/forge-native54-final-contract-clippy.log`.
