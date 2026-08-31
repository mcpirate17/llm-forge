"""Tests for `conductor.snapshot_exposure` -- the missing consumer of
`workspace_hygiene.stale_feature_branches`.

Every repo pair is throwaway: a bare `origin` under `tmp_path` plus a real clone, so
the push side effect this module exists to provide is exercised for real rather than
simulated with a fake `refs/remotes/origin/*` ref (that convention is right for
`workspace_hygiene`'s tests, which only ever read; this module's whole job is to
write to the remote).
"""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from conductor import snapshot_exposure as se


def _git(repo: Path, *args: str) -> str:
    completed = subprocess.run(
        ["git", *args], cwd=repo, capture_output=True, text=True, check=False
    )
    if completed.returncode != 0:
        pytest.fail(f"git {' '.join(args)} failed: {completed.stderr.strip()}")
    return completed.stdout


def _commit(repo: Path, name: str) -> str:
    (repo / name).write_text(f"{name}\n", encoding="utf-8")
    _git(repo, "add", "--all")
    _git(repo, "commit", "--quiet", "-m", f"add {name}")
    return _git(repo, "rev-parse", "HEAD").strip()


@pytest.fixture
def origin(tmp_path: Path) -> Path:
    bare = tmp_path / "origin.git"
    bare.mkdir()
    _git(bare, "init", "--quiet", "--bare", "--initial-branch=master")
    return bare


@pytest.fixture
def repo(tmp_path: Path, origin: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """A clone with `master` pushed (the live/integration branch) and an unpushed
    feature branch -- EXPOSED because it was never pushed at all."""
    clone = tmp_path / "clone"
    _git(tmp_path, "clone", "--quiet", str(origin), str(clone))
    _git(clone, "config", "user.name", "Snapshot Test")
    _git(clone, "config", "user.email", "snapshot@example.invalid")
    _commit(clone, "a.txt")
    _git(clone, "push", "--quiet", "origin", "master")
    _git(clone, "checkout", "--quiet", "-b", "claude/topic-20260829")
    _commit(clone, "b.txt")
    _git(clone, "checkout", "--quiet", "master")
    # gh absent (or unauthenticated) must never be silently read as "no PR" -- see
    # workspace_hygiene's own module note. Keep the fixture's exposure reason to
    # "never pushed" only.
    monkeypatch.setattr(se.workspace_hygiene.shutil, "which", lambda _name: None)
    return clone


def _remote_ref_shas(origin: Path, pattern: str) -> dict[str, str]:
    out = _git(origin, "for-each-ref", "--format=%(refname) %(objectname)", pattern)
    rows = (line.split(" ", 1) for line in out.splitlines() if line.strip())
    return dict(rows)


class TestSnapshotStaleBranches:
    def test_creates_and_pushes_a_ref_at_the_branch_tip(
        self, repo: Path, origin: Path
    ) -> None:
        tip = _git(repo, "rev-parse", "claude/topic-20260829").strip()
        actions = se.snapshot_stale_branches(repo, stamp="20260830T000000Z")
        assert len(actions) == 1
        action = actions[0]
        assert action.branch == "claude/topic-20260829"
        assert action.sha == tip
        assert (
            action.ref
            == "refs/snapshots/branches/claude/topic-20260829/20260830T000000Z"
        )
        assert action.created is True
        assert action.pushed is True
        assert _remote_ref_shas(origin, "refs/snapshots")[action.ref] == tip

    def test_no_push_leaves_the_remote_untouched(
        self, repo: Path, origin: Path
    ) -> None:
        actions = se.snapshot_stale_branches(repo, push=False, stamp="20260830T000000Z")
        assert actions[0].created is True
        assert actions[0].pushed is False
        assert _remote_ref_shas(origin, "refs/snapshots") == {}
        # but the local ref still exists -- a same-machine safety net even unpushed
        assert se._git(repo, "cat-file", "-t", actions[0].ref).strip() == "commit"

    def test_a_later_real_run_pushes_a_ref_a_prior_no_push_run_only_created_locally(
        self, repo: Path, origin: Path
    ) -> None:
        """The gap this closes: --no-push must not make a later real run think the
        branch is already durable just because a local ref happens to exist."""
        first = se.snapshot_stale_branches(repo, push=False, stamp="20260830T000000Z")
        second = se.snapshot_stale_branches(repo, stamp="20260830T111111Z")
        assert second[0].ref == first[0].ref  # reused, not duplicated
        assert second[0].created is False
        assert second[0].pushed is True
        assert _remote_ref_shas(origin, "refs/snapshots")[second[0].ref] == first[0].sha

    def test_idempotent_when_already_pushed_at_the_current_tip(
        self, repo: Path
    ) -> None:
        first = se.snapshot_stale_branches(repo, stamp="20260830T000000Z")
        second = se.snapshot_stale_branches(repo, stamp="20260830T111111Z")
        assert first[0].created is True
        assert second[0].created is False
        assert second[0].ref == first[0].ref
        assert second[0].pushed is True
        assert second[0].sha == first[0].sha

    def test_new_snapshot_when_the_branch_tip_moved(self, repo: Path) -> None:
        first = se.snapshot_stale_branches(repo, stamp="20260830T000000Z")
        _git(repo, "checkout", "--quiet", "claude/topic-20260829")
        _commit(repo, "c.txt")
        _git(repo, "checkout", "--quiet", "master")
        second = se.snapshot_stale_branches(repo, stamp="20260830T111111Z")
        assert second[0].created is True
        assert second[0].ref != first[0].ref
        assert second[0].sha != first[0].sha

    def test_push_failure_is_reported_not_raised(self, repo: Path) -> None:
        actions = se.snapshot_stale_branches(
            repo, remote="does-not-exist", stamp="20260830T000000Z"
        )
        assert actions[0].created is True
        assert actions[0].pushed is False

    def test_no_exposed_branches_is_empty(self, repo: Path) -> None:
        _git(repo, "push", "--quiet", "origin", "claude/topic-20260829")
        from datetime import UTC, datetime

        from conductor import branch_policy as bp

        bp.bind_branch(
            repo, branch="claude/topic-20260829", claim_id="c1", owner="claude"
        )
        bp.record_push(repo, branch="claude/topic-20260829", when=datetime.now(UTC))
        assert se.snapshot_stale_branches(repo) == []


class TestMainCli:
    def test_json_output_reports_pushed_ref(
        self, repo: Path, capsys: pytest.CaptureFixture[str]
    ) -> None:
        exit_code = se.main(["--repo", str(repo), "--json"])
        assert exit_code == 0
        out = capsys.readouterr().out
        assert "claude/topic-20260829" in out
        assert '"pushed": true' in out

    def test_nonzero_exit_when_push_fails(self, repo: Path) -> None:
        assert se.main(["--repo", str(repo), "--remote", "does-not-exist"]) == 1

    def test_zero_exit_with_no_push(self, repo: Path) -> None:
        assert (
            se.main(["--repo", str(repo), "--remote", "does-not-exist", "--no-push"])
            == 0
        )
