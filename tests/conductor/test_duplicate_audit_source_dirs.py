"""The pylint and NiCad duplicate checks scan the host's source_dirs, not a hardcoded list."""

import subprocess
from pathlib import Path

from conductor import run_duplicate_audit as rda


def _host(tmp_path: Path) -> Path:
    (tmp_path / "pyproject.toml").write_text(
        '[tool.forge.duplicate_audit]\nsource_dirs = ["pkg"]\n', encoding="utf-8"
    )
    (tmp_path / "pkg").mkdir()
    (tmp_path / "pkg" / "a.py").write_text("x = 1\n", encoding="utf-8")
    (tmp_path / "research").mkdir()
    (tmp_path / "research" / "b.py").write_text("y = 2\n", encoding="utf-8")
    return tmp_path


def test_pylint_file_list_comes_from_pyproject_source_dirs(tmp_path, monkeypatch):
    root = _host(tmp_path)
    commands = []

    def fake_run(cmd, **_kwargs):
        commands.append(cmd)
        return subprocess.CompletedProcess(cmd, 0)

    monkeypatch.setattr(rda.subprocess, "run", fake_run)
    assert rda.run_pylint(check=True, root=root) == 0
    listed = (root / "tasks" / "audit" / "duplication-python-files.txt").read_text(
        encoding="utf-8"
    )
    assert "pkg" in listed
    assert "research" not in listed


def test_nicad_scans_pyproject_source_dirs(tmp_path, monkeypatch):
    root = _host(tmp_path)
    commands = []

    def fake_run(cmd, **_kwargs):
        commands.append(cmd)
        return subprocess.CompletedProcess(cmd, 0)

    monkeypatch.setattr(rda, "command_path", lambda _name: "/fake/nicad")
    monkeypatch.setattr(rda.subprocess, "run", fake_run)
    assert rda.run_nicad_python(check=True, root=root) == 0
    assert commands == [
        ["/fake/nicad", "functions", "py", str(root / "pkg"), "notests-report"]
    ]
