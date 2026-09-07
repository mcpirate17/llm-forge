"""Snapshot-scoped size, graph, test, and evidence metrics."""

from __future__ import annotations

import hashlib
from pathlib import Path

from conductor.reuse import core as slop_core
from conductor.reuse import graph_index

SCHEMA_VERSION = 2
_CODE_SUFFIXES = {
    ".py",
    ".c",
    ".cc",
    ".cpp",
    ".cu",
    ".h",
    ".hpp",
    ".rs",
    ".js",
    ".jsx",
    ".ts",
    ".tsx",
}
_COMMENT_PREFIXES = ("#", "//", "/*", "*", "--")
_TEST_PARTS = {"tests", "test"}


def snapshot(
    repo: Path,
    targets: list[str],
    exclude: set[str],
    index: graph_index.GraphIndex,
    *,
    duplicate_lines: int,
    native_candidates: int,
    contract_report: dict | None = None,
    incomplete_sources: list[str],
) -> dict:
    records = slop_core.audit_repository_scan(
        str(repo),
        targets,
        sorted(_CODE_SUFFIXES),
        sorted(exclude | {".git", ".venv", "node_modules", "__pycache__"}),
        list(_COMMENT_PREFIXES),
    )
    production_loc = test_loc = 0
    production_files = test_files = 0
    digest = hashlib.sha256()
    for relative_text, loc in records:
        relative = Path(relative_text)
        digest.update(relative.as_posix().encode("utf-8"))
        digest.update(str(loc).encode("ascii"))
        if set(relative.parts) & _TEST_PARTS or relative.name.startswith("test_"):
            test_loc += loc
            test_files += 1
        else:
            production_loc += loc
            production_files += 1
    status = index.status(targets)
    try:
        symbols = len(index.symbols())
        dependencies = index.edge_count("IMPORTS_FROM")
    except Exception:  # noqa: BLE001 - graph metrics are best-effort snapshot context
        symbols = dependencies = 0
    completeness = [*incomplete_sources]
    if not status.complete:
        completeness.append(f"graph:{status.reason}")
    report = contract_report or {}
    contract_violations = len(report.get("manifest_errors", [])) + len(
        report.get("missing_manifest_tests", [])
    )
    unclassified = len(report.get("unclassified_test_leaves", []))
    if completeness:
        completion_status = "incomplete"
    elif contract_violations or unclassified:
        completion_status = "test_contract_migration_required"
    else:
        completion_status = "audit_exhausted_for_snapshot"
    return {
        "schema_version": SCHEMA_VERSION,
        "snapshot_hash": digest.hexdigest(),
        "production_loc": production_loc,
        "test_loc": test_loc,
        "production_files": production_files,
        "test_files": test_files,
        "modules": len(records),
        "symbols": symbols,
        "dependency_edges": dependencies,
        "duplicate_loc": duplicate_lines,
        "native_reuse_candidates": native_candidates,
        "contract_violations": contract_violations,
        "unclassified_test_leaves": unclassified,
        "evidence_complete": not completeness,
        "incomplete_reasons": sorted(set(completeness)),
        "completion_status": completion_status,
        "graph": status.as_dict(),
    }
