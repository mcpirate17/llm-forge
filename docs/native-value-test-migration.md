# Mutation value test migration

The remaining cases in `src/conductor/test_mutation_value.py` are represented
by Rust assertions below. The native tests call the public Python API through
PyO3 where the contract concerns Python wrappers, subprocess capture, and
campaign routing. Fixtures and assertions live in Rust. Existing pure Rust
parser coverage remains in `mutation_value_inputs.rs` (mapped in
`native-test-migration.md`).

| Python case | Rust replacement |
| --- | --- |
| `test_value_spec_requires_high_risk_contracts_bound_to_production` | `python_contracts_mutation_value_core::value_spec_requires_high_risk_contracts_bound_to_production`, `value_spec_rejects_every_invalid_field_and_unbound_contract` (all 17 `_invalid_value_payloads` rows) |
| `test_junit_attribution_maps_parameterized_failures_and_incomplete_reports` | `python_contracts_mutation_value::junit_attribution_maps_parameterized_failures_and_incomplete_reports`, `junit_error_and_unmapped_edge_cases_fail_closed`, plus existing missing-batch collector test |
| `test_pytest_attribution_support_rejects_unmappable_batches` | `python_contracts_mutation_value::pytest_attribution_support_rejects_unmappable_batches` |
| `test_value_analysis_selects_core_and_flags_merge_and_delete_candidates` | `python_contracts_mutation_value_core::value_analysis_selects_core_and_flags_merge_and_delete_candidates` |
| `test_value_analysis_fails_closed_on_flakes_and_cross_contract_kills` | `python_contracts_mutation_value_core::value_analysis_fails_closed_on_flakes_cross_contract_kills_and_incomplete_maps`, `python_contracts_mutation_value_routing::rust_batches_route_to_libtest_and_value_analysis_requires_correct_adapter` (nested `_assert_value_analysis_reaches_native_collectors`) |
| `test_value_analysis_marks_missing_test_map_without_dropping_evidence` | `python_contracts_mutation_value_core::value_analysis_marks_missing_test_map_without_dropping_evidence` |
| `test_value_admission_rejects_unmeasured_and_low_value_new_tests` | `python_contracts_mutation_value_core::value_admission_rejects_unmeasured_and_low_value_new_tests` |
| `test_cargo_attribution_refuses_ambiguity_rather_than_guessing` | `python_contracts_mutation_value_routing::cargo_attribution_refuses_ambiguity_and_reports_collateral`, `cargo_verdict_separates_declared_collateral_and_ambiguous_kills` (nested `_assert_undeclared_rust_killer_is_misattributed`), `rust_batches_route_to_libtest_and_value_analysis_requires_correct_adapter` (nested `_assert_rust_batch_routes_to_libtest`) |
| `test_cargo_attribution_reads_the_full_stdout_not_the_stored_tail` | `python_contracts_mutation_value_routing::command_support_streams_verdict_past_stored_tail_and_on_timeout`, `cargo_collector_reads_full_stdout_and_refuses_python_nodeids` |
| `test_a_ctest_report_separates_the_declared_killer_from_collateral` | `python_contracts_mutation_value::ctest_report_separates_declared_killer_from_collateral` |
| `test_a_c_batch_is_routed_to_the_ctest_collector` | `python_contracts_mutation_value_routing::c_batch_routes_to_ctest_even_when_shell_builds_first` |
| `test_value_analysis_uses_explicit_failed_nodeids_for_killers` | `python_contracts_mutation_value_core::value_analysis_uses_explicit_failed_nodeids_for_killers`, `python_contracts_mutation_value_routing::capable_unattributed_kills_refuse_campaign` (nested `_assert_capable_unattributed_kill_refuses_campaign`) |

`_spec`, `_report`, `_routing_campaign`, and `_routing_spec` are now Rust-owned
fixture builders in `python_contracts/mutation_value_support.rs` and
`python_contracts_mutation_value_routing.rs`; the Python test module is not imported by
another test. `python_contracts_candidate_policy_regressions.rs` has no fixture
dependency on it. Nodeid strings in native tests remain deliberate synthetic
inputs. The grandfathered nodeid snapshot and historical campaign and receipt
references are retained as historical evidence.

All 20 focused Rust compatibility tests passed after their final target rename:
seven core, seven routing, and six report/collector tests. The Python test module
was retired after this run and the consumer audit. Every target matches CI's
`python_contracts_*` selection. The migration does not change production
algorithms or the historical evidence registry.
