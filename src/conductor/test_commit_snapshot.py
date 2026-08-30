"""`governance-commit` must be incapable of losing work.

`git commit` fires pre-commit, whose `staged_files_only` writes a patch and then runs
`git checkout -- .` across the WHOLE worktree. That wipes every unstaged tracked
modification -- including files belonging to other agents in the same checkout -- and
never captures untracked files at all. If the process dies between the checkout and
the restore, the only copy left is a patch under ~/.cache/pre-commit/.

These tests pin the properties that make that survivable: the snapshot exists before
the commit runs, it holds unstaged AND untracked content, and taking it never touches
the repository's shared index.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from conductor.candidate_review.engine import snapshot_working_tree


def _git(repo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=repo, capture_output=True, text=True, check=True
    ).stdout.strip()


@pytest.fixture
def repo(tmp_path: Path) -> Path:
    root = tmp_path / "repo"
    root.mkdir()
    _git(root, "init", "--quiet", "-b", "main")
    _git(root, "config", "user.email", "test@example.invalid")
    _git(root, "config", "user.name", "test")
    (root / "tracked.py").write_text("VALUE = 1\n", encoding="utf-8")
    _git(root, "add", "tracked.py")
    _git(root, "commit", "--quiet", "-m", "first")
    return root


def test_snapshot_returns_a_ref_that_exists(repo: Path) -> None:
    ref = snapshot_working_tree(repo, "agent")
    assert ref is not None
    assert ref.startswith("refs/snapshots/agent/")
    assert _git(repo, "cat-file", "-t", ref) == "commit"


def test_snapshot_captures_an_unstaged_modification(repo: Path) -> None:
    """The exact bytes `git checkout -- .` would discard."""
    (repo / "tracked.py").write_text("VALUE = 999\n", encoding="utf-8")
    ref = snapshot_working_tree(repo, "agent")
    assert _git(repo, "cat-file", "-p", f"{ref}:tracked.py") == "VALUE = 999"


def test_snapshot_captures_an_untracked_file(repo: Path) -> None:
    """pre-commit's isolation never captures these at all."""
    (repo / "untracked.py").write_text("NEW = 2\n", encoding="utf-8")
    ref = snapshot_working_tree(repo, "agent")
    assert _git(repo, "cat-file", "-p", f"{ref}:untracked.py") == "NEW = 2"


def test_snapshot_excludes_gitignored_files(repo: Path) -> None:
    """The other side: run artifacts must not bloat the snapshot.

    The shared checkout carries 83k gitignored files under research/reports/; a
    snapshot that swept those in would be unusable.
    """
    (repo / ".gitignore").write_text("*.log\n", encoding="utf-8")
    (repo / "noise.log").write_text("x\n", encoding="utf-8")
    ref = snapshot_working_tree(repo, "agent")
    listed = _git(repo, "ls-tree", "-r", "--name-only", ref).splitlines()
    assert "noise.log" not in listed
    assert ".gitignore" in listed


def test_snapshot_does_not_touch_the_shared_index(repo: Path) -> None:
    """A private GIT_INDEX_FILE, because writing the real index destroys peers' staging."""
    (repo / "staged.py").write_text("S = 1\n", encoding="utf-8")
    _git(repo, "add", "staged.py")
    before = _git(repo, "diff", "--cached", "--name-only")
    (repo / "untracked.py").write_text("U = 1\n", encoding="utf-8")
    snapshot_working_tree(repo, "agent")
    assert _git(repo, "diff", "--cached", "--name-only") == before


def test_snapshot_leaves_no_index_file_behind(repo: Path) -> None:
    snapshot_working_tree(repo, "agent")
    git_dir = Path(_git(repo, "rev-parse", "--absolute-git-dir"))
    assert not list(git_dir.glob("governance-snapshot-index-*"))


def test_snapshot_parents_the_current_head(repo: Path) -> None:
    head = _git(repo, "rev-parse", "HEAD")
    ref = snapshot_working_tree(repo, "agent")
    assert _git(repo, "rev-parse", f"{ref}^") == head


def test_snapshot_of_a_clean_tree_still_produces_a_ref(repo: Path) -> None:
    """No dirty content is not a reason to skip the safety net."""
    ref = snapshot_working_tree(repo, "agent")
    assert ref is not None
    assert _git(repo, "cat-file", "-p", f"{ref}:tracked.py") == "VALUE = 1"


def test_snapshot_returns_none_outside_a_repository(tmp_path: Path) -> None:
    """Failing to snapshot must not raise -- it must never become a new way to fail
    a commit. The caller warns and proceeds."""
    assert snapshot_working_tree(tmp_path, "agent") is None


def test_owner_appears_in_the_ref(repo: Path) -> None:
    ref = snapshot_working_tree(repo, "glm-flash-04")
    assert ref is not None
    assert "/glm-flash-04/" in ref
