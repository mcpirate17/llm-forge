# Native packaging contract migration

The three Python suites collect 64 cases: 23 resource, one installed-layout,
and 40 tooling-boundary cases. Each row maps one expanded Python case to an
executable Rust assertion. The Rust tests call shipped Python APIs through
PyO3 or isolated subprocesses. They do not import Python test modules.

| Original `test_package_resources.py` case | `python_contracts_package_resources` case |
| --- | --- |
| `test_reads_verified_bytes_from_an_installed_unpacked_wheel` | `reads_verified_bytes_from_an_installed_unpacked_wheel` |
| `test_refuses_invalid_resource_selectors('other', 'fixture.txt')` | `invalid_selector_other_package` |
| `test_refuses_invalid_resource_selectors('tooling', '')` | `invalid_selector_empty_name` |
| `test_refuses_invalid_resource_selectors('tooling', '/fixture.txt')` | `invalid_selector_absolute_name` |
| `test_refuses_invalid_resource_selectors('tooling', '../fixture.txt')` | `invalid_selector_parent_component` |
| `test_refuses_invalid_resource_selectors('tooling', 'nested//fixture.txt')` | `invalid_selector_empty_component` |
| `test_refuses_invalid_resource_selectors('tooling', 'nested\\fixture.txt')` | `invalid_selector_backslash` |
| `test_refuses_invalid_resource_selectors('tooling', 'fixture\x00.txt')` | `invalid_selector_nul` |
| `test_refuses_foreign_namespace_pollution` | `refuses_foreign_namespace_pollution` |
| `test_refuses_actual_project_shadow_before_resource_traversal` | `refuses_actual_project_shadow_before_resource_traversal` |
| `test_refuses_missing_record` | `refuses_missing_record` |
| `test_requires_exact_record_path_membership` | `requires_exact_record_path_membership` |
| `test_refuses_malformed_record_hash_and_size(1, 'md5=not-a-wheel-digest')` | `malformed_record_hash_is_refused` |
| `test_refuses_malformed_record_hash_and_size(2, 'not-a-size')` | `malformed_record_size_is_refused` |
| `test_refuses_record_declared_over_limit_before_read` | `refuses_record_declared_over_limit_before_read` |
| `test_refuses_duplicate_installed_distribution` | `refuses_duplicate_installed_distribution` |
| `test_refuses_editable_and_malformed_direct_url_metadata` | `refuses_editable_and_malformed_direct_url_metadata` |
| `test_refuses_symlink_oversize_and_record_drift` | `refuses_symlink_oversize_and_record_drift` |
| `test_refuses_intermediate_symlink_and_requires_record_path_membership` | `refuses_intermediate_symlink_and_requires_record_path_membership` |
| `test_refuses_asset_path_replaced_after_open` | `refuses_asset_path_replaced_after_open` |
| `test_refuses_fifo_swapped_before_open_without_blocking` | `refuses_fifo_swapped_before_open_without_blocking` |
| `test_refuses_same_byte_symlink_swapped_between_validation_and_open` | `refuses_same_byte_symlink_swapped_between_validation_and_open` |
| `test_anchored_walk_refuses_intermediate_directory_swapped_to_external_same_bytes` | `anchored_walk_refuses_intermediate_directory_swapped_to_external_same_bytes` |

| Original `test_installed_layout.py` case | `python_contracts_installed_layout` case |
| --- | --- |
| `test_installed_layout_resolves_host_not_site` | `installed_layout_resolves_host_not_site` |

| Original `test_tooling_boundary.py` case | `python_contracts_tooling_boundary` case |
| --- | --- |
| `test_rule_a_no_conductor_module_reaches_a_project_package` | `rule_a_no_conductor_module_reaches_a_project_package` |
| `test_rule_b_generic_hooks_carry_no_project_literal` | `rule_b_generic_hooks_carry_no_project_literal` |
| `test_rule_c_non_test_modules_carry_no_host_path_literal` | `rule_c_non_test_modules_carry_no_host_path_literal` |
| `test_rule_d_native_seam_is_the_only_seam_and_exports_real_symbols` | `rule_d_native_seam_is_the_only_seam_and_exports_real_symbols` |
| `test_cli_reports_clean_on_this_repo` | `cli_reports_clean_on_this_repo` |
| `test_repo_root_is_the_tree_that_declares_the_package` | `repo_root_is_the_tree_that_declares_the_package` |
| `test_cli_refuses_a_root_with_no_package_naming_the_configured_path` | `cli_refuses_a_root_with_no_package_naming_the_configured_path` |
| `test_cli_finds_a_package_the_tree_declares_under_src` | `cli_finds_a_package_the_tree_declares_under_src` |
| `test_allowlist_entries_carry_a_reason_and_name_existing_files` | `allowlist_entries_carry_a_reason_and_name_existing_files` |
| `test_rule_a_flags_module_scope_import_with_file_and_line` | `rule_a_flags_module_scope_import_with_file_and_line` |
| `test_rule_a_flags_function_scope_and_try_except_imports` | `rule_a_flags_function_scope_and_try_except_imports` |
| `test_rule_a_flags_importlib_string_literal` | `rule_a_flags_importlib_string_literal` |
| `test_rule_a_ignores_non_module_strings` | `rule_a_ignores_non_module_strings` |
| `test_rule_a_rejects_host_plugin_string_in_generic_module` | `rule_a_rejects_host_plugin_string_in_generic_module` |
| `test_rule_a_scans_tests_too` | `rule_a_scans_tests_too` |
| `test_rule_b_flags_each_literal_in_generic_hooks('research/')` | `rule_b_flags_research_literal` |
| `test_rule_b_flags_each_literal_in_generic_hooks('/home/tim')` | `rule_b_flags_home_literal` |
| `test_rule_b_flags_each_literal_in_generic_hooks('/mnt/data')` | `rule_b_flags_data_literal` |
| `test_rule_b_default_hook_dirs_cover_repo_and_standalone_layouts` | `rule_b_default_hook_dirs_cover_repo_and_standalone_layouts` |
| `test_rule_c_flags_host_paths_in_non_test_modules_only('/home/tim')` | `rule_c_flags_home_literal` |
| `test_rule_c_flags_host_paths_in_non_test_modules_only('/mnt/data')` | `rule_c_flags_data_literal` |
| `test_rule_d_flags_a_symbol_the_crate_does_not_export` | `rule_d_flags_a_symbol_the_crate_does_not_export` |
| `test_rule_d_flags_the_crate_named_outside_the_seam` | `rule_d_flags_the_crate_named_outside_the_seam` |
| `test_rule_d_flags_a_seam_importing_anything_but_the_crate` | `rule_d_flags_a_seam_importing_anything_but_the_crate` |
| `test_project_hooks_unset_without_configuration_resolves_none` | `project_hooks_unset_without_configuration_resolves_none` |
| `test_project_hooks_invokes_configured_callable` | `project_hooks_invokes_configured_callable` |
| `test_project_hooks_environment_bypasses_malformed_config` | `project_hooks_environment_bypasses_malformed_config` |
| `test_project_hooks_refuse_unreadable_or_oversized_config` | `project_hooks_refuse_unreadable_or_oversized_config` |
| `test_project_hooks_missing_config_sections_resolve_none('')` | `missing_config_file_content_is_none` |
| `test_project_hooks_missing_config_sections_resolve_none('[tool]\n')` | `missing_tool_section_is_none` |
| `test_project_hooks_missing_config_sections_resolve_none('[tool.conductor]\n')` | `missing_conductor_section_is_none` |
| `test_project_hooks_missing_config_sections_resolve_none('[tool.conductor.pytest]\n')` | `missing_pytest_section_is_none` |
| `test_project_hooks_config_and_environment_precedence` | `project_hooks_config_and_environment_precedence` |
| `test_project_hooks_refuse_malformed_or_wrong_type_config('[tool', 'invalid TOML configuration')` | `malformed_toml_is_refused` |
| `test_project_hooks_refuse_malformed_or_wrong_type_config('[tool]\nconductor = []\n', '[tool.conductor] must be a table')` | `non_table_conductor_is_refused` |
| `test_project_hooks_refuse_malformed_or_wrong_type_config('[tool.conductor]\npytest = []\n', '[tool.conductor.pytest] must be a table')` | `non_table_pytest_is_refused` |
| `test_project_hooks_refuse_malformed_or_wrong_type_config('[tool.conductor.pytest]\ntest_plugin = 1\n', 'test_plugin must be a string')` | `non_string_plugin_is_refused` |
| `test_project_hooks_refuse_noncallable_configured_attribute` | `project_hooks_refuse_noncallable_configured_attribute` |
| `test_project_hooks_empty_spec_means_no_guard` | `project_hooks_empty_spec_means_no_guard` |
| `test_project_hooks_bogus_spec_fails_loud` | `project_hooks_bogus_spec_fails_loud` |

The resource suite builds one real no-dependency wheel containing current
`project_context.py` and `package_resources.py`, installs it into an isolated
venv, and copies that baseline per case. Rust owns the fixture bytes, RECORD
rows, and wheel ZIP serialization. A precompiled Rust child initializes Python
against each private venv with isolated `PyConfig`, checks the installed module
origin and `sys.path`, and calls the shipped reader through PyO3. Rust callbacks
control the shadow, descriptor, FIFO, and symlink race fixtures; Rust verifies
that each callback fired or remained blocked as the case requires. The reader
returns structured results to the Rust assertions. The resource target's
runtime provider closure includes `package_resources.py`, `project_context.py`,
`package_resources_support.rs`, and `tests/fixtures/package_resources_child.rs`.
The shared baseline is built once per test process and removed on process exit,
including after filtered or serial runs.
The installed-layout case unconditionally installs the Forge package into a
scratch site directory and checks the session preamble, active-state dump,
and gate help from a fake host repository. The boundary suite uses Rust-owned
synthetic trees and PyO3 calls, with scoped callbacks for monkeypatch cases.

All 64 replacement cases passed under `python-compat-tests`. After replacing
the resource callbacks and wheel builder with Rust, all 23 resource cases and
the unconditional installed-layout case passed again; scoped Clippy passed
with warnings denied. A filtered resource run passed with Cargo absent from
`PATH`, and serial and filtered runs left no baseline fixture directory behind.
The Python suites are retired in this cohort. Discovery validates the provider
mappings, and the full local check and verification receipt remain required on
the committed tree before landing.
