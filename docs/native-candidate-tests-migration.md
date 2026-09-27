# Candidate-review test migration

Rust owns the assertions in the `python_contracts_candidate_*` integration targets. The tests call the published Python seams through PyO3 where behavior still depends on Python orchestration or host file I/O; no Python assertion code is embedded in Rust strings. `python_contracts/candidate_support.rs` builds isolated contexts in Rust.

## Retired case map

All rows below passed in their corresponding `python_contracts_candidate_files`, `python_contracts_candidate_tools`, `python_contracts_candidate_verification`, or `python_contracts_candidate_engine` Rust target with the Forge PyO3 build environment before their Python files were removed.

| Retired Python case | Rust replacement |
| --- | --- |
| `test_diff_ranges::test_hunk_without_a_count_is_a_single_line` | `diff_hunks_keep_counts_deletions_anchoring_and_all_hunks` |
| `test_diff_ranges::test_explicit_count_is_inclusive_of_the_first_line` | `diff_hunks_keep_counts_deletions_anchoring_and_all_hunks` |
| `test_diff_ranges::test_pure_deletion_contributes_no_range` | `diff_hunks_keep_counts_deletions_anchoring_and_all_hunks` |
| `test_diff_ranges::test_hunk_syntax_inside_a_source_line_is_not_a_hunk` | `diff_hunks_keep_counts_deletions_anchoring_and_all_hunks` |
| `test_diff_ranges::test_every_hunk_is_reported_not_just_the_first` | `diff_hunks_keep_counts_deletions_anchoring_and_all_hunks` |
| `test_diff_ranges::test_touching_ranges_merge_but_a_gap_survives` | `diff_ranges_merge_touching_nested_and_unsorted_inputs` |
| `test_diff_ranges::test_nested_range_does_not_shorten_the_enclosing_one` | `diff_ranges_merge_touching_nested_and_unsorted_inputs` |
| `test_diff_ranges::test_unsorted_input_is_ordered_before_merging` | `diff_ranges_merge_touching_nested_and_unsorted_inputs` |
| `test_diff_ranges::test_only_the_changed_lines_are_reported_not_their_context` | `changed_line_ranges_uses_zero_context_and_refuses_bad_base` |
| `test_diff_ranges::test_unresolvable_base_raises_rather_than_reporting_no_changes` | `changed_line_ranges_uses_zero_context_and_refuses_bad_base` |
| `test_command_runner::test_changed_files_file_token_is_written_and_substituted` | `command_expansion_writes_scoped_changed_file_lists` |
| `test_command_runner::test_changed_files_file_is_empty_when_no_files_matched` | `command_expansion_writes_scoped_changed_file_lists` |
| `test_command_runner::test_no_changed_files_file_token_writes_no_scratch_file` | `command_expansion_keeps_inline_files_and_skips_unused_scratch` |
| `test_command_runner::test_files_token_still_expands_inline_as_before` | `command_expansion_keeps_inline_files_and_skips_unused_scratch` |
| `test_command_runner::test_changed_files_file_name_is_scoped_per_check_id` | `command_expansion_writes_scoped_changed_file_lists` |
| `test_graph_selection::test_convention_test_is_found_under_a_declared_src_layout` | `convention_tests_follow_configured_layout_and_source_imports` |
| `test_graph_selection::test_convention_test_is_found_under_the_unconfigured_default` | `convention_tests_keep_default_layout_and_skip_absent_roots` |
| `test_graph_selection::test_a_src_tree_is_not_searched_under_the_default_declaration` | `convention_tests_keep_default_layout_and_skip_absent_roots` |
| `test_graph_selection::test_a_test_naming_the_changed_module_is_matched` | `convention_tests_follow_configured_layout_and_source_imports` |
| `test_graph_selection::test_an_unrelated_test_is_not_matched` | `convention_tests_follow_configured_layout_and_source_imports` |
| `test_graph_selection::test_absent_roots_are_skipped` | `convention_tests_keep_default_layout_and_skip_absent_roots` |
| `test_graph_selection::test_module_names_strip_the_declared_package_prefix` | `module_names_strip_only_the_declared_package_prefix` |
| `test_graph_selection::test_module_names_are_unchanged_without_a_prefix` | `module_names_strip_only_the_declared_package_prefix` |
| `test_graph_selection::test_module_names_leave_a_path_outside_the_prefix_alone` | `module_names_strip_only_the_declared_package_prefix` |

| `test_clang_files::test_format_command_passes_one_lines_flag_per_range` | `clang_format_and_tidy_commands_scope_lines_and_database` |
| `test_clang_files::test_format_command_without_ranges_covers_the_whole_file` | `clang_format_and_tidy_commands_scope_lines_and_database` |
| `test_clang_files::test_tidy_is_pointed_at_the_directory_holding_the_database` | `clang_format_and_tidy_commands_scope_lines_and_database` |
| `test_clang_files::test_tidy_line_filter_names_the_basename_and_its_ranges` | `clang_format_and_tidy_commands_scope_lines_and_database` |
| `test_clang_files::test_tidy_without_ranges_sets_no_line_filter` | `clang_format_and_tidy_commands_scope_lines_and_database` |
| `test_clang_files::test_tidy_warnings_are_errors_so_a_finding_fails_the_check` | `clang_format_and_tidy_commands_scope_lines_and_database` |
| `test_clang_files::test_compile_database_beside_the_file_wins` | `clang_database_prefers_nearest_then_build_and_refuses_absent` |
| `test_clang_files::test_build_directory_is_the_fallback` | `clang_database_prefers_nearest_then_build_and_refuses_absent` |
| `test_clang_files::test_no_compile_database_anywhere_is_none` | `clang_database_prefers_nearest_then_build_and_refuses_absent` |
| `test_clang_files::test_format_analyzes_cuda_that_tidy_skips` | `clang_analyzable_distinguishes_cuda_missing_database_and_present_database` |
| `test_clang_files::test_tidy_skips_and_says_why_when_no_database_exists` | `clang_analyzable_distinguishes_cuda_missing_database_and_present_database` |
| `test_clang_files::test_tidy_runs_once_a_database_exists` | `clang_analyzable_distinguishes_cuda_missing_database_and_present_database` |
| `test_clang_files::test_tool_prefers_the_binary_beside_this_interpreter` | `clang_tool_prefers_binary_beside_interpreter` |
| `test_clang_files::test_unchanged_file_is_not_analyzed` | `clang_unchanged_file_never_invokes_subprocess` |
| `test_verification::test_malformed_missing_evidence_row_stays_critical` | `missing_evidence_preserves_malformed_blocking_waiver_and_later_rows` |
| `test_verification::test_malformed_container_and_missing_fields_remain_blocking` | `missing_evidence_preserves_malformed_blocking_waiver_and_later_rows` |
| `test_verification::test_invalid_or_waived_rows_do_not_hide_later_actionable_debt` | `missing_evidence_preserves_malformed_blocking_waiver_and_later_rows` |
| `test_verification::test_new_test_value_findings_read_slim_receipts` | `slim_receipt_detail_still_admits_classified_new_test` |
| `test_engine::test_absent_engine_is_an_empty_hash` | `engine_hash_obeys_layout_source_filtering_and_bytes` |
| `test_engine::test_engine_under_a_declared_src_layout_is_found` | `engine_hash_obeys_layout_source_filtering_and_bytes` |
| `test_engine::test_identical_sources_hash_alike_across_layouts` | `engine_hash_matches_across_layouts_and_ignores_data_only_package` |
| `test_engine::test_engine_under_the_unconfigured_default_is_found` | `engine_hash_matches_across_layouts_and_ignores_data_only_package` |
| `test_engine::test_a_root_level_package_is_not_found_under_a_src_declaration` | `engine_hash_obeys_layout_source_filtering_and_bytes` |
| `test_engine::test_only_python_sources_are_hashed` | `engine_hash_obeys_layout_source_filtering_and_bytes` |
| `test_engine::test_a_changed_source_changes_the_hash` | `engine_hash_obeys_layout_source_filtering_and_bytes` |
| `test_engine::test_the_runtime_root_hashes_this_checkout_s_engine` | `running_checkout_hashes_real_engine_sources` |
| `test_engine::test_a_data_only_package_directory_is_an_absent_engine` | `engine_hash_matches_across_layouts_and_ignores_data_only_package` |
| `test_engine::test_pinned_commit_is_the_lock_fragment` | `pinned_engine_commit_accepts_only_exact_git_lock_fragment` |
| `test_engine::test_pinned_commit_is_none_without_an_exact_git_pin` (7 variants) | `pinned_engine_commit_accepts_only_exact_git_lock_fragment` |
| `test_engine::test_installed_commit_comes_from_the_distribution_provenance` | `installed_engine_commit_requires_valid_git_provenance` |
| `test_engine::test_installed_commit_is_none_without_git_provenance` (4 variants) | `installed_engine_commit_requires_valid_git_provenance` |
| `test_engine::test_installed_commit_is_none_when_the_distribution_is_missing` | `installed_engine_commit_requires_valid_git_provenance` |
| `test_engine::test_in_tree_engine_passes_only_on_an_exact_hash_match` | `engine_findings_prioritize_in_tree_bytes_and_exact_installed_pin` |
| `test_engine::test_in_tree_sources_outrank_installed_provenance` | `engine_findings_prioritize_in_tree_bytes_and_exact_installed_pin` |
| `test_engine::test_installed_engine_passes_when_it_is_the_pinned_commit` | `engine_findings_prioritize_in_tree_bytes_and_exact_installed_pin` |
| `test_engine::test_installed_engine_that_is_not_the_pinned_commit_is_critical` | `engine_findings_prioritize_in_tree_bytes_and_exact_installed_pin` |
| `test_engine::test_no_sources_and_no_matching_pair_of_commits_is_an_absent_engine` (3 variants) | `engine_findings_prioritize_in_tree_bytes_and_exact_installed_pin` |

The remaining 14 candidate-review Python files remain active until their own case maps and Rust replacements pass.
