"""The rehearsal's assembly and report are honest, tested on fake trees (no venv)."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

from conductor import tooling_standalone_smoke as smoke

JUNIT = """<?xml version="1.0" encoding="utf-8"?>
<testsuites>
<testsuite name="pytest" tests="7" failures="2" errors="2" skipped="1">
<testcase classname="src.conductor.test_a" name="test_ok"/>
<testcase classname="src.conductor.test_a" name="test_skip"><skipped message="no gpu"/></testcase>
<testcase classname="src.conductor.test_a" name="test_f1"><failure message="AssertionError: research/ missing&#10;detail">tb</failure></testcase>
<testcase classname="src.conductor.test_b" name="test_f2"><failure message="AssertionError: research/ missing&#10;other">tb</failure></testcase>
<testcase classname="src.conductor.test_b" name="test_e1"><error message="ImportError: no slop_core">tb</error></testcase>
<testcase classname="src.conductor.test_b" name="test_ok2"/>
<testcase classname="src.conductor.test_c" name="test_c"><error message="collection failure">src/conductor/test_c.py:3: in &lt;module&gt;
    import audit
E   ModuleNotFoundError: No module named 'audit'</error></testcase>
</testsuite>
</testsuites>
"""


def _staging(tmp_path: Path) -> Path:
    staging = tmp_path / "staging"
    for src, _ in smoke.LAYOUT:
        path = staging / src
        if Path(src).suffix:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("x\n")
        else:
            path.mkdir(parents=True)
            (path / "marker").write_text("x\n")
    (staging / ".claude" / "hooks" / smoke.EXCLUDED_HOOK_SUBDIR).mkdir()
    (staging / ".claude" / "hooks" / smoke.EXCLUDED_HOOK_SUBDIR / "env.sh").write_text(
        "x\n"
    )
    return staging


def test_lay_out_moves_every_part_and_drops_the_project_hooks(tmp_path: Path) -> None:
    dest = tmp_path / "dest"
    smoke.lay_out(_staging(tmp_path), dest)
    assert (dest / "src" / "conductor" / "marker").is_file()
    assert (dest / "native" / "conductor-native" / "marker").is_file()
    assert (dest / "hooks" / "marker").is_file()
    assert (dest / "pyproject.toml").is_file() and (dest / "README.md").is_file()
    assert not (dest / "hooks" / smoke.EXCLUDED_HOOK_SUBDIR).exists()
    assert not (dest / "conductor").exists()


def test_lay_out_fails_loud_when_a_part_is_missing(tmp_path: Path) -> None:
    staging = _staging(tmp_path)
    (staging / "tooling" / "README.md").unlink()
    with pytest.raises(FileNotFoundError, match="tooling/README.md"):
        smoke.lay_out(staging, tmp_path / "dest")


def test_archive_tree_takes_the_committed_tree_only(tmp_path: Path) -> None:
    repo = tmp_path / "repo"
    for src, _ in smoke.LAYOUT:
        path = repo / src
        if Path(src).suffix:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("x\n")
        else:
            path.mkdir(parents=True)
            (path / "committed.txt").write_text("x\n")
    env = {
        "GIT_AUTHOR_NAME": "t",
        "GIT_AUTHOR_EMAIL": "t@x",
        "GIT_COMMITTER_NAME": "t",
        "GIT_COMMITTER_EMAIL": "t@x",
        "PATH": "/usr/bin:/bin",
    }
    for cmd in (
        ["git", "init", "-q"],
        ["git", "add", "-A"],
        ["git", "commit", "-q", "-m", "seed"],
    ):
        subprocess.run(cmd, cwd=repo, check=True, env=env)
    (repo / "conductor" / "uncommitted.txt").write_text("x\n")
    staging = tmp_path / "staging"
    tree = smoke.archive_tree(repo, "HEAD", staging)
    assert len(tree) == 40
    assert (staging / "conductor" / "committed.txt").is_file()
    assert not (staging / "conductor" / "uncommitted.txt").exists()
    with pytest.raises(RuntimeError, match="rev-parse"):
        smoke.archive_tree(repo, "no-such-ref", tmp_path / "s2")


def test_assert_project_absent_raises_when_a_host_package_imports(
    tmp_path: Path,
) -> None:
    # -S drops site-packages (this venv carries the host on a .pth); the cwd stays
    # on sys.path, so a package created there is the "host present" case.
    python = tmp_path / "python"
    python.write_text(f'#!/bin/sh\nexec {sys.executable} -S "$@"\n')
    python.chmod(0o755)
    smoke.assert_project_absent(str(python), tmp_path)
    package = tmp_path / smoke.PROJECT_PACKAGES[0]
    package.mkdir()
    (package / "__init__.py").write_text("")
    with pytest.raises(RuntimeError, match="importable"):
        smoke.assert_project_absent(str(python), tmp_path)


def test_clean_env_drops_the_host_pythonpath_and_plugin(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("PYTHONPATH", "/host/snapshot")
    monkeypatch.setenv(smoke.PLUGIN_ENV, "conductor._project_hooks")
    monkeypatch.setenv("KEEP_ME", "1")
    env = smoke.clean_env()
    assert "PYTHONPATH" not in env
    assert env[smoke.PLUGIN_ENV] == ""
    assert env["PYTHONDONTWRITEBYTECODE"] == "1"
    assert env["KEEP_ME"] == "1"


def test_assert_project_absent_probes_in_the_clean_env(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A probe inheriting the host's PYTHONPATH or plugin would import the host."""
    python = tmp_path / "python"
    python.write_text(
        f'#!/bin/sh\n[ -n "$PYTHONPATH" ] || [ -n "${smoke.PLUGIN_ENV}" ]\n'
    )
    python.chmod(0o755)
    monkeypatch.setenv("PYTHONPATH", "/host/snapshot")
    monkeypatch.setenv(smoke.PLUGIN_ENV, "conductor._project_hooks")
    smoke.assert_project_absent(str(python), tmp_path)


def test_summarize_junit_counts_nodeids_and_groups_by_first_line() -> None:
    summary = smoke.summarize_junit(JUNIT)
    assert (summary.passed, summary.failed, summary.errors, summary.skipped) == (
        2,
        2,
        2,
        1,
    )
    assert summary.failing_nodeids == [
        "src.conductor.test_a::test_f1",
        "src.conductor.test_b::test_f2",
        "src.conductor.test_b::test_e1",
        "src.conductor.test_c::test_c",
    ]
    assert summary.failure_groups == [
        {"message": "AssertionError: research/ missing", "count": 2},
        {"message": "ImportError: no slop_core", "count": 1},
        {
            "message": "collection failure: ModuleNotFoundError: No module named 'audit'",
            "count": 1,
        },
    ]


def test_verdict_is_never_optimistic() -> None:
    clean = smoke.Summary(passed=3)
    assert smoke.verdict(0, clean) == 0
    assert smoke.verdict(1, clean) == 1
    assert smoke.verdict(0, smoke.Summary(passed=3, failed=1)) == 1
    assert smoke.verdict(0, smoke.Summary(passed=3, errors=1)) == 1
    assert smoke.verdict(0, smoke.Summary(passed=0, skipped=3)) == 0


def test_write_report_is_sorted_json(tmp_path: Path) -> None:
    path = tmp_path / "r" / "report.json"
    smoke.write_report(path, {"b": 1, "a": [2]})
    assert path.read_text() == '{\n  "a": [\n    2\n  ],\n  "b": 1\n}\n'
