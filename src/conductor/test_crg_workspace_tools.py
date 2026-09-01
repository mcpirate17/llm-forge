"""Tests for the workspace sub-graph MCP tools."""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

from conductor import crg_workspace_tools as tools
from conductor.graph_context import (
    FileContextSummary,
    GraphContextError,
    GraphRelationship,
)


def test_snippet_strips_frontmatter_collapses_whitespace_and_truncates() -> None:
    text = "---\nid: X\n---\n\n# Title\n\nline  one\nline two\n"
    assert tools._snippet(text) == "# Title line one line two"
    long = "word " * 100
    out = tools._snippet(long, limit=20)
    assert len(out) == 20 and out.endswith("…")


def test_snippet_without_closing_frontmatter_keeps_text() -> None:
    assert tools._snippet("--- not really frontmatter") == "--- not really frontmatter"


def test_short_path_only_strips_repo_root() -> None:
    assert tools._short_path(str(tools.ROOT / "a/b.md")) == "a/b.md"
    assert tools._short_path("/elsewhere/a.md") == "/elsewhere/a.md"


def _summary(**overrides: Any) -> FileContextSummary:
    base: dict[str, Any] = {
        "file_path": "pkg/mod.py",
        "skeleton": "def f(): ...",
        "symbols": ["f"],
        "callers": [
            GraphRelationship("pkg/mod.py", "CONTAINS", "pkg/mod.py"),
            GraphRelationship("pkg/other.py::g", "CALLS", "pkg/other.py"),
        ],
        "callees": [
            GraphRelationship("pkg/mod.py::f", "CONTAINS", "pkg/mod.py"),
            GraphRelationship("pkg/dep.py::h", "CALLS", "pkg/dep.py"),
        ],
        "graph_status": "ok",
    }
    base.update(overrides)
    return FileContextSummary(**base)


def test_ast_context_filters_contains_edges(monkeypatch: pytest.MonkeyPatch) -> None:
    seen: dict[str, Any] = {}

    def fake_get_file_context(
        repo: Path, file_path: str, target_symbol: str | None, with_graph: bool
    ) -> FileContextSummary:
        seen.update(
            repo=repo, file_path=file_path, symbol=target_symbol, graph=with_graph
        )
        return _summary()

    monkeypatch.setattr(tools, "get_file_context", fake_get_file_context)
    out = tools.ast_context("pkg/mod.py", symbol="f", repo_root="/r")
    assert seen == {
        "repo": Path("/r"),
        "file_path": "pkg/mod.py",
        "symbol": "f",
        "graph": True,
    }
    assert "CONTAINS" not in out
    assert "`pkg/other.py::g` (CALLS)" in out
    assert "`pkg/dep.py::h` (CALLS)" in out
    assert "def f(): ..." in out


def test_ast_context_caps_output_and_says_how_to_narrow(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    big = _summary(skeleton="x" * (tools.AST_CONTEXT_MAX_BYTES + 500))
    monkeypatch.setattr(tools, "get_file_context", lambda *a, **k: big)
    out = tools.ast_context("pkg/mod.py")
    assert len(out) < tools.AST_CONTEXT_MAX_BYTES + 120
    assert "truncated" in out and "pass symbol=<name>" in out
    narrowed = tools.ast_context("pkg/mod.py", symbol="f")
    assert "offset/limit" in narrowed and "pass symbol" not in narrowed
    monkeypatch.setattr(tools, "get_file_context", lambda *a, **k: _summary())
    assert "truncated" not in tools.ast_context("pkg/mod.py")


def test_ast_context_reports_errors_as_text(monkeypatch: pytest.MonkeyPatch) -> None:
    def boom(*_args: Any, **_kwargs: Any) -> FileContextSummary:
        raise GraphContextError("symbol 'nope' not found")

    monkeypatch.setattr(tools, "get_file_context", boom)
    assert tools.ast_context("pkg/mod.py", symbol="nope") == (
        "ast_context ERROR: symbol 'nope' not found"
    )


def test_workspace_recall_shares_one_embedding_and_compacts(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    calls: list[str] = []

    def fake_embed(text: str, **kwargs: Any) -> list[float]:
        calls.append((text, kwargs.get("purpose")))
        return [1.0, 0.0]

    card = tools.kb_retrieve.ScoredCard(
        name="kb_x.md",
        path=str(tools.ROOT / "research/notes/kb_x.md"),
        score=0.4567,
        text="---\nid: K\n---\n# Card body",
    )
    embedders: list[Any] = []

    def fake_kb_query(
        query: str, index: Any, *, top_k: int, embedder: Any
    ) -> list[Any]:
        embedders.append(embedder)
        assert index == {"kb": True} and top_k == 2
        return [card]

    searches: list[Any] = []

    def fake_search(vector: Any, rows: Any, matrix: Any, *, top_k: int) -> list[Any]:
        searches.append((vector, rows, matrix, top_k))
        return [
            {
                "score": 0.51234,
                "weighted_score": 0.6,
                "source": "notes",
                "path": str(tools.ROOT / "research/notes/n.md"),
                "title": "N",
                "text": "note  text",
            }
        ]

    monkeypatch.setattr(tools.kb_retrieve, "embed_text", fake_embed)
    monkeypatch.setattr(tools.kb_retrieve, "load_index", lambda: {"kb": True})
    monkeypatch.setattr(tools.kb_retrieve, "query_index", fake_kb_query)
    monkeypatch.setattr(tools.memory_vectors, "load_sidecar", lambda: (["row"], "M"))
    monkeypatch.setattr(tools.memory_vectors, "search", fake_search)

    out = tools.workspace_recall("graph skeleton", notes_k=4, cards_k=2)
    assert calls == [(tools.kb_retrieve.QUERY_INSTRUCT + "graph skeleton", "query")]
    assert len(embedders) == 1 and embedders[0]("anything") == [1.0, 0.0]
    assert searches == [([1.0, 0.0], ["row"], "M", 4)]
    assert out == {
        "status": "ok",
        "cards": [{"card": "kb_x.md", "score": 0.457, "snippet": "# Card body"}],
        "memory": [
            {
                "source": "notes",
                "path": "research/notes/n.md",
                "title": "N",
                "score": 0.512,
                "snippet": "note text",
            }
        ],
    }


def test_workspace_recall_rejects_empty_query() -> None:
    with pytest.raises(ValueError, match="empty"):
        tools.workspace_recall("   ")


def test_register_workspace_tools_uses_crg_tool_naming() -> None:
    registered: list[tuple[str, Any]] = []

    class FakeMcp:
        def tool(self, name: str) -> Any:
            def decorate(fn: Any) -> Any:
                registered.append((name, fn))
                return fn

            return decorate

    names = tools.register_workspace_tools(FakeMcp())
    assert names == [
        "ast_context_tool",
        "workspace_recall_tool",
        "locate_tool",
        "symbol_source_tool",
        "session_brief_tool",
    ]
    assert dict(registered) == {
        "ast_context_tool": tools.ast_context,
        "workspace_recall_tool": tools.workspace_recall,
        "locate_tool": tools.locate,
        "symbol_source_tool": tools.symbol_source,
        "session_brief_tool": tools.session_brief_tool,
    }


@pytest.fixture
def graph_db(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    src = tmp_path / "pkg" / "mod.py"
    src.parent.mkdir()
    src.write_text(
        '"""Module."""\n\n\ndef compact_state(s):\n    """Compact the state."""\n\n\n'
        'class Preamble:\n    def render(self):\n        """Render text."""\n\n\n'
        "def compact_other():\n    pass\n",
        encoding="utf-8",
    )
    db = tmp_path / ".code-review-graph" / "graph.db"
    db.parent.mkdir()
    import sqlite3

    conn = sqlite3.connect(db)
    conn.execute(
        "CREATE TABLE nodes (qualified_name TEXT, kind TEXT, name TEXT, "
        "parent_name TEXT, file_path TEXT, line_start INTEGER, line_end INTEGER)"
    )
    rows = [
        (f"{src}", "File", str(src), None, str(src), 1, 14),
        (f"{src}::compact_state", "Function", "compact_state", None, str(src), 4, 5),
        (f"{src}::Preamble", "Class", "Preamble", None, str(src), 8, 10),
        (f"{src}::Preamble.render", "Function", "render", "Preamble", str(src), 9, 10),
        (f"{src}::compact_other", "Function", "compact_other", None, str(src), 13, 14),
    ]
    conn.executemany("INSERT INTO nodes VALUES (?,?,?,?,?,?,?)", rows)
    conn.commit()
    conn.close()
    monkeypatch.setattr(tools, "ROOT", tmp_path)
    monkeypatch.setattr(tools, "graph_database_path", lambda _repo: db)
    return db


def test_locate_exact_then_prefix_with_docstrings(graph_db: Path) -> None:
    out = tools.locate("compact")
    assert out["status"] == "ok"
    assert [r["qualified_name"] for r in out["results"]] == [
        "pkg/mod.py::compact_other",
        "pkg/mod.py::compact_state",
    ]
    exact = tools.locate("compact_state")
    assert exact["results"][0] == {
        "qualified_name": "pkg/mod.py::compact_state",
        "kind": "Function",
        "line": 4,
        "doc": "Compact the state.",
    }
    assert "doc" not in tools.locate("compact_other")["results"][0]
    method = tools.locate("render")["results"][0]
    assert method["qualified_name"] == "pkg/mod.py::Preamble.render"
    assert method["doc"] == "Render text."
    assert tools.locate("compact", kind="Class")["results"] == []
    assert tools.locate("Pre", kind="Class", limit=1)["results"][0]["kind"] == "Class"
    assert len(tools.locate("compact", limit=1)["results"]) == 1


def test_locate_rejects_empty_name_and_missing_graph(
    graph_db: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    with pytest.raises(ValueError, match="empty"):
        tools.locate("  ")
    monkeypatch.setattr(tools, "graph_database_path", lambda _repo: graph_db / "nope")
    with pytest.raises(FileNotFoundError, match="graph missing"):
        tools.locate("compact")


def test_enrich_search_results_adds_doc_and_drops_noise(graph_db: Path) -> None:
    src = str(graph_db.parents[1] / "pkg" / "mod.py")
    payload = {
        "status": "ok",
        "results": [
            {
                "name": "compact_state",
                "kind": "Function",
                "file_path": src,
                "score": 0.016,
            },
            {
                "name": "render",
                "kind": "Function",
                "file_path": src,
                "signature": "def render(self)",
                "params": "(self)",
                "return_type": None,
                "line_start": 9,
                "line_end": 10,
                "parent_name": "Preamble",
                "score": 0.015,
            },
            {"name": "ghost", "kind": "Function", "file_path": src, "score": 0.01},
            "not a dict",
        ],
    }
    out = tools.enrich_search_results(payload)
    assert out["status"] == "ok"
    first, second, ghost, other = out["results"]
    assert first == {
        "name": "compact_state",
        "kind": "Function",
        "file_path": src,
        "qualified_name": "pkg/mod.py::compact_state",
        "line": 4,
        "doc": "Compact the state.",
    }
    assert second["signature"] == "def render(self)" and second["doc"] == "Render text."
    assert not {"score", "params", "return_type", "line_end", "parent_name"} & set(
        second
    )
    assert ghost == {"name": "ghost", "kind": "Function", "file_path": src}
    assert other == "not a dict"
    assert tools.enrich_search_results("text") == "text"
    assert tools.enrich_search_results({"status": "error"}) == {"status": "error"}


def test_search_enrichers_target_semantic_search(graph_db: Path) -> None:
    enrichers = tools.search_enrichers()
    assert list(enrichers) == ["semantic_search_nodes_tool"]
    assert enrichers["semantic_search_nodes_tool"]({"results": []}) == {"results": []}


def test_symbol_source_returns_exact_lines(graph_db: Path) -> None:
    out = tools.symbol_source("pkg/mod.py::compact_state")
    assert out == {
        "status": "ok",
        "qualified_name": "pkg/mod.py::compact_state",
        "kind": "Function",
        "lines": "4-5",
        "source": 'def compact_state(s):\n    """Compact the state."""\n',
    }
    by_name = tools.symbol_source("render")
    assert by_name["qualified_name"] == "pkg/mod.py::Preamble.render"
    assert by_name["source"].startswith("    def render(self):")
    assert tools.symbol_source("nope") == {"status": "not_found", "query": "nope"}
    small = tools.symbol_source("compact_state", max_bytes=10)
    assert small["truncated"] is True and len(small["source"].encode()) <= 10
    with pytest.raises(ValueError, match="empty"):
        tools.symbol_source(" ")


def test_symbol_source_reports_ambiguity_with_candidates(
    graph_db: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    import sqlite3

    conn = sqlite3.connect(graph_db)
    src = str(graph_db.parents[1] / "pkg" / "mod.py")
    conn.execute(
        "INSERT INTO nodes VALUES (?,?,?,?,?,?,?)",
        (f"{src}::Other.render", "Function", "render", "Other", src, 9, 10),
    )
    conn.commit()
    conn.close()
    out = tools.symbol_source("render")
    assert out["status"] == "ambiguous"
    assert {c["qualified_name"] for c in out["candidates"]} == {
        "pkg/mod.py::Preamble.render",
        "pkg/mod.py::Other.render",
    }


def test_session_brief_tool_delegates(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(
        tools, "session_brief", lambda task, paths, agent: f"{task}|{paths}|{agent}"
    )
    assert tools.session_brief_tool("t", ["a"], "me") == "t|['a']|me"
    assert tools.session_brief_tool("t") == "t|None|None"
