# Native test migration

At pinned commit `4111905d2cdf6bca1e5855e6fcd56274c2d92a55`, the separate
Rust-test source milestone is **80.6873%**: 85,038 Rust test code lines and
20,354 Python test code lines under the unchanged path and inline-test rules.
The cohort retired three Python test suites and replaced the executable
Python dispatcher stub with a Rust fixture crate. The pinned tree retains 63
executable Python suites; 32 static Python fixture inputs and the shared
conftest are classified separately. Production remains **50.5489% native**.
These are source-composition metrics, not behavioral coverage or a claim that
all tests have been converted. The [full method and pinned evidence](native-migration.md#latest-verified-commit)
record every counted input.

At pinned commit `a344b9ab8c1b432f2429fd01924b4d6c1bffb688`, the separate
Rust-test source milestone is **79.9377%**: 83,694 Rust test code lines and
21,005 Python test code lines under the unchanged path and inline-test rules.
This cohort retired four Python test modules and added 39 named Rust-owned
contracts across six targets. The Mull live-registry case had no registered
Mull campaign to exercise; its Rust early-return PASS carries that explicit
limit. Production is **50.5489% native**. These are source-composition metrics,
not behavioral coverage or a claim that all tests have been converted. The
[full method and pinned evidence](native-migration.md#latest-verified-commit)
record every counted input.

At pinned commit `6dd9fc8d31daab114cf856b744ceb20d64cdd7a3`, the separate
Rust-test source milestone is **78.1709%**: 80,835 Rust test code lines and
22,573 Python test code lines under the unchanged path and inline-test rules.
This cohort retired three Python test modules and added 38 named Rust-owned
contracts across five targets. Production is **50.5475% native**. These are
source-composition metrics, not behavioral coverage or a claim that all tests
have been converted. The [full method and pinned evidence](native-migration.md#latest-verified-commit)
record every counted input.

At pinned commit `1f4459ceb368eb495068032f61fe6d83bc133509`, the separate
Rust-test source milestone is **77.0656%**: 78,795 Rust test code lines and
23,449 Python test code lines under the unchanged path and inline-test rules.
This cohort retired five Python test modules and added 30 named Rust-owned
contracts across six targets, including a bounded subprocess fixture.
Production is **50.5465% native**. These are source-composition metrics, not
behavioral coverage or a claim that all tests have been converted. The
[full method and pinned evidence](native-migration.md#latest-verified-commit)
record every counted input.

At pinned commit `98ce5ea915376f9d52aba173f0eb6e0458f940cf`, the separate
Rust-test source milestone is **75.7083%**: 76,825 Rust test code lines and
24,650 Python test code lines under the unchanged path and inline-test rules.
This cohort retired seven Python test modules and added 58 named Rust-owned
contracts across eight targets. Production is **50.5446% native**. These are
source-composition metrics, not behavioral coverage or a claim that all tests
have been converted. The [full method and pinned evidence](native-migration.md#latest-verified-commit)
record every counted input.

At pinned commit `f8a1af1a4194eaec77e7dfcc0e7a997d47c69e95`, the separate
Rust-test source milestone is **73.9463%**: 73,774 Rust test code lines and
25,993 Python test code lines under the unchanged path and inline-test rules.
This cohort retired three Python test modules and added 46 named Rust-owned
contracts across four targets plus a test-only Rust interpreter fixture.
Production remains **50.5314% native**. These are source-composition metrics,
not behavioral coverage. The [full method and pinned evidence](native-migration.md#latest-verified-commit)
record every counted input.

At the preceding pinned commit `c0ab4e080f661443085628d8035122b4e2823e92`,
the separate Rust-test source milestone was **72.7724%**: 71,993 Rust test code
lines and 26,936 Python test code lines. That cohort retired four Python test
modules and added 53 named Rust-owned contracts across four targets plus a
test-only Rust stdio fixture. Its [pinned evidence](native-metrics/c0ab4e0-evidence.tar.gz)
remains available.

At the earlier pinned commit `4da207110e81697b1591a08285285598f7b8991a`,
the separate Rust-test source milestone was **71.3662%**: 69,657 Rust test code
lines and 27,948 Python test code lines. That cohort retired four Python test
modules and added 60 named Rust-owned contracts across five targets, preserving
69 expanded pytest baseline cases. Its separate production and test evidence
remains in the [pinned archive](native-metrics/4da2071-evidence.tar.gz).

At the earlier pinned commit `bc8fdefa1ed6fcc405d3b90237489a89883317fe`,
the separate Rust-test source milestone was **69.9620%**: 67,216 Rust test code
lines and 28,859 Python test code lines under the same path and inline-test
rules. That cohort retired 12 Python test modules and added 134 named
Rust-owned contract tests across 13 targets; the post-tool and Bash parity
tests also exercise 44 and 64 fixture rows. Its separate production and test
evidence remains in the [pinned archive](native-metrics/bc8fdef-evidence.tar.gz).

## Mutation value parsers

`native/conductor-native/tests/mutation_value_inputs.rs` tests the pure Rust
parser used by both the Python extension and Forge. The following parser-only
tests were removed from `src/conductor/test_mutation_value.py` after equivalent
Rust assertions were added. The Python batch, routing, and scorer integration
tests remain because they exercise a different boundary.

| Retired Python test | Rust replacement |
| --- | --- |
| `test_junit_parser_preserves_skipped_and_unmapped_outcomes` | `pytest_skipped_case_remains_skipped_with_unmapped_neighbor` |
| `test_junit_parser_aggregates_precedence_and_failed_nodeids` | `pytest_parameterized_cases_keep_failure_priority_and_duration_sum` |
| `test_ctest_parser_maps_disabled_notrun_and_failure_statuses` | `ctest_disabled_notrun_and_missing_attributes_keep_distinct_outcomes`, `ctest_error_failure_status_and_missing_names_preserve_contract` |
| `test_ctest_parser_preserves_xml_markers_and_rounds_duration` | `ctest_last_registration_wins_and_unranked_failure_is_visible`, `ctest_error_failure_status_and_missing_names_preserve_contract` |
| `test_junit_parser_distinguishes_pass_defaults_and_unmapped_defaults` | `pytest_status_precedence_and_default_attributes_match_junit_contract` |
| `test_junit_parser_skipped_then_passed_is_passed` | `pytest_status_precedence_and_default_attributes_match_junit_contract` |
| `test_junit_parser_error_and_failed_precedence_is_stable` | `pytest_status_precedence_and_default_attributes_match_junit_contract` |
| `test_junit_parser_failed_and_skipped_precedence_is_stable` | `pytest_status_precedence_and_default_attributes_match_junit_contract` |
| `test_junit_parser_keeps_scanning_after_unmapped_case` | `pytest_unmapped_and_missing_cases_fail_closed_in_order` |
| `test_cargo_parser_keeps_scanning_after_unranked_and_preserves_failure` | `libtest_preserves_failure_without_fabricating_time_and_rejects_collision` |
| `test_a_ctest_nodeid_maps_onto_the_name_cmake_registers` | `identity_checks_refuse_unseparable_ranked_tests` |
| `test_ctest_attribution_refuses_names_it_cannot_separate` | `identity_checks_refuse_unseparable_ranked_tests` |
| `test_ctest_parser_reports_error_and_bad_duration_fail_closed` | `ctest_error_failure_status_and_missing_names_preserve_contract` |
| `test_ctest_parser_defaults_missing_attributes_without_fabricating_duration` | `ctest_disabled_notrun_and_missing_attributes_keep_distinct_outcomes` |

The removed `_assert_ctest_repeated_status_and_unranked_failure` helper is
covered by `ctest_last_registration_wins_and_unranked_failure_is_visible`.
`xml_rejects_dtd_malformed_and_oversized_documents` and
`libtest_json_lines_preserve_ranked_and_unranked_failure_provenance` add native
coverage of bounded hostile input and JSON-lines output.
`junit_file_decodes_declared_latin1_attribute_bytes` checks the shipped file
reader's declared-encoding path.

The remaining Python tests check public wrappers, argv rewriting, stale report
removal, captured subprocess output, adapter routing, killer verdicts, and
value-analysis integration. They should move only with those host boundaries.

`test_value_analysis_uses_explicit_failed_nodeids_for_killers` now calls the
public Python wrapper through the production native entrypoint. It checks that
an explicit failed nodeid wins even when the per-test outcome says `PASSED`,
and retains the separate Python receipt-enforcement assertions. The native
`mutation_value_inputs::mutant_evidence_prefers_explicit_failed_nodeids_and_preserves_order`
test covers the core killer selection and evidence ordering. The former
monkeypatch targeted `analyze_test_value_native`, which the wrapper no longer
imports after native report collection absorbed that step.

The two `test_collect_{pytest,ctest}_batch_reports_all_fail_closed_fields`
cases and the CTest stale-report helper now run in
`tests/python_contracts_mutation_value.rs`. Rust assertions verify the full
missing-evidence result, stale-report removal and unattributed-kill verdict
through the Python process API. Both Rust cases passed before the duplicate
Python cases were retired.

## Graph refresh waits

Three cases from `src/tooling/hooks/agent/test_crg_refresh_state.py` now run in
`native/forge/src/crg_refresh.rs`. The other worker, batch, doctor and integration
cases remain at their Python boundaries.

| Retired Python test | Rust replacement |
| --- | --- |
| `test_wait_times_out_while_a_worker_holds_the_lock` | `wait_times_out_while_worker_holds_the_lock` |
| `test_wait_returns_fresh_once_marker_and_worker_are_gone` | `wait_respawns_once_then_returns_fresh_when_pending_clears` |
| `test_wait_output_reports_a_stale_graph_only_on_timeout` | `wait_output_warns_on_timeout_and_is_quiet_after_worker_exits` |

## Legacy hook bodies

The shell target, guard, impact, output bounding and read budget algorithms now
live only in `native/forge/src/`. Old Python hook paths forward through the
hidden `forge legacy-hook` JSON API. Detailed Python behavior tests were
retired after their native equivalents and the CLI bridge test passed. Small
Python tests remain to check installed entrypoint binding and shell wiring.

| Retired Python tests | Native replacement |
| --- | --- |
| `test_guard_parity_corpus.py` (all fixture cases) | `native/forge/tests/guard_parity.rs` (`bash_guard_matches_python_on_every_corpus_command`, `write_targets_match_python_on_every_corpus_command`) |
| `test_bash_impact_parity.py` (all fixture cases) | `native/forge/tests/bash_impact_parity.rs` (`classify_matches_python_on_every_corpus_command`) |
| `test_tool_quiet_parity_corpus.py` (all fixture cases) | `native/forge/tests/tool_quiet_parity.rs` (`tool_quiet_native_hooks_match_the_frozen_corpus`) |
| `test_bash_guard.py` command parser cases | `native/forge/src/bash_guard.rs` (nine corresponding unit tests), `native/forge/tests/legacy_hook_api.rs` (`write_targets_and_guard_share_the_native_shell_parser`) |
| `test_bash_quiet.py`, `test_post_tool_quiet.py` split, cap, shape and spill cases | `native/forge/src/tool_quiet.rs` (six unit tests), `native/forge/tests/tool_quiet_parity.rs`, `native/forge/tests/legacy_hook_api.rs` (`quiet_cli_bounds_and_spills_using_the_shared_native_kernel`) |
| `test_read_budget.py` traversal, step and tally cases | `native/forge/src/read_budget.rs` (five unit tests), `native/forge/tests/legacy_hook_api.rs` (`read_budget_and_gate_start_write_native_state`) |

The installed shell entrypoint cases formerly in `test_pre_bash_e2e.py` now run in the Rust-owned Claude pre-Bash contract. `legacy_shell_propagates_native_guard_and_impact_failures` checks
the shell's error handling with a native test and a fake `python3` executable;
it runs no Python interpreter. Narrow Python smoke cases in the four retained
test modules check that those entrypoints reach forge and preserve their output
envelopes.
### Native algorithm test migration: A2A compaction, receipt codec, and memory chunking

The native crate's `a2a_compaction::tests` exercises the production pure-Rust `validate_coordination_v2_value`, `compact_message_value`, and `compact_threads_value` functions. The following Python algorithm cases in `src/conductor/test_a2a_compaction.py` were retired after the root agent's no-default Rust test run passed 32 tests including all new A2A cases:

| Retired Python case/helper | Rust equivalent |
| --- | --- |
| `test_validate_coordination_v2_rejects_invalid_protocol` | `coordination_rejects_invalid_protocol_shapes` |
| `test_validate_coordination_v2_enforces_utf8_summary_and_supersedes_bounds`, `_assert_validate_coordination_v2_normalizes_only_summary_whitespace` | `coordination_bounds_use_utf8_bytes_and_supersedes_count`, `coordination_normalizes_summary_and_preserves_supersedes_order` |
| `test_legacy_fallback_is_conservative_bounded_and_model_free`, `_assert_compact_message_is_deterministic_and_contains_no_raw_payload`, `_assert_empty_legacy_body_uses_deterministic_label` | `legacy_fallback_is_bounded_and_empty_body_has_a_label`, `compact_message_is_deterministic_and_omits_raw_payload` |
| `test_actionability_preserves_status_and_requires_response_independently`, `test_missing_v2_status_fails_closed_as_actionable` | `actionability_preserves_status_and_response_independently` |
| `_assert_invalid_v2_shape_cannot_silently_fall_back_to_legacy` | `malformed_v2_and_self_supersession_fail_closed` |
| `test_invalid_legacy_json_is_hashed_and_safely_summarized`, `_assert_unicode_byte_accounting_is_exact` | `malformed_json_and_unicode_have_exact_byte_accounting` |
| `_assert_all_emitted_prose_labels_obey_utf8_byte_bounds`, `test_oversized_row_metadata_fails_closed` | `prose_fields_truncate_on_utf8_boundaries_and_metadata_fails_loud` |

The Python A2A file retains three boundary tests for mapping conversion, JSON incompatibility, native error translation, and dictionary field shape. The new Rust thread tests exercise deduplication, conflict rejection, order, actionable retention, and caps with no Python interpreter.

`src/conductor/test_mutation_receipt_slim.py` retired `test_small_campaigns_stay_inline_and_round_trip`, `test_big_campaigns_become_one_blob_with_verbatim_numbers`, `test_legacy_receipts_pass_through_unchanged`, `test_superseded_pointers_fail_loud_naming_the_replacement`, and `test_compaction_honours_the_audit_keep_set`. Existing native `receipt_slim::tests` cover the same production core, including arbitrary-precision number token preservation, keep-set selection, and missing-target rejection. Python retains wrapper type/error, lazy field reader, and canonical writer checks.

`memory_chunking::tests` has six passing pure-Rust cases including heading split, heading-only fallback, UTF-8 character cap, blank input, and whole-mode truncation. The mapped `test_chunk_text_splits_on_headings`, `test_chunk_text_returns_empty_for_blank_input`, `test_chunk_text_heading_only_falls_back_to_file_name_title`, and `test_chunk_text_whole_mode_caps_at_3000` were retired from `src/conductor/test_memory_index.py`. Randomized CPython parity, exotic-boundary parity, and Python tuple-to-dict seam remain.

`candidate_structure::tests` adds direct fact extraction checks for the existing parser, leak, lock, abstraction, config, and cache algorithms. The Python structure-audit policy cases remain because they cover Finding severity, cross-file adjudication, CPython expression normalization, and scan-ledger behavior.

# A2A retention test migration map

Source: former `src/conductor/test_a2a_retention.py` at `HEAD` on `feat/native-pretool-and-results`. Native tests use disposable SQLite files only. `native/forge/tests/mailbox_retention.rs` is abbreviated as `mailbox_retention.rs`; the deterministic evidence race unit test lives in `native/forge/src/mailbox_retention.rs`.

| Former Python case | Replacement | Specific boundary |
|---|---|---|
| `test_preview_reports_exact_receipts_without_any_database_mutation` | `preview_is_read_only_and_apply_writes_exact_manifest_receipt` | Existing SQLite bytes and event count unchanged after preview; receipt fields and byte counts checked. |
| `test_apply_requires_allowlist_and_only_mutates_named_store` | `apply_requires_one_exact_actor_and_cannot_escape_state_dir` | Explicit one-store and matching actor checks; a second real store remains unchanged after apply. |
| `test_apply_rejects_allowlisted_store_symlink_escape` | `apply_requires_one_exact_actor_and_cannot_escape_state_dir` | A store symlink outside selected state root is refused before writes. |
| `test_grace_is_inclusive_and_uses_latest_terminal_transition` | `cutoff_is_inclusive_but_latest_terminal_timestamp_controls`, `bounded_batch_order_limit_and_timestamp_validation_fail_closed`, `future_read_timestamp_and_nonfile_evidence_abort_without_changes`, Python `test_bridge_fails_closed_on_invalid_time_and_native_receipt` | Exact cutoff, later supersede, bounded deterministic selection, invalid grace, invalid/naive/future times. |
| `test_only_read_terminal_unheld_operational_inbound_rows_compact` | `selection_excludes_unread_unresolved_held_pinned_outbound_and_evidence`, `bounded_batch_order_limit_and_timestamp_validation_fail_closed` | Each exclusion and valid superseded row; invalid batch limits. |
| `test_gate_and_unstructured_evidence_remain_pinned` | `selection_excludes_unread_unresolved_held_pinned_outbound_and_evidence` | Explicit gate and unstructured payload rows with pinned retention class are unchanged. |
| `test_evidence_referenced_message_ids_are_mechanically_excluded` | `selection_excludes_unread_unresolved_held_pinned_outbound_and_evidence` | Nested gate receipt protects exact ID; nonmatching report filename does not count. |
| `test_evidence_scan_fails_closed_on_unsafe_inputs` (`malformed`, `oversized`, `nonfile`) | `malformed_evidence_and_content_drift_fail_without_writes`, `evidence_symlink_escape_and_size_limit_fail_closed`, `future_read_timestamp_and_nonfile_evidence_abort_without_changes` | Malformed JSON, oversized file, and directory-shaped evidence each fail before tombstones. |
| `test_oversized_evidence_is_refused_before_the_file_is_read` | `evidence_symlink_escape_and_size_limit_fail_closed` | Oversized evidence is chmod 000 before invocation; the size error wins before file open. |
| `test_evidence_drift_aborts_and_rolls_back_the_batch` | unit `evidence_appearing_between_snapshot_and_commit_rolls_back_every_write` | Inject a new exact gate receipt after all row updates, before second evidence scan; transaction rolls back both event and tombstone. |
| `test_manifest_hash_event_and_unicode_byte_accounting` | `preview_is_read_only_and_apply_writes_exact_manifest_receipt` | Recompute canonical manifest and event SHA-256, verify Unicode byte count and private structured field removal. |
| `test_apply_is_idempotent_and_tombstone_text_is_not_an_eligibility_marker` | `tombstone_text_does_not_drive_eligibility_and_apply_is_idempotent` | Body initially equals tombstone; first apply compacts, second finds zero eligible; one event and original timestamp. |
| `test_content_drift_aborts_and_rolls_back_entire_batch` | `whole_batch_rolls_back_when_a_trigger_changes_later_content` | SQLite trigger mutates the second row after first tombstone; CAS refusal rolls back first row and event. |
| `test_state_drift_aborts_and_rolls_back_atomically` (five fields) | `whole_batch_rolls_back_when_a_trigger_changes_later_lifecycle_fields` | SQLite triggers mutate summary, status, requires-response, data hash, and data bytes one case each; all rollback. |
| `test_preexisting_content_metadata_drift_fails_before_writes` (four fields) | `preexisting_content_metadata_drift_fails_before_any_write` | Body/data digest and size each fail manifest validation before any event. |
| `test_cli_defaults_to_preview_and_apply_requires_store` | Rust `preview_is_read_only_and_apply_writes_exact_manifest_receipt`, `apply_requires_one_exact_actor_and_cannot_escape_state_dir`; Python `test_cli_is_preview_by_default_and_preserves_native_errors` | Default preview, explicit apply guard, Python command forwarding and error propagation. |
| `test_import_has_no_store_or_scheduler_side_effect` | Python `test_import_has_no_store_or_scheduler_side_effect` | Retained subprocess seam; import cannot open SQLite or create state and no server reference exists. |

New Python tests only verify forwarding and import behavior. The candidate policy, evidence scan, manifest, and SQLite mutation are native. Focused validation commands: `cargo test --manifest-path native/forge/Cargo.toml --test mailbox_retention`; `cargo test --manifest-path native/forge/Cargo.toml --bin forge evidence_appearing_between_snapshot_and_commit`; `FORGE_BIN=$PWD/native/forge/target/debug/forge uv run pytest -q src/conductor/test_a2a_retention.py`.

## Conductor source analysis and receipt tests

This cohort uses direct Rust assertions in `native/conductor-native/src/`. The
root's source-analysis Rust run passed 132 tests, including the new cases. The
following Python cases were removed after mapping their assertions to those
native cases:

| Retired Python cases | Rust replacement |
| --- | --- |
| `test_dead_tests.py`: explicit-root untracked resolver, guarded and relative scanner imports, ten-thousand-module cycle, deterministic module JSON | `dead_tests::tests`: `resolver_scopes_untracked_files_to_its_explicit_root`, `dead_test_scanner_preserves_guard_relative_string_and_native_semantics`, `dead_test_closure_handles_ten_thousand_module_cycle`, `scanned_module_json_orders_dependencies_independently_of_tracked_input` |
| `test_kb_retrieve.py`: index-order ties and `top_k` truncation | `kb_retrieve::tests::score_cards_ties_keep_index_order_and_top_k_truncates` |
| `test_mutation_coverage.py`: Rust test surface, path and registry errors, Git failure and cache skip, changed-result defect and legacy-row classification, legacy GitHub table, merge-base inventory | `mutation_coverage::tests`: attribute and surface cases, `path_globs_normalization_and_skip_sets_preserve_inventory_rules`, `registry_rejects_outside_malformed_and_noncanonical_patterns`, `git_errors_and_inventory_include_content_declared_rust_tests`, `evidence_exit_codes_separate_debt_from_validator_defects`, `row_kinds_and_legacy_defaults_are_fail_closed`, `github_output_keeps_annotations_order_and_legacy_table_defaults`, `changed_from` cases |
| `test_receipt_verify.py`: missing manifest, manifest hash mismatch, source pin drift and missing path, receipt-map divergence, independent inventory digest | `receipt_auth::tests`: `missing_manifest_blocks_all_dependent_checks`, `manifest_hash_lie_only_fails_part_two`, `drifted_source_fails_pins_and_inventory`, `unreadable_source_blocks_inventory_reproduction`, `receipt_map_lie_fails_only_inventory_reproduction`, `inventory_digest_matches_independent_hash_sorted_lines` |

Python still checks its distinct host seams: dead-test CLI root and error
translation, KB retrieval embedding/index and exact CPython floating-point
parity, and receipt verifier CLI and subprocess behavior. The Rust mutation
inventory test now includes a file with inline Rust tests that no test glob
matches, along with the Git failure case; this completes the former Python
inventory assertion. The former mutation-coverage `changed`/`canary` dispatch,
GitHub summary-file, and mutation-testing CLI cases now run in the Rust-owned
contracts mapped in [the coverage and run-scope migration](native-mutation-coverage-scope-tests-migration.md);
`test_mutation_coverage.py` has been retired. This does not imply that all
remaining Python tests or host seams have been converted.
