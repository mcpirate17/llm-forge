"""The test-value-scan CLI: version answer, span scoping, exit codes."""

from __future__ import annotations

import subprocess
from pathlib import Path

from conductor.candidate_review import test_value_scan as cli

ASSERTLESS = "def test_a():\n    run()\n    other()\n"


def _git(root: Path, *args: str) -> None:
    subprocess.run(
        ["git", "-c", "user.name=t", "-c", "user.email=t@example.invalid", *args],
        cwd=root,
        check=True,
        capture_output=True,
    )


def _repo(tmp_path: Path, monkeypatch) -> Path:
    _git(tmp_path, "init", "-q")
    (tmp_path / "tests").mkdir()
    (tmp_path / "tests" / "test_x.py").write_text(ASSERTLESS)
    _git(tmp_path, "add", ".")
    _git(tmp_path, "commit", "-q", "-m", "base")
    monkeypatch.chdir(tmp_path)
    return tmp_path


def test_version_answers_without_files(capsys):
    assert cli.main(["--version"]) == 0
    assert capsys.readouterr().out.startswith("test-value-scan ")


def test_full_tree_run_reports_the_assertless_test(tmp_path, monkeypatch, capsys):
    _repo(tmp_path, monkeypatch)
    assert cli.main(["tests/test_x.py"]) == 1
    assert "tests/test_x.py:1: test/no-assertion:" in capsys.readouterr().err


def test_non_python_files_are_ignored(tmp_path, monkeypatch):
    _repo(tmp_path, monkeypatch)
    assert cli.main(["tests/data.json"]) == 0


def test_an_edit_to_the_body_holds_the_candidate_to_the_def_line_finding(
    tmp_path, monkeypatch, capsys
):
    root = _repo(tmp_path, monkeypatch)
    (root / "tests" / "test_x.py").write_text(ASSERTLESS.replace("other()", "more()"))
    assert cli.main(["--base", "HEAD", "tests/test_x.py"]) == 1
    assert "no-assertion" in capsys.readouterr().err


def test_an_edit_outside_the_test_is_not_held_to_it(tmp_path, monkeypatch):
    root = _repo(tmp_path, monkeypatch)
    (root / "tests" / "test_x.py").write_text(ASSERTLESS + "\nVALUE = 1\n")
    assert cli.main(["--base", "HEAD", "tests/test_x.py"]) == 0


def test_a_test_with_an_assert_passes(tmp_path, monkeypatch):
    root = _repo(tmp_path, monkeypatch)
    (root / "tests" / "test_x.py").write_text("def test_a():\n    assert run() == 1\n")
    assert cli.main(["--base", "HEAD", "tests/test_x.py"]) == 0
