"""Test selection for a candidate: graph-selected and convention-matched pytest files.

Split out of ``verification.py`` (which owns the evidence checks) so each stays
under the module size bar. The graph query reads the code-review-graph SQLite
store read-only and fails closed when its head does not match the candidate.
"""

from __future__ import annotations

import sqlite3
from pathlib import Path, PurePosixPath
from typing import Sequence

from conductor.candidate_review.checks import ReviewContext
from conductor.candidate_review.model import sha256_json


def _graph_database(repo: Path) -> Path:
    return repo / ".code-review-graph" / "graph.db"


def _graph_test_paths(
    ctx: ReviewContext, source_paths: Sequence[str]
) -> tuple[set[str], dict[str, object]]:
    database = _graph_database(ctx.repo)
    if not database.is_file():
        raise RuntimeError("code-review graph database is missing")
    uri = f"file:{database.as_posix()}?mode=ro&immutable=1"
    connection = sqlite3.connect(uri, uri=True, timeout=2.0)
    try:
        metadata = dict(connection.execute("SELECT key, value FROM metadata"))
        expected = (
            ctx.candidate.base_commit_oid
            if ctx.candidate.kind == "index"
            else ctx.candidate.commit_oid
        )
        if not expected or metadata.get("git_head_sha") != expected:
            raise RuntimeError(
                "stale code-review graph: "
                f"expected {expected}, found {metadata.get('git_head_sha')}"
            )
        absolute = [str((ctx.repo / path).resolve()) for path in source_paths]
        if not absolute:
            return set(), {
                "head_sha": expected,
                "schema_version": metadata.get("schema_version"),
            }
        placeholders = ",".join("?" for _ in absolute)
        rows = connection.execute(
            f"""
            SELECT DISTINCT source.file_path, edge.kind, target.qualified_name
            FROM nodes AS target
            JOIN edges AS edge ON edge.target_qualified = target.qualified_name
            JOIN nodes AS source ON source.qualified_name = edge.source_qualified
            WHERE target.file_path IN ({placeholders}) AND source.is_test = 1
            ORDER BY source.file_path, edge.kind, target.qualified_name
            """,
            absolute,
        ).fetchall()
        tests: set[str] = set()
        evidence_rows: list[tuple[str, str, str]] = []
        for file_path, edge_kind, target in rows:
            try:
                relative = Path(file_path).resolve().relative_to(ctx.repo).as_posix()
            except ValueError:
                continue
            # Rust `#[test]` nodes are is_test too; they run under cargo test, not pytest.
            if relative.endswith(".py") and (ctx.snapshot / relative).is_file():
                tests.add(relative)
                evidence_rows.append((relative, edge_kind, target))
        graph = {
            "head_sha": expected,
            "schema_version": metadata.get("schema_version"),
            "last_updated": metadata.get("last_updated"),
            "selected_edges": len(evidence_rows),
            "evidence_sha256": sha256_json(evidence_rows),
        }
        return tests, graph
    finally:
        connection.close()


def _convention_tests(ctx: ReviewContext, source_paths: Sequence[str]) -> set[str]:
    names = {f"test_{PurePosixPath(path).stem}.py" for path in source_paths}
    tests: set[str] = set()
    for base in (
        "conductor",
        "research/tests",
        "component_fab/tests",
        "aria_core/tests",
        "aria_designer/tests",
    ):
        root = ctx.snapshot / base
        if not root.is_dir():
            continue
        for path in root.rglob("test*.py"):
            rel = path.relative_to(ctx.snapshot).as_posix()
            if path.name in names:
                tests.add(rel)
                continue
            try:
                text = path.read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError):
                continue
            for source in source_paths:
                module = source.removesuffix(".py").replace("/", ".")
                if module in text:
                    tests.add(rel)
                    break
    return tests
