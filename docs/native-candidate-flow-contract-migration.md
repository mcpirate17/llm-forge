# Native candidate flow contracts

This slice moves 67 expanded Python cases to Rust-owned assertions: 28 from
`src/conductor/test_candidate_review.py` (the first 28 of its 56 cases) and all
39 from `src/conductor/test_candidate_review_call_and_evidence.py`. Both
original Python modules were retired after the full candidate-review cohort
and benchmark input were replaced. `candidate_review_support.rs` supplies local fixtures; its
`gate_context` and `anchor_snapshot_inventory` API is shared with the other
candidate-review migration targets.

In the tables, `A`, `B`, and `E` mean the Rust targets
`python_contracts_candidate_review_flow_a`,
`python_contracts_candidate_review_flow_b`, and
`python_contracts_candidate_call_evidence`. Every row denotes one Rust test.
The Python source name omits only its leading `test_` when the Rust name is
identical. For expanded parameter cases, the Python source function is named
in full and each Rust row records its distinct input.

| # | Python source case | Rust target and test |
|---:|---|---|
| 1 | `test_index_candidate_ignores_unstaged_and_untracked_content` | A `index_candidate_ignores_unstaged_and_untracked_content` |
| 2 | `test_structured_claims_reject_exact_path_overlap_and_bind_content` | A `structured_claims_reject_exact_path_overlap_and_bind_content` |
| 3 | `test_ownership_claim_is_independent_of_ignored_worktree_ledger` | A `ownership_claim_is_independent_of_ignored_worktree_ledger` |
| 4 | `test_range_candidate_uses_merge_base_even_with_a_clean_index` | A `range_candidate_uses_merge_base_even_with_a_clean_index` |
| 5 | `test_ci_empty_range_fails_closed_with_clean_index` | A `ci_empty_range_fails_closed_with_clean_index` |
| 6 | `test_local_and_ci_policy_findings_are_parity_bound` | A `local_and_ci_policy_findings_are_parity_bound` |
| 7 | `test_analyzer_git_mutation_cannot_rebind_shared_worktree` | A `analyzer_git_mutation_cannot_rebind_shared_worktree` |
| 8 | `test_adversarial_builtin_matrix_exercises_real_candidate_flows` | A `adversarial_builtin_matrix_exercises_real_candidate_flows` |
| 9 | `test_dynamic_execution_gate_distinguishes_builtins_from_method_calls` | A `dynamic_execution_gate_distinguishes_builtins_from_method_calls` |
| 10 | `test_protocol_ellipsis_methods_are_not_flagged_as_stubs` | A `protocol_ellipsis_methods_are_not_flagged_as_stubs` |
| 11 | `test_analyzer_reporting_includes_stdout_alongside_warning_stderr` | A `analyzer_reporting_includes_stdout_alongside_warning_stderr` |
| 12 | `test_command_cache_mutex_and_attestation_contracts` | A `command_cache_mutex_and_attestation_contracts` |
| 13 | `test_targeted_test_selection_execution_and_coverage` | B `targeted_test_selection_execution_and_coverage` |
| 14 | `test_targeted_test_sharding_preserves_the_changed_coverage_verdict` | B `targeted_test_sharding_preserves_the_changed_coverage_verdict` |
| 15 | `test_targeted_test_shard_killed_by_signal_is_not_reported_as_a_failure` | B `targeted_test_shard_killed_by_signal_is_not_reported_as_a_failure` |
| 16 | `test_engine_and_tree_integrity_fail_closed_without_candidate_evidence` | B `engine_and_tree_integrity_fail_closed_without_candidate_evidence` |
| 17 | `test_graph_selected_tests_use_immutable_matching_metadata` | B `graph_selected_tests_use_immutable_matching_metadata` |
| 18 | `test_index_preserves_deletion_and_rename_identity` | B `index_preserves_deletion_and_rename_identity` |
| 19 | `test_rename_retains_old_path_risk_and_policy_classes` | B `rename_retains_old_path_risk_and_policy_classes` |
| 20 | `test_materialize_tree_allows_internal_symlink_and_rejects_escape` | B `materialize_tree_allows_internal_symlink_and_rejects_escape` |
| 21 | `test_materialize_tree_rejects_blob_before_loading_over_budget` | B `materialize_tree_rejects_blob_before_loading_over_budget` |
| 22 | `test_gitlink_is_represented_without_materializing_foreign_tree` | B `gitlink_is_represented_without_materializing_foreign_tree` |
| 23 | `test_malformed_and_ambiguous_refs_fail_closed` | B `malformed_and_ambiguous_refs_fail_closed` |
| 24 | `test_malformed_policy_fails_closed` | B `malformed_policy_fails_closed` |
| 25 | `test_expired_policy_fails_closed` | B `expired_policy_fails_closed` |
| 26 | `test_blanket_exception_scope_fails_closed` | B `blanket_exception_scope_fails_closed` |
| 27 | `test_receipt_digest_is_invalidated_by_candidate_tree_mutation` | B `receipt_digest_is_invalidated_by_candidate_tree_mutation` |
| 28 | `test_human_sarif_and_junit_reports_preserve_identity_and_findings` | B `human_sarif_and_junit_reports_preserve_identity_and_findings` |

For rows 29–67, the Python source is
`src/conductor/test_candidate_review_call_and_evidence.py` and every Rust
target is `E`.

| # | Python source function and parameter | Rust test |
|---:|---|---|
| 29 | `test_call_name_never_guesses_a_builtin_from_an_unresolvable_receiver`: `eval(payload)` | `call_name_eval_builtin` |
| 30 | same: `exec(payload)` | `call_name_exec_builtin` |
| 31 | same: `os.system(cmd)` | `call_name_os_system` |
| 32 | same: `pickle.load(handle)` | `call_name_pickle_load` |
| 33 | same: `yaml.load(stream)` | `call_name_yaml_load` |
| 34 | same: `obj.eval()` | `call_name_obj_eval` |
| 35 | same: `self.eval()` | `call_name_self_eval` |
| 36 | same: `torch.nn.Module.eval(model)` | `call_name_torch_module_eval` |
| 37 | same: `model.to(device).eval()` | `call_name_chained_eval_unresolved` |
| 38 | same: `build().exec()` | `call_name_chained_exec_unresolved` |
| 39 | same: `registry['key'].eval()` | `call_name_subscript_eval_unresolved` |
| 40 | same: `(a + b).eval()` | `call_name_binary_eval_unresolved` |
| 41 | `test_dynamic_execution_flags_real_calls_only`: chained `eval` | `dynamic_execution_chained_eval_is_clean` |
| 42 | same: named `eval` | `dynamic_execution_named_eval_is_clean` |
| 43 | same: builtin `eval` | `dynamic_execution_builtin_eval_is_flagged` |
| 44 | same: builtin `exec` | `dynamic_execution_builtin_exec_is_flagged` |
| 45 | same: `os.system` | `dynamic_execution_os_system_is_flagged` |
| 46 | `test_property_regex_still_satisfies_the_gate` | `property_regex_still_satisfies_the_gate` |
| 47 | `test_mutation_evidence_satisfies_the_gate_without_property_text` | `mutation_evidence_satisfies_gate_without_property_text` |
| 48 | `test_mutation_evidence_must_cover_a_selected_test`: `test_thing.py` | `mutation_evidence_selected_path_counts` |
| 49 | same: `test_unrelated.py` | `mutation_evidence_unrelated_path_does_not_count` |
| 50 | `test_missing_registry_is_not_mutation_evidence` | `missing_registry_is_not_mutation_evidence` |
| 51 | `test_broken_registry_does_not_claim_evidence` | `broken_registry_does_not_claim_evidence` |
| 52 | `test_jscpd_fingerprint_ignores_audit_roots_and_pair_order` | `jscpd_fingerprint_ignores_audit_roots_and_pair_order` |
| 53 | `test_equivalence_probe_reports_only_reachable_untested_branches` | `equivalence_probe_reports_only_reachable_untested_branches` |
| 54 | `test_equivalence_probe_never_probes_a_test_file` | `equivalence_probe_never_probes_a_test_file` |
| 55 | `test_severity_follows_the_tier_so_only_shipped_code_blocks` | `severity_follows_tier_so_only_shipped_code_blocks` |
| 56 | `test_the_gate_hands_its_findings_to_the_backlog` | `gate_hands_findings_to_backlog_as_complete_artifact` |
| 57 | `test_a_backlog_write_failure_does_not_fail_the_review` | `backlog_write_failure_does_not_fail_review` |
| 58 | `test_a_missing_engine_is_a_named_critical_finding_not_a_crash` | `missing_engine_is_named_critical_finding_without_probe_run` |
| 59 | `test_device_kernel_loops_are_not_reported_as_python_hot_paths`: `@jit` | `device_kernel_bare_jit_is_clean` |
| 60 | same: `@numba.cuda.jit` | `device_kernel_numba_cuda_jit_is_clean` |
| 61 | same: `@numba.njit` | `device_kernel_numba_njit_is_clean` |
| 62 | same: `@triton.autotune(...)` | `device_kernel_triton_autotune_is_clean` |
| 63 | same: `@triton.jit` | `device_kernel_triton_jit_is_clean` |
| 64 | `test_triton_static_range_kernel_is_clean` | `triton_static_range_kernel_is_clean` |
| 65 | `test_a_real_python_nested_loop_beside_a_kernel_still_reports` | `real_python_nested_loop_beside_kernel_still_reports` |
| 66 | `test_undecorated_helper_after_a_kernel_is_not_swallowed` | `undecorated_helper_after_kernel_is_not_swallowed` |
| 67 | `test_hot_path_flag_still_gates_the_rule` | `hot_path_flag_still_gates_the_rule` |

## Provider rows for the parent integration

The new target names need rows in
`native/conductor-native/src/python_contract_targets.tsv`. These are the
provider-to-target edges for this slice; rows for the three existing targets'
existing providers remain as they are. File paths below are relative to the
Forge root.

| Provider | Targets |
|---|---|
| `native/conductor-native/tests/python_contracts/candidate_review_support.rs` | A, B, `python_contracts_candidate_cli`, `python_contracts_candidate_verification`, `python_contracts_candidate_policy_regressions` |
| `native/conductor-native/tests/python_contracts/git_fixture_support.rs` | A, B, `python_contracts_candidate_cli`, `python_contracts_candidate_verification`, `python_contracts_candidate_policy_regressions` |
| `native/conductor-native/tests/python_contracts/support.rs` | A, B, E |
| `src/conductor/_native.py` | E |
| `src/conductor/candidate_review/checks.py` | A, B, E |
| `src/conductor/candidate_review/command_runner.py` | A, E |
| `src/conductor/candidate_review/engine.py` | A, B |
| `src/conductor/candidate_review/git_source.py` | A, B |
| `src/conductor/candidate_review/model.py` | A, B, E |
| `src/conductor/candidate_review/ownership.py` | A |
| `src/conductor/candidate_review/policy.py` | A, B, E |
| `src/conductor/candidate_review/policy_path.py` | A, B |
| `src/conductor/candidate_review/reporters.py` | B |
| `src/conductor/candidate_review/sharding.py` | B |
| `src/conductor/candidate_review/verification.py` | A, B, E |
| `src/conductor/mutation_testing.py` | E |
| `src/conductor/slop_gate.py` | E |
| `src/conductor/slop_ledger.py` | E |

The three rewired targets now use `fixture_receipt` or `gate_context` from
`candidate_review_support.rs` and keep the returned `Vec<AttrPatch>` alive
through each gate assertion. A scan of those three Rust targets for Python
fixture module strings `conductor.test_candidate_review` and
`conductor.test_candidate_review_hardening`, and for `pytest.MonkeyPatch`,
returns zero matches. The original Python fixture modules and the other
candidate-review suites were retired together after the benchmark stopped
using a test file as its input.

Focused verification: all six affected integration targets passed (67 new and
24 existing cases); scoped `cargo clippy -- -D warnings` and `rustfmt --check`
passed. No discovery or full gate was run in this slice.
