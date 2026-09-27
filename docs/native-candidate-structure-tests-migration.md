# Candidate crate-version and structure-audit test migration

The two retired Python modules contained 38 declared tests: 21 crate-version
tests and 17 structure-audit tests. Three structure tests had three parameter
rows each, so pytest collected 44 cases. The two Rust integration targets below
own all 44 assertions. They call the shipped Python checks through PyO3 and do
not import or execute the retired pytest modules.

The crate fixture creates a real temporary Git repository, commits a base
tree, writes a separate candidate snapshot, and passes the actual base tree
OID to `check_crate_version`. Its changes, tree entries, policy, manual/full
review context, and one `installed_version` callback reproduce the original
fixture. The structure fixture writes only temporary Python source snapshots
and uses the original change metadata and review context. All 20 named source
snippets in its positive and negative cases match the former Python strings.
`Case` removes the temporary files after each Rust test.

| Retired crate-version test | Rust test in `python_contracts_candidate_crate_version.rs` |
| --- | --- |
| `test_a_bumped_crate_passes` | `a_bumped_crate_passes` |
| `test_the_missing_bump_is_blocking` | `the_missing_bump_is_blocking` |
| `test_a_manifest_only_change_needs_no_bump` | `a_manifest_only_change_needs_no_bump` |
| `test_a_new_crate_is_not_asked_to_bump` | `a_new_crate_is_not_asked_to_bump` |
| `test_a_workspace_member_is_charged_not_its_root` | `a_workspace_member_is_charged_not_its_root` |
| `test_a_non_rust_change_in_a_crate_is_ignored` | `a_non_rust_change_in_a_crate_is_ignored` |
| `test_a_c_source_change_counts_as_a_build_input` | `a_c_source_change_counts_as_a_build_input` |
| `test_an_inherited_workspace_version_is_reported_not_passed` | `an_inherited_workspace_version_is_reported_not_passed` |
| `test_an_unreadable_manifest_is_reported_not_skipped` | `an_unreadable_manifest_is_reported_not_skipped` |
| `test_installed_drift_is_reported_against_the_running_interpreter` | `installed_drift_is_reported_against_the_running_interpreter` |
| `test_an_uninstalled_crate_reports_no_drift` | `an_uninstalled_crate_reports_no_drift` |
| `test_the_deepest_crate_owns_a_nested_source` | `the_deepest_crate_owns_a_nested_source` |
| `test_a_source_outside_every_crate_is_charged_to_nobody` | `a_source_outside_every_crate_is_charged_to_nobody` |
| `test_adding_an_inline_test_does_not_demand_a_bump` | `adding_an_inline_test_does_not_demand_a_bump` |
| `test_shipped_code_beside_a_test_module_still_demands_a_bump` | `shipped_code_beside_a_test_module_still_demands_a_bump` |
| `test_an_integration_test_directory_is_not_a_build_input` | `an_integration_test_directory_is_not_a_build_input` |
| `test_an_unparseable_source_demands_the_bump` | `an_unparseable_source_demands_the_bump` |
| `test_a_new_source_file_is_a_build_input` | `a_new_source_file_is_a_build_input` |
| `test_a_raw_string_holding_a_brace_does_not_derail_the_stripper` | `a_raw_string_holding_a_brace_does_not_derail_the_stripper` |
| `test_a_commented_brace_does_not_derail_the_stripper` | `a_commented_brace_does_not_derail_the_stripper` |
| `test_a_non_block_cfg_test_item_is_stripped_at_its_semicolon` | `a_non_block_cfg_test_item_is_stripped_at_its_semicolon` |

| Retired structure-audit test and parameter row | Rust test in `python_contracts_candidate_structure_audit.rs` |
| --- | --- |
| `test_cleanup_flags_release_only_on_the_success_path` | `cleanup_flags_release_only_on_the_success_path` |
| `test_cleanup_ignores_structurally_guaranteed_release[finally]` | `cleanup_ignores_release_in_finally` |
| `test_cleanup_ignores_structurally_guaranteed_release[with-statement]` | `cleanup_ignores_release_in_with_statement` |
| `test_cleanup_ignores_structurally_guaranteed_release[ownership-transferred]` | `cleanup_ignores_ownership_transferred_to_caller` |
| `test_connection_leak_gets_its_own_advisory_rule` | `connection_leak_gets_its_own_advisory_rule` |
| `test_cleanup_finding_is_blocking` | `cleanup_finding_is_blocking` |
| `test_lock_order_flags_both_sides_of_an_inversion` | `lock_order_flags_both_sides_of_an_inversion` |
| `test_lock_order_ignores_a_consistent_global_order` | `lock_order_ignores_a_consistent_global_order` |
| `test_lock_order_reports_only_the_changed_side` | `lock_order_reports_only_the_changed_side` |
| `test_abstraction_flags_a_two_method_interface_with_one_implementation` | `abstraction_flags_two_method_interface_with_one_implementation` |
| `test_abstraction_ignores_justified_interfaces[two-implementations]` | `abstraction_ignores_two_implementations` |
| `test_abstraction_ignores_justified_interfaces[structural-protocol]` | `abstraction_ignores_structural_protocol` |
| `test_abstraction_ignores_justified_interfaces[single-method-interface]` | `abstraction_ignores_single_method_interface` |
| `test_config_flags_two_defaults_for_one_key` | `config_flags_two_defaults_for_one_key` |
| `test_config_ignores_one_default_repeated_through_a_constant` | `config_ignores_one_default_repeated_through_a_constant` |
| `test_unbounded_state_flags_a_cache_that_only_grows` | `unbounded_state_flags_a_cache_that_only_grows` |
| `test_unbounded_state_ignores_bounded_containers[has-eviction]` | `unbounded_state_ignores_cache_with_eviction` |
| `test_unbounded_state_ignores_bounded_containers[bounded-maxlen]` | `unbounded_state_ignores_bounded_deque` |
| `test_unbounded_state_ignores_bounded_containers[populated-at-import]` | `unbounded_state_ignores_import_time_registry` |
| `test_unbounded_state_stays_advisory` | `unbounded_state_stays_advisory` |
| `test_audit_is_silent_when_nothing_python_changed` | `audit_is_silent_when_nothing_python_changed` |
| `test_audit_skips_unparseable_modules_without_failing` | `audit_skips_unparseable_modules_without_failing` |
| `test_audit_findings_are_sorted_by_path_then_line` | `audit_findings_are_sorted_by_path_then_line` |

The original modules passed all 44 cases before retirement. The two Rust
targets passed 21 and 23 cases respectively under `python-compat-tests` with
one test thread, an unchanged release Forge 0.8.0 binary, two Cargo jobs, and
CUDA masked. CI already selects them through `--test 'python_contracts_*'`.
A repository-wide exact-name search found no active import or selector for the
retired modules or their private helpers. No production module, shared test
support, or historical mutation evidence changed. `gh issue list --limit 30`
showed no open issue covering this test migration.
