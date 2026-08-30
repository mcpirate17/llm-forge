from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from conductor import check_protected_deletes


def _git(repo: Path, *args: str) -> None:
    subprocess.run(["git", *args], cwd=repo, check=True, capture_output=True)


def _init_repo(repo: Path) -> None:
    repo.mkdir(parents=True, exist_ok=True)
    _git(repo, "init", "-b", "main")
    _git(repo, "config", "user.email", "governance-tests@example.invalid")
    _git(repo, "config", "user.name", "Governance Tests")
    _git(repo, "config", "commit.gpgsign", "false")


def _write(repo: Path, relative: str, content: str) -> None:
    path = repo / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def test_explicit_root_scans_the_named_repo_not_cwd(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    target = tmp_path / "target"
    decoy = tmp_path / "decoy"
    _init_repo(target)
    _init_repo(decoy)

    protected = "research/runtime/champion_example.json"
    _write(target, protected, "{}\n")
    _git(target, "add", "--all")
    _git(target, "commit", "-m", "base")
    (target / protected).unlink()
    _git(target, "add", "--update")

    monkeypatch.chdir(decoy)
    assert check_protected_deletes.main(["--root", str(target)]) == 1


def test_default_root_uses_cwd_toplevel_not_module_location(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The bug: resolving from Path(__file__) instead of cwd would silently
    scan the checkout that supplied the imported module (here, the module's
    real repo) instead of this throwaway repo the test builds. Asserting a
    clean exit here would pass by accident if the real repo also happened to
    have no staged deletions, so the meaningful assertion is that a deletion
    staged ONLY in this throwaway repo is found -- proof the tool followed cwd.
    """
    repo = tmp_path / "repo"
    _init_repo(repo)
    protected = "research/runtime/champion_example.json"
    _write(repo, protected, "{}\n")
    _git(repo, "add", "--all")
    _git(repo, "commit", "-m", "base")
    (repo / protected).unlink()
    _git(repo, "add", "--update")

    monkeypatch.chdir(repo)
    assert check_protected_deletes.main([]) == 1
    assert check_protected_deletes.ROOT == repo.resolve()


def test_cwd_outside_worktree_refuses_rather_than_falling_back(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    outside = tmp_path / "not_a_repo"
    outside.mkdir()
    monkeypatch.chdir(outside)

    assert check_protected_deletes.main([]) == 2


def test_resolved_root_is_printed(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    repo = tmp_path / "repo"
    _init_repo(repo)
    _git(repo, "commit", "--allow-empty", "-m", "base")
    monkeypatch.chdir(repo)

    assert check_protected_deletes.main([]) == 0
    out = capsys.readouterr().out
    assert f"root={repo.resolve()}" in out


def test_root_mismatch_warns(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    target = tmp_path / "target"
    decoy = tmp_path / "decoy"
    _init_repo(target)
    _init_repo(decoy)
    _git(target, "commit", "--allow-empty", "-m", "base")

    monkeypatch.chdir(decoy)
    assert check_protected_deletes.main(["--root", str(target)]) == 0
    err = capsys.readouterr().err
    assert "WARNING" in err
    assert str(target.resolve()) in err
