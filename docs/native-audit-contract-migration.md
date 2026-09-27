# Native audit contract migration

Three Python suites have Rust-owned PyO3 contracts for their shipped behavior.
The Rust targets own the assertions and fixture data; they call production
Python modules rather than importing the Python test files. The original
files remain available for retirement review.

| Original suite | Rust target | Expanded cases |
| --- | --- | ---: |
| `src/conductor/test_ci_history_fetch.py` | `python_contracts_ci_history_fetch` | 11 |
| `src/conductor/test_ref_aware_governance.py` | `python_contracts_ref_aware_governance` | 8 |
| `src/conductor/test_run_duplicate_audit.py` | `python_contracts_run_duplicate_audit` | 31 |
| **Total** | | **50** |

| `test_ci_history_fetch.py` case | Rust case |
| --- | --- |
| `test_the_schema_round_trips_the_rust_fixture` | `the_schema_round_trips_the_rust_fixture` |
| `test_first_push_ci_classifies_the_recorded_check_runs` | `first_push_ci_classifies_the_recorded_check_runs` |
| `test_the_merged_listing_always_names_an_explicit_limit` | `the_merged_listing_always_names_an_explicit_limit` |
| `test_trailers_come_from_interpret_trailers_not_a_regex` | `trailers_come_from_interpret_trailers_not_a_regex` |
| `test_a_full_fetch_reads_the_pr_head_and_writes_the_cache` | `a_full_fetch_reads_the_pr_head_and_writes_the_cache` |
| `test_an_incremental_run_leaves_cached_prs_alone` | `an_incremental_run_leaves_cached_prs_alone` |
| `test_an_unresolvable_pr_keeps_every_resolvable_one` | `an_unresolvable_pr_keeps_every_resolvable_one` |
| `test_main_exits_2_without_gh_and_writes_nothing` | `main_exits_2_without_gh_and_writes_nothing` |
| `test_main_exits_2_on_an_unauthenticated_gh` | `main_exits_2_on_an_unauthenticated_gh` |
| `test_main_exits_2_on_an_unreadable_existing_cache` | `main_exits_2_on_an_unreadable_existing_cache` |
| `test_dry_run_calls_no_gh_and_writes_nothing` | `dry_run_calls_no_gh_and_writes_nothing` |

| `test_ref_aware_governance.py` case | Rust case |
| --- | --- |
| `test_guardrail_audit_reads_staged_snapshot` | `guardrail_audit_reads_staged_snapshot` |
| `test_guardrail_audit_reads_head_for_from_ref_with_clean_index` | `guardrail_audit_reads_head_for_from_ref_with_clean_index` |
| `test_protected_delete_reads_staged_index` | `protected_delete_reads_staged_index` |
| `test_protected_delete_reads_from_ref_with_clean_index` | `protected_delete_reads_from_ref_with_clean_index` |
| `test_duplicate_body_reads_staged_index` | `duplicate_body_reads_staged_index` |
| `test_duplicate_body_reads_from_ref_with_clean_index` | `duplicate_body_reads_from_ref_with_clean_index` |
| `test_duplicate_body_from_ref_allows_move` | `duplicate_body_from_ref_allows_move` |
| `test_duplicate_body_git_and_cli_fail_closed` | `duplicate_body_git_and_cli_fail_closed` |

| `test_run_duplicate_audit.py` case | Rust case |
| --- | --- |
| `test_resolve_audit_root_explicit_path_wins` | `resolve_audit_root_explicit_path_wins` |
| `test_resolve_audit_root_fails_closed_outside_git` | `resolve_audit_root_fails_closed_outside_git` |
| `test_main_passes_cwd_git_root_to_selected_tool` | `main_passes_cwd_git_root_to_selected_tool` |
| `test_main_threads_changed_file_cli_flags_to_baseline_supported_tool` | `main_threads_changed_file_cli_flags_to_baseline_supported_tool` |
| `test_jscpd_live_scan_uses_git_visible_sources` | `jscpd_live_scan_uses_git_visible_sources` |
| `test_materialized_sources_are_exact_index_blobs` | `materialized_sources_are_exact_index_blobs` |
| `test_vulture_check_ignores_untracked_but_fails_for_index_violation` | `vulture_check_ignores_untracked_but_fails_for_index_violation` |
| `test_jscpd_check_ignores_untracked_but_fails_for_index_duplicates` | `jscpd_check_ignores_untracked_but_fails_for_index_duplicates` |
| `test_jscpd_snapshot_preserves_repository_relative_ignores` | `jscpd_snapshot_preserves_repository_relative_ignores` |
| `test_jscpd_report_failures_are_blocking[nonzero]` | `jscpd_report_nonzero_is_blocking` |
| `test_jscpd_report_failures_are_blocking[missing]` | `jscpd_report_missing_is_blocking` |
| `test_jscpd_report_failures_are_blocking[malformed]` | `jscpd_report_malformed_is_blocking` |
| `test_pmd_report_failures_are_blocking[nonzero]` | `pmd_report_nonzero_is_blocking` |
| `test_pmd_report_failures_are_blocking[missing]` | `pmd_report_missing_is_blocking` |
| `test_pmd_report_failures_are_blocking[malformed]` | `pmd_report_malformed_is_blocking` |
| `test_report_command_rejects_nonzero_exit[jscpd]` | `jscpd_report_command_rejects_nonzero_exit` |
| `test_report_command_rejects_nonzero_exit[pmd-cpd]` | `pmd_report_command_rejects_nonzero_exit` |
| `test_baseline_count_and_entry_keys_are_validated[payload0]` | `baseline_count_mismatch_is_rejected` |
| `test_baseline_count_and_entry_keys_are_validated[payload1]` | `baseline_invalid_key_is_rejected` |
| `test_check_against_baseline_without_changed_files_blocks_every_new_pair` | `baseline_without_changed_files_blocks_every_new_pair` |
| `test_check_against_baseline_caused_via_left_side_blocks` | `baseline_caused_via_left_side_blocks` |
| `test_check_against_baseline_caused_via_right_side_blocks` | `baseline_caused_via_right_side_blocks` |
| `test_check_against_baseline_inherited_via_neither_side_does_not_block` | `baseline_inherited_via_neither_side_does_not_block` |
| `test_check_against_baseline_no_new_findings_exits_zero_either_way` | `baseline_no_new_findings_exits_zero_either_way` |
| `test_check_against_baseline_changed_baseline_only_file_not_reported_as_caused` | `changed_baseline_only_file_is_not_reported_as_caused` |
| `test_jscpd_index_check_reads_staged_baseline` | `jscpd_index_check_reads_staged_baseline` |
| `test_resolve_pmd_executable_prefers_an_explicit_override` | `resolve_pmd_executable_prefers_an_explicit_override` |
| `test_resolve_pmd_executable_prefers_a_project_local_binary` | `resolve_pmd_executable_prefers_a_project_local_binary` |
| `test_resolve_pmd_executable_falls_back_to_path` | `resolve_pmd_executable_falls_back_to_path` |
| `test_resolve_pmd_executable_raises_when_nothing_resolves` | `resolve_pmd_executable_raises_when_nothing_resolves` |
| `test_pmd_index_check_reads_staged_baseline` | `pmd_index_check_reads_staged_baseline` |

`python_contracts/audit_fixture.rs` creates temporary Git repositories and
restores process environment and patched Python attributes. It clears
inherited Git selectors and disables system and global Git configuration.
CI-history calls to `gh` are intercepted, including the unavailable and
unauthenticated cases; the schema fixture follows the outcome join in
`native/forge/src/ledger/outcome.rs`. The ref-aware cases test staged and
`from_ref` content with a real temporary index. The duplicate-audit target
also includes `python_contracts/duplicate_audit_fixture.rs`, which supplies
temporary analyzer inputs, reports, and baseline data. No test contacts
GitHub or scans the shared checkout.

Discovery registration in
`native/conductor-native/src/python_contract_targets.tsv` includes the direct
production modules, their eager import closure, the relevant native Rust
providers, and every direct test helper. `guardrail_audit` eagerly imports
`vulture_audit`, `guardrail_targets`, and `run_duplicate_audit`; the latter
brings in its configuration, changed-file, and project-path helpers. The
duplicate target calls native duplicate-body normalization and baseline
comparison. The ref-aware target also calls native AST metrics and duplicate
body fingerprints. These paths import `conductor._native`, whose `slop_core`
import is opportunistic; no target calls `slop_core()`, so the runner's
`SLOP_CONSUMERS` list needs no addition. Discovery also accepts Rust providers
under `native/forge/src/`, including nested modules, so changes to the outcome
schema producer select the CI-history contracts. A native discovery regression
checks top-level and nested Forge providers while rejecting traversal, invalid
extensions, empty path segments, and paths outside the source directory.

A bounded executable-importer audit found no source import of any of the nine
Python originals in this broader migration. `candidate_review/benchmark.py`
still requires a host-specific `conductor/` layout and several LLM-only paths,
including old test filenames. Those are different from Forge's retired
`src/conductor/` paths; this preexisting limitation prevents using that benchmark
directly on Forge and remains outside this migration. Its existing native
contracts synthesize the required host layout. Historical mutation fixtures
and baselines also carry literal old test paths as data.

The draft's 50 native cases and scoped Clippy check passed in the prior
validation lane. The integrating lane reruns them with the selected native
extension, checks discovery, and handles original-test retirement.
