# Native guardrail audit and duplicate contract migration

This cohort moves all 27 expanded cases from `src/conductor/test_guardrail_audit.py`
and all 14 from `src/conductor/test_guardrail_duplicates.py` into Rust-owned PyO3
contracts. The original Python baseline passed 41/41 in 3.61 seconds
(`/tmp/forge-guardrail-contract-python-baseline.log`). The original files were
retired after independent parity review and target registration.

| Original audit case | Rust case |
| --- | --- |
| `test_iter_files_accepts_explicit_file_target` | `iter_files_accepts_explicit_file_target` |
| `test_resolve_tool_command_prefers_running_environment` | `resolve_tool_command_prefers_running_environment` |
| `test_run_tool_reports_timeout_without_raising` | `run_tool_reports_timeout_without_raising` |
| `test_incomplete_external_tools_fail_closed` | `incomplete_external_tools_fail_closed` |
| `test_expected_tool_finding_exit_codes_are_complete` | `expected_tool_finding_exit_codes_are_complete` |
| `test_check_mode_blocks_high_severity_findings` | `check_mode_blocks_high_severity_findings` |
| `test_ref_selection_and_structural_parse_fail_closed` | `ref_selection_and_structural_parse_fail_closed` |
| `test_candidate_text_has_deterministic_latin1_fallback` | `candidate_text_has_deterministic_latin1_fallback` |
| `test_explicit_root_scans_the_named_repo_not_cwd` | `explicit_root_scans_the_named_repo_not_cwd` |
| `test_default_root_uses_cwd_toplevel_not_module_location` | `default_root_uses_cwd_toplevel_not_module_location` |
| `test_cwd_outside_worktree_refuses_rather_than_falling_back` | `cwd_outside_worktree_refuses_rather_than_falling_back` |
| `test_resolved_root_is_printed` | `resolved_root_is_printed` |
| `test_root_mismatch_warns` | `root_mismatch_warns` |
| `test_load_allowlist_reads_the_host_copy_not_a_package_copy` | `load_allowlist_reads_the_host_copy_not_a_package_copy` |
| `test_load_allowlist_is_empty_when_the_host_has_none` | `load_allowlist_is_empty_when_the_host_has_none` |
| `test_load_allowlist_honors_a_conductor_table_override` | `load_allowlist_honors_a_conductor_table_override` |
| `test_default_targets_scan_forge_python_and_rust_and_prune_builds` | `default_targets_scan_forge_python_and_rust_and_prune_builds` |
| `test_host_target_configuration_and_cli_override` | `host_target_configuration_and_cli_override` |
| `test_invalid_targets_fail_loudly[targets0]` (`[]`) | `invalid_targets_empty_list` |
| `test_invalid_targets_fail_loudly[targets1]` (`["missing"]`) | `invalid_targets_missing` |
| `test_invalid_targets_fail_loudly[targets2]` (`["../escape"]`) | `invalid_targets_parent_escape` |
| `test_invalid_targets_fail_loudly[targets3]` (`["/tmp"]`) | `invalid_targets_absolute` |
| `test_invalid_targets_fail_loudly[targets4]` (`[""]`) | `invalid_targets_empty_string` |
| `test_invalid_targets_fail_loudly[source]` | `invalid_targets_string_not_sequence` |
| `test_no_python_and_scoped_reports_do_not_claim_zero_tool_findings` | `no_python_and_scoped_reports_do_not_claim_zero_tool_findings` |
| `test_incomplete_audit_returns_error_without_code_critical` | `incomplete_audit_returns_error_without_code_critical` |
| `test_duplicate_failure_is_unavailable_in_external_summary` | `duplicate_failure_is_unavailable_in_external_summary` |

| Original duplicate case | Rust case |
| --- | --- |
| `test_indexed_matches_pylint_across_directories_and_ignored_lines` | `indexed_matches_pylint_across_directories_and_ignored_lines` |
| `test_threshold_and_repeated_windows_match_pylint[9]` | `threshold_9_matches_pylint` |
| `test_threshold_and_repeated_windows_match_pylint[10]` | `threshold_10_matches_pylint` |
| `test_threshold_and_repeated_windows_match_pylint[11]` | `threshold_11_matches_pylint` |
| `test_threshold_and_repeated_windows_match_pylint[25]` | `threshold_25_matches_pylint` |
| `test_global_index_has_no_pairs_for_unrelated_files` | `global_index_has_no_pairs_for_unrelated_files` |
| `test_invalid_source_and_encoding_are_errors_not_empty_results` | `invalid_source_and_encoding_are_errors_not_empty_results` |
| `test_scan_reports_global_counts_and_source_locations` | `scan_reports_global_counts_and_source_locations` |
| `test_source_suppression_is_not_lost_by_native_candidate_selection[disable=duplicate-code]` | `suppression_disable_duplicate_code` |
| `test_source_suppression_is_not_lost_by_native_candidate_selection[skip-file]` | `suppression_skip_file` |
| `test_scoped_suppression_respects_reenable` | `scoped_suppression_respects_reenable` |
| `test_host_normalization_configuration_is_preserved[.pylintrc]` | `host_pylintrc_normalization_configuration_is_preserved` |
| `test_host_normalization_configuration_is_preserved[pyproject.toml]` | `host_pyproject_normalization_configuration_is_preserved` |
| `test_all_host_ignore_options_are_resolved` | `all_host_ignore_options_are_resolved` |

The audit target exercises the shipped `conductor.guardrail_audit` API, its
`audit_root`, `guardrail_targets`, `project_paths`, `run_duplicate_audit`, and
`candidate_review.vulture_audit` helpers, plus the duplicate scanner through
the external-summary path. Its structural metrics run through
`conductor._native.guardrail_ast_metrics_native` in
`native/conductor-native/src/guardrail_ast.rs`. The duplicate target compares
`_IndexedSymilar` to Pylint's `Symilar`, then exercises `scan_duplicates` and
host config resolution; candidate selection runs through
`conductor._native.guardrail_duplicate_candidates_native` in
`native/conductor-native/src/guardrail_duplicates.rs`. Both paths require the
`conductor_native` extension. Neither requires the optional `slop_core`
extension on the exercised path.

The two main audit cases create and scan only disposable Git repositories with
one synthetic Python file. The test process and fixture Git children clear
ambient Git directory, object, namespace, discovery, template, and config
selectors. A scratch `PYLINTRC` prevents fallback to home configuration while
explicit host `.pylintrc` and `pyproject.toml` fixtures remain discoverable;
ambient Vulture selectors are cleared too. All other external
tool paths are patched with Rust callbacks, except Pylint configuration and
duplicate scans against explicit temporary source fixtures. No case scans the
Forge checkout or starts a model, GPU job, or network request.

Validation: the two Rust targets passed 27/27 and 14/14 cases respectively
(`/tmp/forge-guardrail-contract-cargo-test-final.log`), and scoped Clippy passed
with `-D warnings` (`/tmp/forge-guardrail-contract-clippy-final.log`). The runner
used native 0.1.66, two Cargo jobs, and hidden CUDA devices.

Exact dependency rows for the parent registry:

```text
src/conductor/guardrail_audit.py	python_contracts_guardrail_audit
src/conductor/audit_root.py	python_contracts_guardrail_audit
src/conductor/guardrail_targets.py	python_contracts_guardrail_audit
src/conductor/project_paths.py	python_contracts_guardrail_audit
src/conductor/run_duplicate_audit.py	python_contracts_guardrail_audit
src/conductor/candidate_review/vulture_audit.py	python_contracts_guardrail_audit
src/conductor/guardrail_duplicates.py	python_contracts_guardrail_audit
src/conductor/guardrail_duplicates.py	python_contracts_guardrail_duplicates
src/conductor/_native.py	python_contracts_guardrail_audit
src/conductor/_native.py	python_contracts_guardrail_duplicates
native/conductor-native/src/guardrail_ast.rs	python_contracts_guardrail_audit
native/conductor-native/src/guardrail_duplicates.rs	python_contracts_guardrail_audit
native/conductor-native/src/guardrail_duplicates.rs	python_contracts_guardrail_duplicates
native/conductor-native/src/project_paths.rs	python_contracts_guardrail_audit
native/conductor-native/src/lib.rs	python_contracts_guardrail_audit
native/conductor-native/src/lib.rs	python_contracts_guardrail_duplicates
native/conductor-native/tests/python_contracts/support.rs	python_contracts_guardrail_audit
native/conductor-native/tests/python_contracts/support.rs	python_contracts_guardrail_duplicates
```
