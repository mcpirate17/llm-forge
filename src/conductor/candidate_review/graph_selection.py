"""Test selection for a candidate: graph-selected pytest files and crate tests.

Split out of ``verification.py`` (which owns the evidence checks) so each stays
under the module size bar. The graph query reads the code-review-graph SQLite
store read-only and fails closed when its head does not match the candidate.
"""

from __future__ import annotations

from pathlib import Path, PurePosixPath
from typing import Sequence

from conductor.candidate_review.checks import ReviewContext
from conductor.candidate_review.model import sha256_json


def _graph_database(repo: Path) -> Path:
    from conductor.graph_test_select import graph_database_path

    return graph_database_path(repo)


def _graph_test_paths(
    ctx: ReviewContext, source_paths: Sequence[str]
) -> tuple[set[str], dict[str, object]]:
    from conductor.graph_test_select import GraphSelectError, graph_test_plan

    expected = (
        ctx.candidate.base_commit_oid
        if ctx.candidate.kind == "index"
        else ctx.candidate.commit_oid
    )
    if not expected:
        raise RuntimeError("candidate revision is missing")
    try:
        plan = graph_test_plan(
            ctx.repo, source_paths, expected_head=expected, inventory_root=ctx.snapshot
        )
    except GraphSelectError as exc:
        raise RuntimeError(str(exc)) from exc
    tests = {
        relative
        for relative in plan["paths"]
        if relative.endswith(".py") and (ctx.snapshot / relative).is_file()
    }
    metadata = plan["metadata"]
    if metadata.get("git_head_sha") != expected:
        raise RuntimeError(
            f"stale code-review graph: expected {expected}, found {metadata.get('git_head_sha')}"
        )
    graph = {
        "head_sha": metadata.get("git_head_sha"),
        "schema_version": metadata.get("schema_version"),
        "last_updated": metadata.get("last_updated"),
        "generation": plan["generation"],
        "selection_scope": plan["scope"],
        "complete": plan["complete"],
        "fallback_reasons": plan["reasons"],
        "unmapped": plan["unmapped"],
        "selected_edges": len(tests),
        "evidence_sha256": sha256_json(sorted(tests)),
    }
    return tests, graph


def _rust_crate_tests(
    ctx: ReviewContext, source_paths: Sequence[str]
) -> dict[str, tuple[str, ...]]:
    """Each changed Rust source mapped to the test files of its own crate.

    The graph selector here yields pytest files. A crate whose tests are
    ``#[cfg(test)]`` modules or files under ``tests/`` therefore selected
    nothing, so a change
    confined to Rust reported "no targeted tests" while its own suite sat beside
    it -- structural for a project whose compute is deliberately native.

    Returned per source rather than as one set, so a change that touches Python
    as well is still answerable for the Python half. These paths are evidence
    that the changed code is tested, not work for the pytest runner, so the
    caller keeps them out of the executed set.
    """

    crates: dict[str, PurePosixPath] = {}
    for source in source_paths:
        path = PurePosixPath(source)
        if path.suffix != ".rs":
            continue
        for parent in path.parents:
            if (ctx.snapshot / parent / "Cargo.toml").is_file():
                crates[source] = parent
                break
    cache: dict[PurePosixPath, tuple[str, ...]] = {}
    covered: dict[str, tuple[str, ...]] = {}
    for source, crate in crates.items():
        files = cache.get(crate)
        if files is None:
            files = _crate_test_files(ctx, crate)
            cache[crate] = files
        if files:
            covered[source] = files
    return covered


def _crate_test_files(ctx: ReviewContext, crate: PurePosixPath) -> tuple[str, ...]:
    """The Rust files in ``crate`` that carry tests, in path order."""

    tests: list[str] = []
    for candidate in sorted((ctx.snapshot / crate).rglob("*.rs")):
        rel = candidate.relative_to(ctx.snapshot).as_posix()
        # target/ is build output: a vendored dependency's test modules there
        # would report coverage this crate does not have.
        if "/target/" in f"/{rel}":
            continue
        if candidate.parent == ctx.snapshot / crate / "tests":
            tests.append(rel)
            continue
        try:
            text = candidate.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        if "#[cfg(test)]" in text:
            tests.append(rel)
    return tuple(tests)
