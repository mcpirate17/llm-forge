# Mutation test migration to Rust

The Rust `python_contracts_mutation_*` targets run with the `python-compat-tests`
feature. They construct inputs in Rust, call the public Python boundary only
where host orchestration remains Python, and assert outcomes in Rust. A Python
file is retired only after its mapped Rust target passes.

## Retired Python cases

| Python case | Rust replacement and contract |
| --- | --- |
| `test_check_generated_mutants.py::test_rejects_the_hand_authored_engines` | `python_contracts_mutation_guard::engine_classifier_admits_only_generated_direct_campaign_manifests`: four forbidden engines stay distinct. |
| `test_check_generated_mutants.py::test_admits_every_engine_the_runner_can_execute` | Same: each live runner engine is accepted. |
| `test_check_generated_mutants.py::test_the_admitted_set_is_the_runners_own` | Same: compares against the runner's exported engine set. |
| `test_check_generated_mutants.py::test_a_manifest_declaring_no_engine_is_an_offence` | Same: missing engine reports `<none declared>`. |
| `test_check_generated_mutants.py::test_ignores_the_registry_and_the_baseline` | Same: noncampaign JSON at the campaign root is ignored. |
| `test_check_generated_mutants.py::test_ignores_receipts_and_patches_below_the_campaign_directory` | Same: receipt, fragment, and patch subdirectories are ignored. |
| `test_check_generated_mutants.py::test_ignores_manifests_outside_the_campaign_directory` | Same: unrelated path and extension are ignored. |
| `test_check_generated_mutants.py::test_ignores_a_file_that_is_not_json` | Same: malformed JSON is ignored. |
| `test_check_generated_mutants.py::test_paths_at_base_reports_only_what_the_base_commit_held` | `python_contracts_mutation_guard::argument_and_base_lookup_fail_closed`: real Git base holds only old campaign; empty list stays empty. |
| `test_check_generated_mutants.py::test_a_bad_base_raises_rather_than_calling_everything_new` | Same: bad Git ref raises an `ls-tree` error. |
| `test_check_generated_mutants.py::test_parse_argv_takes_the_flags_and_leaves_the_paths` | Same: both flag orders preserve positional paths. |
| `test_check_generated_mutants.py::test_parse_argv_refuses_to_guess_a_missing_base` | Same: missing base raises `SystemExit`. |
| `test_check_generated_mutants.py::test_editing_a_campaign_that_existed_at_the_base_is_allowed` | `python_contracts_mutation_guard::changed_campaign_cli_distinguishes_existing_generated_and_new_handwritten`: edited old campaign exits zero. |
| `test_check_generated_mutants.py::test_a_new_hand_written_campaign_is_refused` | Same: new hand-authored campaign exits one and is named. |
| `test_check_generated_mutants.py::test_a_new_generated_campaign_is_admitted` | Same: new generated campaign exits zero. |
| `test_check_generated_mutants.py::test_names_only_the_offender` | Same: mixed diff names only new hand-authored campaign. |
| `test_check_generated_mutants.py::test_no_changed_files_is_not_an_offence` | Same: empty diff exits zero. |
| `test_check_generated_mutants.py::test_a_missing_path_does_not_stop_the_scan` | Same: deleted first path does not hide later offender. |
| `test_mutation_attribution_verdicts.py::test_a_kill_is_confirmed_only_when_a_declared_killer_actually_failed` | `python_contracts_mutation_verdicts::declared_failed_error_and_collateral_tests_are_disjointly_reported`: every actionable set and field is asserted. |
| `test_mutation_attribution_verdicts.py::test_a_kill_by_nobody_who_was_declared_is_misattributed_not_confirmed` | Same: stranger failure remains collateral and unmatched. |
| `test_mutation_attribution_verdicts.py::test_unranked_failures_are_carried_so_a_blunt_mutant_is_visible` | Same: unranked failures remain in verdict. |
| `test_mutation_attribution_verdicts.py::test_a_kill_nobody_can_attribute_is_refused_and_says_which_kind` | `python_contracts_mutation_verdicts::unavailable_incomplete_and_survived_runs_keep_distinct_verdicts`: all three outcomes retain reason and fields. |
| `test_mutation_attribution_verdicts.py::test_only_a_confirmed_kill_is_routine_enough_to_fold` | `python_contracts_mutation_verdicts::folding_requires_confirmed_kill_and_preserves_actionable_fields`: both required conditions and adverse statuses are asserted. |
| `test_mutation_attribution_verdicts.py::test_folding_a_routine_kill_keeps_every_field_a_reader_acts_on` | Same: fields, counts, omitted matrix, and malformed matrix behavior are asserted. |

Focused validation: `cargo +1.98.0 test --offline --locked --manifest-path
native/conductor-native/Cargo.toml --features python-compat-tests --test
python_contracts_mutation_guard -- --test-threads=1` (3 passed), and the same
command with `python_contracts_mutation_verdicts` (3 passed).

| Python case | Rust replacement and contract |
| --- | --- |
| `test_mutation_receipt_build.py::test_default_receipt_path_falls_back_to_the_monorepo_literal_unconfigured` | `python_contracts_mutation_receipt_paths::default_receipt_path_uses_configured_or_legacy_root_and_creates_directories`: exact legacy root, directory, filename prefix and suffix. |
| `test_mutation_receipt_build.py::test_default_receipt_path_honours_the_configured_mutation_receipt_root` | Same: pyproject setting selects the configured host root. |
| `test_mutation_receipt_build.py::test_default_receipt_path_creates_a_directory_that_does_not_exist_yet` | Same: nested configured directory is created. |
| `test_mutation_receipt_build.py::test_default_receipt_path_fails_loud_when_the_directory_cannot_be_created` | `python_contracts_mutation_receipt_paths::receipt_directory_creation_failure_is_a_named_campaign_error`: blocking file raises named `CampaignError`. |
| `test_mutation_receipt_build.py::test_resolve_receipt_path_uses_the_default_when_none_is_given` | `python_contracts_mutation_receipt_paths::resolved_receipt_path_uses_default_or_explicit_target`: configured default and relative return are checked. |
| `test_mutation_receipt_build.py::test_resolve_receipt_path_still_honours_an_explicit_path` | Same: explicit absolute and relative paths are preserved. |
| `test_mutation_receipt_slim.py::test_python_codec_returns_dict_and_maps_native_error` | `python_contracts_mutation_receipt_slim::python_codec_round_trips_and_reports_superseded_pointer`: dict shape, expansion, and typed superseded error. |
| `test_mutation_receipt_slim.py::test_the_field_reader_decompresses_only_what_is_asked_for` | `python_contracts_mutation_receipt_slim::field_reader_uses_summary_legacy_and_detail_without_guessing_missing_keys`: summary, legacy, compressed detail and absent key. |
| `test_mutation_receipt_slim.py::test_write_slim_receipt_lands_canonical_bytes` | `python_contracts_mutation_receipt_slim::writer_lands_canonical_json_and_expands_all_detail`: zstd encoding, full expansion, two-space sorted JSON and final newline. |
| `test_mutation_receipt_slim.py::test_compaction_error_maps_to_python_detail_error` | `python_contracts_mutation_receipt_slim::compactor_maps_native_errors_to_receipt_detail_error`: absent kept receipt fails with typed error. |
| `test_mutation_value_gate_edges.py::test_mutation_value_evidence_envelope_fails_closed` | `python_contracts_mutation_gate_edges::non_list_evidence_envelope_blocks_new_test_value`: one critical finding names the test and path. |
| `test_mutation_value_gate_edges.py::test_mutation_value_evidence_receipt_field_fails_closed` | `python_contracts_mutation_gate_edges::non_string_receipt_field_blocks_new_test_value`: null receipt has the same fail-closed finding. |
| `test_mutation_plan_parity.py::test_public_plan_matches_the_frozen_python_corpus` (15 parameter values) | Existing `python_contracts_campaign_plan::public_plan_matches_all_fifteen_frozen_python_cases`: copies each fixture tree, checks exact public Python output or typed error against frozen expected files. |
| `test_mutation_plan_parity.py::test_at_least_fifteen_fixture_cases_are_frozen` | Same Rust case asserts at least 15 fixture directories were checked. |

Further focused validation: `python_contracts_mutation_receipt_paths` (3 passed),
`python_contracts_mutation_receipt_slim` (4 passed),
`python_contracts_mutation_gate_edges` (2 passed), and existing
`python_contracts_campaign_plan` (4 passed), using the same offline Cargo feature,
environment and single-thread runner as above.

| Python case | Rust replacement and contract |
| --- | --- |
| `test_mutation_patch_apply.py::test_a_hunk_applies_where_its_content_is_not_where_its_numbers_say` | `python_contracts_mutation_patch_apply::content_anchor_survives_line_shift_neighbour_edit_and_eof_append`: 20-line shift still edits exact construct. |
| `test_mutation_patch_apply.py::test_an_edit_to_a_neighbouring_context_line_does_not_retire_the_hunk` | Same: whole file preserves edited neighbour while changing only return. |
| `test_mutation_patch_apply.py::test_appending_past_an_eof_anchored_hunk_does_not_retire_it` | Same: whole appended file is preserved. |
| `test_mutation_patch_apply.py::test_an_ambiguous_anchor_is_refused_rather_than_guessed` | `python_contracts_mutation_patch_apply::ambiguity_refuses_tie_but_header_disambiguates_and_gone_edit_refuses`: equidistant duplicate blocks refuse. |
| `test_mutation_patch_apply.py::test_a_repeated_construct_is_resolved_by_the_declared_line_not_by_order` | Same: declared line selects second block. |
| `test_mutation_patch_apply.py::test_a_hunk_whose_construct_is_gone_is_refused` | Same: missing edited line refuses. |
| `test_mutation_patch_apply.py::test_relaxing_context_keeps_the_edited_line_as_the_anchor` | Same: unique return anchor edits without disturbing different context. |
| `test_mutation_patch_apply.py::test_a_file_with_no_trailing_newline_keeps_that_property` | `python_contracts_mutation_patch_apply::newline_and_pure_insertion_preserve_all_neighbouring_bytes`: no final newline is preserved. |
| `test_mutation_patch_apply.py::test_a_pure_insertion_hunk_places_the_new_lines` | Same: exact whole file gains line and loses none. |
| `test_mutation_patch_apply.py::test_a_pure_insertions_leading_anchor_is_never_relaxed_away` | Same: missing leading context refuses. |
| `test_mutation_patch_apply.py::test_a_patch_that_creates_or_deletes_a_file_is_refused` | `python_contracts_mutation_patch_apply::creation_deletion_rename_and_dev_null_forms_are_refused`: three directives each refuse. |
| `test_mutation_patch_apply.py::test_a_dev_null_source_or_target_is_refused` | Same: both `/dev/null` spellings refuse with distinct messages. |
| `test_mutation_patch_apply.py::test_a_patch_targeting_a_missing_file_is_refused` | `python_contracts_mutation_patch_apply::dry_run_matches_apply_without_writing_and_reports_touched_paths`: check refuses absent file. |
| `test_mutation_patch_apply.py::test_check_reports_the_same_answer_as_apply_without_writing` | Same: check preserves bytes and apply changes them. |
| `test_mutation_patch_apply.py::test_apply_reports_every_path_it_touched` | Same: return names exactly `m.py`. |

`python_contracts_mutation_patch_apply` passed all five grouped Rust tests under
the same offline single-thread Cargo command.
