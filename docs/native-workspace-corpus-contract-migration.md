# Workspace and exposure corpus contract migration

Rust owns fixtures, process and Git setup, callbacks, control flow, and
assertions. PyO3 calls the shipped Python APIs directly. The two Conductor
Python test files (49 cases) retired after the native target selection was
registered. The hook corpus Python file (2 cases) retired when the local-check
policy switched to Rust-owned test selection.

## Case map

The three rows below are one-for-one ordered maps. The exact source and target test functions appear in the listed files, in the same order. Counts were checked from the definitions after formatting.

| Source Python test file | Draft Rust test file | Python cases | Rust cases |
| --- | --- | ---: | ---: |
| `src/conductor/test_workspace_hygiene.py` | `native/conductor-native/tests/python_contracts_workspace_hygiene.rs` | 17 | 17 |
| `src/conductor/test_worktree_reap.py` | `native/conductor-native/tests/python_contracts_worktree_reap.rs` | 32 | 32 |
| `src/tooling/hooks/claude/test_workspace_exposure_parity_corpus.py` | `native/conductor-native/tests/python_contracts_workspace_exposure_corpus.rs` | 2 | 2 |

### Hygiene, in source order

| Python case | Rust case |
| --- | --- |
| `test_default_integration_ref_reads_the_configured_branch` | `configured_branch_is_the_integration_ref` |
| `test_default_integration_ref_falls_back_to_the_local_head_symref` | `local_origin_head_symref_is_the_integration_ref` |
| `test_default_integration_ref_resolves_a_lone_conventional_ref_offline` | `lone_conventional_ref_resolves_without_network` |
| `test_default_integration_ref_offline_ambiguity_needs_the_network_opt_in` | `offline_ambiguity_requires_network_opt_in` |
| `test_default_integration_ref_asks_the_remote_when_nothing_local_is_bound` | `unconventional_line_uses_remote_advertisement_when_allowed` |
| `test_default_integration_ref_prefers_origin_over_the_local_branch` | `origin_ref_wins_over_unpushed_local_head` |
| `test_default_integration_ref_refuses_a_repo_with_no_line_at_all` | `repo_without_any_integration_line_refuses` |
| `test_decide_judges_containment_against_the_resolved_line` | `decide_checks_containment_against_resolved_main` |
| `test_reap_preview_survives_a_main_line_repo` | `reap_preview_survives_a_main_line_repo` |
| `test_live_ref_or_default_resolves_without_a_literal` | `live_ref_resolution_has_no_master_literal` |
| `test_cheap_exposure_counts_degrade_to_unknown_when_offline_cannot_resolve` | `cheap_counts_show_unknown_on_offline_ambiguity` |
| `test_root_is_the_repository_root` | `root_is_the_repository_root` |
| `test_manifest_state_finds_the_configured_registry` | `manifest_state_uses_the_configured_registry` |
| `test_idle_claims_read_paths_from_the_repository_root` | `idle_claims_resolve_claim_paths_from_repository_root` |
| `test_claim_checks_never_spawn_a_second_interpreter` | `claim_checks_never_spawn_a_second_interpreter` |
| `test_exposure_line_reports_the_cheap_counts` | `exposure_line_reports_the_cheap_counts` |
| `test_exposure_line_says_unknown_when_there_is_no_integration_line` | `exposure_line_shows_unknown_without_integration_line` |

### Reaper, in source order

| Python case | Rust case |
| --- | --- |
| `test_parse_worktrees_preserves_safety_markers` | `parse_worktrees_preserves_safety_markers` |
| `test_parse_worktrees_strips_head_branch_prefix_and_bare_marker` | `parse_worktrees_strips_branch_prefix_and_marks_bare_missing` |
| `test_parse_worktrees_keeps_empty_head_and_boolean_marker_lines` | `parse_worktrees_keeps_empty_head_and_boolean_markers` |
| `test_unreadable_processes_are_counted_not_treated_as_active` | `unreadable_process_is_counted_without_blocking` |
| `test_live_process_cwd_is_still_reported` | `live_process_cwd_is_reported` |
| `test_unlistable_proc_root_is_fatal` | `unlistable_proc_root_is_fatal` |
| `test_primary_checkout_is_never_eligible` | `primary_checkout_is_never_eligible` |
| `test_live_process_inside_a_worktree_blocks_removal` | `live_process_inside_tree_blocks_removal` |
| `test_a_process_cwd_deeper_inside_the_tree_also_blocks` | `process_cwd_deeper_inside_tree_blocks_removal` |
| `test_locked_worktree_is_kept` | `locked_worktree_is_kept` |
| `test_current_directory_is_kept` | `current_directory_is_kept` |
| `test_live_lease_keeps_an_idle_worktree` | `live_lease_keeps_idle_worktree` |
| `test_expired_lease_makes_a_dirty_unlanded_worktree_eligible` | `expired_lease_makes_dirty_unlanded_tree_eligible` |
| `test_idle_worktree_with_unlanded_commits_is_eligible` | `idle_unlanded_worktree_is_eligible` |
| `test_busy_unlanded_worktree_without_a_lease_is_held` | `busy_unlanded_tree_without_lease_is_held` |
| `test_merged_head_is_eligible_even_while_busy` | `merged_head_is_eligible_even_while_busy` |
| `test_stale_registration_is_eligible_without_a_directory` | `stale_registration_is_eligible_without_a_directory` |
| `test_remote_branch_gone_needs_a_tracking_ref` | `remote_branch_gone_needs_tracking_ref` |
| `test_idle_probe_failure_is_not_read_as_idle` | `failed_idle_probe_is_not_read_as_idle` |
| `test_idle_probe_ignores_the_hardlinked_venv` | `idle_probe_ignores_hardlinked_venv` |
| `test_apply_keeps_nothing_but_the_moved_checkpoints` | `apply_keeps_only_moved_checkpoints` |
| `test_move_checkpoints_never_overwrites_an_existing_target` | `move_checkpoints_never_overwrites_existing_target` |
| `test_apply_force_removes_a_dirty_tree_and_deletes_the_branch` | `apply_force_removes_dirty_tree_and_deletes_branch` |
| `test_apply_refuses_a_tree_that_became_active_after_the_decision` | `apply_refuses_tree_that_became_active_after_decision` |
| `test_apply_never_touches_an_ineligible_tree` | `apply_never_touches_ineligible_tree` |
| `test_second_apply_is_refused_while_one_holds_the_lock` | `second_apply_is_refused_while_lock_is_held` |
| `test_apply_refuses_to_run_without_a_checkpoint_root` | `apply_refuses_without_checkpoint_root` |
| `test_archive_root_is_no_longer_accepted` | `archive_root_option_is_rejected` |
| `test_apply_checkpoint_root_comes_from_the_environment` | `apply_checkpoint_root_comes_from_environment` |
| `test_main_is_preview_by_default_and_reports_state` | `main_is_preview_by_default_and_reports_state` |
| `test_main_text_output_names_the_state_and_the_dry_run` | `main_text_output_names_state_and_dry_run` |
| `test_slug_is_filesystem_safe` | `slug_is_filesystem_safe` |

### Exposure corpus, in source order

| Python case | Rust case |
| --- | --- |
| `test_fixture_files_exist_and_are_shared_with_the_rust_test` | `fixture_files_exist_and_are_shared_with_native_parity` |
| `test_python_exposure_line_matches_the_frozen_corpus` | `python_exposure_line_matches_frozen_corpus` |

## Providers and importer closure

- Python subjects: `conductor.workspace_hygiene` and `conductor.worktree_reap`. The claim fixture also calls shipped `conductor.candidate_review.ownership.create_claim` and `conductor.candidate_review.model.sha256_json` to create a schema-valid claim.
- Rust fixture provider: `native/conductor-native/tests/python_contracts/workspace_fixture.rs`, using the existing `native/conductor-native/tests/python_contracts/support.rs` for isolated `Case`, Python imports, paths, attribute restoration, and exception checks.
- Shared corpus builder: `native/conductor-native/tests/fixtures/workspace_exposure_corpus.rs`. Both the new Python contract target and the existing Forge parity target use it to rebuild the 16 recipes.
- Frozen corpus providers: `native/forge/tests/fixtures/workspace_exposure_corpus.json` and `native/forge/tests/fixtures/workspace_exposure_expected.json`. The Python contract target checks all 16 shipped Python `exposure_line` strings against the frozen expected lines. The Forge parity target checks all 16 Rust `exposure_line` strings against those same lines.
- Test selection provider rows needed for this cohort: `src/conductor/workspace_hygiene.py` to `python_contracts_workspace_hygiene` and `python_contracts_workspace_exposure_corpus`; `src/conductor/worktree_reap.py` to `python_contracts_workspace_hygiene` and `python_contracts_worktree_reap`; `native/conductor-native/tests/python_contracts/workspace_fixture.rs` to the hygiene and reaper targets; `native/conductor-native/tests/fixtures/workspace_exposure_corpus.rs` and both JSON fixtures to `python_contracts_workspace_exposure_corpus`. The Forge parity target also imports the shared builder and both JSON fixtures.
- A bounded importer search across `src/`, `native/`, `pyproject.toml`, and `Makefile` found no runtime import of the three Python test modules and no import of their test helper functions. References are the native twin’s comment and `docs/native-agent-context-tests-migration.md`. Retiring the Python test files removes no production provider.
- Git porcelain strings and JSON corpus recipes are data consumed by the tested APIs. No Python program input fixture is needed.

## Validation

The three new core targets compiled and passed 17/17, 32/32, and 2/2 tests
with Rust 1.98.0, `python-compat-tests`, the Forge Python environment, and
one test thread. The affected `workspace_exposure_parity` Forge target passed
15/15 tests; its corpus assertion compared all 16 recipes with the frozen
expected lines. Scoped Clippy passed with `-D warnings` for all four targets.
Rustfmt and `git diff --check` passed on the changed files.
The provider registry selects the corpus target when its shared Rust fixture or
either frozen JSON file changes. Discovery passed 32/32 after these rows were
added and rejects a missing or unregistered Rust fixture include.
