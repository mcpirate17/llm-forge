# Native gate contract migration

The two original Python suites have 56 collected cases: 33 in
`src/conductor/test_gate.py` and 23 in `src/conductor/test_gate_rollout.py`.
Each case below has a Rust-owned assertion in a PyO3 integration target. The
targets call the shipped Python APIs; they do not import the Python test files.
The originals remain present while the native targets and discovery wiring are
reviewed.

| Original `test_gate.py` case | `python_contracts_gate` case |
| --- | --- |
| `test_search_path_prepends_node_bin_when_present` | `search_path_prepends_node_bin_when_present` |
| `test_search_path_is_unchanged_when_node_bin_absent` | `search_path_is_unchanged_when_node_bin_absent` |
| `test_probe_tool_reports_a_matching_version` | `probe_tool_reports_a_matching_version` |
| `test_probe_tool_reports_a_drifted_version` | `probe_tool_reports_a_drifted_version` |
| `test_probe_tool_reports_a_missing_tool` | `probe_tool_reports_a_missing_tool` |
| `test_probe_tool_treats_a_failing_version_command_as_versionless` | `probe_tool_treats_a_failing_version_command_as_versionless` |
| `test_preflight_passes_when_every_required_tool_is_present` | `preflight_passes_when_every_required_tool_is_present` |
| `test_preflight_refuses_a_drifted_version` | `preflight_refuses_a_drifted_version` |
| `test_preflight_reports_a_missing_tool_before_a_drifted_one` | `preflight_reports_a_missing_tool_before_a_drifted_one` |
| `test_preflight_filters_tools_by_profile` | `preflight_filters_tools_by_profile` |
| `test_preflight_names_npm_ci_when_node_modules_is_absent` | `preflight_names_npm_ci_when_node_modules_is_absent` |
| `test_export_contains_tracked_files_and_no_git_directory` | `export_contains_tracked_files_and_no_git_directory` |
| `test_export_omits_untracked_files` | `export_omits_untracked_files` |
| `test_export_refuses_an_unknown_ref` | `export_refuses_an_unknown_ref` |
| `test_discover_skips_vendored_configs` | `discover_skips_vendored_configs` |
| `test_sample_test_file_picks_the_smallest` | `sample_test_file_picks_the_smallest` |
| `test_sample_test_file_is_none_without_tests` | `sample_test_file_is_none_without_tests` |
| `test_pytest_config_check_fails_on_an_unparseable_addopts` | `pytest_config_check_fails_on_an_unparseable_addopts` |
| `test_pytest_config_check_passes_on_a_valid_addopts` | `pytest_config_check_passes_on_a_valid_addopts` |
| `test_the_pytest_probes_run_with_isolated_bytecode_caches` | `the_pytest_probes_run_with_isolated_bytecode_caches` |
| `test_waivers_are_active_on_their_pinned_base` | `waivers_are_active_on_their_pinned_base` |
| `test_waivers_are_inert_on_any_other_base` | `waivers_are_inert_on_any_other_base` |
| `test_waiver_activation_is_reported_per_waiver_not_all_or_nothing` | `waiver_activation_is_reported_per_waiver_not_all_or_nothing` |
| `test_render_distinguishes_pass_fail_and_refused` | `render_distinguishes_pass_fail_and_refused` |
| `test_exit_codes_are_distinct` | `exit_codes_are_distinct` |
| `test_corpus_audit_passes_at_the_recorded_baseline` | `corpus_audit_passes_at_the_recorded_baseline` |
| `test_corpus_audit_reads_the_export_not_the_working_tree` | `corpus_audit_reads_the_export_not_the_working_tree` |
| `test_corpus_audit_forwards_the_candidate_diff` | `corpus_audit_forwards_the_candidate_diff` |
| `test_corpus_audit_fails_on_a_newly_rotted_mutant` | `corpus_audit_fails_on_a_newly_rotted_mutant` |
| `test_corpus_audit_fails_when_a_baseline_entry_stops_failing` | `corpus_audit_fails_when_a_baseline_entry_stops_failing` |
| `test_corpus_audit_fails_on_a_non_patch_regression` | `corpus_audit_fails_on_a_non_patch_regression` |
| `test_corpus_audit_refuses_when_the_candidate_has_no_registry` | `corpus_audit_refuses_when_the_candidate_has_no_registry` |
| `test_corpus_detail_truncates_long_id_lists_but_keeps_the_count` | `corpus_detail_truncates_long_id_lists_but_keeps_the_count` |

| Original `test_gate_rollout.py` case | `python_contracts_gate_rollout` case |
| --- | --- |
| `test_promotion_is_refused_one_run_below_the_threshold` | `promotion_is_refused_one_run_below_the_threshold` |
| `test_promotion_is_allowed_exactly_at_the_threshold` | `promotion_is_allowed_exactly_at_the_threshold` |
| `test_promotion_is_allowed_above_the_threshold` | `promotion_is_allowed_above_the_threshold` |
| `test_a_red_resets_the_promotion_count` | `a_red_resets_the_promotion_count` |
| `test_required_runs_do_not_count_toward_promotion` | `required_runs_do_not_count_toward_promotion` |
| `test_promotion_counts_are_per_check` | `promotion_counts_are_per_check` |
| `test_one_unrelated_red_does_not_demote` | `one_unrelated_red_does_not_demote` |
| `test_two_consecutive_unrelated_reds_demote` | `two_consecutive_unrelated_reds_demote` |
| `test_a_red_related_to_its_own_diff_does_not_count` | `a_red_related_to_its_own_diff_does_not_count` |
| `test_a_green_between_reds_breaks_the_demotion_streak` | `a_green_between_reds_breaks_the_demotion_streak` |
| `test_advisory_reds_never_demote` | `advisory_reds_never_demote` |
| `test_ledger_round_trips` | `ledger_round_trips` |
| `test_missing_ledger_is_empty_not_an_error` | `missing_ledger_is_empty_not_an_error` |
| `test_unknown_ledger_schema_is_refused` | `unknown_ledger_schema_is_refused` |
| `test_promote_refuses_without_the_owner_acknowledgement` | `promote_refuses_without_the_owner_acknowledgement` |
| `test_a_failed_gh_call_names_the_command_and_carries_its_stderr` | `a_failed_gh_call_names_the_command_and_carries_its_stderr` |
| `test_setting_required_checks_replaces_only_the_status_check_rule` | `setting_required_checks_replaces_only_the_status_check_rule` |
| `test_an_empty_context_list_removes_the_rule_rather_than_emptying_it` | `an_empty_context_list_removes_the_rule_rather_than_emptying_it` |
| `test_a_rejected_ruleset_update_is_raised_not_swallowed` | `a_rejected_ruleset_update_is_raised_not_swallowed` |
| `test_reading_required_checks_returns_the_contexts_in_the_rule` | `reading_required_checks_returns_the_contexts_in_the_rule` |
| `test_a_ruleset_with_no_status_check_rule_reads_as_no_required_checks` | `a_ruleset_with_no_status_check_rule_reads_as_no_required_checks` |
| `test_record_appends_a_run_and_status_reads_it_back` | `record_appends_a_run_and_status_reads_it_back` |
| `test_status_on_an_empty_ledger_says_so_instead_of_printing_nothing` | `status_on_an_empty_ledger_says_so_instead_of_printing_nothing` |

The shared `gate_support.rs` creates a fresh temporary root per case and
restores environment and Python module state through `Case` and `AttrPatch`.
Its fixture Git commands clear inherited environment, disable system and
global Git configuration, and create a one-commit repository under the
temporary root. Tool probes use temporary executable files. Pytest config
probes use the embedded interpreter's `sys.executable`; their bytecode cache
scratch sits beside the temporary export. Corpus audit tests replace only
`audit_corpus` and retain production registry resolution, exit-code selection,
and detail rendering. Rollout ruleset tests replace `subprocess.run`, capture
the JSON PUT body, and never contact GitHub. The unacknowledged promotion case
also asserts that no PUT was attempted.

Discovery must register each target's production provider and both direct
helpers (`gate_support.rs` and `support.rs`). `gate.py` imports policy,
policy-path, project-path, and bytecode isolation helpers. Its corpus phase
imports `mutation_patch_audit` lazily; that import brings mutation campaign,
receipt, scope, value, and patch helpers into the runtime closure. The
project-path and receipt helpers import `conductor._native`, so the native
extension entry point and their relevant Rust provider paths are registered.
Neither target calls `slop_core()`, so no `SLOP_CONSUMERS` entry is needed.
The integrating lane registers the exact providers in
`native/conductor-native/src/python_contract_targets.tsv`.

Scoped validation passed on the drafted targets with Cargo 1.98.0,
`python-compat-tests`, offline locked dependencies, two build jobs, one test
thread, and CUDA hidden. `python_contracts_gate` passed 33/33 in 1.60 seconds;
`python_contracts_gate_rollout` passed 23/23 in 0.03 seconds. The complete
Cargo invocation took 1.87 seconds with warm artifacts. Scoped Clippy passed
with `-D warnings`, and rustfmt reported no differences. Logs are
`/tmp/forge-native-next-gate/gate-cargo-test.log` and
`/tmp/forge-native-next-gate/gate-cargo-clippy.log`. These results establish
the draft targets' local behavior; retirement and integrated discovery checks
belong to the subsequent change.
