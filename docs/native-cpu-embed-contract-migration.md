# CPU embedding contract test migration

`src/conductor/test_cpu_embed.py` has 53 named test functions. Its two-value
busy-threshold parameter becomes two separate Rust tests, so
`python_contracts_cpu_embed` has 54 cases. The target calls the shipped
`conductor.cpu_embed` and `conductor.embedding_contract` APIs through PyO3.
All case assertions, expected values, callback behavior, and mock setup live
in Rust. The target uses temporary files and intercepted HTTP calls; it does
not load an embedding model or require a GPU.

The target is `native/conductor-native/tests/python_contracts_cpu_embed.rs`.
Its cases are in `python_contracts/cpu_embed_cases_{a,b,c}.rs`, with shared
Rust fixtures in `python_contracts/cpu_embed_support.rs` and the existing
`python_contracts/support.rs`. The six HTTP route cases enter and exit the
Starlette `TestClient` context so the application lifecycle is exercised.

The case inventory below records the original provider-to-Rust mapping.
The 54-case native target passed, and its provider and helper rows are registered
in `python_contract_targets_extra.tsv`, including `embedding_routes.toml`.
Importer checks found no production consumer of the original test module, so
`test_cpu_embed.py` retired in this cohort.

| Original Python case | Rust case |
| --- | --- |
| `test_pin_options_are_bounded` | `pin_options_are_bounded` |
| `test_choose_num_gpu_returns_zero_when_nvidia_smi_unavailable` | `choose_num_gpu_returns_zero_when_nvidia_smi_unavailable` |
| `test_choose_num_gpu_returns_zero_at_busy_threshold[at_threshold]` | `choose_num_gpu_returns_zero_at_busy_threshold` |
| `test_choose_num_gpu_returns_zero_at_busy_threshold[above_threshold]` | `choose_num_gpu_returns_zero_above_busy_threshold` |
| `test_choose_num_gpu_returns_ninety_nine_when_gpu_free` | `choose_num_gpu_returns_ninety_nine_when_gpu_free` |
| `test_choose_num_gpu_ignores_skipped_holders` | `choose_num_gpu_ignores_skipped_holders` |
| `test_choose_num_gpu_skips_malformed_csv_rows` | `choose_num_gpu_skips_malformed_csv_rows` |
| `test_openai_route_forwards_pinned_ollama_payload` | `openai_route_forwards_pinned_ollama_payload` |
| `test_rejects_non_string_input` | `rejects_non_string_input` |
| `test_rejects_malformed_embedding_vectors` | `rejects_malformed_embedding_vectors` |
| `test_unavailable_local_route_falls_back_to_enabled_paid_route` | `unavailable_local_route_falls_back_to_enabled_paid_route` |
| `test_quality_failure_does_not_call_local_route` | `quality_failure_does_not_call_local_route` |
| `test_ensure_service_rejects_running_broker_with_different_route` | `ensure_service_rejects_running_broker_with_different_route` |
| `test_embed_with_routing_skips_route_over_cost_cap` | `embed_with_routing_skips_route_over_cost_cap` |
| `test_embed_with_routing_rechecks_gpu_and_retries_after_ollama_restart` | `embed_with_routing_rechecks_gpu_and_retries_after_ollama_restart` |
| `test_embed_with_routing_rejects_unsupported_protocol` | `embed_with_routing_rejects_unsupported_protocol` |
| `test_embed_with_routing_aggregates_failures_from_every_route` | `embed_with_routing_aggregates_failures_from_every_route` |
| `test_embed_with_routing_stops_after_first_failure_when_fingerprint_required` | `embed_with_routing_stops_after_first_failure_when_fingerprint_required` |
| `test_paid_route_rejects_duplicate_response_indexes` | `paid_route_rejects_duplicate_response_indexes` |
| `test_post_json_returns_parsed_object_body` | `post_json_returns_parsed_object_body` |
| `test_post_json_rejects_non_http_scheme` | `post_json_rejects_non_http_scheme` |
| `test_post_json_wraps_network_error` | `post_json_wraps_network_error` |
| `test_post_json_rejects_non_object_response` | `post_json_rejects_non_object_response` |
| `test_health_route_reports_primary_route_metadata` | `health_route_reports_primary_route_metadata` |
| `test_health_route_returns_503_when_routing_fails` | `health_route_returns_503_when_routing_fails` |
| `test_embed_with_routing_rejects_empty_texts` | `embed_with_routing_rejects_empty_texts` |
| `test_embed_with_routing_wraps_contract_error` | `embed_with_routing_wraps_contract_error` |
| `test_choose_num_gpu_rejects_non_integer_forced_value` | `choose_num_gpu_rejects_non_integer_forced_value` |
| `test_pin_options_rejects_invalid_explicit_num_gpu` | `pin_options_rejects_invalid_explicit_num_gpu` |
| `test_ollama_embed_returns_empty_for_no_texts` | `ollama_embed_returns_empty_for_no_texts` |
| `test_ollama_embed_wraps_network_error` | `ollama_embed_wraps_network_error` |
| `test_ollama_embed_rejects_wrong_vector_count` | `ollama_embed_rejects_wrong_vector_count` |
| `test_ollama_embed_rejects_empty_vector` | `ollama_embed_rejects_empty_vector` |
| `test_l2_normalize_rejects_zero_vector` | `l2_normalize_rejects_zero_vector` |
| `test_openai_compatible_embed_requires_credential_env` | `openai_compatible_embed_requires_credential_env` |
| `test_openai_compatible_embed_rejects_non_list_data` | `openai_compatible_embed_rejects_non_list_data` |
| `test_openai_compatible_embed_rejects_row_missing_embedding` | `openai_compatible_embed_rejects_row_missing_embedding` |
| `test_openai_compatible_embed_rejects_invalid_index` | `openai_compatible_embed_rejects_invalid_index` |
| `test_openai_compatible_embed_rejects_non_numeric_value` | `openai_compatible_embed_rejects_non_numeric_value` |
| `test_openai_compatible_embed_returns_vectors_ordered_by_index` | `openai_compatible_embed_returns_vectors_ordered_by_index` |
| `test_startup_lock_serializes_and_releases_a_real_file_lock` | `startup_lock_serializes_and_releases_a_real_file_lock` |
| `test_read_json_url_returns_parsed_object` | `read_json_url_returns_parsed_object` |
| `test_read_json_url_rejects_non_object_response` | `read_json_url_rejects_non_object_response` |
| `test_validate_route_vectors_rejects_count_mismatch` | `validate_route_vectors_rejects_count_mismatch` |
| `test_validate_route_vectors_rejects_dimension_mismatch` | `validate_route_vectors_rejects_dimension_mismatch` |
| `test_validate_route_vectors_rejects_non_finite_values` | `validate_route_vectors_rejects_non_finite_values` |
| `test_validate_route_vectors_normalizes_only_for_l2_client_routes` | `validate_route_vectors_normalizes_only_for_l2_client_routes` |
| `test_resolve_model_returns_primary_route_model` | `resolve_model_returns_primary_route_model` |
| `test_resolve_model_falls_back_to_env_or_default_on_contract_error` | `resolve_model_falls_back_to_env_or_default_on_contract_error` |
| `test_models_route_lists_every_attempt_as_openai_style_model` | `models_route_lists_every_attempt_as_openai_style_model` |
| `test_models_route_propagates_contract_error_uncaught` | `models_route_propagates_contract_error_uncaught` |
| `test_warm_reports_what_the_model_load_cost` | `warm_reports_what_the_model_load_cost` |
| `test_warm_fails_loud_when_the_broker_returns_no_vector` | `warm_fails_loud_when_the_broker_returns_no_vector` |
| `test_broker_side_embed_calls_budget_for_a_cold_load` | `broker_side_embed_calls_budget_for_a_cold_load` |

Validation on Rust 1.98 with the Forge Python 3.12 environment passed:
`cargo test --offline --locked --features python-compat-tests --test
python_contracts_cpu_embed -- --test-threads=1` ran all 54 cases with no
failures or ignored tests. Scoped `cargo clippy` for the same target passed
with `-D warnings`. The build used two Cargo jobs and hid GPU devices.
