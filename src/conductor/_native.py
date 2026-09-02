"""The single seam between ``conductor`` and the project's native runtime.

Every native primitive the agent tooling uses is imported here and nowhere else in
``conductor/``. Today the symbols come from ``research_runtime_native`` (the Rust crate
under ``research/runtime/native/rust/research-runtime``; its ``mutation_*.rs`` and
``repository_analysis.rs`` modules are conductor-only helpers). Step 2 of the tooling
extraction swaps this module's import for a conductor-owned crate; nothing else in the
package changes.

Importing this module is as eager as importing the extension itself: modules that must
stay collectable without the extension import it at the point of use, never at module
scope.
"""

from research_runtime_native import (
    DeadTestsAnalysisNative,
    DeadTestsResolverNative,
    dead_tests_closure_native,
    inspect_mutation_campaign_native,
    is_mutation_test_path_native,
    load_mutation_campaign_native,
    load_mutation_registry_native,
    mutation_git_paths_native,
    mutation_patch_paths_native,
    mutation_registry_patterns_native,
    mutation_runner_lineage_accepts_native,
    mutation_source_drift_native,
    mutation_test_inventory_native,
    normalize_mutation_path_native,
    plan_mutation_evidence_native,
    plan_mutation_repin_native,
    plan_mutation_scaffold_native,
    scan_untracked_import_closure_native,
    should_skip_mutation_path_native,
    validate_mutation_receipt_native,
    verify_mutation_evidence_native,
)

__all__ = [
    "DeadTestsAnalysisNative",
    "DeadTestsResolverNative",
    "dead_tests_closure_native",
    "inspect_mutation_campaign_native",
    "is_mutation_test_path_native",
    "load_mutation_campaign_native",
    "load_mutation_registry_native",
    "mutation_git_paths_native",
    "mutation_patch_paths_native",
    "mutation_registry_patterns_native",
    "mutation_runner_lineage_accepts_native",
    "mutation_source_drift_native",
    "mutation_test_inventory_native",
    "normalize_mutation_path_native",
    "plan_mutation_evidence_native",
    "plan_mutation_repin_native",
    "plan_mutation_scaffold_native",
    "scan_untracked_import_closure_native",
    "should_skip_mutation_path_native",
    "validate_mutation_receipt_native",
    "verify_mutation_evidence_native",
]
