"""The single seam between ``conductor`` and its native crates.

Every native primitive the agent tooling uses is imported here and nowhere else in
``conductor/``. The symbols come from two conductor-owned Rust crates under
``tooling/native``: ``conductor_native`` (mutation campaigns, receipts, evidence,
dead-test and untracked-import closure), which is required, and ``slop_core`` (the
ablation engine and test index behind the equivalence probe), whose absence is decided
once, here, at import time -- see :func:`slop_core`. The project's research crate,
``research_runtime_native``, is no longer imported anywhere in this package.

Importing this module is as eager as importing the extensions themselves: modules that
must stay collectable without them import it at the point of use, never at module
scope.
"""

from __future__ import annotations

from conductor_native import (
    DeadTestsAnalysisNative,
    DeadTestsResolverNative,
    a2a_compact_message_native,
    a2a_compact_threads_native,
    a2a_retention_evidence_native,
    a2a_retention_manifests_native,
    a2a_validate_coordination_v2_native,
    admission_errors_native,
    analyze_test_value_native,
    branch_policy_second_branch_conflict_native,
    branch_policy_stamp_age_hours_native,
    branch_policy_validate_bindings_native,
    branch_policy_validate_name_native,
    candidate_structure_facts_native,
    compare_duplicate_baseline_native,
    context_telemetry_append_native,
    context_telemetry_event_native,
    context_telemetry_format_summary_native,
    context_telemetry_hook_event_native,
    context_telemetry_hook_event_with_hash_native,
    context_telemetry_injected_context_native,
    context_telemetry_model_visible_output_native,
    context_telemetry_parse_since_native,
    context_telemetry_record_native,
    context_telemetry_report_native,
    context_telemetry_rotate_native,
    context_telemetry_summarize_native,
    dead_tests_closure_native,
    duplicate_body_fingerprints_native,
    fleet_status_build_report_native,
    fleet_status_render_native,
    git_changed_lines_native,
    git_diff_changes_native,
    git_rename_sources_native,
    git_tree_entries_native,
    graph_context_native,
    guardrail_ast_metrics_native,
    hook_installer_is_managed_native,
    hook_installer_merge_install_native,
    hook_installer_shlex_join_native,
    hook_installer_shlex_split_native,
    hook_installer_without_managed_native,
    hook_merge_native,
    hook_context_project_native,
    inspect_mutation_campaign_native,
    is_mutation_test_path_native,
    kb_retrieve_l2_normalize_native,
    kb_retrieve_score_cards_native,
    load_mutation_campaign_native,
    load_mutation_registry_native,
    load_tree_receipt_native,
    load_value_analysis_native,
    memory_index_build_sidecar_native,
    memory_index_chunk_text_native,
    memory_index_metadata_native,
    memory_index_query_file_native,
    memory_index_query_sidecar_native,
    memory_index_score_rows_native,
    memory_index_sidecar_header_native,
    memory_index_sidecar_is_fresh_native,
    mutation_git_paths_native,
    mutation_patch_paths_native,
    mutation_plan_native,
    mutation_registry_patterns_native,
    mutation_runner_lineage_accepts_native,
    mutation_rust_test_surface_native,
    mutation_source_drift_native,
    mutation_test_inventory_native,
    native_reuse_candidates_native,
    normalize_duplicate_rows_native,
    normalize_jscpd_report_native,
    normalize_mutation_path_native,
    plan_mutation_evidence_native,
    project_context_parse_config_native,
    receipt_compact_directory_native,
    receipt_expand_detail_native,
    receipt_inventory_digest_native,
    receipt_manifest_pins_native,
    receipt_sha256_native,
    receipt_slim_detail_native,
    scan_untracked_import_closure_native,
    should_skip_mutation_path_native,
    stable_duplicate_key_native,
    test_value_receipt_errors_native,
    tooling_boundary_facts_native,
    validate_mutation_receipt_native,
    verify_mutation_evidence_native,
    verify_tree_receipt_native,
)

SLOP_CORE_BUILD_HINT = (
    "slop_core is not installed. Build it into the venv with `make slop-core` "
    "(`maturin develop --release` in tooling/native/slop-core) or `uv sync`."
)


def a2a_store_native(root: str, name: str) -> object:
    from conductor_native import A2aSqliteStore

    return A2aSqliteStore(root, name)


def candidate_benchmark_python_input_native(source_dir: str) -> tuple[str, bytes]:
    from conductor_native import candidate_benchmark_python_input_native as prepare

    return prepare(source_dir)


def candidate_checks_native(operation: str, payload_json: str) -> str:
    from conductor_native import candidate_checks_native as evaluate

    return evaluate(operation, payload_json)


def candidate_verification_native(operation: str, request_json: str) -> str:
    from conductor_native import candidate_verification_native as decide

    return decide(operation, request_json)


def contract_test_plan_native(repo_root: str, changed_paths: list[str]) -> str:
    from conductor_native import contract_test_plan_native as plan

    return plan(repo_root, changed_paths)


def resolve_forge_binary_native(
    project_dir: str,
    python_executable: str,
    configured: str | None,
    path_forge: str | None,
) -> str | None:
    from conductor_native import resolve_forge_binary_native as resolve

    return resolve(project_dir, python_executable, configured, path_forge)


def candidate_verification_ast_native(source: str, path: str) -> str:
    from conductor_native import candidate_verification_ast_native as definitions

    return definitions(source, path)


def workspace_runtime_matrix_native(operation: str, payload_json: str) -> str:
    from conductor_native import workspace_runtime_matrix_native as dispatch

    return dispatch(operation, payload_json)


def candidate_policy_parse_native(raw_json: str, today_iso: str) -> str:
    from conductor_native import candidate_policy_parse_native as parse

    return parse(raw_json, today_iso)


def candidate_policy_classify_native(change_json: str, globs_json: str) -> str:
    from conductor_native import candidate_policy_classify_native as classify

    return classify(change_json, globs_json)


def candidate_value_waivers_parse_native(raw_json: str) -> str:
    from conductor_native import candidate_value_waivers_parse_native as parse

    return parse(raw_json)


def candidate_policy_fragment_native(
    operation: str, raw_json: str, today_iso: str
) -> str:
    from conductor_native import candidate_policy_fragment_native as parse

    return parse(operation, raw_json, today_iso)


def mutation_refresh_native(request_json: str) -> str:
    """Load native campaign refresh at its compatibility boundary."""
    from conductor_native import mutation_refresh_native as refresh

    return refresh(request_json)


def reuse_consolidation_native(operation: str, payload_json: str) -> str:
    from conductor_native import reuse_consolidation_native as evaluate

    return evaluate(operation, payload_json)


def project_context_native(operation: str, payload_json: str) -> str:
    from conductor_native import project_context_native as evaluate

    return evaluate(operation, payload_json)


def project_paths_relative_native(raw: str, source: str) -> str:
    from conductor_native import project_paths_relative_native as resolve

    return resolve(raw, source)


def project_paths_resolve_native(root: str) -> list[tuple[str, bool]]:
    from conductor_native import project_paths_resolve_native as resolve

    return resolve(root)


def project_paths_integration_branch_native(root: str) -> str:
    from conductor_native import project_paths_integration_branch_native as resolve

    return resolve(root)


def project_paths_retired_integration_branches_native(root: str) -> list[str]:
    from conductor_native import (
        project_paths_retired_integration_branches_native as resolve,
    )

    return resolve(root)


def project_paths_worktree_patterns_native(root: str) -> list[str]:
    from conductor_native import project_paths_worktree_patterns_native as resolve

    return resolve(root)


def project_paths_enclosing_repo_native(start: str) -> str | None:
    from conductor_native import project_paths_enclosing_repo_native as resolve

    return resolve(start)


def project_paths_host_root_native(start: str | None = None) -> str:
    from conductor_native import project_paths_host_root_native as resolve

    return resolve(start)


def project_paths_package_tree_root_native(package_dir: str) -> str:
    from conductor_native import project_paths_package_tree_root_native as resolve

    return resolve(package_dir)


def guardrail_duplicate_candidates_native(
    files: list[list[str]], window: int
) -> tuple[list[tuple[int, int]], int, int]:
    """Load the audit index when needed; an older extension fails at this seam."""
    from conductor_native import guardrail_duplicate_candidates_native as candidates

    return candidates(files, window)


# Resolve new entry points on use, so an older installed wheel does not prevent
# unrelated tooling from importing this seam while the native package is updated.
def analyze_test_value_reports_native(
    spec_json: str,
    baseline_reports_json: str,
    mutant_reports_json: str,
    mutant_outcomes_json: str,
) -> str:
    from conductor_native import analyze_test_value_reports_native as implementation

    return implementation(
        spec_json, baseline_reports_json, mutant_reports_json, mutant_outcomes_json
    )


def mutation_value_pytest_identity_native(nodeid: str) -> tuple[str, str]:
    from conductor_native import mutation_value_pytest_identity_native as implementation

    return implementation(nodeid)


def mutation_value_cargo_identity_native(nodeid: str) -> str:
    from conductor_native import mutation_value_cargo_identity_native as implementation

    return implementation(nodeid)


def mutation_value_ctest_identity_native(nodeid: str) -> str:
    from conductor_native import mutation_value_ctest_identity_native as implementation

    return implementation(nodeid)


def mutation_value_attribution_supported_native(
    adapter: str, ranked: list[str]
) -> bool:
    from conductor_native import (
        mutation_value_attribution_supported_native as implementation,
    )

    return implementation(adapter, ranked)


def mutation_value_parse_junit_native(
    path: str, adapter: str, ranked: list[str]
) -> str:
    from conductor_native import mutation_value_parse_junit_native as implementation

    return implementation(path, adapter, ranked)


def mutation_value_parse_cargo_native(stdout: str, ranked: list[str]) -> str:
    from conductor_native import mutation_value_parse_cargo_native as implementation

    return implementation(stdout, ranked)


def mutation_evidence_exit_code_native(result_json: str) -> int:
    from conductor_native import mutation_evidence_exit_code_native as implementation

    return implementation(result_json)


def mutation_canary_verdict_native(report_json: str) -> str:
    from conductor_native import mutation_canary_verdict_native as implementation

    return implementation(report_json)


def mutation_github_output_native(result_json: str) -> str:
    from conductor_native import mutation_github_output_native as implementation

    return implementation(result_json)


class SlopCoreUnavailable(ImportError):
    """The ``slop_core`` extension is missing; the message names the build step.

    An ``ImportError`` so ``pytest.importorskip(..., exc_type=ImportError)`` in the
    engine's test modules still skips cleanly on an unbuilt tree.
    """


def _import_slop_core():
    """``(module, None)`` when the extension imports, else ``(None, why)``."""
    try:
        import slop_core
    except ImportError as exc:
        return None, f"{SLOP_CORE_BUILD_HINT} ({exc})"
    return slop_core, None


# One decision, taken when this module loads. Consumers either take the module from
# `slop_core()` and fail loud, or read SLOP_CORE_UNAVAILABLE and report it by name.
# There is no per-call retry and no pure-Python fallback.
_SLOP_CORE, SLOP_CORE_UNAVAILABLE = _import_slop_core()


def slop_core():
    """The ``slop_core`` extension, or :class:`SlopCoreUnavailable` naming why not."""
    if _SLOP_CORE is None:
        raise SlopCoreUnavailable(SLOP_CORE_UNAVAILABLE)
    return _SLOP_CORE


__all__ = [
    "SLOP_CORE_BUILD_HINT",
    "SLOP_CORE_UNAVAILABLE",
    "DeadTestsAnalysisNative",
    "DeadTestsResolverNative",
    "SlopCoreUnavailable",
    "a2a_compact_message_native",
    "a2a_compact_threads_native",
    "a2a_retention_evidence_native",
    "a2a_retention_manifests_native",
    "a2a_store_native",
    "a2a_validate_coordination_v2_native",
    "admission_errors_native",
    "analyze_test_value_native",
    "analyze_test_value_reports_native",
    "branch_policy_second_branch_conflict_native",
    "branch_policy_stamp_age_hours_native",
    "branch_policy_validate_bindings_native",
    "branch_policy_validate_name_native",
    "candidate_checks_native",
    "candidate_policy_classify_native",
    "candidate_policy_fragment_native",
    "candidate_policy_parse_native",
    "candidate_structure_facts_native",
    "candidate_value_waivers_parse_native",
    "candidate_verification_ast_native",
    "candidate_verification_native",
    "compare_duplicate_baseline_native",
    "context_telemetry_append_native",
    "context_telemetry_event_native",
    "context_telemetry_format_summary_native",
    "context_telemetry_hook_event_native",
    "context_telemetry_hook_event_with_hash_native",
    "context_telemetry_injected_context_native",
    "context_telemetry_model_visible_output_native",
    "context_telemetry_parse_since_native",
    "context_telemetry_record_native",
    "context_telemetry_report_native",
    "context_telemetry_rotate_native",
    "context_telemetry_summarize_native",
    "dead_tests_closure_native",
    "duplicate_body_fingerprints_native",
    "fleet_status_build_report_native",
    "fleet_status_render_native",
    "git_changed_lines_native",
    "git_diff_changes_native",
    "git_rename_sources_native",
    "git_tree_entries_native",
    "graph_context_native",
    "guardrail_ast_metrics_native",
    "guardrail_duplicate_candidates_native",
    "hook_installer_is_managed_native",
    "hook_installer_merge_install_native",
    "hook_installer_shlex_join_native",
    "hook_installer_shlex_split_native",
    "hook_installer_without_managed_native",
    "hook_merge_native",
    "hook_context_project_native",
    "inspect_mutation_campaign_native",
    "is_mutation_test_path_native",
    "kb_retrieve_l2_normalize_native",
    "kb_retrieve_score_cards_native",
    "load_mutation_campaign_native",
    "load_mutation_registry_native",
    "load_tree_receipt_native",
    "load_value_analysis_native",
    "memory_index_build_sidecar_native",
    "memory_index_chunk_text_native",
    "memory_index_metadata_native",
    "memory_index_query_file_native",
    "memory_index_query_sidecar_native",
    "memory_index_score_rows_native",
    "memory_index_sidecar_header_native",
    "memory_index_sidecar_is_fresh_native",
    "mutation_canary_verdict_native",
    "mutation_evidence_exit_code_native",
    "mutation_git_paths_native",
    "mutation_github_output_native",
    "mutation_patch_paths_native",
    "mutation_plan_native",
    "mutation_refresh_native",
    "mutation_registry_patterns_native",
    "mutation_runner_lineage_accepts_native",
    "mutation_rust_test_surface_native",
    "mutation_source_drift_native",
    "mutation_test_inventory_native",
    "mutation_value_attribution_supported_native",
    "mutation_value_cargo_identity_native",
    "mutation_value_ctest_identity_native",
    "mutation_value_parse_cargo_native",
    "mutation_value_parse_junit_native",
    "mutation_value_pytest_identity_native",
    "native_reuse_candidates_native",
    "normalize_duplicate_rows_native",
    "normalize_jscpd_report_native",
    "normalize_mutation_path_native",
    "plan_mutation_evidence_native",
    "project_context_native",
    "project_context_parse_config_native",
    "project_paths_enclosing_repo_native",
    "project_paths_host_root_native",
    "project_paths_integration_branch_native",
    "project_paths_package_tree_root_native",
    "project_paths_relative_native",
    "project_paths_resolve_native",
    "project_paths_retired_integration_branches_native",
    "project_paths_worktree_patterns_native",
    "receipt_compact_directory_native",
    "receipt_expand_detail_native",
    "receipt_inventory_digest_native",
    "receipt_manifest_pins_native",
    "receipt_sha256_native",
    "receipt_slim_detail_native",
    "reuse_consolidation_native",
    "scan_untracked_import_closure_native",
    "should_skip_mutation_path_native",
    "slop_core",
    "stable_duplicate_key_native",
    "test_value_receipt_errors_native",
    "tooling_boundary_facts_native",
    "validate_mutation_receipt_native",
    "verify_mutation_evidence_native",
    "verify_tree_receipt_native",
    "workspace_runtime_matrix_native",
]
