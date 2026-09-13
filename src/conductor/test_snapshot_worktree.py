from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from conductor import snapshot_worktree
from conductor.project_paths import ProjectPathError


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


def test_fixture_trees_survive_a_snapshot_regardless_of_suffix(
    tmp_path: Path,
) -> None:
    """`.db`, `.txt` and extensionless fixtures must reach the snapshot worktree.

    Dropping them by suffix sent campaigns whose tests walk such a tree home as
    BASELINE_FAILED: the baseline run itself could not pass inside a snapshot
    that had silently lost its fixtures.
    """

    repo = _repository(tmp_path)
    fixtures = {
        "tests/ledger.db": b"\x00sqlite payload",
        "fixtures/notes.txt": "plain text\n",
        "tests/runnable": "#!/bin/sh\n",
    }
    for relative, body in fixtures.items():
        path = repo / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(body, bytes):
            path.write_bytes(body)
        else:
            path.write_text(body, encoding="utf-8")
    (repo / "src/kept.py").parent.mkdir(parents=True, exist_ok=True)
    (repo / "src/kept.py").write_text("kept = 1\n", encoding="utf-8")
    (repo / "data/dropped.bin").parent.mkdir(parents=True, exist_ok=True)
    (repo / "data/dropped.bin").write_bytes(b"\xff")
    (repo / "data/extensionless").write_text("no suffix, no fixture dir\n", encoding="utf-8")

    with snapshot_worktree.isolated_snapshot(repo) as snapshot:
        for relative, body in fixtures.items():
            assert relative in snapshot.included_untracked
            expected = body if isinstance(body, bytes) else body.encode()
            assert (snapshot.worktree / relative).read_bytes() == expected
        assert "src/kept.py" in snapshot.included_untracked
        assert "data/dropped.bin" not in snapshot.included_untracked
        assert "data/extensionless" not in snapshot.included_untracked
        assert not (snapshot.worktree / "data/dropped.bin").exists()


def test_extra_snapshot_suffixes_are_configurable_outside_fixture_trees(
    tmp_path: Path,
) -> None:
    """A host names the suffixes its data files carry where no fixture path rules."""

    repo = _repository(tmp_path)
    (repo / "pyproject.toml").write_text(
        '[tool.conductor]\nsnapshot_extra_suffixes = [".db", "dat"]\n',
        encoding="utf-8",
    )
    for relative in ("data/ledger.db", "data/series.dat"):
        path = repo / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b"payload")

    with snapshot_worktree.isolated_snapshot(repo) as snapshot:
        assert "data/ledger.db" in snapshot.included_untracked
        assert "data/series.dat" in snapshot.included_untracked
        assert (snapshot.worktree / "data/ledger.db").read_bytes() == b"payload"

    (repo / "pyproject.toml").write_text(
        '[tool.conductor]\nsnapshot_extra_suffixes = "db"\n', encoding="utf-8"
    )
    with pytest.raises(ProjectPathError, match="snapshot_extra_suffixes"):
        snapshot_worktree.snapshot_untracked_paths(repo)


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


def test_the_exported_interpreter_defaults_to_the_running_one(
    tmp_path: Path,
) -> None:
    """Which interpreter a snapshot hands its tests, in preference order."""

    import sys

    repo = _repository(tmp_path)
    # No configuration: the interpreter building the snapshot, which by
    # construction is one that can import conductor.
    assert snapshot_worktree.snapshot_python(repo) == sys.executable

    # A host that needs a specific one names it once.
    (repo / "pyproject.toml").write_text(
        '[tool.conductor]\nsnapshot_python = "/opt/other/python"\n',
        encoding="utf-8",
    )
    assert snapshot_worktree.snapshot_python(repo) == "/opt/other/python"

    # Anything that is not a usable path is a configuration error, not a guess.
    for bad in ('snapshot_python = ""', "snapshot_python = 3"):
        (repo / "pyproject.toml").write_text(
            f"[tool.conductor]\n{bad}\n", encoding="utf-8"
        )
        with pytest.raises(ProjectPathError, match="snapshot_python"):
            snapshot_worktree.snapshot_python(repo)


def test_the_exported_interpreter_imports_conductor_inside_the_snapshot(
    tmp_path: Path,
) -> None:
    """Gap 6's acceptance: a snapshot carries no .venv, yet a test that shells
    out to Python inside it must reach an interpreter that can import
    `conductor` -- the situation whose absence killed Rust campaigns at their
    own baseline inside snapshots while the same suite passed on the host."""

    from conductor import mutation_engine_generated

    repo = _repository(tmp_path)
    with snapshot_worktree.isolated_snapshot(repo) as snapshot:
        # The snapshot is a git tree and .venv is gitignored: whatever resolves
        # `python3` from PATH here is not an interpreter with conductor in it.
        assert not (snapshot.worktree / ".venv").exists()
        exported = mutation_engine_generated.snapshot_python_environment()
        proc = subprocess.run(
            [exported["CONDUCTOR_SNAPSHOT_PYTHON"], "-c", "import conductor"],
            cwd=snapshot.worktree,
            capture_output=True,
            text=True,
            check=False,
        )
        assert proc.returncode == 0, proc.stderr
