"""Tests for `conductor.snapshot_retention` -- index and expiry for the
`refs/snapshots/**` namespace, which had accumulated 80+ refs with no way to list or
prune them (`research/notes/branch_exposure_audit_2026-08-30.md`).

A real bare repo stands in for `origin` throughout: this module's entire job is
reading and deleting *remote* refs, so a fake `refs/remotes/origin/*` local ref (the
convention `workspace_hygiene`'s tests use) would test nothing real.
"""

from __future__ import annotations

import os
import subprocess
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from conductor import snapshot_retention as sr


def _git(repo: Path, *args: str) -> str:
    completed = subprocess.run(
        ["git", *args], cwd=repo, capture_output=True, text=True, check=False
    )
    if completed.returncode != 0:
        pytest.fail(f"git {' '.join(args)} failed: {completed.stderr.strip()}")
    return completed.stdout


@pytest.fixture
def origin(tmp_path: Path) -> Path:
    bare = tmp_path / "origin.git"
    bare.mkdir()
    _git(bare, "init", "--quiet", "--bare", "--initial-branch=master")
    return bare


@pytest.fixture
def repo(tmp_path: Path, origin: Path) -> Path:
    clone = tmp_path / "clone"
    _git(tmp_path, "clone", "--quiet", str(origin), str(clone))
    _git(clone, "config", "user.name", "Retention Test")
    _git(clone, "config", "user.email", "retention@example.invalid")
    (clone / "a.txt").write_text("a\n", encoding="utf-8")
    _git(clone, "add", "--all")
    _git(clone, "commit", "--quiet", "-m", "first")
    _git(clone, "push", "--quiet", "origin", "master")
    return clone


def _push_snapshot(repo: Path, name: str, *, commit_age_days: float = 0.0) -> str:
    """A `refs/snapshots/<name>` ref on origin, backdated by committing with an
    explicit committer date -- `%(creatordate)` reads the commit, not the push."""
    sha = _git(repo, "rev-parse", "HEAD").strip()
    when = (datetime.now(UTC) - timedelta(days=commit_age_days)).isoformat()
    env = os.environ.copy()
    env["GIT_AUTHOR_DATE"] = when
    env["GIT_COMMITTER_DATE"] = when
    tree = _git(repo, "rev-parse", "HEAD^{tree}").strip()
    commit = subprocess.run(
        ["git", "commit-tree", tree, "-p", sha, "-m", f"snapshot {name}"],
        cwd=repo,
        env=env,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    ref = f"refs/snapshots/{name}"
    _git(repo, "update-ref", ref, commit)
    _git(repo, "push", "--quiet", "origin", ref)
    _git(repo, "update-ref", "-d", ref)  # keep the local checkout clean of it
    return commit


class TestListRemoteSnapshots:
    def test_empty_when_none_exist(self, repo: Path) -> None:
        assert sr.list_remote_snapshots(repo) == []

    def test_lists_a_pushed_snapshot_with_its_age(self, repo: Path) -> None:
        stamp = (datetime.now(UTC) - timedelta(days=5.0)).strftime(
            sr.SNAPSHOT_TIMESTAMP_FORMAT
        )
        commit = _push_snapshot(repo, f"a/{stamp}", commit_age_days=90.0)
        refs = sr.list_remote_snapshots(repo)
        assert len(refs) == 1
        assert refs[0].name == f"refs/snapshots/a/{stamp}"
        assert refs[0].sha == commit
        age_days = refs[0].age_days
        assert age_days is not None
        assert 4.9 < age_days < 5.1

    def test_old_commit_in_a_new_snapshot_is_kept(self, repo: Path) -> None:
        stamp = datetime.now(UTC).strftime(sr.SNAPSHOT_TIMESTAMP_FORMAT)
        _push_snapshot(repo, f"a/{stamp}", commit_age_days=90.0)
        refs = sr.list_remote_snapshots(repo)
        assert sr.expired(refs, ttl_days=21.0) == []

    def test_unknown_timestamp_is_kept_fail_closed(self, repo: Path) -> None:
        _push_snapshot(repo, "legacy/no-timestamp", commit_age_days=90.0)
        refs = sr.list_remote_snapshots(repo)
        assert refs[0].created_at is None
        assert refs[0].age_days is None
        assert sr.expired(refs, ttl_days=0.0) == []

    def test_local_only_snapshot_ref_is_not_the_durable_set(self, repo: Path) -> None:
        """A ref never pushed to origin is session-private litter, not evidence."""
        sha = _git(repo, "rev-parse", "HEAD").strip()
        _git(repo, "update-ref", "refs/snapshots/local-only/x", sha)
        assert sr.list_remote_snapshots(repo) == []


class TestExpired:
    def test_just_under_ttl_is_kept(self) -> None:
        ref = sr.SnapshotRef(
            name="r", sha="s", created_at=datetime.now(UTC) - timedelta(days=20.99)
        )
        assert sr.expired([ref], ttl_days=21.0) == []

    def test_just_over_ttl_is_expired(self) -> None:
        ref = sr.SnapshotRef(
            name="r", sha="s", created_at=datetime.now(UTC) - timedelta(days=21.01)
        )
        assert sr.expired([ref], ttl_days=21.0) == [ref]

    @pytest.mark.parametrize("ttl_days", [-1.0, float("inf"), float("nan")])
    def test_invalid_ttl_is_rejected(self, ttl_days: float) -> None:
        with pytest.raises(ValueError, match="finite and non-negative"):
            sr.expired([], ttl_days=ttl_days)


class TestDeleteRemoteSnapshots:
    def test_deletes_the_ref_on_origin(self, repo: Path, origin: Path) -> None:
        _push_snapshot(repo, "a/20260101T000000Z")
        [ref] = sr.list_remote_snapshots(repo)
        deleted = sr.delete_remote_snapshots(repo, [ref])
        assert deleted == [ref.name]
        assert (
            _git(origin, "for-each-ref", "--format=%(refname)", "refs/snapshots") == ""
        )

    def test_only_named_refs_are_deleted(self, repo: Path) -> None:
        _push_snapshot(repo, "keep/20260101T000000Z")
        _push_snapshot(repo, "drop/20260101T000000Z")
        refs = sr.list_remote_snapshots(repo)
        to_drop = [r for r in refs if "drop" in r.name]
        sr.delete_remote_snapshots(repo, to_drop)
        remaining = sr.list_remote_snapshots(repo)
        assert [r.name for r in remaining] == [r.name for r in refs if "keep" in r.name]

    def test_refuses_refs_heads_input(self, repo: Path, origin: Path) -> None:
        master_sha = _git(repo, "rev-parse", "master").strip()
        malicious = sr.SnapshotRef(
            name="refs/heads/master", sha=master_sha, created_at=datetime.now(UTC)
        )
        with pytest.raises(sr.SnapshotRetentionError, match="outside refs/snapshots"):
            sr.delete_remote_snapshots(repo, [malicious])
        assert "refs/heads/master" in _git(
            origin, "for-each-ref", "--format=%(refname)", "refs/heads"
        )

    def test_sha_lease_refuses_to_delete_a_moved_snapshot(self, repo: Path) -> None:
        _push_snapshot(repo, "a/20260101T000000Z")
        [stale] = sr.list_remote_snapshots(repo)
        new_sha = _git(repo, "rev-parse", "HEAD").strip()
        _git(repo, "push", "--force", "origin", f"{new_sha}:{stale.name}")
        assert sr.delete_remote_snapshots(repo, [stale]) == []
        [current] = sr.list_remote_snapshots(repo)
        assert current.sha == new_sha


class TestMainCli:
    def test_list_json(self, repo: Path, capsys: pytest.CaptureFixture[str]) -> None:
        _push_snapshot(repo, "a/20260101T000000Z")
        assert sr.main(["--repo", str(repo), "list", "--json"]) == 0
        assert "refs/snapshots/a/20260101T000000Z" in capsys.readouterr().out

    def test_expire_dry_run_deletes_nothing(self, repo: Path) -> None:
        _push_snapshot(repo, "a/20260101T000000Z")
        assert sr.main(["--repo", str(repo), "expire", "--ttl-days", "21"]) == 0
        assert len(sr.list_remote_snapshots(repo)) == 1

    def test_expire_apply_deletes_stale_refs(self, repo: Path) -> None:
        old_stamp = (datetime.now(UTC) - timedelta(days=30.0)).strftime(
            sr.SNAPSHOT_TIMESTAMP_FORMAT
        )
        new_stamp = datetime.now(UTC).strftime(sr.SNAPSHOT_TIMESTAMP_FORMAT)
        _push_snapshot(repo, f"old/{old_stamp}")
        _push_snapshot(repo, f"new/{new_stamp}")
        assert (
            sr.main(["--repo", str(repo), "expire", "--ttl-days", "21", "--apply"]) == 0
        )
        remaining = sr.list_remote_snapshots(repo)
        assert [r.name for r in remaining] == [f"refs/snapshots/new/{new_stamp}"]
