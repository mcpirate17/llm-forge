from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from conductor import dead_tests


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


def _commit_broken_test(repo: Path) -> None:
    # "pkg" must be a tracked first-party directory for the missing submodule
    # import to register as `broken` rather than being ignored as third-party.
    _write(repo, "pkg/__init__.py", "")
    _write(repo, "test_probe.py", "import pkg.missing_module\n")
    _git(repo, "add", "--all")
    _git(repo, "commit", "-m", "base")


def test_explicit_root_scans_the_named_repo_not_cwd(tmp_path: Path) -> None:
    target = tmp_path / "target"
    decoy = tmp_path / "decoy"
    _init_repo(target)
    _init_repo(decoy)
    _commit_broken_test(target)

    assert dead_tests.main(["--root", str(target), "--check"]) == 1


def test_default_root_uses_cwd_toplevel_not_module_location(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The bug: Path(__file__)-derived resolution always points at the
    checkout that supplied the imported module. A broken test that exists
    only in this throwaway repo is invisible under that resolution; finding
    it here proves the tool followed cwd instead.
    """
    repo = tmp_path / "repo"
    _init_repo(repo)
    _commit_broken_test(repo)

    monkeypatch.chdir(repo)
    assert dead_tests.main(["--check"]) == 1


def test_cwd_outside_worktree_refuses_rather_than_falling_back(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    outside = tmp_path / "not_a_repo"
    outside.mkdir()
    monkeypatch.chdir(outside)

    assert dead_tests.main([]) == 2


def test_resolved_root_is_printed(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    repo = tmp_path / "repo"
    _init_repo(repo)
    _git(repo, "commit", "--allow-empty", "-m", "base")

    assert dead_tests.main(["--root", str(repo), "--json-out", "out.json"]) == 0
    out = capsys.readouterr().out
    assert f"root={repo.resolve()}" in out
    assert (repo / "out.json").is_file()


def test_root_mismatch_warns(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    target = tmp_path / "target"
    decoy = tmp_path / "decoy"
    _init_repo(target)
    _init_repo(decoy)
    _git(target, "commit", "--allow-empty", "-m", "base")

    monkeypatch.chdir(decoy)
    assert dead_tests.main(["--root", str(target)]) == 0
    err = capsys.readouterr().err
    assert "WARNING" in err
    assert str(target.resolve()) in err


def test_json_out_relative_path_resolves_against_root(tmp_path: Path) -> None:
    target = tmp_path / "target"
    _init_repo(target)
    _git(target, "commit", "--allow-empty", "-m", "base")

    assert dead_tests.main(["--root", str(target)]) == 0
    assert (target / "tasks" / "audit" / "dead_tests.json").is_file()


def test_resolver_resolve_untracked_honours_explicit_root(tmp_path: Path) -> None:
    target = tmp_path / "target"
    decoy = tmp_path / "decoy"
    _init_repo(target)
    _init_repo(decoy)
    _write(decoy, "pkg/helper.py", "value = 1\n")
    resolver = dead_tests.Resolver(["main.py"], root=target)

    # helper.py exists on disk under decoy, not target: the resolver must not
    # find it when scoped to target -- proof it uses the passed root, not
    # some other tree.
    assert resolver.resolve_untracked("pkg.helper", "main.py") is None

    _write(target, "pkg/helper.py", "value = 1\n")
    assert resolver.resolve_untracked("pkg.helper", "main.py") == "pkg/helper.py"
