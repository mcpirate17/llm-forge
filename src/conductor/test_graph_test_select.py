"""Tests for conductor.graph_test_select."""

from __future__ import annotations

import json
import sqlite3
import subprocess
from pathlib import Path
import pytest

from conductor.graph_test_select import (
    GraphSelectError,
    convention_tests_for_path,
    git_changed_and_untracked_files,
    graph_database_path,
    is_test_file,
    main,
    query_graph_tests,
    run_tests,
    select_tests_for_sources,
)


def _init_graph_db(repo: Path) -> Path:
    """Create an empty code-review graph with the node/edge columns selection reads."""
    db_path = graph_database_path(repo)
    db_path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(db_path)
    conn.execute(
        "CREATE TABLE nodes (qualified_name TEXT, file_path TEXT, is_test INTEGER)"
    )
    conn.execute("CREATE TABLE edges (source_qualified TEXT, target_qualified TEXT)")
    conn.commit()
    conn.close()
    return db_path


@pytest.fixture
def graph_test_repo(tmp_path: Path) -> Path:
    r = tmp_path / "graph_ws"
    r.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init", "--quiet", str(r)], check=True)
    subprocess.run(["git", "config", "user.name", "gselect"], cwd=r, check=True)
    subprocess.run(
        ["git", "config", "user.email", "gselect@test.com"], cwd=r, check=True
    )
    (r / "base.py").write_text("def base(): pass\n", encoding="utf-8")
    subprocess.run(["git", "add", "base.py"], cwd=r, check=True)
    subprocess.run(["git", "commit", "-m", "init-base", "--quiet"], cwd=r, check=True)
    _init_graph_db(r)
    return r


def test_convention_tests_for_path(graph_test_repo: Path) -> None:
    (graph_test_repo / "pkg").mkdir(parents=True, exist_ok=True)
    src_file = graph_test_repo / "pkg" / "engine.py"
    src_file.write_text("def run(): pass\n", encoding="utf-8")

    test_file = graph_test_repo / "pkg" / "test_engine.py"
    test_file.write_text("def test_run(): pass\n", encoding="utf-8")

    selected = convention_tests_for_path(graph_test_repo, "pkg/engine.py")
    assert "pkg/test_engine.py" in selected


def test_convention_tests_for_self_test(graph_test_repo: Path) -> None:
    (graph_test_repo / "tests").mkdir(parents=True, exist_ok=True)
    t_file = graph_test_repo / "tests" / "test_suite.py"
    t_file.write_text("def test_one(): pass\n", encoding="utf-8")

    assert is_test_file("tests/test_suite.py") is True
    assert is_test_file("pkg/engine.py") is False

    selected = convention_tests_for_path(graph_test_repo, "tests/test_suite.py")
    assert "tests/test_suite.py" in selected


def test_query_graph_tests(graph_test_repo: Path) -> None:
    conn = sqlite3.connect(graph_database_path(graph_test_repo))

    src_abs = str((graph_test_repo / "pkg" / "engine.py").resolve())
    test_abs = str((graph_test_repo / "tests" / "test_engine.py").resolve())
    (graph_test_repo / "tests").mkdir(parents=True, exist_ok=True)
    (graph_test_repo / "tests" / "test_engine.py").write_text(
        "def test_it(): pass\n", encoding="utf-8"
    )

    conn.execute("INSERT INTO nodes VALUES (?, ?, ?)", ("pkg.engine", src_abs, 0))
    conn.execute(
        "INSERT INTO nodes VALUES (?, ?, ?)", ("tests.test_engine", test_abs, 1)
    )
    conn.execute("INSERT INTO edges VALUES (?, ?)", ("tests.test_engine", "pkg.engine"))
    conn.commit()
    conn.close()

    tests = query_graph_tests(graph_test_repo, ["pkg/engine.py"])
    assert "tests/test_engine.py" in tests


@pytest.mark.parametrize("damage", ["missing", "corrupt"])
def test_graph_unavailable_fails_loud(
    graph_test_repo: Path, damage: str, capsys: pytest.CaptureFixture[str]
) -> None:
    db_path = graph_database_path(graph_test_repo)
    if damage == "missing":
        db_path.unlink()
    else:
        db_path.write_bytes(b"not a sqlite database\n")
    (graph_test_repo / "pkg").mkdir(parents=True, exist_ok=True)
    (graph_test_repo / "pkg" / "engine.py").write_text("x = 1\n", encoding="utf-8")
    (graph_test_repo / "pkg" / "test_engine.py").write_text(
        "def test_x(): pass\n", encoding="utf-8"
    )

    with pytest.raises(GraphSelectError, match="code-review graph"):
        query_graph_tests(graph_test_repo, ["pkg/engine.py"])
    # The convention match alone must not rescue a selection with no graph behind it.
    with pytest.raises(GraphSelectError):
        select_tests_for_sources(graph_test_repo, ["pkg/engine.py"])

    code = main(["--repo", str(graph_test_repo), "--json", "pkg/engine.py"])
    assert code == 2
    out, err = capsys.readouterr()
    assert out == ""
    assert "graph-test-select ERROR" in err


def test_select_tests_for_sources(graph_test_repo: Path) -> None:
    (graph_test_repo / "pkg").mkdir(parents=True, exist_ok=True)
    (graph_test_repo / "pkg" / "calc.py").write_text(
        "def add(): pass\n", encoding="utf-8"
    )
    (graph_test_repo / "pkg" / "test_calc.py").write_text(
        "def test_add(): pass\n", encoding="utf-8"
    )

    selected = select_tests_for_sources(graph_test_repo, ["pkg/calc.py"])
    assert selected == ["pkg/test_calc.py"]

    # Non-source files return empty
    assert select_tests_for_sources(graph_test_repo, ["README.md"]) == []


def test_git_changed_and_untracked_files(graph_test_repo: Path) -> None:
    (graph_test_repo / "untracked.py").write_text("x = 1\n", encoding="utf-8")
    (graph_test_repo / "base.py").write_text(
        "def base_modified(): pass\n", encoding="utf-8"
    )
    changed = git_changed_and_untracked_files(graph_test_repo)
    assert "base.py" in changed
    assert "untracked.py" in changed


def test_run_tests_empty(capsys: pytest.CaptureFixture[str]) -> None:
    code = run_tests(Path("."), [])
    assert code == 0
    out, _ = capsys.readouterr()
    assert "No targeted tests selected." in out


def test_main_cli(graph_test_repo: Path, capsys: pytest.CaptureFixture[str]) -> None:
    (graph_test_repo / "pkg").mkdir(parents=True, exist_ok=True)
    (graph_test_repo / "pkg" / "cli_src.py").write_text("x = 1\n", encoding="utf-8")
    (graph_test_repo / "pkg" / "test_cli_src.py").write_text(
        "def test_cli(): pass\n", encoding="utf-8"
    )

    code = main(
        [
            "--repo",
            str(graph_test_repo),
            "--json",
            "pkg/cli_src.py",
        ]
    )
    assert code == 0
    out, _ = capsys.readouterr()
    data = json.loads(out)
    assert data["selected_tests"] == ["pkg/test_cli_src.py"]
    assert data["scope"] == "direct-dependencies-and-conventions"

    # Plain output mode
    code_plain = main(
        [
            "--repo",
            str(graph_test_repo),
            "pkg/cli_src.py",
        ]
    )
    assert code_plain == 0
