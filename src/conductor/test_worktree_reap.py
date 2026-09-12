from __future__ import annotations

import json
import os
import subprocess
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from conductor import worktree_reap

GIT_ENV = {
    **os.environ,
    "GIT_AUTHOR_NAME": "Reap Test",
    "GIT_AUTHOR_EMAIL": "reap@example.invalid",
    "GIT_COMMITTER_NAME": "Reap Test",
    "GIT_COMMITTER_EMAIL": "reap@example.invalid",
    "GIT_CONFIG_GLOBAL": "/dev/null",
    "GIT_CONFIG_SYSTEM": "/dev/null",
}


def _git(cwd: Path, *args: str) -> str:
    done = subprocess.run(
        ["git", *args], cwd=cwd, env=GIT_ENV, capture_output=True, text=True, check=True
    )
    return done.stdout


def _porcelain(
    path: Path, head: str = "a" * 40, branch: str = "refs/heads/topic"
) -> str:
    return f"worktree {path}\nHEAD {head}\nbranch {branch}\n\n"


def _age(path: Path, hours: float) -> None:
    """Backdate every file in ``path`` so the idle probe sees an untouched tree."""
    when = datetime.now(UTC).timestamp() - hours * 3600
    for root, _dirs, files in os.walk(path):
        for name in files:
            target = Path(root) / name
            if target.is_symlink():
                continue
            os.utime(target, (when, when))


@pytest.fixture
def repo(tmp_path: Path) -> Path:
    """A primary checkout wired to a real ``origin`` with one commit on master."""
    origin = tmp_path / "origin.git"
    _git(tmp_path, "init", "--bare", "-b", "master", str(origin))
    primary = tmp_path / "primary"
    _git(tmp_path, "clone", str(origin), str(primary))
    (primary / "seed.txt").write_text("seed\n")
    _git(primary, "add", "seed.txt")
    _git(primary, "commit", "-m", "seed")
    _git(primary, "push", "origin", "master")
    return primary


def _worktree(repo: Path, name: str, branch: str) -> Path:
    path = repo.parent / name
    _git(repo, "worktree", "add", "-b", branch, str(path), "origin/master")
    return path


def _proc(tmp_path: Path, pid: str = "4242", cwd: Path | None = None) -> Path:
    root = tmp_path / f"proc-{pid}"
    (root / pid).mkdir(parents=True, exist_ok=True)
    if cwd is not None:
        os.symlink(cwd, root / pid / "cwd")
    return root


def _decide(repo: Path, **kwargs):
    kwargs.setdefault("proc_root", _proc(repo.parent / "empty-proc"))
    kwargs.setdefault("current", repo)
    return worktree_reap.decide(repo, **kwargs)


def _state(decisions, path: Path) -> worktree_reap.Decision:
    for decision in decisions:
        if decision.worktree.path.resolve() == path.resolve():
            return decision
    raise AssertionError(f"{path} missing from decisions")


def test_parse_worktrees_preserves_safety_markers(tmp_path):
    rows = worktree_reap.parse_worktrees(
        "worktree "
        + str(tmp_path / "one")
        + "\nHEAD "
        + "a" * 40
        + "\nlocked reason\n\n"
        + "worktree /gone\nHEAD "
        + "b" * 40
        + "\nprunable gitdir missing\n\n"
    )
    assert rows[0].head == "a" * 40
    assert rows[0].locked
    assert rows[1].head == "b" * 40
    assert rows[1].prunable and rows[1].missing


def test_parse_worktrees_strips_head_branch_prefix_and_bare_marker(tmp_path):
    (tmp_path / "one").mkdir()
    rows = worktree_reap.parse_worktrees(
        f"worktree {tmp_path / 'one'}\n"
        "HEAD aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n"
        "branch refs/heads/topic\n\n"
        "worktree /bare\n"
        "HEAD bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n"
        "bare\n\n"
    )
    assert rows[0].branch == "topic"
    assert not rows[0].missing
    assert rows[1].branch == ""
    assert rows[1].missing


def test_parse_worktrees_keeps_empty_head_and_boolean_marker_lines(tmp_path):
    path = tmp_path / "one"
    path.mkdir()
    rows = worktree_reap.parse_worktrees(
        f"worktree {path}\nbranch refs/heads/topic\nlocked\nprunable\nbare\n\n"
    )
    assert rows[0].head == ""
    assert rows[0].locked
    assert rows[0].prunable


def test_unreadable_processes_are_counted_not_treated_as_active(monkeypatch, tmp_path):
    """The bug that made the reaper a permanent no-op: EACCES must not block."""
    root = _proc(tmp_path, "7", cwd=tmp_path)
    real = os.readlink

    def denied(path, *args, **kwargs):
        if str(path).endswith("/7/cwd"):
            raise PermissionError(13, "Permission denied")
        return real(path, *args, **kwargs)

    monkeypatch.setattr(worktree_reap.os, "readlink", denied)
    found, unreadable = worktree_reap.active_process_cwds(tmp_path, root)
    assert found == []
    assert unreadable == 1


def test_live_process_cwd_is_still_reported(tmp_path):
    tree = tmp_path / "tree"
    tree.mkdir()
    found, unreadable = worktree_reap.active_process_cwds(
        tree, _proc(tmp_path, "9", tree)
    )
    assert found == [f"pid 9: {tree}"]
    assert unreadable == 0


def test_unlistable_proc_root_is_fatal(tmp_path):
    found, unreadable = worktree_reap.active_process_cwds(tmp_path, tmp_path / "absent")
    assert found and found[0].startswith("unknown process state: cannot inspect")
    assert unreadable == 0


def test_primary_checkout_is_never_eligible(repo):
    decisions = _decide(repo)
    primary = _state(decisions, repo)
    assert primary.state == worktree_reap.PRIMARY
    assert not primary.eligible


def test_live_process_inside_a_worktree_blocks_removal(repo, tmp_path):
    tree = _worktree(repo, "busy", "topic/busy")
    _age(tree, 48)
    decisions = worktree_reap.decide(
        repo, proc_root=_proc(tmp_path, "31337", tree), current=repo
    )
    decision = _state(decisions, tree)
    assert decision.state == worktree_reap.ACTIVE
    assert not decision.eligible


def test_a_process_cwd_deeper_inside_the_tree_also_blocks(repo, tmp_path):
    tree = _worktree(repo, "deep", "topic/deep")
    inner = tree / "research" / "reports"
    inner.mkdir(parents=True)
    _age(tree, 48)
    decisions = worktree_reap.decide(
        repo, proc_root=_proc(tmp_path, "31338", inner), current=repo
    )
    assert _state(decisions, tree).state == worktree_reap.ACTIVE


def test_locked_worktree_is_kept(repo):
    tree = _worktree(repo, "locked", "topic/locked")
    _git(repo, "worktree", "lock", str(tree))
    _age(tree, 48)
    decision = _state(_decide(repo), tree)
    assert decision.state == worktree_reap.LOCKED
    assert not decision.eligible


def test_current_directory_is_kept(repo):
    tree = _worktree(repo, "here", "topic/here")
    _age(tree, 48)
    decisions = worktree_reap.decide(
        repo, proc_root=_proc(repo.parent / "empty"), current=tree
    )
    assert _state(decisions, tree).state == worktree_reap.CURRENT


def test_live_lease_keeps_an_idle_worktree(repo):
    tree = _worktree(repo, "leased", "topic/leased")
    (tree / ".worktree-lease.json").write_text(
        json.dumps(
            {
                "schema": "worktree-lease.v1",
                "owner": "llm-b0",
                "purpose": "still running",
                "branch": "topic/leased",
                "worktree": str(tree),
                "opened_at": datetime.now(UTC).isoformat(),
                "expires_at": (datetime.now(UTC) + timedelta(hours=4)).isoformat(),
            }
        )
    )
    _age(tree, 48)
    decision = _state(_decide(repo), tree)
    assert decision.state == worktree_reap.LEASED
    assert not decision.eligible


def test_expired_lease_makes_a_dirty_unlanded_worktree_eligible(repo):
    """Dirty and unproven no longer protect a tree: the archive does."""
    tree = _worktree(repo, "expired", "topic/expired")
    (tree / "work.txt").write_text("uncommitted\n")
    _git(tree, "add", "work.txt")
    _git(tree, "commit", "-m", "unlanded work")
    (tree / "scratch.txt").write_text("dirty\n")
    (tree / ".worktree-lease.json").write_text(
        json.dumps(
            {
                "schema": "worktree-lease.v1",
                "owner": "llm-b0",
                "purpose": "done",
                "branch": "topic/expired",
                "worktree": str(tree),
                "opened_at": (datetime.now(UTC) - timedelta(hours=9)).isoformat(),
                "expires_at": (datetime.now(UTC) - timedelta(hours=1)).isoformat(),
            }
        )
    )
    decision = _state(_decide(repo), tree)
    assert decision.eligible
    assert decision.state == worktree_reap.REMOVE
    assert any("expired" in reason for reason in decision.reasons)


def test_idle_worktree_with_unlanded_commits_is_eligible(repo):
    tree = _worktree(repo, "idle", "topic/idle")
    (tree / "work.txt").write_text("unlanded\n")
    _git(tree, "add", "work.txt")
    _git(tree, "commit", "-m", "unlanded work")
    _age(tree, 48)
    decision = _state(_decide(repo, idle_hours=6.0), tree)
    assert decision.eligible
    assert any(reason.startswith("idle:") for reason in decision.reasons)


def test_busy_unlanded_worktree_without_a_lease_is_held(repo):
    tree = _worktree(repo, "busy-unlanded", "topic/busy-unlanded")
    (tree / "work.txt").write_text("fresh\n")
    _git(tree, "add", "work.txt")
    _git(tree, "commit", "-m", "unlanded work")
    decision = _state(_decide(repo, idle_hours=6.0), tree)
    assert not decision.eligible
    assert decision.state == worktree_reap.HELD
    assert decision.reasons == ["run is not over"]


def test_merged_head_is_eligible_even_while_busy(repo):
    tree = _worktree(repo, "merged", "topic/merged")
    (tree / "fresh.txt").write_text("modified just now\n")
    decision = _state(_decide(repo, idle_hours=6.0), tree)
    assert decision.eligible
    assert any("contained in origin/master" in reason for reason in decision.reasons)


def test_stale_registration_is_eligible_without_a_directory(repo):
    tree = _worktree(repo, "gone", "topic/gone")
    subprocess.run(["rm", "-rf", str(tree)], check=True)
    decision = _state(_decide(repo), tree)
    assert decision.eligible
    assert decision.reasons == ["stale registration: worktree directory is absent"]


def test_remote_branch_gone_needs_a_tracking_ref(repo):
    tree = _worktree(repo, "never-pushed", "topic/never-pushed")
    assert not worktree_reap._remote_branch_gone(repo, "topic/never-pushed")
    _git(tree, "push", "-u", "origin", "topic/never-pushed")
    assert not worktree_reap._remote_branch_gone(repo, "topic/never-pushed")
    _git(repo, "push", "origin", "--delete", "topic/never-pushed")
    assert worktree_reap._remote_branch_gone(repo, "topic/never-pushed")


def test_idle_probe_failure_is_not_read_as_idle(tmp_path):
    assert worktree_reap.recent_change(tmp_path / "absent", 6.0) is not None


def test_idle_probe_ignores_the_hardlinked_venv(repo):
    tree = _worktree(repo, "venv-tree", "topic/venv-tree")
    _age(tree, 48)
    venv = tree / ".venv" / "bin"
    venv.mkdir(parents=True)
    (venv / "python").write_text("fresh\n")
    assert worktree_reap.recent_change(tree, 6.0) is None


def test_archive_preserves_patches_diff_status_and_moves_artifacts(repo, tmp_path):
    tree = _worktree(repo, "archive-me", "topic/archive-me")
    (tree / "landed.txt").write_text("committed\n")
    _git(tree, "add", "landed.txt")
    _git(tree, "commit", "-m", "unlanded commit")
    (tree / "landed.txt").write_text("committed then edited\n")
    (tree / "untracked.txt").write_text("untracked\n")
    reports = tree / "research" / "reports"
    reports.mkdir(parents=True)
    (reports / "run.json").write_text("{}\n")
    (tree / "model.pt").write_bytes(b"weights")
    row = _state(_decide(repo), tree).worktree
    record = worktree_reap.archive_worktree(
        repo,
        row,
        integration_ref="origin/master",
        archive_root=tmp_path / "archive",
        checkpoint_root=tmp_path / "ckpt",
    )
    dest = Path(str(record["archive"]))
    assert list(dest.glob("0001-*.patch"))
    assert "committed then edited" in (dest / "worktree.diff").read_text()
    assert "untracked.txt" in (dest / "status.txt").read_text()
    moved = [Path(p) for p in record["moved_artifacts"]]  # type: ignore[union-attr]
    assert {p.name for p in moved} == {"run.json", "model.pt"}
    assert all(p.exists() for p in moved)
    assert not (tree / "model.pt").exists()
    assert not (reports / "run.json").exists()


def test_apply_force_removes_a_dirty_tree_and_deletes_the_branch(repo, tmp_path):
    tree = _worktree(repo, "reap-me", "topic/reap-me")
    (tree / "dirty.txt").write_text("uncommitted\n")
    _age(tree, 48)
    decisions = _decide(repo, idle_hours=6.0)
    removed = worktree_reap.apply(
        repo,
        decisions,
        archive_root=tmp_path / "archive",
        checkpoint_root=tmp_path / "ckpt",
        delete_remote=False,
        current=repo,
        proc_root=_proc(repo.parent / "empty-proc"),
    )
    assert [record["worktree"] for record in removed] == [str(tree)]
    assert not tree.exists()
    assert "topic/reap-me" not in _git(repo, "branch", "--list", "topic/reap-me")
    assert "reap-me" not in _git(repo, "worktree", "list")


def test_apply_refuses_a_tree_that_became_active_after_the_decision(repo, tmp_path):
    tree = _worktree(repo, "raced", "topic/raced")
    _age(tree, 48)
    decisions = _decide(repo, idle_hours=6.0)
    assert _state(decisions, tree).eligible
    with pytest.raises(worktree_reap.ReapError, match="refusing"):
        worktree_reap.apply(
            repo,
            decisions,
            archive_root=tmp_path / "archive",
            checkpoint_root=tmp_path / "ckpt",
            delete_remote=False,
            current=repo,
            proc_root=_proc(tmp_path, "999", tree),
        )
    assert tree.exists()


def test_apply_never_touches_an_ineligible_tree(repo, tmp_path):
    tree = _worktree(repo, "kept", "topic/kept")
    (tree / "fresh.txt").write_text("fresh\n")
    _git(tree, "add", "fresh.txt")
    _git(tree, "commit", "-m", "unlanded")
    decisions = _decide(repo, idle_hours=6.0)
    assert not _state(decisions, tree).eligible
    removed = worktree_reap.apply(
        repo,
        decisions,
        archive_root=tmp_path / "archive",
        checkpoint_root=tmp_path / "ckpt",
        delete_remote=False,
        current=repo,
        proc_root=_proc(repo.parent / "empty-proc"),
    )
    assert removed == []
    assert tree.exists()


def test_second_apply_is_refused_while_one_holds_the_lock(repo, tmp_path, capsys):
    handle = worktree_reap._hold_lock(repo)
    assert handle is not None
    roots = [
        "--archive-root",
        str(tmp_path / "archive"),
        "--checkpoint-root",
        str(tmp_path / "ckpt"),
    ]
    try:
        assert worktree_reap.main(["--repo", str(repo), "--apply", *roots]) == 0
    finally:
        handle.close()
    assert "another reap is running" in capsys.readouterr().err


def test_apply_refuses_to_run_with_nowhere_to_archive(repo, monkeypatch, capsys):
    """The data volume's path is the machine's, not this module's -- so it must be given."""
    monkeypatch.delenv(worktree_reap.ARCHIVE_ROOT_ENV, raising=False)
    monkeypatch.delenv(worktree_reap.CHECKPOINT_ROOT_ENV, raising=False)
    assert worktree_reap.main(["--repo", str(repo), "--apply"]) == 2
    assert "needs somewhere to archive to" in capsys.readouterr().err


def test_apply_roots_come_from_the_environment(repo, tmp_path, monkeypatch):
    monkeypatch.setenv(worktree_reap.ARCHIVE_ROOT_ENV, str(tmp_path / "archive"))
    monkeypatch.setenv(worktree_reap.CHECKPOINT_ROOT_ENV, str(tmp_path / "ckpt"))
    tree = _worktree(repo, "env-roots", "topic/env-roots")
    (tree / "work.txt").write_text("unlanded\n")
    _git(tree, "add", "work.txt")
    _git(tree, "commit", "-m", "unlanded work")
    _age(tree, 48)
    assert worktree_reap.main(["--repo", str(repo), "--apply"]) == 0
    assert not tree.exists()
    assert list((tmp_path / "archive" / "topic-env-roots").glob("0001-*.patch"))


def test_main_is_preview_by_default_and_reports_state(repo, capsys):
    tree = _worktree(repo, "preview", "topic/preview")
    assert worktree_reap.main(["--repo", str(repo), "--json"]) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload["dry_run"] is True
    assert payload["removed"] == []
    states = {row["worktree"]: row["state"] for row in payload["decisions"]}
    assert states[str(repo)] == worktree_reap.PRIMARY
    assert str(tree) in states


def test_main_text_output_names_the_state_and_the_dry_run(repo, capsys):
    _worktree(repo, "text", "topic/text")
    assert worktree_reap.main(["--repo", str(repo)]) == 0
    out = capsys.readouterr().out
    assert "PRIMARY" in out
    assert "pass --apply" in out


def test_slug_is_filesystem_safe(tmp_path):
    row = worktree_reap.Worktree(tmp_path, "a" * 40, "llm-b0/trident2-sdsm-r9")
    assert worktree_reap.slug_for(row) == "llm-b0-trident2-sdsm-r9"
    assert worktree_reap.slug_for(worktree_reap.Worktree(tmp_path / "plain")) == "plain"
