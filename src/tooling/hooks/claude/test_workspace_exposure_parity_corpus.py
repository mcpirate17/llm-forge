"""Differential parity twin (Python side) for the Rust port of the
SessionStart EXPOSED line: `conductor.workspace_hygiene.exposure_line`,
native in `native/forge/src/workspace_hygiene.rs`.

`native/forge/tests/workspace_exposure_parity.rs` and this file load the SAME
two fixtures -- `workspace_exposure_corpus.json` (16 case descriptors: each is
a recipe for a complete repository state -- branch, optional bare origin and
conductor integration table, unpushed commits, dirty files with pinned mtimes,
registered worktrees) and `workspace_exposure_expected.json` (the frozen
EXPOSED lines, captured once from this very Python implementation) -- and each
independently rebuilds the repository state from its recipe and asserts its
own live implementation still matches the frozen line. That pins both
implementations to one shared ground truth instead of comparing them to each
other at test time, following the same shape as
`test_tool_quiet_parity_corpus.py`.

Determinism: every mtime the line can see is pinned to a fixed epoch
(`1000000000` reads as decades stale on any machine that runs this test;
`4102444800` is in the future and never stale) or left at "now" (fresh by a
margin no test run can close), and the line embeds no paths, so the frozen
strings are identical regardless of where the scratch directory lives.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

_HERE = Path(__file__).resolve().parent
_FIXTURES = (
    _HERE.parent.parent.parent.parent / "native" / "forge" / "tests" / "fixtures"
)

sys.path.insert(0, str(_HERE.parent.parent.parent.parent / "src"))
from conductor.workspace_hygiene import exposure_line  # noqa: E402


def _git_ok(where: Path, *args: str) -> str:
    done = subprocess.run(
        ["git", *args], cwd=where, capture_output=True, text=True, check=False
    )
    assert done.returncode == 0, f"git {' '.join(args)} in {where}: {done.stderr}"
    return done.stdout.strip()


def _apply_mtime(repo: Path, relative: str, mtime: int | str) -> None:
    """Pin a file's mtime the way both twins do: epoch via utime, "now" left
    alone (the file was just written, so it is fresh on any test schedule)."""
    if mtime == "now":
        return
    target = repo / relative
    os.utime(target, (int(mtime), int(mtime)))


def build_case(parent: Path, case: dict) -> Path:
    """Rebuild one corpus case's repository state from its recipe and return
    the repo path. Shared verbatim with the fixture generator, so the frozen
    expected lines can never drift from what this builder produces."""
    branch = case["branch"]
    name = case["id"]
    repo = parent / f"{name}-repo"
    if case.get("origin"):
        _git_ok(
            parent,
            "init",
            "--quiet",
            "--bare",
            "-b",
            branch,
            str(parent / f"{name}-origin.git"),
        )
    _git_ok(parent, "init", "--quiet", "-b", branch, str(repo))
    _git_ok(repo, "config", "user.email", "parity@example.invalid")
    _git_ok(repo, "config", "user.name", "parity")
    (repo / "seed.txt").write_text("seed\n", encoding="utf-8")
    if case.get("integration"):
        (repo / "pyproject.toml").write_text(
            f'[tool.conductor]\nintegration_branch = "{case["integration"]}"\n',
            encoding="utf-8",
        )
        _git_ok(repo, "add", "pyproject.toml")
    _git_ok(repo, "add", "seed.txt")
    _git_ok(repo, "commit", "--quiet", "-m", "seed")
    if case.get("origin"):
        _git_ok(repo, "remote", "add", "origin", str(parent / f"{name}-origin.git"))
        _git_ok(repo, "push", "--quiet", "origin", branch)
    if case.get("origin_head"):
        _git_ok(
            repo,
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            f"refs/remotes/origin/{case['origin_head']}",
        )
    for commit_name in case.get("commits", []):
        (repo / commit_name).write_text(f"{commit_name}\n", encoding="utf-8")
        _git_ok(repo, "add", commit_name)
        _git_ok(repo, "commit", "--quiet", "-m", commit_name)
    for entry in case.get("files", []):
        target = repo / entry["path"]
        if entry["kind"] == "modify_tracked":
            target.write_text("modified\n", encoding="utf-8")
        else:
            target.write_text("untracked\n", encoding="utf-8")
        _apply_mtime(repo, entry["path"], entry["mtime"])
    for index, worktree in enumerate(case.get("worktrees", [])):
        wt_branch = worktree["branch"]
        wt_path = parent / f"{name}-wt{index}"
        # "pushed" registers the worktree at the line that was already pushed
        # (a merged feature tree while main moved on); the default is HEAD.
        start = f"origin/{branch}" if worktree.get("at") == "pushed" else "HEAD"
        _git_ok(repo, "worktree", "add", "--quiet", "-b", wt_branch, str(wt_path), start)
        if worktree.get("commit"):
            (wt_path / worktree["commit"]).write_text(
                f"{worktree['commit']}\n", encoding="utf-8"
            )
            _git_ok(wt_path, "add", worktree["commit"])
            _git_ok(wt_path, "commit", "--quiet", "-m", worktree["commit"])
        if worktree.get("pruned_upstream"):
            _git_ok(wt_path, "push", "--quiet", "-u", "origin", wt_branch)
            _git_ok(wt_path, "push", "--quiet", "origin", "--delete", wt_branch)
        if worktree.get("dirty"):
            (wt_path / "leftover.txt").write_text("leftover\n", encoding="utf-8")
    return repo


def _load_json(name: str):
    return json.loads((_FIXTURES / name).read_text())


def test_fixture_files_exist_and_are_shared_with_the_rust_test() -> None:
    corpus = _load_json("workspace_exposure_corpus.json")
    expected = _load_json("workspace_exposure_expected.json")
    assert len(corpus) == len(expected)
    assert len(corpus) >= 12, f"expected 12+ corpus cases, got {len(corpus)}"


def test_python_exposure_line_matches_the_frozen_corpus(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    corpus = _load_json("workspace_exposure_corpus.json")
    expected = _load_json("workspace_exposure_expected.json")
    monkeypatch.delenv("CONDUCTOR_INTEGRATION_BRANCH", raising=False)
    failures = []
    for case in corpus:
        repo = build_case(tmp_path, case)
        actual = exposure_line(repo)
        if actual != expected[case["id"]]:
            failures.append(
                f"case {case['id']!r}: python={actual!r} expected={expected[case['id']]!r}"
            )
    assert not failures, f"{len(failures)} parity mismatches:\n" + "\n".join(failures)
