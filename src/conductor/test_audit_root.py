from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from conductor import audit_root


def _git(repo: Path, *args: str) -> None:
    subprocess.run(
        ["git", *args],
        cwd=repo,
        check=True,
        capture_output=True,
    )


def _init_repo(repo: Path) -> None:
    repo.mkdir(parents=True, exist_ok=True)
    _git(repo, "init", "-b", "main")
    _git(repo, "config", "user.email", "governance-tests@example.invalid")
    _git(repo, "config", "user.name", "Governance Tests")
    _git(repo, "config", "commit.gpgsign", "false")
    (repo / "marker.txt").write_text("x\n", encoding="utf-8")
    _git(repo, "add", "marker.txt")
    _git(repo, "commit", "-m", "init")


def test_explicit_root_honoured_over_cwd(tmp_path: Path) -> None:
    real_repo = tmp_path / "real"
    other_repo = tmp_path / "other"
    _init_repo(real_repo)
    _init_repo(other_repo)

    resolved = audit_root.resolve_audit_root(str(other_repo), cwd=real_repo)

    assert resolved == other_repo.resolve()


def test_default_resolution_uses_cwd_toplevel_not_a_nested_dir(tmp_path: Path) -> None:
    repo = tmp_path / "repo"
    _init_repo(repo)
    nested = repo / "a" / "b"
    nested.mkdir(parents=True)

    resolved = audit_root.resolve_audit_root(None, cwd=nested)

    assert resolved == repo.resolve()


def test_cwd_outside_worktree_refuses_without_explicit_root(tmp_path: Path) -> None:
    outside = tmp_path / "not_a_repo"
    outside.mkdir()

    with pytest.raises(audit_root.AuditRootError, match="not inside a Git worktree"):
        audit_root.resolve_audit_root(None, cwd=outside)


def test_explicit_root_must_exist(tmp_path: Path) -> None:
    repo = tmp_path / "repo"
    _init_repo(repo)

    with pytest.raises(audit_root.AuditRootError, match="does not exist"):
        audit_root.resolve_audit_root(str(tmp_path / "missing"), cwd=repo)


def test_resolved_root_is_printed(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    repo = tmp_path / "repo"
    _init_repo(repo)

    audit_root.print_audit_provenance("demo-tool", repo.resolve(), cwd=repo)

    out = capsys.readouterr().out
    assert f"root={repo.resolve()}" in out
    assert "git-head=" in out


def test_mismatch_between_root_and_cwd_toplevel_warns(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    standing_in = tmp_path / "standing_in"
    elsewhere = tmp_path / "elsewhere"
    _init_repo(standing_in)
    _init_repo(elsewhere)

    audit_root.print_audit_provenance("demo-tool", elsewhere.resolve(), cwd=standing_in)

    err = capsys.readouterr().err
    assert "WARNING" in err
    assert str(elsewhere.resolve()) in err
    assert str(standing_in.resolve()) in err


def test_no_warning_when_root_matches_cwd_toplevel(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    repo = tmp_path / "repo"
    _init_repo(repo)

    audit_root.print_audit_provenance("demo-tool", repo.resolve(), cwd=repo)

    assert capsys.readouterr().err == ""
