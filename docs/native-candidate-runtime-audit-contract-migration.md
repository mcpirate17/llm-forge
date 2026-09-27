# Candidate runtime and mutation patch audit contracts in Rust

This slice moves the final 17 cases of `src/conductor/test_candidate_review.py`
and all 27 cases of `src/conductor/test_mutation_patch_audit.py` into Rust owned
fixtures and assertions. Each Rust test calls the shipped Python API through
PyO3. Fixture trees, callbacks, comparisons, and assertions live in Rust; the
Python test modules are never imported as harnesses. These are contract ports,
not a claim that the production Python modules have been rewritten in Rust.

The target aliases in the maps are exact files under
`native/conductor-native/tests/`: `runtime_a` and `runtime_b` expand to
`python_contracts_candidate_runtime_a.rs` and `_b.rs`; `audit_a`, `audit_b`,
and `audit_c` expand to `python_contracts_mutation_patch_audit_a.rs`, `_b.rs`,
and `_c.rs`. Shared Rust fixtures live under `tests/python_contracts/` in
`candidate_runtime_support.rs` and `mutation_audit_support.rs`.

| Python source case (line) | Rust target | Production provider exercised |
| --- | --- | --- |
| `test_locked_commit_reuses_mutex_in_precommit_hook` (2528) | `runtime_a` | `candidate_review.engine.run_locked_git_commit`, `governance_lock` |
| `test_inherited_lock_descriptor_validation` (2562) | `runtime_a` | `engine._inherited_lock_fd`, `_inherited_lock_token_valid`, `_held_governance_lock` |
| `test_shard_thread_environment_gives_each_worker_a_fair_share` (2608) | `runtime_a` | `candidate_review.sharding.shard_thread_environment` |
| `test_wall_budget_is_separable_from_the_cpu_budget` (2630) | `runtime_a` | `candidate_review.policy.load_policy`, check wall timeout property |
| `test_a_stalled_shard_does_not_discard_the_other_shards_results` (2647) | `runtime_a` | `candidate_review.verification.run_targeted_tests` |
| `test_every_stalled_shard_still_fails_the_check` (2702) | `runtime_a` | `verification.run_targeted_tests` |
| `test_changed_coverage_judges_each_risk_class_on_its_own_lines` (2743) | `runtime_a` | `verification._risk_buckets` |
| `test_changed_lines_score_a_move_by_its_edited_hunks_only` (2776) | `runtime_a` | `candidate_review.git_source.changed_line_numbers` |
| `test_changed_lines_pair_a_move_that_leaves_an_alias_shim` (2817) | `runtime_a` | `git_source.changed_line_numbers` |
| `test_research_integrity_arms_on_changed_lines_not_the_whole_file` (2890) | `runtime_b` | `candidate_review.checks.check_research_evidence` |
| `test_research_integrity_still_fires_on_a_line_this_change_added` (2910) | `runtime_b` | `checks.check_research_evidence` |
| `test_research_integrity_reads_provenance_the_candidate_did_not_touch` (2931) | `runtime_b` | `checks.check_research_evidence` |
| `test_research_integrity_falls_back_to_whole_files_when_the_diff_fails` (2956) | `runtime_b` | `checks.check_research_evidence`, `git_source.changed_line_numbers` |
| `test_ci_attestation_flags_commits_with_no_agent_trailer` (3030) | `runtime_b` | `candidate_review.engine._bypass_evidence` |
| `test_ci_attestation_accepts_an_agent_trailer_beside_other_trailers` (3056) | `runtime_b` | `engine._bypass_evidence` |
| `test_ci_attestation_exempts_commits_written_before_the_rule` (3079) | `runtime_b` | `engine._bypass_evidence` |
| `test_agent_trailer_requirement_fails_closed_on_an_unreadable_date` (3097) | `runtime_b` | `engine._agent_trailer_required` |
| `test_a_rotted_anchor_is_reported_and_a_live_patch_is_not` (80) | `audit_a` | `mutation_patch_audit._patch_verdict` |
| `test_the_check_runs_against_the_repo_root_not_the_process_directory` (103) | `audit_a` | `_patch_verdict` |
| `test_a_missing_or_drifted_patch_is_reported_without_being_applied` (129) | `audit_a` | `_patch_verdict` |
| `test_one_unloadable_manifest_does_not_hide_the_rest_of_the_corpus` (149) | `audit_a` | `audit_patches`, `load_registered_campaigns` |
| `test_a_corpus_that_only_fails_to_load_is_not_reported_clean` (200) | `audit_a` | `audit_patches`, `load_registered_campaigns` |
| `test_an_absolute_interpreter_is_reported_and_a_bare_one_is_not` (232) | `audit_a` | `_interpreter_verdict` |
| `test_receipts_are_indexed_by_their_declared_id_not_their_filename` (266) | `audit_a` | `_receipts_by_campaign` |
| `test_a_receipt_is_evidence_only_when_a_known_runner_produced_it` (304) | `audit_a` | `_receipt_rejection`, `PASSING_RECEIPT_STATUSES` |
| `test_one_acceptable_receipt_covers_a_campaign_and_none_leaves_it_uncovered` (391) | `audit_a` | `_evidence_verdict` |
| `test_a_campaign_is_unmeasured_or_its_inert_tests_are_named` (457) | `audit_a` | `_value_verdicts` |
| `test_the_newest_acceptable_receipt_is_the_one_that_speaks` (527) | `audit_a` | `_acceptable_receipt` |
| `test_every_dimension_reduces_to_comparable_ids` (559) | `audit_b` | `_findings`, `BASELINE_KEYS` |
| `test_the_baseline_ratchet_bites_in_both_directions` (601) | `audit_b` | `_baseline_delta` |
| `test_a_missing_baseline_makes_every_finding_new` (638) | `audit_b` | `_load_baseline`, `_baseline_delta` |
| `test_the_exit_code_and_summary_carry_the_verdict` (729) | `audit_b` | `main`, `audit_corpus`, `corpus_exit_code` |
| `test_write_baseline_records_instead_of_judging` (779) | `audit_b` | `main`, `_write_baseline` |
| `test_optional_attribution_rejects_each_malformed_shape` (831) | `audit_b` | `_value_verdicts` |
| `test_receipt_and_baseline_io_validate_shapes_and_preserve_later_rows` (916) | `audit_b` | `_receipts_by_campaign`, `_load_baseline`, `_write_baseline` |
| `test_empty_patch_corpus_is_clean_and_external_interpreter_is_identified` (938) | `audit_b` | `audit_patches`, `_interpreter_verdict` |
| `test_audit_reproducibility_resolves_lineage_from_runner_component_root` (956) | `audit_c` | `audit_reproducibility`, `runner_component_root` |
| `test_a_receipt_pinning_other_bytes_is_not_evidence` (1049) | `audit_c` | `_receipt_rejection`, `audit_reproducibility` |
| `test_one_stale_hash_fails_the_whole_audit` (1113) | `audit_c` | `main`, `audit_corpus`, `audit_reproducibility` |
| `test_a_new_file_beside_a_covered_file_is_reported_uncovered` (1167) | `audit_c` | `_uncovered_changed_files` |
| `test_a_deleted_file_inside_territory_is_not_reported_uncovered` (1208) | `audit_c` | `_uncovered_changed_files` |
| `test_measured_territory_is_limited_to_the_campaign_language` (1232) | `audit_c` | `_uncovered_changed_files` |
| `test_unpinned_changed_files_fail_the_whole_audit` (1292) | `audit_c` | `main`, `audit_corpus`, `_uncovered_changed_files` |
| `test_value_verdicts_read_slim_receipts_the_same_as_legacy` (1369) | `audit_c` | `_value_verdicts`, `mutation_receipt_slim.slim_receipt` |

Validation uses the repository's Python 3.12 virtual environment, offline locked
Cargo dependencies, `python-compat-tests`, two build jobs, one test thread,
and `CUDA_VISIBLE_DEVICES=''`. The five exact targets pass **9 + 8 + 11 + 8 + 8
= 44 tests**. Scoped `cargo +1.98.0 clippy` with `-D warnings` passes on the
same targets. `rustfmt +1.98.0` formatted the seven owned Rust files.

The Forge issue list returned no open issue covering this migration.

The five original candidate-review and mutation-audit Python suites were
retired after the benchmark no longer depended on them. This retirement also
removed `src/conductor/conftest.py`, whose fixtures had no remaining test
consumers. The production Python modules remain covered by the Rust contracts.
