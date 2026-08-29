"""Tests for conductor.graph_context."""

from __future__ import annotations

import json
import sqlite3
import subprocess
from pathlib import Path
import pytest

from conductor.graph_context import (
    GraphContextError,
    GraphRelationship,
    extract_ast_skeleton,
    find_syntactic_callers,
    format_markdown_context,
    get_file_context,
    main,
    query_graph_relationships,
)


@pytest.fixture
def context_repo(tmp_path: Path) -> Path:
    repo = tmp_path / "ctx_ws"
    repo.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init", "--quiet", str(repo)], check=True)
    subprocess.run(["git", "config", "user.name", "graph-tester"], cwd=repo, check=True)
    subprocess.run(
        ["git", "config", "user.email", "graph@example.com"], cwd=repo, check=True
    )
    (repo / "CONVENTIONS.md").write_text(
        "# Graph Context Test Fixture\n", encoding="utf-8"
    )
    subprocess.run(["git", "add", "CONVENTIONS.md"], cwd=repo, check=True)
    subprocess.run(
        ["git", "commit", "-m", "init-graph-context", "--quiet"], cwd=repo, check=True
    )
    return repo


def test_extract_ast_skeleton_functions() -> None:
    code = (
        "import os\n"
        "from typing import Optional\n\n"
        "def calculate_total(price: float, tax: float = 0.05) -> float:\n"
        '    """Compute total with tax."""\n'
        "    subtotal = price * 1.0\n"
        "    return subtotal + (subtotal * tax)\n"
    )
    skeleton, symbols = extract_ast_skeleton(code)
    assert "import os" in skeleton
    assert "def calculate_total(" in skeleton
    assert ") -> float:" in skeleton
    assert '"""Compute total with tax."""' in skeleton
    assert "..." in skeleton
    assert "subtotal = price" not in skeleton
    assert "calculate_total" in symbols


def test_extract_ast_skeleton_classes() -> None:
    code = (
        "class ModelTrainer:\n"
        '    """Trainer engine."""\n'
        "    batch_size: int = 32\n\n"
        "    def train_step(self, x: int) -> bool:\n"
        '        """One step."""\n'
        "        y = x * 2\n"
        "        return True\n"
    )
    skeleton, symbols = extract_ast_skeleton(code)
    assert "class ModelTrainer:" in skeleton
    assert '"""Trainer engine."""' in skeleton
    assert "def train_step(self, x: int) -> bool:" in skeleton
    assert '"""One step."""' in skeleton
    assert "..." in skeleton
    assert "y = x * 2" not in skeleton
    assert "ModelTrainer" in symbols


def test_extract_ast_skeleton_filter_target_symbol() -> None:
    code = "def func_a():\n    return 1\n\ndef func_b():\n    return 2\n"
    skeleton, symbols = extract_ast_skeleton(code, target_symbol="func_b")
    assert "func_b" in skeleton
    assert "func_a" not in skeleton
    assert "func_a" in symbols
    assert "func_b" in symbols


def test_query_graph_relationships(context_repo: Path) -> None:
    crg_dir = context_repo / ".code-review-graph"
    crg_dir.mkdir(parents=True)
    db_path = crg_dir / "graph.db"

    conn = sqlite3.connect(db_path)
    conn.execute("""
        CREATE TABLE nodes (
            id INTEGER PRIMARY KEY,
            kind TEXT,
            name TEXT,
            qualified_name TEXT,
            file_path TEXT,
            line_start INTEGER,
            line_end INTEGER,
            language TEXT,
            parent_name TEXT,
            params TEXT,
            return_type TEXT,
            modifiers TEXT,
            is_test INTEGER DEFAULT 0,
            file_hash TEXT,
            extra TEXT DEFAULT '{}',
            updated_at REAL DEFAULT 0.0,
            signature TEXT,
            community_id INTEGER DEFAULT 0
        )
    """)
    conn.execute("""
        CREATE TABLE edges (
            id INTEGER PRIMARY KEY,
            kind TEXT,
            source_qualified TEXT,
            target_qualified TEXT,
            file_path TEXT,
            line INTEGER DEFAULT 0,
            extra TEXT DEFAULT '{}',
            updated_at REAL DEFAULT 0.0,
            confidence REAL DEFAULT 1.0,
            confidence_tier TEXT DEFAULT 'EXTRACTED'
        )
    """)

    src_abs = str((context_repo / "pkg" / "engine.py").resolve())
    caller_abs = str((context_repo / "pkg" / "caller.py").resolve())

    conn.execute(
        "INSERT INTO nodes (qualified_name, name, file_path) VALUES (?, ?, ?)",
        ("pkg.engine.run", "run", src_abs),
    )
    conn.execute(
        "INSERT INTO nodes (qualified_name, name, file_path) VALUES (?, ?, ?)",
        ("pkg.caller.invoke", "invoke", caller_abs),
    )
    conn.execute(
        "INSERT INTO nodes (qualified_name, name, file_path) VALUES (?, ?, ?)",
        ("pkg.sub.helper", "helper", "pkg/sub.py"),
    )

    conn.execute(
        "INSERT INTO edges (source_qualified, target_qualified, kind) VALUES (?, ?, ?)",
        ("pkg.caller.invoke", "pkg.engine.run", "calls"),
    )
    conn.execute(
        "INSERT INTO edges (source_qualified, target_qualified, kind) VALUES (?, ?, ?)",
        ("pkg.engine.run", "pkg.sub.helper", "calls"),
    )
    conn.commit()
    conn.close()

    callers, callees, status = query_graph_relationships(context_repo, "pkg/engine.py")
    assert status == "ok"
    assert len(callers) == 1
    assert callers[0].qualified_name == "pkg.caller.invoke"
    assert len(callees) == 1
    assert callees[0].qualified_name == "pkg.sub.helper"


def test_get_file_context_full_flow(context_repo: Path) -> None:
    (context_repo / "pkg").mkdir(parents=True, exist_ok=True)
    src_file = context_repo / "pkg" / "mod.py"
    src_file.write_text("def ping() -> str:\n    return 'pong'\n", encoding="utf-8")

    ctx = get_file_context(context_repo, "pkg/mod.py", with_graph=False)
    assert ctx.file_path == "pkg/mod.py"
    assert "def ping() -> str:" in ctx.skeleton
    assert "ping" in ctx.symbols
    assert ctx.callers == []

    md = format_markdown_context(ctx)
    assert "### AST Context: `pkg/mod.py`" in md
    assert "def ping() -> str:" in md


def test_format_markdown_context_with_relationships() -> None:
    from conductor.graph_context import FileContextSummary

    summary = FileContextSummary(
        file_path="foo.py",
        skeleton="def foo(): ...",
        symbols=["foo"],
        callers=[
            GraphRelationship(
                qualified_name="bar.caller", kind="calls", file_path="bar.py"
            )
        ],
        callees=[
            GraphRelationship(
                qualified_name="baz.callee", kind="calls", file_path="baz.py"
            )
        ],
        graph_status="unavailable (graph.db missing)",
    )
    md = format_markdown_context(summary)
    assert "Called By (Inbound Call Sites)" in md
    assert "bar.caller" in md
    assert "Calls (Outbound Dependencies)" in md
    assert "baz.callee" in md
    assert "Notice: code-review-graph unavailable" in md


def test_main_cli(context_repo: Path, capsys: pytest.CaptureFixture[str]) -> None:
    (context_repo / "pkg").mkdir(parents=True, exist_ok=True)
    (context_repo / "pkg" / "cli_test.py").write_text(
        "def cli_func(x: int) -> int:\n    return x + 1\n", encoding="utf-8"
    )

    code = main(
        [
            "--repo",
            str(context_repo),
            "--no-graph",
            "--json",
            "pkg/cli_test.py",
        ]
    )
    assert code == 0
    out, _ = capsys.readouterr()
    data = json.loads(out)
    assert data["file_path"] == "pkg/cli_test.py"
    assert "def cli_func(x: int) -> int:" in data["skeleton"]
    assert "cli_func" in data["symbols"]


def test_error_handling_missing_file_and_syntax_error(context_repo: Path) -> None:
    with pytest.raises(GraphContextError, match="file not found"):
        get_file_context(context_repo, "nonexistent.py")

    with pytest.raises(GraphContextError, match="syntax error in source"):
        extract_ast_skeleton("def broken_syntax(:\n")


def test_extract_ast_skeleton_async_function() -> None:
    code = (
        "async def fetch_data(url: str) -> dict:\n"
        '    """Fetch async."""\n'
        "    res = await get(url)\n"
        "    return res\n\n"
        "async def other():\n"
        "    pass\n"
    )
    skeleton, symbols = extract_ast_skeleton(code, target_symbol="fetch_data")
    assert "async def fetch_data(" in skeleton
    assert '"""Fetch async."""' in skeleton
    assert "async def other" not in skeleton
    assert "fetch_data" in symbols
    assert "other" in symbols


def test_extract_ast_skeleton_class_method_filtering() -> None:
    code = (
        "class Service:\n"
        "    def method_a(self):\n"
        "        return 1\n"
        "    def method_b(self):\n"
        "        return 2\n\n"
        "class OtherService:\n"
        "    def method_c(self):\n"
        "        return 3\n"
    )
    skeleton, symbols = extract_ast_skeleton(code, target_symbol="method_a")
    assert "class Service:" in skeleton
    assert "def method_a(" in skeleton
    assert "def method_b(" not in skeleton
    assert "class OtherService:" not in skeleton


def test_graph_relationship_immutability() -> None:
    from dataclasses import FrozenInstanceError

    rel = GraphRelationship(
        qualified_name="pkg.foo", kind="calls", file_path="pkg/foo.py"
    )
    with pytest.raises(FrozenInstanceError):
        rel.qualified_name = "other"  # type: ignore[misc]


def test_query_graph_with_target_symbol(context_repo: Path) -> None:
    crg_dir = context_repo / ".code-review-graph"
    crg_dir.mkdir(parents=True, exist_ok=True)
    db_path = crg_dir / "graph.db"

    conn = sqlite3.connect(db_path)
    conn.execute(
        "CREATE TABLE IF NOT EXISTS nodes (id INTEGER PRIMARY KEY, kind TEXT, name TEXT, qualified_name TEXT, file_path TEXT, line_start INTEGER, line_end INTEGER, language TEXT, parent_name TEXT, params TEXT, return_type TEXT, modifiers TEXT, is_test INTEGER DEFAULT 0, file_hash TEXT, extra TEXT DEFAULT '{}', updated_at REAL DEFAULT 0.0, signature TEXT, community_id INTEGER DEFAULT 0)"
    )
    conn.execute(
        "CREATE TABLE IF NOT EXISTS edges (id INTEGER PRIMARY KEY, kind TEXT, source_qualified TEXT, target_qualified TEXT, file_path TEXT, line INTEGER DEFAULT 0, extra TEXT DEFAULT '{}', updated_at REAL DEFAULT 0.0, confidence REAL DEFAULT 1.0, confidence_tier TEXT DEFAULT 'EXTRACTED')"
    )

    src_abs = str((context_repo / "pkg" / "engine.py").resolve())
    conn.execute(
        "INSERT INTO nodes (qualified_name, name, file_path) VALUES (?, ?, ?)",
        ("pkg.engine.run", "run", src_abs),
    )
    conn.execute(
        "INSERT INTO nodes (qualified_name, name, file_path) VALUES (?, ?, ?)",
        ("pkg.caller.invoke", "invoke", "pkg/caller.py"),
    )
    conn.execute(
        "INSERT INTO edges (source_qualified, target_qualified, kind) VALUES (?, ?, ?)",
        ("pkg.caller.invoke", "pkg.engine.run", "calls"),
    )
    conn.commit()
    conn.close()

    callers, callees, status = query_graph_relationships(
        context_repo, "pkg/engine.py", target_symbol="run"
    )
    assert status == "ok"
    assert len(callers) == 1
    assert callers[0].qualified_name == "pkg.caller.invoke"

    callers_none, _, _ = query_graph_relationships(
        context_repo, "pkg/engine.py", target_symbol="nonexistent"
    )
    assert callers_none == []


def test_find_syntactic_callers(context_repo: Path) -> None:
    (context_repo / "pkg").mkdir(parents=True, exist_ok=True)
    (context_repo / "pkg" / "callee.py").write_text(
        "def target_op(): pass\n", encoding="utf-8"
    )
    (context_repo / "pkg" / "caller.py").write_text(
        "from pkg.callee import target_op\ntarget_op()\n", encoding="utf-8"
    )

    callers = find_syntactic_callers(context_repo, "target_op", "pkg/callee.py")
    assert any("pkg/caller.py" in c.file_path for c in callers)


def test_unknown_symbol_raises_error() -> None:
    code = "def foo(): pass\n"
    with pytest.raises(GraphContextError, match="symbol 'bar' not found"):
        extract_ast_skeleton(code, target_symbol="bar")


def test_extract_ast_skeleton_preserves_assign_constants() -> None:
    code = (
        "MAX_RETRIES = 5\nCONFIG_SPEC = {'a': 1, 'b': 2}\n\ndef run():\n    return 1\n"
    )
    skeleton, _ = extract_ast_skeleton(code)
    assert "MAX_RETRIES = 5" in skeleton
    assert "CONFIG_SPEC = ..." in skeleton
    assert "def run():" in skeleton


def test_main_cli_error_path(
    context_repo: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    code = main(
        [
            "--repo",
            str(context_repo),
            "nonexistent_file.py",
        ]
    )
    assert code == 2
    _, err = capsys.readouterr()
    assert "graph-context ERROR: file not found" in err


def test_is_test_path_discriminates_test_files() -> None:
    from conductor.graph_context import _is_test_path

    assert _is_test_path("conductor/test_graph_context.py")
    assert _is_test_path("research/tests/helpers.py")
    assert _is_test_path("pkg/util_test.py")
    assert not _is_test_path("conductor/mutation_testing.py")
    assert not _is_test_path("research/tools/attestation.py")
    assert not _is_test_path("contest/protest.py")


def test_format_markdown_binning_survives_test_substring_in_name() -> None:
    from conductor.graph_context import FileContextSummary

    summary = FileContextSummary(
        file_path="conductor/mutation_testing.py",
        skeleton="def run_campaign(): ...",
        symbols=["run_campaign"],
        callers=[
            GraphRelationship(
                qualified_name="conductor/mutation_testing.py::_apply_mutation",
                kind="CALLS",
                file_path="conductor/mutation_testing.py",
            ),
            GraphRelationship(
                qualified_name="conductor/test_mutation_testing.py:72",
                kind="TESTED_BY",
                file_path="conductor/test_mutation_testing.py",
            ),
        ],
        callees=[
            GraphRelationship(
                qualified_name="audit/orchestrator/snapshot_worktree.py::isolated_snapshot",
                kind="CALLS",
                file_path="audit/orchestrator/snapshot_worktree.py",
            ),
            GraphRelationship(
                qualified_name="conductor/test_graph_context.py::context_repo",
                kind="CALLS",
                file_path="conductor/test_graph_context.py",
            ),
        ],
        graph_status="ok",
    )
    md = format_markdown_context(summary)
    called_by = md.split("**Called By")[1].split("**Calls")[0]
    calls = md.split("**Calls")[1].split("**Tested By")[0]
    tested_by = md.split("**Tested By")[1]
    assert "_apply_mutation" in called_by
    assert "isolated_snapshot" in calls
    assert "test_mutation_testing.py:72" in tested_by
    assert "test_graph_context.py::context_repo" in tested_by
    assert "_apply_mutation" not in tested_by
