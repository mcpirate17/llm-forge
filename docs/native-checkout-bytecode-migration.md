# Bytecode isolation, checkout sync, and policy crash test migration

The 23 test cases in three Python test modules now have Rust-owned drivers. They call the shipped Python APIs through PyO3 where behavior depends on Python, use temporary fixture repositories or directories, and retain the exact Python input programs needed to test bytecode caching and traceback frames. They do not import the retired Python test modules or `conftest.py`.

| Retired Python module | Rust target | Cases |
| --- | --- | ---: |
| `src/conductor/test_bytecode_isolation.py` | `python_contracts_bytecode_isolation` | 10 |
| `src/conductor/test_checkout_sync.py` | `python_contracts_checkout_sync` | 7 |
| `src/conductor/test_policy_engine_crash.py` | `python_contracts_policy_engine_crash` | 6 |

## Case correspondence

| Python test function (without `test_`) | Rust test function |
| --- | --- |
| `a_same_second_edit_runs_stale_code_until_the_env_is_isolated` | `same_second_edit_runs_stale_code_until_env_is_isolated` |
| `the_env_carries_base_plus_the_run_private_prefix` | `env_carries_base_plus_run_private_prefix` |
| `the_scratch_is_marked_the_moment_it_is_created` | `scratch_is_marked_the_moment_it_is_created` |
| `each_scratch_names_its_own_prefix` | `each_scratch_names_its_own_prefix` |
| `an_unusable_scratch_fails_loud` | `unusable_scratch_fails_loud` |
| `a_second_mutant_of_the_same_file_never_reads_the_firsts_bytecode` | `second_mutant_of_same_file_never_reads_first_bytecode` |
| `an_unmutated_module_is_served_from_the_prefix_on_the_second_child` | `unmutated_module_is_served_from_prefix_on_second_child` |
| `eviction_refuses_to_leave_the_runs_own_scratch` | `eviction_refuses_to_leave_runs_own_scratch` |
| `the_runs_scratch_root_is_one_pinned_name` | `runs_scratch_root_is_one_pinned_name` |
| `eviction_reports_exactly_the_files_it_removed` | `eviction_reports_exactly_the_files_it_removed` |
| `a_checkout_already_even_with_the_line_is_left_alone` | `checkout_already_even_with_line_is_left_alone` |
| `a_dirty_checkout_fast_forwards_and_its_whole_tree_is_recoverable` | `dirty_checkout_fast_forwards_and_whole_tree_is_recoverable` |
| `the_snapshot_never_stages_anything_in_the_callers_index` | `snapshot_never_stages_anything_in_callers_index` |
| `a_clean_tree_has_nothing_to_snapshot` | `clean_tree_has_nothing_to_snapshot` |
| `a_tracked_file_changed_on_both_sides_blocks_the_merge_and_names_itself` | `tracked_file_changed_on_both_sides_blocks_merge_and_names_itself` |
| `a_checkout_holding_unlanded_commits_is_refused_not_fast_forwarded` | `checkout_holding_unlanded_commits_is_refused` |
| `a_dry_run_reports_the_move_without_making_it` | `dry_run_reports_move_without_making_it` |
| `a_crashed_check_names_the_line_it_raised_on` | `crashed_check_names_the_line_it_raised_on` |
| `the_frames_reach_the_receipt_even_though_the_message_holds_one_line` | `frames_reach_receipt_even_though_message_holds_one_line` |
| `a_long_traceback_is_truncated_to_the_frames_that_name_the_defect` | `long_traceback_is_truncated_to_frames_naming_defect` |
| `a_crash_finding_carries_no_path_so_it_can_never_read_as_inherited` | `crash_finding_carries_no_path_or_line` |
| `an_unserializable_summary_is_reported_not_fatal` | `unserializable_summary_is_reported_not_fatal` |
| `the_crash_help_names_a_flag_the_gate_actually_accepts` | `crash_help_names_flag_gate_actually_accepts` |

## Fixture and side-effect audit

- Bytecode tests write only under `Case::root()`. Child interpreters receive an explicit environment, and the symlink escape test points only into another temporary fixture directory. The same-size Python programs, pinned file mtime, cache-count/inode assertions, all three optimization cache paths, and 120-second child deadline are preserved.
- Checkout tests initialize an upstream repository and local clone inside `Case::root()`. Fetch uses that local path, with no network remote. `sync` and `snapshot` receive only the fixture clone; the shared Forge checkout is never an argument. Git global/system configuration, Git index and repository environment variables are isolated for these cases.
- Crash tests compile only a small traceback fixture with the original `inner`/`outer` call shape and `test_policy_engine_crash.py` filename. Backlog output is patched to a temporary directory, Python stderr and the traceback cap are restored by guards, and the help check reads the shipped source and runs its parser without a gate execution.
- A source search found no runtime imports of any of the three retired Python test modules in `src/` or `native/`. Historical mutation campaign manifests and receipts mention the old paths as evidence references; preserve them as historical records.

## Discovery registration

`native/conductor-native/src/python_contract_targets.tsv` registers these direct providers:

```text
src/conductor/bytecode_isolation.py	python_contracts_bytecode_isolation
src/conductor/checkout_sync.py	python_contracts_checkout_sync
src/conductor/candidate_review/engine.py	python_contracts_policy_engine_crash
src/conductor/candidate_review/equivalence_probe_check.py	python_contracts_policy_engine_crash
```

The exercised Python dependency rows include:

```text
src/conductor/workspace_hygiene.py	python_contracts_checkout_sync
src/conductor/worktree_reap.py	python_contracts_checkout_sync
src/conductor/project_paths.py	python_contracts_checkout_sync
src/conductor/_native.py	python_contracts_checkout_sync
src/conductor/candidate_review/model.py	python_contracts_policy_engine_crash
src/conductor/slop_ledger.py	python_contracts_policy_engine_crash
src/conductor/slop_gate.py	python_contracts_policy_engine_crash
src/conductor/_native.py	python_contracts_policy_engine_crash
```

The bytecode source already has rows for `python_contracts_mutation_pycache_evict` and `python_contracts_mutation_engine_fest`; keep those providers.

Each target maps its sole Rust helper, `python_contracts/support.rs`. Checkout
also maps the native project-path implementation and bindings. The crash target
maps `native/slop-core/src/lib.rs` and is a declared `SLOP_CONSUMERS` entry:
its backlog case imports `slop_ledger`, which loads that native extension even
though the case does not invoke native ledger operations. A runtime-plan
regression requires the candidate's slop manifest, checks its extension build
and destination, rejects an external manifest symlink, and verifies that the
candidate extension directory precedes ambient Python packages.

## Verification

The audited original modules passed `pytest -q -p no:cacheprovider` with **23 passed**. The three exact Rust targets passed with `cargo test --features python-compat-tests --test python_contracts_bytecode_isolation --test python_contracts_checkout_sync --test python_contracts_policy_engine_crash -- --test-threads=2`: **10 + 7 + 6 passed**. The Rust run used the isolated Python 3 environment at `/tmp/forge-native62-install-venv`, the fresh native extension at `/tmp/forge-contract-native63`, `SQLITE3_LIB_DIR=/tmp/forge-sqlite-link`, and two Cargo build jobs. Logs: `/tmp/forge-bytecode-checkout-crash-pytest.log` and `/tmp/forge-bytecode-checkout-crash-test.log`.

`gh issue list --limit 100` returned no issue covering this migration.
