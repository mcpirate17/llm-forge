# Mutation coverage and run-scope Python contracts

`src/conductor/test_mutation_coverage.py` (11 named cases) and
`src/conductor/test_mutation_run_scope.py` (6 named cases) are migrated to
`native/conductor-native/tests/python_contracts_mutation_coverage.rs` and
`python_contracts_mutation_run_scope.rs`. The original pytest baseline passed
17/17. The PyO3 targets call the shipped Python APIs; fixture construction,
Git setup, callbacks, and assertions live in Rust. Their dedicated Rust helper
is `tests/python_contracts/mutation_coverage_scope_support.rs`.

The coverage target retains the native glob-call tracking, tracked and
untracked Git inventory, `verify_evidence` callback signature, changed-evidence
debt/defect exit codes, GitHub annotations and summary text, canary verdict
shape, and malformed-campaign refusals. It reads the checked-in coverage
campaign and registry JSON fixtures through the same `mutation_testing.REPO_ROOT`
paths as the original. The ranked Python path in the fixture manifest is data,
not a test module imported by these cases. The timeout case still runs the
bounded `sleep 1` child with a 0.01-second timeout and checks its result.

The scope target retains the manifest's tuple sources/operators, dict pins,
temporary source files, strict changed-source callback, selected-source
allowlist, symlink escape, cargo package-relative paths, and Mull regex and
operator config. The generated-run CLI case patches campaign loading and
engine selection, then asserts `REFUSED` and zero adapter calls. Its broad
campaign fails `validate_run_scope` before any receipt, snapshot, or mutation
engine starts. The other cases only call validation and config generation.

Only temporary Git repositories and synthetic campaign data are created;
these tests do not produce mutation evidence or run fest, cargo-mutants, or
Mull. Independent case-by-case review found no parity gap or live test-module
import consumer, and the two original pytest modules were retired. The
post-retirement exact targets passed 11/11 and 6/6
(`/tmp/forge-mutation-coverage-scope-postretire-tests.log`); scoped Clippy with
`-D warnings` passed (`/tmp/forge-mutation-coverage-scope-postretire-clippy.log`).

The pinned `src/conductor/candidate_review/grandfathered_test_nodeids_61343f57.json`
inventory retains historical coverage-test nodeids and remains unchanged:
`candidate_review.verification` validates that provenance. The docstring in
`src/conductor/testdata/coverage/claude_bash_quiet_ranked.py` likewise names
two historical nodeids as fixture context; it does not import the old tests.
