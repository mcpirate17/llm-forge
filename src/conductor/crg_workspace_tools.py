#!/usr/bin/env python3
"""Workspace sub-graph tools registered on the code-review-graph MCP server.

Makes the AST projection (``conductor.graph_context``) and the memory/KB
retrievers (``conductor.memory_index``, ``conductor.kb_retrieve``) reachable
as MCP tools. Every harness that launches ``conductor.crg_server`` gets them
without shelling out, and a call counts as the mandatory graph-gate call
because it shares the ``mcp__code-review-graph__`` prefix.
"""

from __future__ import annotations

import os
import sqlite3
from collections.abc import Callable
from dataclasses import replace
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Final

from conductor import kb_retrieve, memory_vectors
from conductor.crg_embedding_text import docstring_for
from conductor.graph_context import (
    GraphContextError,
    format_markdown_context,
    get_file_context,
)
from conductor.graph_test_select import graph_database_path
from conductor.project_paths import host_root
from conductor.session_brief import brief as session_brief
from conductor.session_brief import snippet as _snippet

ROOT: Final[Path] = host_root()
# Skeletons of big files exceeded the 8 KB context bound on 56 % of calls (telemetry
# baseline 2026-09-01); cap and say how to narrow instead of shipping the whole thing.
AST_CONTEXT_MAX_BYTES: Final[int] = int(
    os.environ.get("CRG_AST_CONTEXT_MAX_BYTES", "8000")
)
MAX_SOURCE_BYTES: Final[int] = 12_000
LOCATE_LIMIT: Final[int] = 10
# Search-result fields that only restate ``signature``/``qualified_name`` or carry
# a fused-rank score no agent can interpret.
SEARCH_DROP: Final[frozenset[str]] = frozenset(
    {"score", "params", "return_type", "line_end", "parent_name"}
)
# graph_context filters ``'contains'`` but the store holds ``'CONTAINS'``; drop
# the leak here until claim-f289c62a lands the upstream fix.
CONTAINS_KIND: Final[str] = "CONTAINS"


def _short_path(path: str) -> str:
    prefix = str(ROOT) + "/"
    return path[len(prefix) :] if path.startswith(prefix) else path


def ast_context(
    file_path: str,
    symbol: str | None = None,
    with_graph: bool = True,
    repo_root: str | None = None,
) -> str:
    """Token-minimal skeleton of a Python file plus its graph relationships.

    Bodies are replaced by ``...``; signatures, docstrings, type hints, and
    module-level constants survive. ``symbol`` narrows to one function/class
    (and its enclosing class). Relationships come from ``.code-review-graph``.
    Use this instead of reading a large file whole.
    """
    repo = Path(repo_root) if repo_root else ROOT
    try:
        summary = get_file_context(
            repo, file_path, target_symbol=symbol, with_graph=with_graph
        )
    except GraphContextError as exc:
        return f"ast_context ERROR: {exc}"
    summary = replace(
        summary,
        callers=[c for c in summary.callers if c.kind != CONTAINS_KIND],
        callees=[c for c in summary.callees if c.kind != CONTAINS_KIND],
    )
    return _cap_context(format_markdown_context(summary), symbol)


def _cap_context(text: str, symbol: str | None) -> str:
    """Cut a skeleton to the context bound and say how to narrow it."""
    if len(text) <= AST_CONTEXT_MAX_BYTES:
        return text
    hint = (
        "pass symbol=<name> to narrow to one function/class"
        if symbol is None
        else "Read the file with offset/limit for the rest"
    )
    dropped = len(text) - AST_CONTEXT_MAX_BYTES
    return (
        text[:AST_CONTEXT_MAX_BYTES]
        + f"\n… truncated {dropped} of {len(text)} bytes; {hint}."
    )


def workspace_recall(query: str, notes_k: int = 5, cards_k: int = 3) -> dict[str, Any]:
    """Compact recall over standing-law cards and workspace memory.

    Cards: ``research/notes/kb_*.md`` (kb_retrieve). Memory: notes, archives,
    vault, codex memories, skills (memory_index). One query embedding is
    shared by both retrievers. Returns paths + short snippets; open only the
    hits you need.
    """
    if not query.strip():
        raise ValueError("query must not be empty")
    vector = kb_retrieve.embed_text(
        kb_retrieve.QUERY_INSTRUCT + query.strip(), purpose="query"
    )

    def embedder(_text: str) -> list[float]:
        return vector

    cards = kb_retrieve.query_index(
        query, kb_retrieve.load_index(), top_k=cards_k, embedder=embedder
    )
    rows, matrix = memory_vectors.load_sidecar()
    notes = memory_vectors.search(vector, rows, matrix, top_k=notes_k)
    return {
        "status": "ok",
        "cards": [
            {
                "card": card.name,
                "score": round(card.score, 3),
                "snippet": _snippet(card.text),
            }
            for card in cards
        ],
        "memory": [
            {
                "source": hit["source"],
                "path": _short_path(hit["path"]),
                "title": hit["title"],
                "score": round(hit["score"], 3),
                "snippet": _snippet(hit["text"]),
            }
            for hit in notes
        ],
    }


def _graph_rows(repo: Path, sql: str, params: tuple[Any, ...]) -> list[sqlite3.Row]:
    db_path = graph_database_path(repo)
    if not db_path.is_file():
        raise FileNotFoundError(f"code-review graph missing: {db_path}")
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    conn.row_factory = sqlite3.Row
    try:
        return conn.execute(sql, params).fetchall()
    finally:
        conn.close()


def _node_view(row: sqlite3.Row, repo: Path) -> dict[str, Any]:
    node = SimpleNamespace(
        file_path=row["file_path"],
        name=row["name"],
        parent_name=row["parent_name"],
        line_start=row["line_start"],
    )
    view: dict[str, Any] = {
        "qualified_name": _short_path(row["qualified_name"]),
        "kind": row["kind"],
        "line": row["line_start"],
    }
    doc = docstring_for(node)
    if doc:
        view["doc"] = doc
    return view


def locate(
    name: str, kind: str | None = None, limit: int = LOCATE_LIMIT
) -> dict[str, Any]:
    """Exact symbol lookup: where is ``name`` defined, with its first docstring line.

    Exact name matches come first, then prefix matches. Faster and more precise
    than semantic search when you already know (part of) the identifier.
    """
    name = name.strip()
    if not name:
        raise ValueError("name must not be empty")
    kind_clause = " AND kind = ?" if kind else ""
    base = (
        "SELECT qualified_name, kind, name, parent_name, file_path, line_start "
        "FROM nodes WHERE kind != 'File'" + kind_clause
    )
    kind_params: tuple[Any, ...] = (kind,) if kind else ()
    rows = _graph_rows(
        ROOT,
        base + " AND name = ? ORDER BY file_path LIMIT ?",
        (*kind_params, name, limit),
    )
    if len(rows) < limit:
        rows += _graph_rows(
            ROOT,
            base + " AND name LIKE ? AND name != ? ORDER BY name, file_path LIMIT ?",
            (*kind_params, name + "%", name, limit - len(rows)),
        )
    return {
        "status": "ok",
        "query": name,
        "results": [_node_view(r, ROOT) for r in rows],
    }


def enrich_search_results(payload: Any, repo: Path = ROOT) -> Any:
    """Add ``line`` + ``doc`` to semantic-search hits and drop uninterpretable fields."""
    if not isinstance(payload, dict) or not isinstance(payload.get("results"), list):
        return payload
    enriched: list[Any] = []
    for hit in payload["results"]:
        if not isinstance(hit, dict) or "name" not in hit or "file_path" not in hit:
            enriched.append(hit)
            continue
        rows = _graph_rows(
            repo,
            "SELECT qualified_name, kind, name, parent_name, file_path, line_start "
            "FROM nodes WHERE file_path = ? AND name = ? LIMIT 1",
            (hit["file_path"], hit["name"]),
        )
        out = {k: v for k, v in hit.items() if k not in SEARCH_DROP}
        if rows:
            out.update(_node_view(rows[0], repo))
        enriched.append(out)
    return {**payload, "results": enriched}


def symbol_source(name: str, max_bytes: int = MAX_SOURCE_BYTES) -> dict[str, Any]:
    """Source of one symbol by qualified name (``pkg/mod.py::Class.method``) or bare name.

    Returns just that definition's lines from the graph's line range instead of
    the whole file. A bare name with several definitions returns ``candidates``
    (use the qualified form or ``locate_tool``).
    """
    name = name.strip()
    if not name:
        raise ValueError("name must not be empty")
    columns = "qualified_name, kind, name, parent_name, file_path, line_start, line_end"
    if "::" in name:
        qualified = name if name.startswith("/") else str(ROOT / name)
        rows = _graph_rows(
            ROOT, f"SELECT {columns} FROM nodes WHERE qualified_name = ?", (qualified,)
        )
    else:
        rows = _graph_rows(
            ROOT,
            f"SELECT {columns} FROM nodes WHERE kind != 'File' AND name = ? "
            "ORDER BY file_path LIMIT ?",
            (name, LOCATE_LIMIT + 1),
        )
    if not rows:
        return {"status": "not_found", "query": name}
    if len(rows) > 1:
        return {
            "status": "ambiguous",
            "query": name,
            "candidates": [_node_view(r, ROOT) for r in rows],
        }
    row = rows[0]
    start, end = (
        int(row["line_start"] or 1),
        int(row["line_end"] or row["line_start"] or 1),
    )
    try:
        lines = (
            Path(row["file_path"]).read_text(encoding="utf-8").splitlines(keepends=True)
        )
    except (OSError, UnicodeDecodeError) as exc:
        return {"status": "error", "query": name, "error": str(exc)}
    text = "".join(lines[start - 1 : end])
    truncated = len(text.encode("utf-8")) > max_bytes
    if truncated:
        text = text.encode("utf-8")[:max_bytes].decode("utf-8", "ignore")
    out = {
        "status": "ok",
        "qualified_name": _short_path(row["qualified_name"]),
        "kind": row["kind"],
        "lines": f"{start}-{end}",
        "source": text,
    }
    if truncated:
        out["truncated"] = True
    return out


def session_brief_tool(
    task: str, paths: list[str] | None = None, agent: str | None = None
) -> str:
    """One-call session bootstrap: mandates, live headings, claims overlapping
    *paths*, unread A2A previews for *agent* (default ``A2A_AGENT_NAME``), and
    the top knowledge cards for *task*. Call this first in a session.
    """
    return session_brief(task, paths, agent)


def search_enrichers(repo: Path = ROOT) -> dict[str, Callable[[Any], Any]]:
    """Per-tool result enrichers for ``install_response_shim``."""
    return {
        "semantic_search_nodes_tool": lambda payload: enrich_search_results(
            payload, repo
        )
    }


def register_workspace_tools(mcp: Any) -> list[str]:
    """Register the workspace tools on a FastMCP server; returns their names."""
    registry = {
        "ast_context_tool": ast_context,
        "workspace_recall_tool": workspace_recall,
        "locate_tool": locate,
        "symbol_source_tool": symbol_source,
        "session_brief_tool": session_brief_tool,
    }
    for name, fn in registry.items():
        mcp.tool(name=name)(fn)
    return list(registry)
