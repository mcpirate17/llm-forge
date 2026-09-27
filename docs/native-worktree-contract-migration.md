# Worktree contract migration

This cohort gives Rust ownership of the assertions and fixture data for four
Python API contract suites. The production Python APIs remain the compatibility
subjects called through PyO3. None of the Rust targets imports `conftest.py` or
a Python test module.

| Python source | Rust target | Expanded cases |
| --- | --- | ---: |
| `src/conductor/test_worktree_runtime.py` | `python_contracts_worktree_runtime` | 3 |
| `src/conductor/test_worktree_lease.py` | `python_contracts_worktree_lease` | 11 |
| `src/conductor/test_snapshot_worktree.py` | `python_contracts_snapshot_worktree` | 7 |
| `src/conductor/test_crg_seed_worktree.py` | `python_contracts_crg_seed_worktree` | 7 |
| **Total** | | **28** |

## Case map

| Python case | Rust case |
| --- | --- |
| `test_relocation_is_idempotent_and_leaves_non_python_or_nonregular_entries_linked` | `relocation_is_idempotent_and_leaves_non_python_or_nonregular_entries_linked` |
| `test_relocation_refuses_the_same_or_incomplete_venv` | `relocation_refuses_same_or_incomplete_venv` |
| `test_relocation_refuses_a_destination_bin_symlink_to_the_source` | `relocation_refuses_destination_bin_symlink_without_touching_source` |
| `test_a_lease_records_the_owner_purpose_and_a_deadline_it_computed` | `lease_records_trimmed_owner_purpose_and_computed_deadline` |
| `test_an_unexplained_or_unowned_lease_is_refused` | `unexplained_or_unowned_lease_is_refused_without_file` |
| `test_a_lease_longer_than_a_week_or_no_time_at_all_is_refused[0.0]` | `lease_hours_refuses_zero` |
| `test_a_lease_longer_than_a_week_or_no_time_at_all_is_refused[-1.0]` | `lease_hours_refuses_negative` |
| `test_a_lease_longer_than_a_week_or_no_time_at_all_is_refused[169.0]` | `lease_hours_refuses_over_one_week` |
| `test_a_tree_with_no_lease_reads_as_none_but_a_broken_one_raises` | `absent_lease_is_none_but_invalid_json_and_schema_raise` |
| `test_a_lease_missing_its_deadline_is_broken_not_merely_empty` | `lease_missing_deadline_is_broken_not_empty` |
| `test_state_separates_a_live_lease_from_an_expired_one_and_from_none` | `state_distinguishes_live_expired_and_unleased_with_overdue_minutes` |
| `test_state_skips_a_registration_whose_directory_is_gone` | `state_skips_registration_for_missing_directory` |
| `test_the_main_checkout_is_not_a_disposable_worktree` | `main_checkout_is_not_a_disposable_worktree` |
| `test_the_cli_defaults_the_owner_to_the_tree_it_is_leasing` | `cli_defaults_owner_to_tree_name_and_prints_record` |
| `test_snapshot_reproduces_dirty_tree_without_polluting_host_objects` | `snapshot_reproduces_dirty_tree_without_polluting_host_objects` |
| `test_fixture_trees_survive_a_snapshot_regardless_of_suffix` | `fixture_trees_survive_snapshot_regardless_of_suffix` |
| `test_extra_snapshot_suffixes_are_configurable_outside_fixture_trees` | `extra_snapshot_suffixes_are_configurable_outside_fixture_trees` |
| `test_snapshot_exception_removes_temporary_repository` | `snapshot_exception_removes_temporary_repository` |
| `test_snapshot_from_linked_worktree_leaves_shared_objects_unchanged` | `snapshot_from_linked_worktree_keeps_shared_objects_unchanged` |
| `test_the_exported_interpreter_defaults_to_the_running_one` | `exported_interpreter_defaults_to_running_one_and_validates_override` |
| `test_the_exported_interpreter_imports_conductor_inside_the_snapshot` | `exported_interpreter_imports_conductor_inside_snapshot_without_venv` |
| `test_rewrite_replaces_the_prefix_in_every_text_column_and_spares_other_rows` | `rewrite_replaces_prefix_in_every_text_column_and_spares_other_rows` |
| `test_rewrite_skips_the_fts_virtual_table_and_its_shadow_tables` | `rewrite_skips_fts_virtual_and_shadow_tables` |
| `test_fts_rebuild_reindexes_the_rewritten_rows` | `fts_rebuild_reindexes_rewritten_rows` |
| `test_copy_refuses_while_a_wal_sits_beside_the_source` | `copy_refuses_source_wal_unless_forced` |
| `test_resolve_crg_bin_prefers_the_env_override_and_fails_loud_when_absent` | `resolve_crg_bin_prefers_override_and_fails_loud_when_absent` |
| `test_seed_stamps_the_head_when_update_left_it_at_the_main_checkouts_sha` | `seed_stamps_head_when_update_kept_main_checkout_sha` |
| `test_seed_reports_no_stamp_when_update_already_matched_the_worktree_head` | `seed_reports_no_stamp_when_update_matched_worktree_head` |

## Fixture and provider boundaries

`python_contracts/worktree_migration_support.rs` shares the `Case` process
mutex, clears and restores every inherited `GIT_*` selector, disables system
Git configuration, and routes global configuration to `/dev/null`. Its
repositories, linked worktree, and snapshot contexts are created only beneath
the per-test temporary root. It does not inspect or alter host worktrees,
claims, model processes, GPUs, or network state.

The CRG seed tests intercept `conductor.crg_seed_worktree.subprocess.run` only
when the command executable is exactly `/fake/crg`. The callback stamps the
temporary graph database through the production `stamp_head` API and returns a
successful `CompletedProcess`; every other subprocess call delegates to the
captured original `subprocess.run`, so Git operations and the seeder's command
construction still run.

The provider rows for the registry are:

```text
src/conductor/worktree_runtime.py	python_contracts_worktree_runtime
native/conductor-native/tests/python_contracts/support.rs	python_contracts_worktree_runtime
src/conductor/worktree_lease.py	python_contracts_worktree_lease
native/conductor-native/tests/python_contracts/support.rs	python_contracts_worktree_lease
native/conductor-native/tests/python_contracts/worktree_migration_support.rs	python_contracts_worktree_lease
src/conductor/snapshot_worktree.py	python_contracts_snapshot_worktree
src/conductor/project_paths.py	python_contracts_snapshot_worktree
src/conductor/mutation_engine_generated.py	python_contracts_snapshot_worktree
src/conductor/mutation_campaign_model.py	python_contracts_snapshot_worktree
src/conductor/mutation_testing_support.py	python_contracts_snapshot_worktree
src/conductor/mutation_scope.py	python_contracts_snapshot_worktree
src/conductor/mutation_value.py	python_contracts_snapshot_worktree
src/conductor/mutation_patch_apply.py	python_contracts_snapshot_worktree
src/conductor/bytecode_isolation.py	python_contracts_snapshot_worktree
src/conductor/_native.py	python_contracts_snapshot_worktree
native/conductor-native/src/project_paths.rs	python_contracts_snapshot_worktree
native/conductor-native/src/lib.rs	python_contracts_snapshot_worktree
native/conductor-native/tests/python_contracts/support.rs	python_contracts_snapshot_worktree
native/conductor-native/tests/python_contracts/worktree_migration_support.rs	python_contracts_snapshot_worktree
src/conductor/crg_seed_worktree.py	python_contracts_crg_seed_worktree
native/conductor-native/tests/python_contracts/support.rs	python_contracts_crg_seed_worktree
native/conductor-native/tests/python_contracts/worktree_migration_support.rs	python_contracts_crg_seed_worktree
```

The snapshot target imports `mutation_engine_generated`; its eager local import
closure includes `mutation_patch_apply` through `mutation_testing_support` and
`mutation_value` through `mutation_campaign_model`. Neither target calls
`slop_core()`, so this cohort does not require candidate-local `slop-core`.
An executable source audit found no imports of the four Python test modules
outside their own files; pytest collection is their only executable importer.
The target-to-case map above records the assertions retained when the four
Python originals are retired from this cohort.
