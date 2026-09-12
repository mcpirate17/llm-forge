from __future__ import annotations

import os
import subprocess
from pathlib import Path

import pytest

from conductor import sandbox

GIT_ENV = {
    **os.environ,
    "GIT_AUTHOR_NAME": "Sandbox Test",
    "GIT_AUTHOR_EMAIL": "sandbox@example.invalid",
    "GIT_COMMITTER_NAME": "Sandbox Test",
    "GIT_COMMITTER_EMAIL": "sandbox@example.invalid",
    "GIT_CONFIG_GLOBAL": "/dev/null",
    "GIT_CONFIG_SYSTEM": "/dev/null",
}


@pytest.fixture
def repo(tmp_path: Path) -> Path:
    root = tmp_path / "repo"
    (root / "wanted").mkdir(parents=True)
    (root / "unwanted").mkdir()
    (root / "wanted" / "tool.py").write_text("print('v1')\n")
    (root / "unwanted" / "heavy.pt").write_bytes(b"weights")
    subprocess.run(["git", "init", "-b", "master", str(root)], check=True, env=GIT_ENV)
    subprocess.run(["git", "add", "."], cwd=root, check=True, env=GIT_ENV)
    subprocess.run(["git", "commit", "-m", "one"], cwd=root, check=True, env=GIT_ENV)
    return root


def test_export_takes_only_the_named_paths(repo, tmp_path):
    dest = sandbox.export(repo, "HEAD", ["wanted"], tmp_path / "box")
    assert (dest / "wanted" / "tool.py").read_text() == "print('v1')\n"
    assert not (dest / "unwanted").exists()
    assert not (dest / ".git").exists()


def test_export_is_frozen_at_the_commit(repo, tmp_path):
    first = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=repo,
        capture_output=True,
        text=True,
        check=True,
        env=GIT_ENV,
    ).stdout.strip()
    (repo / "wanted" / "tool.py").write_text("print('v2')\n")
    subprocess.run(["git", "commit", "-am", "two"], cwd=repo, check=True, env=GIT_ENV)
    dest = sandbox.export(repo, first, ["wanted"], tmp_path / "box")
    assert (dest / "wanted" / "tool.py").read_text() == "print('v1')\n"


def test_export_replaces_a_previous_sandbox(repo, tmp_path):
    dest = tmp_path / "box"
    dest.mkdir()
    (dest / "stale.txt").write_text("old\n")
    sandbox.export(repo, "HEAD", ["wanted"], dest)
    assert not (dest / "stale.txt").exists()


def test_export_refuses_an_empty_path_list(repo, tmp_path):
    with pytest.raises(ValueError, match="directories the run imports"):
        sandbox.export(repo, "HEAD", [], tmp_path / "box")


def test_export_fails_loudly_on_an_unknown_commit(repo, tmp_path):
    with pytest.raises(RuntimeError, match="git archive"):
        sandbox.export(repo, "nope", ["wanted"], tmp_path / "box")
    assert not (tmp_path / "box").exists()


def test_main_prints_the_sandbox_path(repo, tmp_path, capsys):
    code = sandbox.main(
        [
            "--repo",
            str(repo),
            "--root",
            str(tmp_path / "root"),
            "--name",
            "run",
            "wanted",
        ]
    )
    assert code == 0
    printed = Path(capsys.readouterr().out.strip())
    assert printed == tmp_path / "root" / "run"
    assert (printed / "wanted" / "tool.py").exists()


def test_main_reports_failure_without_a_traceback(repo, tmp_path, capsys):
    code = sandbox.main(
        [
            "--repo",
            str(repo),
            "--root",
            str(tmp_path / "root"),
            "--name",
            "run",
            "absent",
        ]
    )
    assert code == 2
    assert "sandbox:" in capsys.readouterr().err


def test_default_root_prefers_the_session_scratchpad(monkeypatch, tmp_path):
    monkeypatch.delenv("SANDBOX_ROOT", raising=False)
    monkeypatch.setenv("CLAUDE_SCRATCHPAD_DIR", str(tmp_path / "scratch"))
    assert sandbox.default_root() == tmp_path / "scratch"
    monkeypatch.setenv("SANDBOX_ROOT", str(tmp_path / "explicit"))
    assert sandbox.default_root() == tmp_path / "explicit"
