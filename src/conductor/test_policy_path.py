"""The policy path resolves from a flag, the environment, the repo, or the package.

Never from a cwd-relative literal: a standalone install reviews a foreign tree from a
foreign cwd. With a candidate tree the value is always candidate-relative and never
falls back to the working tree or the package, so a checkout cannot change a verdict.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from conductor.candidate_review.policy import PolicyError, load_policy
from conductor.candidate_review.policy_path import (
    DEFAULT_POLICY_RELATIVE,
    PACKAGE_POLICY,
    POLICY_ENV,
    enclosing_repo,
    resolve_policy_path,
)


@pytest.fixture(autouse=True)
def _clean_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv(POLICY_ENV, raising=False)


def _policy(root: Path, relative: str = DEFAULT_POLICY_RELATIVE.as_posix()) -> Path:
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("schema_version = 1\n", encoding="utf-8")
    return path


# ---------------------------------------------------------------------------
# Precedence without a tree
# ---------------------------------------------------------------------------


def test_explicit_flag_beats_environment(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    flag = _policy(tmp_path, "flag.toml")
    env = _policy(tmp_path, "env.toml")
    monkeypatch.setenv(POLICY_ENV, str(env))
    assert resolve_policy_path(flag) == flag
    assert resolve_policy_path(str(flag)) == flag


def test_environment_beats_the_default(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    env = _policy(tmp_path, "env.toml")
    _policy(tmp_path)
    monkeypatch.setenv(POLICY_ENV, str(env))
    monkeypatch.chdir(tmp_path)
    assert resolve_policy_path() == env


def test_empty_flag_and_blank_environment_mean_default(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    (tmp_path / ".git").mkdir()
    default = _policy(tmp_path)
    monkeypatch.setenv(POLICY_ENV, "   ")
    monkeypatch.chdir(tmp_path)
    assert resolve_policy_path("") == default


def test_default_is_the_enclosing_repo_from_a_subdirectory(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    (tmp_path / ".git").write_text("gitdir: elsewhere\n", encoding="utf-8")
    default = _policy(tmp_path)
    nested = tmp_path / "a" / "b"
    nested.mkdir(parents=True)
    monkeypatch.chdir(nested)
    assert resolve_policy_path() == default


def test_default_outside_any_repo_is_the_package_policy(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)
    assert (
        enclosing_repo(tmp_path.resolve()) is None
        or not (tmp_path / DEFAULT_POLICY_RELATIVE.as_posix()).exists()
    )
    assert resolve_policy_path() == PACKAGE_POLICY
    assert PACKAGE_POLICY.name == "candidate_policy.toml"
    assert PACKAGE_POLICY.parent.name == "conductor"


def test_repo_without_a_policy_falls_through_to_the_package(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    (tmp_path / ".git").mkdir()
    monkeypatch.chdir(tmp_path)
    assert resolve_policy_path() == PACKAGE_POLICY


def test_missing_explicit_or_environment_path_fails_loud(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    missing = tmp_path / "absent.toml"
    with pytest.raises(PolicyError, match=r"no candidate policy \(--policy\)"):
        resolve_policy_path(missing)
    monkeypatch.setenv(POLICY_ENV, str(missing))
    with pytest.raises(PolicyError, match=rf"\({POLICY_ENV}\); tried: {missing}"):
        resolve_policy_path()


# ---------------------------------------------------------------------------
# With a candidate tree: candidate-relative, never the package
# ---------------------------------------------------------------------------


def test_tree_default_is_joined_to_the_tree(tmp_path: Path) -> None:
    default = _policy(tmp_path)
    assert resolve_policy_path(tree=tmp_path) == default


def test_tree_explicit_and_environment_are_tree_relative(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    flag = _policy(tmp_path, "flag/policy.toml")
    env = _policy(tmp_path, "env/policy.toml")
    monkeypatch.setenv(POLICY_ENV, "env/policy.toml")
    assert resolve_policy_path(tree=tmp_path) == env
    assert resolve_policy_path("flag/policy.toml", tree=tmp_path) == flag


@pytest.mark.parametrize("raw", ["/etc/policy.toml", "../policy.toml", "a/../../p"])
def test_tree_rejects_paths_that_escape_the_candidate(tmp_path: Path, raw: str) -> None:
    _policy(tmp_path)
    with pytest.raises(PolicyError, match="must be candidate-relative"):
        resolve_policy_path(raw, tree=tmp_path)


def test_tree_without_a_policy_never_falls_back_to_the_package(
    tmp_path: Path,
) -> None:
    """The absent file is reported by load_policy; the package copy is never used."""
    assert PACKAGE_POLICY.is_file()
    resolved = resolve_policy_path(tree=tmp_path)
    assert resolved == tmp_path / DEFAULT_POLICY_RELATIVE.as_posix()
    assert not resolved.exists()
    with pytest.raises(PolicyError, match="required candidate policy is unreadable"):
        load_policy(resolved)


def test_enclosing_repo_stops_at_the_nearest_git_marker(tmp_path: Path) -> None:
    outer = tmp_path / "outer"
    inner = outer / "inner"
    (inner / "deep").mkdir(parents=True)
    (outer / ".git").mkdir()
    (inner / ".git").write_text("gitdir: x\n", encoding="utf-8")
    assert enclosing_repo(inner / "deep") == inner
    assert enclosing_repo(outer / "other") == outer
