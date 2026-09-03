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
    candidate_structure_facts_native,
    dead_tests_closure_native,
    duplicate_body_fingerprints_native,
    guardrail_ast_metrics_native,
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
    native_reuse_candidates_native,
    normalize_duplicate_rows_native,
    normalize_jscpd_report_native,
    normalize_mutation_path_native,
    plan_mutation_evidence_native,
    plan_mutation_repin_native,
    plan_mutation_scaffold_native,
    scan_untracked_import_closure_native,
    should_skip_mutation_path_native,
    stable_duplicate_key_native,
    tooling_boundary_facts_native,
    validate_mutation_receipt_native,
    verify_mutation_evidence_native,
)

SLOP_CORE_BUILD_HINT = (
    "slop_core is not installed. Build it into the venv with `make slop-core` "
    "(`maturin develop --release` in tooling/native/slop-core) or `uv sync`."
)


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
    "candidate_structure_facts_native",
    "dead_tests_closure_native",
    "duplicate_body_fingerprints_native",
    "guardrail_ast_metrics_native",
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
    "native_reuse_candidates_native",
    "normalize_duplicate_rows_native",
    "normalize_jscpd_report_native",
    "normalize_mutation_path_native",
    "plan_mutation_evidence_native",
    "plan_mutation_repin_native",
    "plan_mutation_scaffold_native",
    "scan_untracked_import_closure_native",
    "should_skip_mutation_path_native",
    "slop_core",
    "stable_duplicate_key_native",
    "tooling_boundary_facts_native",
    "validate_mutation_receipt_native",
    "verify_mutation_evidence_native",
]
