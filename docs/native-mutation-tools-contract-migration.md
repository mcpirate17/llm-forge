# Mutation campaign generation and retention contract migration

The original Python suites contain 57 named tests and 58 statically expanded cases: 26 for campaign generation and 32 for retention. The two Rust contract targets map each expanded case to a separate Rust test. The 27 patch-audit cases remain outside this cohort.

Rust owns fixture setup, callbacks, control flow, and assertions. Small synthetic Python source strings are parser inputs that the production planner needs; they are fixture data, and no Python test module or test helper is imported. The tests plan synthetic campaigns and construct synthetic receipts, but do not run mutation engines or real campaigns.

## Exact case maps

### test_mutation_campaign_generate.py (26 expanded cases)

| Original pytest case | Rust case |
| --- | --- |
| `test_test_file_recognition_covers_all_supported_layouts_only` | `test_file_recognition_covers_supported_layouts_only` |
| `test_rust_planning_intersects_scope_instead_of_expanding_to_the_crate` | `rust_planning_intersects_scope_instead_of_expanding_crate` |
| `test_a_crate_with_a_package_but_no_sources_is_refused` | `crate_with_package_but_no_sources_is_refused` |
| `test_an_unmirrored_basename_collision_refuses_naming_both_candidates` | `unmirrored_basename_collision_refuses_both_candidates` |
| `test_a_tests_mirror_beats_a_same_basename_stranger` | `tests_mirror_beats_same_basename_stranger` |
| `test_a_subject_a_committed_campaign_already_covers_is_skipped` | `committed_campaign_subject_is_skipped` |
| `test_write_refuses_to_replace_a_recorded_baseline` | `write_refuses_to_replace_recorded_baseline` |
| `test_a_generated_rust_manifest_loads_as_a_generated_campaign` | `generated_rust_manifest_loads_as_generated_campaign` |
| `test_a_generated_python_manifest_runs_the_tests_that_name_its_subject` | `generated_python_manifest_runs_tests_naming_its_subject` |
| `test_shared_dirty_scope_never_admits_another_lanes_files` | `shared_dirty_scope_excludes_other_lanes_files` |
| `test_explicit_scope_is_exact_and_does_not_read_shared_dirty_files` | `explicit_scope_is_exact_and_ignores_shared_dirty_files` |
| `test_default_campaign_day_is_utc_and_an_explicit_day_wins` | `default_campaign_day_is_utc_and_explicit_day_wins` |
| `test_a_git_scope_failure_without_output_is_actionable` | `git_scope_failure_without_output_is_actionable` |
| `test_a_git_scope_failure_prefers_stderr_over_an_empty_stdout` | `git_scope_failure_prefers_stderr_over_empty_stdout` |
| `test_an_empty_scope_is_refused_instead_of_becoming_a_whole_tree_sweep` | `empty_scope_refuses_whole_tree_sweep` |
| `test_refresh_rebinds_a_cargo_campaign_to_the_sources_it_was_asked_for` | `refresh_rebinds_cargo_campaign_to_requested_sources` |
| `test_rust_refresh_rejects_empty_or_unbound_implicit_scope[declared0]` | `rust_refresh_rejects_empty_implicit_scope` |
| `test_rust_refresh_rejects_empty_or_unbound_implicit_scope[declared1]` | `rust_refresh_rejects_unbound_implicit_scope` |
| `test_rust_plan_reports_an_untested_scoped_crate_without_hiding_its_identity` | `rust_plan_reports_untested_scoped_crate_with_identity` |
| `test_refresh_rebinds_a_fest_campaign_without_erasing_its_baseline` | `refresh_rebinds_fest_campaign_without_erasing_baseline` |
| `test_refresh_carries_an_extra_test_campaign_forward_without_erasing_its_baseline` | `refresh_carries_extra_test_campaign_without_erasing_baseline` |
| `test_refresh_refuses_anything_that_is_not_a_generated_cargo_campaign` | `refresh_refuses_non_generated_cargo_campaign` |
| `test_rust_refresh_repairs_one_legacy_manifest_path_and_retains_its_note` | `rust_refresh_repairs_legacy_manifest_path_and_retains_note` |
| `test_rust_refresh_refuses_an_ambiguous_legacy_package_path` | `rust_refresh_refuses_ambiguous_legacy_package_path` |
| `test_a_narrow_second_campaign_may_be_planned_over_a_covered_source` | `narrow_second_campaign_can_cover_existing_source` |
| `test_an_extra_test_pairs_a_subject_no_test_is_named_after` | `extra_test_pairs_subject_without_matching_test_name` |

### test_mutation_retention.py (32 expanded cases)

| Original pytest case | Rust case |
| --- | --- |
| `test_superseded_pass_receipts_are_swept_and_the_newest_survives` | `superseded_pass_receipts_are_swept_and_newest_survives` |
| `test_the_newest_pass_is_decided_by_the_gates_ordering_not_by_filename` | `newest_pass_uses_gate_ordering_not_filename` |
| `test_a_cited_receipt_survives_even_when_a_newer_pass_exists` | `cited_receipt_survives_newer_pass` |
| `test_a_citation_outranks_a_missing_manifest_too` | `citation_outranks_missing_manifest` |
| `test_receipts_of_a_campaign_with_no_manifest_are_swept` | `receipts_without_manifest_are_swept` |
| `test_a_manifest_is_matched_by_its_campaign_id_not_its_filename` | `manifest_is_matched_by_campaign_id_not_filename` |
| `test_a_broken_registry_does_not_refuse_the_sweep` | `broken_registry_does_not_refuse_sweep` |
| `test_a_missing_campaign_directory_refuses_the_sweep` | `missing_campaign_directory_refuses_sweep` |
| `test_a_campaign_with_no_pass_keeps_every_receipt` | `campaign_with_no_pass_keeps_every_receipt` |
| `test_a_failing_receipt_is_swept_once_a_pass_supersedes_it` | `failing_receipt_is_swept_after_pass` |
| `test_an_unparseable_receipt_is_kept_not_swept` | `unparseable_receipt_is_kept_not_swept` |
| `test_a_receipt_naming_no_campaign_is_kept_not_swept` | `receipt_without_campaign_id_is_kept_not_swept` |
| `test_a_gate_that_cannot_run_refuses_the_sweep` | `gate_failure_refuses_sweep_and_preserves_files` |
| `test_an_unreadable_manifest_refuses_the_sweep` | `unreadable_manifest_refuses_sweep_and_preserves_receipt` |
| `test_a_missing_receipt_directory_refuses_the_sweep` | `missing_receipt_directory_refuses_sweep` |
| `test_protect_overrides_the_rule` | `protect_overrides_deletion_rule` |
| `test_plan_touches_nothing_and_apply_removes_exactly_the_planned_files` | `plan_touches_nothing_and_apply_removes_exactly_planned_files` |
| `test_cli_reports_without_applying_and_applies_when_told` | `cli_reports_plan_then_applies_only_when_told` |
| `test_the_cli_reports_an_undecidable_corpus_as_exit_two` | `cli_reports_undecidable_corpus_as_exit_two` |
| `test_cited_receipts_reads_the_receipt_field_of_every_evidence_row` | `cited_receipts_uses_receipt_field_of_every_evidence_row` |
| `test_a_coverage_gate_that_raises_becomes_a_retention_error` | `coverage_gate_exception_becomes_retention_error` |
| `test_an_evidence_row_without_a_receipt_path_refuses` | `evidence_row_without_receipt_path_refuses` |
| `test_a_receipt_the_corpus_audit_reads_survives_a_newer_pass` | `receipt_read_by_corpus_audit_survives_newer_pass` |
| `test_the_audit_keeps_only_the_receipt_the_audit_itself_would_read` | `audit_keeps_only_receipt_it_would_read` |
| `test_a_receipt_the_audit_rejects_is_never_read` | `receipt_rejected_by_audit_is_never_read` |
| `test_an_audit_that_cannot_read_the_runner_refuses_the_sweep` | `audit_runner_failure_refuses_sweep` |
| `test_the_audit_double_carries_the_production_seams_signature` | `audit_predicate_seam_has_production_signature` |
| `test_audited_receipts_runs_end_to_end_without_patching_the_predicate` | `audited_receipts_uses_real_manifest_and_rejection_predicate` |
| `test_an_orphan_campaign_does_not_stop_the_audit_later_receipts_still_are` | `orphan_campaign_does_not_stop_audit_of_later_receipts` |
| `test_the_report_counts_what_protection_saved` | `report_counts_receipts_saved_by_explicit_protection` |
| `test_a_no_pass_campaign_is_stepped_over_not_terminal` | `no_pass_campaign_is_skipped_while_later_campaign_is_swept` |
| `test_compacted_receipts_are_judged_by_their_summary_alone` | `compacted_receipts_are_judged_by_summary_alone` |

## Provider closure for registry integration

Map these paths to `python_contracts_mutation_campaign_generate`:

```
src/conductor/mutation_campaign_generate.py
src/conductor/mutation_plan_bridge.py
src/conductor/mutation_campaign_model.py
src/conductor/mutation_engine_generated.py
src/conductor/mutation_run_scope.py
src/conductor/mutation_receipt_slim.py
src/conductor/snapshot_worktree.py
src/conductor/mutation_scope.py
src/conductor/mutation_testing_support.py
src/conductor/mutation_testing.py
src/conductor/mutation_receipt_build.py
src/conductor/mutation_value.py
src/conductor/mutation_patch_apply.py
src/conductor/bytecode_isolation.py
src/conductor/project_paths.py
src/conductor/candidate_review/identity.py
src/conductor/candidate_review/ownership.py
src/conductor/candidate_review/git_source.py
src/conductor/candidate_review/model.py
src/conductor/_native.py
native/conductor-native/src/lib.rs
native/conductor-native/src/mutation_plan.rs
native/conductor-native/src/receipt_slim.rs
native/conductor-native/src/mutation_value.rs
native/conductor-native/src/mutation_value_inputs.rs
native/conductor-native/src/project_paths.rs
native/conductor-native/tests/python_contracts/support.rs
native/conductor-native/tests/python_contracts/mutation_campaign_generate_support.rs
```

Map these paths to `python_contracts_mutation_retention`:

```
src/conductor/mutation_retention.py
src/conductor/mutation_coverage.py
src/conductor/mutation_patch_audit.py
src/conductor/mutation_testing.py
src/conductor/mutation_testing_support.py
src/conductor/mutation_receipt_build.py
src/conductor/mutation_campaign_model.py
src/conductor/mutation_scope.py
src/conductor/mutation_value.py
src/conductor/mutation_receipt_slim.py
src/conductor/mutation_patch_apply.py
src/conductor/changed_files_cli.py
src/conductor/project_paths.py
src/conductor/_native.py
native/conductor-native/src/lib.rs
native/conductor-native/src/mutation_value.rs
native/conductor-native/src/mutation_value_inputs.rs
native/conductor-native/src/project_paths.rs
native/conductor-native/src/mutation_coverage.rs
native/conductor-native/src/receipt_slim.rs
native/conductor-native/tests/python_contracts/support.rs
native/conductor-native/tests/python_contracts/mutation_retention_support.rs
```

The lists include direct Python APIs, their relevant import and native-binding closure, and the exact Rust helper includes. A change to a listed provider must select the corresponding contract target. Shared helper `support.rs` maps to both targets. The Rust fixtures construct their own temporary `.py` inputs, so there are no tracked Python fixture-file dependencies to register.

## Importers and ownership

- Production `mutation_campaign_generate` is used by the Forge command path. Its bridge uses native `mutation_plan.rs`; preserve these modules.
- Production `mutation_retention` reads coverage citations and patch-audit citations. Preserve those modules.
- No production module imports either original Python test module or its private helpers.
- The two native targets and dedicated Rust helpers replace their corresponding Python test modules. Their 50 provider and helper rows are registered in `python_contract_targets_extra.tsv`; the 27 patch-audit cases remain for a separate cohort.
- Forge `gh issue list` returned no open issue covering this migration.

## Validation

On 2026-09-27, the exact Rust targets passed with `python-compat-tests` enabled:

| Check | Result |
| --- | --- |
| `cargo +1.98.0 test --offline --locked --manifest-path native/conductor-native/Cargo.toml --features python-compat-tests --test python_contracts_mutation_campaign_generate --test python_contracts_mutation_retention -- --test-threads=1` | 26 generation and 32 retention tests passed; 0 failed |
| `cargo +1.98.0 clippy --offline --locked --manifest-path native/conductor-native/Cargo.toml --features python-compat-tests --test python_contracts_mutation_campaign_generate --test python_contracts_mutation_retention -- -D warnings` | Passed |
| `rustfmt +1.98.0 --check` on both targets and their dedicated helpers | Passed |

The Cargo checks used the Forge virtual environment for PyO3 and `PYTHONPATH`, `/tmp/forge-sqlite-link` for SQLite linking, two build jobs, one test thread, and `CUDA_VISIBLE_DEVICES=''`. They did not execute mutation engines. Registry discovery passed 32/32 after integration. The full Forge local check and verification receipt remain the landing gate.
