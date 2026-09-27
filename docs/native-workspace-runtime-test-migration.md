# Workspace runtime matrix test migration

The 26 named Python cases in `src/conductor/test_workspace_runtime_matrix.py`
were retired after Rust-owned integration tests under
`native/conductor-native/tests/python_contracts_workspace_runtime_*.rs`
passed. These tests call the shipped Python API through PyO3; Rust owns the
inputs and assertions. Python callbacks only stand in for subprocess, HTTP,
output capture, and fixture wiring. The clerk tests never contact Ollama or a
GPU.

| Old Python case | Rust assertion target |
| --- | --- |
| `test_aggregate_status_precedence` | `core::required_and_optional_status_precedence` |
| `test_optional_cell_does_not_block_pass` | `core::required_and_optional_status_precedence` |
| `test_graph_evidence_requires_semantic_result` | `core::graph_requires_semantic_result_and_rejects_mixed_provider_rows` |
| `test_graph_evidence_rejects_mixed_provider_rows` | `core::graph_requires_semantic_result_and_rejects_mixed_provider_rows` |
| `test_missing_graph_evidence_is_not_ready` | `core::graph_requires_semantic_result_and_rejects_mixed_provider_rows` |
| `test_extract_reported_tokens_from_jsonl` | existing `matrix::status_token_and_graph_wrappers_match_native_policy` |
| `test_hook_program_controls_pass` | `reconcile::hook_program_controls_pass_in_generated_foreign_repository` |
| `test_grok_inspect_command_is_injectable` | `core::grok_command_and_launcher_specs_remain_bounded` |
| `test_runtime_support_uses_foreign_root_and_accepts_empty_policy` | `support::foreign_root_uses_package_import_path_and_accepts_empty_policy` |
| `test_runtime_support_refuses_malformed_preamble_payload` (three parameters) | `support::malformed_preamble_payloads_fail_even_when_grok_passes` |
| `test_launcher_specs_cover_required_programs` | `core::grok_command_and_launcher_specs_remain_bounded` |
| `test_reconcile_receipt_uses_preserved_terminal_usage` | `reconcile::launcher_reconciliation_uses_preserved_terminal_usage` |
| `test_reconcile_graph_evidence_preserves_other_cells` | `reconcile::graph_reconciliation_preserves_other_cells_and_archives_old_receipt` |
| `test_clerk_gpu_preflight_blocks_active_novel_gpu_claim` | `core::gpu_preflight_blocks_active_novel_claims_and_filters_desktop_process` |
| `test_clerk_gpu_preflight_blocks_compute_process_and_loaded_model` | `core::gpu_preflight_blocks_active_novel_claims_and_filters_desktop_process` |
| `test_ollama_model_rows_and_token_metrics_fail_closed` | `core::ollama_rows_and_token_metrics_fail_closed` |
| `test_clerk_canary_defers_without_loading_during_avo` | existing `matrix::blocked_gpu_preflight_never_invokes_clerk_http` |
| `test_clerk_canary_passes_bounded_schema_and_unloads` | `clerk::valid_clerk_canary_uses_bounded_schema_and_unloads` |
| `test_clerk_canary_invalid_schema_fails_closed_and_unloads` | `clerk::invalid_clerk_schema_fails_closed_and_unloads` |
| `test_reconcile_clerk_preserves_expensive_cells` | `reconcile::clerk_reconciliation_preserves_expensive_cells_and_archives_old_receipt` |
| `test_explicit_root_scans_the_named_repo_not_cwd` | `cli::explicit_root_scans_named_repo_instead_of_cwd` |
| `test_default_root_uses_cwd_toplevel_not_module_location` | `cli::default_root_uses_current_worktree_not_module_location` |
| `test_cwd_outside_worktree_refuses_rather_than_falling_back` | `cli::cwd_outside_worktree_refuses_without_building_receipt` |
| `test_resolved_root_is_printed` | `cli::resolved_root_is_printed` |
| `test_root_mismatch_warns` | `cli::root_mismatch_warns_with_requested_root` |
| `test_default_output_relative_path_resolves_against_root` | `cli::default_relative_output_is_resolved_against_root` |

The old helpers map as follows: `_cell` is `core::cell`; `_valid_grok_inspect`
is `support::inspect_payload`; `_ready_clerk_preflight` is constructed in
`clerk::exercise`; `_git` and `_init_repo` are `cli::git` and `cli::init_repo`;
`_stub_build_receipt` is the `CliMock` callback. The shared `hook_repo`
fixture is reused through `workspace_runtime_fixture::HookRepo`, which restores
its monkeypatches on drop. The existing native unit tests also cover status,
graph, usage, GPU policy, clerk adjudication, and receipt replacement directly
without Python.

Feature-enabled integration targets use the `python_contracts_` prefix so the
CI selector `--test 'python_contracts_*'` includes them.

The six feature-enabled targets passed 22 tests in the serialized Forge run
recorded at `/tmp/forge-runtime-migration-tests.log`: 19 new tests and three
existing matrix boundary tests. The consumer audit found no production or
active test imports of the retired module or its private helpers, and no
explicit path in the Makefile, package manifest, or CI workflow. Historical
mutation campaign hashes, grandfathered node IDs, and duplication baselines
still name the old path; they remain intact as historical records. The issue
tracker listed no covering issue when this migration began.
