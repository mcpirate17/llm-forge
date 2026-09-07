"""Contracts for the checkout fast-forward: never lose a tree, never merge blind."""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from conductor.checkout_sync import SNAPSHOT_NAMESPACE, SyncError, snapshot, sync


def _git(repo: Path, *args: str) -> str:
    done = subprocess.run(
        ["git", *args], cwd=repo, capture_output=True, text=True, check=True
    )
    return done.stdout


def _pair(tmp_path: Path) -> tuple[Path, Path]:
    """An upstream repo on master and a clone of it, both ready to commit."""

    upstream = tmp_path / "upstream"
    upstream.mkdir()
    _git(upstream, "init", "-q", "-b", "master")
    _git(upstream, "config", "user.email", "t@example.invalid")
    _git(upstream, "config", "user.name", "test")
    (upstream / "tracked.txt").write_text("one\n")
    _git(upstream, "add", "-A")
    _git(upstream, "commit", "-qm", "first")

    clone = tmp_path / "clone"
    _git(tmp_path, "clone", "-q", str(upstream), str(clone))
    _git(clone, "config", "user.email", "t@example.invalid")
    _git(clone, "config", "user.name", "test")
    return upstream, clone


def _advance(upstream: Path, clone: Path, name: str, body: str) -> None:
    (upstream / name).write_text(body)
    _git(upstream, "add", "-A")
    _git(upstream, "commit", "-qm", f"add {name}")
    _git(clone, "fetch", "-q", "origin")


def test_a_checkout_already_even_with_the_line_is_left_alone(tmp_path):
    _, clone = _pair(tmp_path)

    result = sync(clone)

    assert result["outcome"] == "already even"
    assert result["snapshot"] is None


def test_a_dirty_checkout_fast_forwards_and_its_whole_tree_is_recoverable(tmp_path):
    upstream, clone = _pair(tmp_path)
    _advance(upstream, clone, "incoming.txt", "landed\n")
    (clone / "tracked.txt").write_text("edited locally\n")
    (clone / "untracked.txt").write_text("never committed\n")

    result = sync(clone)

    assert result["outcome"] == "fast-forwarded"
    assert result["behind"] == 1
    assert (clone / "incoming.txt").read_text() == "landed\n"
    assert (clone / "tracked.txt").read_text() == "edited locally\n"

    saved = str(result["snapshot"])
    assert saved.startswith(SNAPSHOT_NAMESPACE)
    assert _git(clone, "show", f"{saved}:untracked.txt") == "never committed\n"
    assert _git(clone, "show", f"{saved}:tracked.txt") == "edited locally\n"


def test_the_snapshot_never_stages_anything_in_the_callers_index(tmp_path):
    _, clone = _pair(tmp_path)
    (clone / "untracked.txt").write_text("never committed\n")

    snapshot(clone)

    assert _git(clone, "diff", "--cached", "--name-only") == ""
    assert "?? untracked.txt" in _git(clone, "status", "--porcelain")


def test_a_clean_tree_has_nothing_to_snapshot(tmp_path):
    _, clone = _pair(tmp_path)

    assert snapshot(clone) is None


def test_a_tracked_file_changed_on_both_sides_blocks_the_merge_and_names_itself(
    tmp_path,
):
    upstream, clone = _pair(tmp_path)
    _advance(upstream, clone, "tracked.txt", "changed upstream\n")
    (clone / "tracked.txt").write_text("changed locally\n")
    before = _git(clone, "rev-parse", "HEAD")

    result = sync(clone)

    assert result["outcome"] == "blocked"
    assert result["blocked_by"] == ["tracked.txt"]
    assert result["snapshot"] is None
    assert _git(clone, "rev-parse", "HEAD") == before


def test_a_checkout_holding_unlanded_commits_is_refused_not_fast_forwarded(tmp_path):
    upstream, clone = _pair(tmp_path)
    _advance(upstream, clone, "incoming.txt", "landed\n")
    (clone / "local.txt").write_text("mine\n")
    _git(clone, "add", "-A")
    _git(clone, "commit", "-qm", "local work")

    with pytest.raises(SyncError, match="rather than fast-forwarding over them"):
        sync(clone)


def test_a_dry_run_reports_the_move_without_making_it(tmp_path):
    upstream, clone = _pair(tmp_path)
    _advance(upstream, clone, "incoming.txt", "landed\n")
    before = _git(clone, "rev-parse", "HEAD")

    result = sync(clone, dry_run=True)

    assert result["outcome"] == "would fast-forward"
    assert result["snapshot"] is None
    assert _git(clone, "rev-parse", "HEAD") == before
    assert not (clone / "incoming.txt").exists()
