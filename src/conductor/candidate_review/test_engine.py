"""Paired tests for engine.py's self-integrity hashing.

`engine-integrity` is the check that refuses a candidate whose tree does not carry
the governance engine reviewing it. It therefore has to find that engine twice --
once in the candidate snapshot and once around the running package -- and both
lookups used to assume the monorepo's root-level ``conductor/``. Under any other
layout the candidate side found nothing and the check fired CRITICAL on every
review, which is the one failure mode a self-integrity check must not have.
"""

from __future__ import annotations

import json
from pathlib import Path


import pytest

from conductor.candidate_review import engine as review_engine
from conductor.candidate_review.engine import (
    _engine_findings,
    _installed_engine_commit,
    _package_hash,
    _pinned_engine_commit,
)
from conductor.project_paths import package_path, package_tree_root

SRC_LAYOUT = '[tool.conductor]\npackage_root = "src/conductor"\n'


def _engine_tree(root: Path, relative: str, bodies: dict[str, str]) -> Path:
    package = root / relative / "candidate_review"
    package.mkdir(parents=True)
    for name, body in bodies.items():
        (package / name).write_text(body, encoding="utf-8")
    return package


def test_absent_engine_is_an_empty_hash(tmp_path: Path) -> None:
    (tmp_path / "pyproject.toml").write_text(SRC_LAYOUT, encoding="utf-8")
    assert _package_hash(tmp_path) == ("", {})


def test_engine_under_a_declared_src_layout_is_found(tmp_path: Path) -> None:
    (tmp_path / "pyproject.toml").write_text(SRC_LAYOUT, encoding="utf-8")
    _engine_tree(tmp_path, "src/conductor", {"engine.py": "x = 1\n"})
    digest, files = _package_hash(tmp_path)
    assert digest
    assert list(files) == ["candidate_review/engine.py"]


def test_identical_sources_hash_alike_across_layouts(tmp_path: Path) -> None:
    """Layout is not part of engine identity: the same sources are the same engine."""
    flat = tmp_path / "flat"
    src = tmp_path / "src-layout"
    flat.mkdir()
    src.mkdir()
    (src / "pyproject.toml").write_text(SRC_LAYOUT, encoding="utf-8")
    _engine_tree(flat, "conductor", {"engine.py": "x = 1\n"})
    _engine_tree(src, "src/conductor", {"engine.py": "x = 1\n"})
    assert _package_hash(flat) == _package_hash(src)


def test_engine_under_the_unconfigured_default_is_found(tmp_path: Path) -> None:
    """No configuration is the monorepo, and it keeps answering as before."""
    _engine_tree(tmp_path, "conductor", {"engine.py": "x = 1\n"})
    digest, files = _package_hash(tmp_path)
    assert digest
    assert list(files) == ["candidate_review/engine.py"]


def test_a_root_level_package_is_not_found_under_a_src_declaration(
    tmp_path: Path,
) -> None:
    """The declaration is obeyed, not guessed around: a stray copy is not the engine."""
    (tmp_path / "pyproject.toml").write_text(SRC_LAYOUT, encoding="utf-8")
    _engine_tree(tmp_path, "conductor", {"engine.py": "x = 1\n"})
    assert _package_hash(tmp_path) == ("", {})


def test_only_python_sources_are_hashed(tmp_path: Path) -> None:
    (tmp_path / "pyproject.toml").write_text(SRC_LAYOUT, encoding="utf-8")
    package = _engine_tree(tmp_path, "src/conductor", {"engine.py": "x = 1\n"})
    (package / "notes.md").write_text("prose\n", encoding="utf-8")
    (package / "nested").mkdir()
    (package / "nested" / "deep.py").write_text("y = 2\n", encoding="utf-8")
    _, files = _package_hash(tmp_path)
    assert list(files) == ["candidate_review/engine.py"]


def test_a_changed_source_changes_the_hash(tmp_path: Path) -> None:
    (tmp_path / "pyproject.toml").write_text(SRC_LAYOUT, encoding="utf-8")
    package = _engine_tree(tmp_path, "src/conductor", {"engine.py": "x = 1\n"})
    before, _ = _package_hash(tmp_path)
    (package / "engine.py").write_text("x = 2\n", encoding="utf-8")
    after, _ = _package_hash(tmp_path)
    assert before != after


def test_the_runtime_root_hashes_this_checkout_s_engine() -> None:
    """What `_engine_integrity` compares the candidate against: a non-empty hash.

    The old fixed ``parents[2]`` walk landed on ``src/`` here, and the runtime side
    went empty too -- two absent engines comparing equal is a check that passes by
    seeing nothing.
    """
    runtime_root = package_tree_root(Path(__file__).resolve().parents[1])
    digest, files = _package_hash(runtime_root)
    assert digest
    assert "candidate_review/engine.py" in files
    assert package_path(runtime_root).is_dir()


# -- a consumer host: the engine is installed, the tree carries none of its sources --

FULL_SHA = "5ca892e0db6cf853b7b46be098c63032849e3cfa"
OTHER_SHA = "591f1986b1f1ce2c7f0d4d0d7c4b1c0c3d2e1f00"
LOCK_GIT_SOURCE = (
    '[[package]]\nname = "conductor-tooling"\nversion = "0.1.0"\n'
    'source = { git = "https://github.com/mcpirate17/llm-forge?rev=5ca892e#%s" }\n'
)


class _Distribution:
    def __init__(self, direct_url: str | None) -> None:
        self._direct_url = direct_url

    def read_text(self, name: str) -> str | None:
        assert name == "direct_url.json"
        return self._direct_url


def _install_from(monkeypatch: pytest.MonkeyPatch, direct_url: str | None) -> None:
    monkeypatch.setattr(
        review_engine.importlib_metadata,
        "distribution",
        lambda name: _Distribution(direct_url),
    )


def test_a_data_only_package_directory_is_an_absent_engine(tmp_path: Path) -> None:
    """The host keeps its grandfathered-nodeid list under the package path; that is
    data the installed engine reads, not a modified copy of the engine."""
    package = tmp_path / "conductor" / "candidate_review"
    package.mkdir(parents=True)
    (package / "grandfathered_test_nodeids_61343f57.json").write_text(
        "[]\n", encoding="utf-8"
    )
    assert _package_hash(tmp_path) == ("", {})


def test_pinned_commit_is_the_lock_fragment(tmp_path: Path) -> None:
    (tmp_path / "uv.lock").write_text(LOCK_GIT_SOURCE % FULL_SHA, encoding="utf-8")
    assert _pinned_engine_commit(tmp_path) == FULL_SHA


@pytest.mark.parametrize(
    "lock",
    [
        None,
        "",
        '[[package]]\nname = "requests"\nversion = "2.0"\nsource = { registry = "x" }\n',
        '[[package]]\nname = "conductor-tooling"\nsource = { editable = "../llm-forge" }\n',
        '[[package]]\nname = "conductor-tooling"\nsource = { git = "https://x/y?rev=abc" }\n',
        '[[package]]\nname = "conductor-tooling"\nsource = { git = "https://x/y#notasha" }\n',
        "this is not toml [[[",
    ],
    ids=[
        "no-lock",
        "empty",
        "other-package",
        "editable",
        "no-fragment",
        "bad-fragment",
        "bad-toml",
    ],
)
def test_pinned_commit_is_none_without_an_exact_git_pin(
    tmp_path: Path, lock: str | None
) -> None:
    if lock is not None:
        (tmp_path / "uv.lock").write_text(lock, encoding="utf-8")
    assert _pinned_engine_commit(tmp_path) is None


def test_installed_commit_comes_from_the_distribution_provenance(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _install_from(
        monkeypatch,
        json.dumps(
            {
                "url": "https://github.com/mcpirate17/llm-forge",
                "vcs_info": {
                    "vcs": "git",
                    "commit_id": FULL_SHA,
                    "requested_revision": "5ca892e",
                },
            }
        ),
    )
    assert _installed_engine_commit() == FULL_SHA


@pytest.mark.parametrize(
    "direct_url",
    [
        None,
        json.dumps({"url": "file:///home/x/llm-forge", "dir_info": {"editable": True}}),
        json.dumps({"url": "https://x", "vcs_info": {"vcs": "git"}}),
        "{not json",
    ],
    ids=["no-provenance", "editable-path-install", "no-commit", "bad-json"],
)
def test_installed_commit_is_none_without_git_provenance(
    monkeypatch: pytest.MonkeyPatch, direct_url: str | None
) -> None:
    _install_from(monkeypatch, direct_url)
    assert _installed_engine_commit() is None


def test_installed_commit_is_none_when_the_distribution_is_missing(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def _missing(name: str) -> _Distribution:
        raise review_engine.importlib_metadata.PackageNotFoundError(name)

    monkeypatch.setattr(review_engine.importlib_metadata, "distribution", _missing)
    assert _installed_engine_commit() is None


def _rules(findings: list) -> list[str]:
    return [finding.rule_id for finding in findings]


def test_in_tree_engine_passes_only_on_an_exact_hash_match() -> None:
    assert _engine_findings("h", "h", None, None) == []
    assert _rules(_engine_findings("h", "other", None, None)) == ["dirty-engine-source"]


def test_in_tree_sources_outrank_installed_provenance() -> None:
    """A tree that carries the engine is judged by its bytes, whatever the venv says."""
    assert _rules(_engine_findings("h", "other", FULL_SHA, FULL_SHA)) == [
        "dirty-engine-source"
    ]
    assert _engine_findings("h", "h", FULL_SHA, OTHER_SHA) == []


def test_installed_engine_passes_when_it_is_the_pinned_commit() -> None:
    assert _engine_findings("", "runtime", FULL_SHA, FULL_SHA) == []


def test_installed_engine_that_is_not_the_pinned_commit_is_critical() -> None:
    findings = _engine_findings("", "runtime", FULL_SHA, OTHER_SHA)
    assert _rules(findings) == ["engine-pin-mismatch"]
    assert findings[0].severity is review_engine.Severity.CRITICAL
    assert FULL_SHA[:12] in findings[0].message
    assert OTHER_SHA[:12] in findings[0].message


@pytest.mark.parametrize(
    ("installed", "pinned"),
    [(None, None), (FULL_SHA, None), (None, FULL_SHA)],
    ids=["neither", "installed-only", "pinned-only"],
)
def test_no_sources_and_no_matching_pair_of_commits_is_an_absent_engine(
    installed: str | None, pinned: str | None
) -> None:
    findings = _engine_findings("", "runtime", installed, pinned)
    assert _rules(findings) == ["engine-absent-from-candidate"]
    assert findings[0].severity is review_engine.Severity.CRITICAL
