# Runtime and support test migration

The Rust integration suites below own the case data, control flow, and assertions. They call the shipped Python API at the PyO3 boundary because these modules retain Python orchestration. Python test code is not evaluated or forwarded to pytest.

| Python source case | Rust test and preserved assertion |
| --- | --- |
| `test_context_telemetry.py::test_native_usage_ignores_downstream_tool_output` | `usage_is_attributed_only_to_top_level_native_source`: top level usage wins; nested tool usage is ignored. |
| `test_append_accepts_exact_byte_cap_and_refuses_overflow` | `append_accepts_exact_utf8_cap_and_refuses_overflow`: Unicode byte count, cap, and unchanged file. |
| `test_concurrent_append_keeps_each_record_intact` | `concurrent_appends_preserve_all_complete_records`: 64 threads, complete unique JSONL records. |
| `test_event_handles_nonserializable_values_without_retaining_them` | `unserializable_payloads_are_not_retained_and_bounding_requires_marker`: set/object payloads contribute zero bytes and tokens. |
| `test_output_bounded_requires_a_structured_marker` | Same Rust test: literal “elided” does not count; structured truncation does. |
| `test_hook_context_event_measures_only_the_injected_context` | `context_counts_only_injected_text_and_deny_reason`: UTF-8 bytes, token estimate, event override, quiet and nonmapping payloads. |
| `test_record_rotates_a_full_log_instead_of_dropping` | `rotation_preserves_full_log_and_prunes_oldest`: full old record is moved and new record appended. |
| `test_summary_groups_by_event_and_tool_and_counts_over_bound` | `summarize_groups_events_and_ignores_malformed_lines`: grouping, malformed row skip, bound counts, ordering, shares. |
| `test_event_measures_edit_and_write_as_the_agent_sees_them` | `edit_and_write_count_agent_visible_projection`: projected edit/write bytes, bash bytes, native nonmapping pass through. |
| `test_hook_context_event_counts_a_deny_reason_as_injected_context` | `context_counts_only_injected_text_and_deny_reason`: denied hook reason bytes. |
| `test_record_prunes_rotated_logs_beyond_keep` | `rotation_preserves_full_log_and_prunes_oldest`: two newest rotations retained after six writes. |
| `test_record_disables_process_after_unwritable_directory` | `failed_sink_disables_future_records_and_reports_reason_once`: deterministic failing append, stderr, disabled state and no retry. |
| `test_hook_timing_event_reports_elapsed_ms_and_status` | `hook_timing_and_instruction_hash_contract`: rounded ms, labels, optional session ID. |
| `test_hook_context_event_tags_instructions_category_with_content_hash` | Same Rust test: stable SHA-256 prefix across sessions; quiet event omits optional fields. |
| `test_summarize_report_adds_sessions_hook_ms_and_instructions` | `report_counts_sessions_timings_and_repeat_instruction_content`: sessions, timing aggregates, repeated hashes, template and since. |
| `test_summarize_report_since_filters_out_older_events` | `report_since_filters_old_events_and_rejects_bad_duration`: two timestamps, one retained by 30m filter. |
| `test_parse_since_rejects_bad_duration` | Same Rust test: invalid duration raises `ValueError`. |
| `test_fleet_status.py::test_run_raises_on_nonzero` | `run_reports_nonzero_status_and_returns_stdout`: exit code and stderr in error. |
| `test_run_returns_stdout` | Same Rust test: command stdout returned. |
| `test_read_last_heard_keeps_newest_per_sender` | `newest_inbox_message_wins_and_summary_is_capped`: flags, timestamp ordering, 160 character cap. |
| `test_read_last_heard_rejects_invalid_json_contract` (3 payloads) | `invalid_compact_inbox_shapes_raise_fleet_error`: malformed JSON, null messages, missing fields. |
| `test_heading_seat_extraction` | `heading_seat_and_report_join_all_name_sources`: trailing seat and no marker. |
| `test_build_report_joins_all_name_sources` | Same Rust test: peer, heard, claim, heading seats. |
| `test_build_report_a2a_status_labels` | `report_labels_a2a_presence_and_aggregates_claims`: up/down/identity labels. |
| `test_build_report_claims_aggregation` | Same Rust test: count, sorted paths, soonest expiry. |
| `test_render_truncates_claim_paths` | `render_truncates_claim_paths_and_classifies_worktree_processes`: four visible paths and overflow marker. |
| `test_render_worktree_sections` | Same Rust test: disposable/process detail, protected summary. |
| `test_main_json_mode` | `cli_emits_json_or_human_report_and_module_help`: JSON stdout and exit status. |
| `test_main_human_mode` | Same Rust test: human heading and exit status. |
| `test_module_entrypoint` | Same Rust test: module help succeeds. |
| `test_worktree_regex_is_configured_via_project_paths` | `worktree_pattern_comes_from_project_paths`: assembled regex and positive/negative examples. |
| `test_hook_installer.py::test_merge_install_is_idempotent_and_preserves_unrelated_settings` (4 providers) | `install_merge_is_idempotent_for_every_provider_and_keeps_foreign_settings`: all providers, one managed command, unrelated config retained. |
| `test_startup_command_uses_current_interpreter_and_fixed_context_bounds` | `startup_command_has_fixed_context_bounds_and_grok_uses_first_turn`: interpreter, module, caps and no home path. |
| `test_grok_uses_first_turn_path_not_session_start_injection` | Same Rust test: existing SessionStart retained; UserPromptSubmit and once flag. |
| `test_default_install_is_dry_run` | `default_install_is_dry_run_and_invalid_json_is_not_written`: summary, unchanged bytes, no backup. |
| `test_apply_is_idempotent_and_uninstall_preserves_other_hooks` | `apply_is_idempotent_and_uninstall_keeps_foreign_hooks`: repeated apply and foreign settings/hooks after uninstall. |
| `test_rollback_restores_exact_preinstall_bytes` | `rollback_restores_exact_bytes_and_second_rollback_redoes_install`: exact original bytes and reversible rollback. |
| `test_explicit_backup_and_missing_file_rollback` | `explicit_backup_restores_missing_file_state`: explicit backup and absent file restoration. |
| `test_startup_command_round_trips_adversarial_identity` (4 identities) | `adversarial_identity_round_trips_and_blank_identity_is_rejected`: quote, dollar, space and backslash identities. |
| `test_startup_command_rejects_blank_identity` | Same Rust test: blank identity error. |
| `test_merge_install_rejects_non_object_root` | `merge_rejects_non_object_root_and_uninstall_keeps_foreign_groups`: typed error. |
| `test_merge_uninstall_keeps_foreign_groups_intact` | Same Rust test: remove managed SessionStart group and retain foreign UserPromptSubmit/model. |

The 3 retired Python files contained 752 Python code lines by `tokei` (317 context telemetry, 187 fleet status, 248 hook installer). The isolated runtime gate in `/tmp/forge-rust-contracts-expanded.log` passed the replacement suites: 12 telemetry, 8 fleet status, and 8 hook installer Rust tests, all with 0 failures. Every named Python case and parameter variant is mapped above. A repository search before deletion found no production or active test imports of the retired modules or their private helpers. Historical automatic mutation campaign manifests and receipts still name these old test paths and were preserved as provenance.
