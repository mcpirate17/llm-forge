# Memory and KB retrieval test migration

The three Rust integration binaries own fixtures, callbacks, control flow, and assertions. They exercise the public Python boundary through PyO3 and the existing native implementation underneath it. They do not embed Python test scripts or run pytest. All 35 Rust tests passed, and the two mapped Python files were retired after checking for imports and helper consumers.

`src/conductor/test_kb_retrieve.py` has 33 declared tests. Every declaration maps below; the `top_k` parameter test covers both `0` and `-1` in the Rust case.

| Original `test_` suffix | Rust test in `python_contracts_kb_retrieve.rs` |
| --- | --- |
| `query_ranks_instruct_query_against_document_vectors` | `query_ranks_instruct_query_against_document_vectors` |
| `load_index_rejects_unbounded_gpu`; `load_index_allows_guest_gpu`; `load_index_requires_valid_fingerprint`; `load_index_rejects_unsupported_schema_version`; `load_index_rejects_card_vector_dimension_mismatch` | `index_load_validation_and_guest_gpu` |
| `embed_payload_pins_ctx_and_guest_gpu` | `embed_payload_pins_context_and_guest_gpu` |
| `broker_client_auto_starts_once_after_connection_refused` | `broker_auto_starts_once_after_connection_refused` |
| `load_cards_requires_kb_glob`; `load_cards_reads_all_kb_files_sorted`; `load_cards_requires_existing_directory` | `cards_globs_order_and_missing_directory` |
| `l2_normalize_rejects_zero_vector`; `assert_embedding_meta_requires_sha256_fingerprint`; `assert_embedding_meta_requires_positive_dimension`; `assert_embedding_meta_requires_paid_flag`; `assert_embedding_meta_requires_pinned_num_ctx_when_unpaid` | `embedding_metadata_validation_and_zero_vector` |
| `build_index_with_injected_embedder_is_deterministic`; `save_index_then_load_index_round_trips`; `save_index_writes_atomically_and_cleans_up_temp_file` | `build_save_load_and_atomic_cleanup` |
| `query_index_rejects_non_positive_top_k` (`0`, `-1`); `query_index_with_injected_embedder_ranks_by_dot_product` | `injected_query_ranking_and_both_invalid_top_k_values` |
| `main_index_builds_saves_and_reports_card_count` | `main_index_builds_saves_and_reports_count` |
| `main_query_prints_ranked_hits_from_the_given_index` | `main_query_routes_args_and_prints_rounded_hits` |
| `main_reports_retrieve_error_and_exits_2` | `main_retrieve_error_exits_two_and_prints_json_error` |
| `native_scoring_matches_builtin_sum_bit_for_bit` | `native_scoring_matches_python_builtin_sum_bit_for_bit` |
| `native_scoring_dim_mismatch_maps_to_retrieve_error` | `native_score_dimension_error_and_cancellation_parity` |
| `native_l2_normalize_matches_builtin_sum_reference` | `native_normalize_matches_python_builtin_sum_for_fixed_seed_samples` |
| `default_notes_dir_reads_the_workspace_configuration`; `default_notes_dir_resolves_research_notes_for_a_host_so_configured`; `default_notes_dir_environment_overrides_the_table` | `configured_notes_root_and_environment_precedence` |
| `every_embed_entry_point_budgets_for_a_cold_model_load` | `all_embed_entrypoints_have_cold_load_budget` |
| `broker_timeout_names_the_budget_and_the_recovery` | `broker_timeout_reports_budget_and_malformed_json_stays_distinct` |
| `malformed_json_is_not_reported_as_a_timeout` | `malformed_json_is_not_reported_as_a_timeout` |

`src/conductor/test_memory_index.py` has 36 declared tests. Its `top_k` parameter test also covers both `0` and `-1`. The randomized chunk differential keeps the fixed Python RNG seed, 60 documents, both modes, and the retired algorithm as a Rust-controlled reference in `python_contracts_memory_chunking.rs`. Other cases are in `python_contracts_memory_index.rs`.

| Original `test_` suffix | Rust test in `python_contracts_memory_index.rs` |
| --- | --- |
| `iter_source_files_respects_exclude`; `include_dirs_source_takes_named_subtrees_and_drops_the_index_file` | `source_iteration_exclusions_and_include_dirs` |
| `query_index_ranks_with_injected_embedder`; `query_index_rejects_non_positive_top_k` (`0`, `-1`); `query_index_with_injected_embedder_ranks_and_truncates_text` | `injected_query_ranking_and_both_invalid_top_k_values` |
| `load_index_allows_guest_gpu_rows`; `decode_index_row_rejects_wrong_schema_version`; `decode_index_row_requires_dimension_match` | `load_and_decode_rows_validate_schema_dimension_and_guest_gpu` |
| `reuse_rows_skips_unchanged_files`; `rows_by_path_groups_by_source_and_path`; `load_index_partition_splits_selected_preserved_and_dropped` | `row_grouping_reuse_and_partition_accounting` |
| `catalog_does_not_index_live_current_work`; `load_catalog_requires_matching_schema_version`; `load_catalog_requires_at_least_one_source`; `catalog_indexes_the_canonical_auto_memory` | `catalog_schema_sources_and_live_current_work_exclusion` |
| `save_index_replaces_atomically`; `index_build_result_total_rows_sums_new_and_preserved` | `save_index_atomic_replace_and_total_rows` |
| `partial_refresh_preserves_unselected_sources` | `partial_refresh_preserves_other_sources_and_reuses_unchanged_note` |
| `partial_refresh_requires_existing_full_index` | `partial_refresh_requires_an_existing_full_index` |
| `path_matches_source_honors_exclude_dir_names`; `path_matches_source_honors_exclude_globs`; `path_matches_source_defaults_glob_to_markdown` | `source_path_matching_dir_glob_and_default_markdown` |
| `index_write_lock_excludes_concurrent_holder_and_releases_on_exit` | `index_write_lock_excludes_other_holder_and_releases_after_exception` |
| `main_index_builds_saves_and_prints_when_changed` | `main_index_routes_sources_full_save_and_reports_counts` |
| `main_index_skips_save_when_unchanged_and_index_exists` | `main_index_skips_save_when_unchanged_and_cache_exists` |
| `main_query_prints_hits_from_the_given_index` | `main_query_uses_given_virtual_index_and_reports_hits` |
| `main_reports_retrieve_error_and_exits_2` | `main_reports_retrieve_error_from_both_query_paths` |
| `chunk_text_randomized_matches_python_reference`; `chunk_text_exotic_boundaries_parity`; `chunk_text_maps_native_tuple_fields_into_dict_fields` | `python_contracts_memory_chunking.rs::chunk_text_randomized_and_exotic_boundaries_match_reference` |
| `expand_root_resolves_relative_roots_against_the_workspace`; `expand_root_keeps_the_monorepo_default_for_a_host_so_configured` | `expand_root_resolves_configured_notes_and_literal_relative_roots` |
| `host_catalog_path_prefers_the_host_copy`; `host_catalog_path_falls_back_to_the_packaged_copy`; `host_catalog_path_refuses_a_named_catalog_that_is_absent`; `both_catalog_readers_resolve_the_same_file` | `host_catalog_precedence_and_auto_index_reader_agree` |

The three contract binaries are feature gated by `python-compat-tests`. Run them with the repository's Python environment and native extension available. The KB suite passed 18 cases, the memory suite passed 16, and the randomized chunking differential passed. Historical baseline and receipt references remain intact. `gh issue list --limit 100` returned no covering issue when this migration began.
