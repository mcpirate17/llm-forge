# Native scope contract migration

The three Python suites below have 23 named test functions and 24 expanded pytest
cases. Their Rust-owned PyO3 contracts call the shipped Python entry points and
assert their public results. The Python suites were retired after independent
parity review and successful native validation; Rust owns their fixtures too.

| Python suite | Rust contract target | Named / expanded cases |
| --- | --- | ---: |
| `src/conductor/candidate_review/test_native_test_selection.py` | `python_contracts_native_test_selection.rs` | 8 / 8 |
| `src/conductor/candidate_review/test_receipt_scope.py` | `python_contracts_receipt_scope.rs` | 7 / 7 |
| `src/conductor/test_complete_scope_drift.py` | `python_contracts_complete_scope_drift.rs` | 8 / 9 |

The selection and receipt contracts use a Rust-owned fixture in
`scope_contract_support.rs`. It builds a real temporary Git anchor, writes the
grandfather inventory with Python's standard JSON serialization to preserve its
bytes, pins those bytes with Rust SHA-256, and constructs the production
`Change`, `Candidate`, and `ReviewContext` objects through PyO3. Rust attribute
guards restore each patched verification constant and callback after its test.
Neither contract imports executable Python test modules. The receipt callback validates the same
`(_registry, paths, **_kwargs)` binding and records the exact path list. The
selection callbacks validate their original positional signatures and retain
the high-risk evidence observation. The drift contract builds the same temporary
manifest, registry, and baseline structures and identical test source bytes as
the Python suite. Its JSON serialization uses compact whitespace.

| Python case | Rust case |
| --- | --- |
| `test_an_inline_test_module_counts_as_the_crates_tests` | `an_inline_test_module_counts_as_the_crates_tests` |
| `test_an_integration_test_file_counts_without_the_marker` | `an_integration_test_file_counts_without_the_marker` |
| `test_build_output_is_not_evidence` | `build_output_is_not_evidence` |
| `test_a_source_outside_any_crate_is_not_covered` | `a_source_outside_any_crate_is_not_covered` |
| `test_crate_tests_answer_the_finding_without_being_run` | `crate_tests_answer_the_finding_without_being_run` |
| `test_native_mutation_evidence_answers_the_high_risk_gate` | `native_mutation_evidence_answers_the_high_risk_gate` |
| `test_an_untested_crate_is_still_reported` | `an_untested_crate_is_still_reported` |
| `test_a_covered_crate_does_not_answer_for_python` | `a_covered_crate_does_not_answer_for_python` |
| `test_a_touched_historical_test_file_needs_no_campaign` | `a_touched_historical_test_file_needs_no_campaign` |
| `test_a_new_test_definition_still_demands_a_receipt` | `a_new_test_definition_still_demands_a_receipt` |
| `test_an_unreadable_inventory_fails_closed` | `an_unreadable_inventory_fails_closed` |
| `test_exempt_and_gated_files_are_separated_in_one_candidate` | `exempt_and_gated_files_are_separated_in_one_candidate` |
| `test_only_paths_with_gated_nodeids_are_required` | `only_paths_with_gated_nodeids_are_required` |
| `test_an_unevaluable_inventory_requires_every_changed_test` | `an_unevaluable_inventory_requires_every_changed_test` |
| `test_an_empty_nodeid_tuple_does_not_gate_a_path` | `an_empty_nodeid_tuple_does_not_gate_a_path` |
| `test_added_test_is_reported_as_new_drift` | `added_test_is_reported_as_new_drift` |
| `test_removed_test_is_reported_as_extra` | `removed_test_is_reported_as_extra` |
| `test_reordering_alone_is_drift` | `reordering_alone_is_drift` |
| `test_scope_in_sync_reports_nothing` | `scope_in_sync_reports_nothing` |
| `test_unregistered_manifest_is_ignored` | `unregistered_manifest_is_ignored` |
| `test_baseline_absorbed_drift_is_labelled_known` | `baseline_absorbed_drift_is_labelled_known` |
| `test_restrict_skips_files_that_were_not_changed` | `restrict_skips_files_that_were_not_changed` |
| `test_non_complete_scopes_are_ignored[partial]` | `non_complete_scopes_are_ignored_partial` |
| `test_non_complete_scopes_are_ignored[unset]` | `non_complete_scopes_are_ignored_unset` |

Focused baseline: the original Python suites passed 24/24 on 2026-09-27 with
the isolated Forge installation. The three Rust targets passed 8/8, 7/7, and
9/9 with `python-compat-tests`, the same interpreter, and two Cargo workers.
The host import search found references to two suite filenames in
`docs/native-candidate-verification-migration.md`; it found no runtime import
of the three original test modules. The discovery registry includes production
providers, native dependencies, and each directly included Rust helper.
