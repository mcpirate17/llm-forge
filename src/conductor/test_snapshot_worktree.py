from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from conductor import snapshot_worktree


def _git(repo: Path, *args: str) -> str:
    proc = subprocess.run(
        ["git", *args],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.returncode == 0, proc.stderr
    return proc.stdout.strip()


def _repository(tmp_path: Path) -> Path:
    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init", "--quiet")
    _git(repo, "config", "user.name", "Snapshot Test")
    _git(repo, "config", "user.email", "snapshot@test.invalid")
    (repo / "changed.py").write_text("before\n", encoding="utf-8")
    (repo / "deleted.py").write_text("remove me\n", encoding="utf-8")
    _git(repo, "add", "changed.py", "deleted.py")
    _git(repo, "commit", "--quiet", "-m", "baseline")
    return repo


def _object_inventory(repo: Path) -> tuple[str, ...]:
    object_dir = Path(_git(repo, "rev-parse", "--git-path", "objects"))
    if not object_dir.is_absolute():
        object_dir = repo / object_dir
    return tuple(
        sorted(
            path.relative_to(object_dir).as_posix()
            for path in object_dir.rglob("*")
            if path.is_file()
        )
    )


def test_snapshot_reproduces_dirty_tree_without_polluting_host_objects(
    tmp_path: Path,
) -> None:
    repo = _repository(tmp_path)
    (repo / "changed.py").write_text("after\n", encoding="utf-8")
    (repo / "deleted.py").unlink()
    (repo / "added.py").write_text("new\n", encoding="utf-8")
    receipt = repo / "conductor/mutation_campaigns/receipts/running.json"
    receipt.parent.mkdir(parents=True)
    receipt.write_text('{"status":"RUNNING"}\n', encoding="utf-8")
    objects_before = _object_inventory(repo)
    worktrees_before = _git(repo, "worktree", "list", "--porcelain")

    with snapshot_worktree.isolated_snapshot(repo) as snapshot:
        assert (snapshot.worktree / "changed.py").read_text() == "after\n"
        assert not (snapshot.worktree / "deleted.py").exists()
        assert (snapshot.worktree / "added.py").read_text() == "new\n"
        assert "added.py" in snapshot.included_untracked
        assert "conductor/mutation_campaigns/receipts/running.json" not in (
            snapshot.included_untracked
        )
        assert not (snapshot.worktree / receipt.relative_to(repo)).exists()
        assert _git(repo, "worktree", "list", "--porcelain") == worktrees_before
        assert _object_inventory(repo) == objects_before

    assert _git(repo, "worktree", "list", "--porcelain") == worktrees_before
    assert _object_inventory(repo) == objects_before


def test_snapshot_exception_removes_temporary_repository(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    repo = _repository(tmp_path)
    snapshot_root = tmp_path / "forced-snapshot-root"

    def make_snapshot_root(*_args: object, **_kwargs: object) -> str:
        snapshot_root.mkdir()
        return str(snapshot_root)

    monkeypatch.setattr(snapshot_worktree.tempfile, "mkdtemp", make_snapshot_root)
    objects_before = _object_inventory(repo)
    worktrees_before = _git(repo, "worktree", "list", "--porcelain")

    with (
        pytest.raises(RuntimeError, match="stop inside snapshot"),
        snapshot_worktree.isolated_snapshot(repo),
    ):
        raise RuntimeError("stop inside snapshot")

    assert not snapshot_root.exists()
    assert _git(repo, "worktree", "list", "--porcelain") == worktrees_before
    assert _object_inventory(repo) == objects_before


def test_snapshot_from_linked_worktree_leaves_shared_objects_unchanged(
    tmp_path: Path,
) -> None:
    repo = _repository(tmp_path)
    linked = tmp_path / "linked"
    _git(repo, "worktree", "add", "--quiet", "-b", "linked-test", str(linked))
    (linked / "changed.py").write_text("linked change\n", encoding="utf-8")
    objects_before = _object_inventory(linked)
    worktrees_before = _git(linked, "worktree", "list", "--porcelain")

    with snapshot_worktree.isolated_snapshot(linked) as snapshot:
        assert (snapshot.worktree / "changed.py").read_text() == "linked change\n"
        assert _git(linked, "worktree", "list", "--porcelain") == worktrees_before
        assert _object_inventory(linked) == objects_before

    assert _git(linked, "worktree", "list", "--porcelain") == worktrees_before
    assert _object_inventory(linked) == objects_before
