"""Paired tests for engine.py's self-integrity hashing.

`engine-integrity` is the check that refuses a candidate whose tree does not carry
the governance engine reviewing it. It therefore has to find that engine twice --
once in the candidate snapshot and once around the running package -- and both
lookups used to assume the monorepo's root-level ``conductor/``. Under any other
layout the candidate side found nothing and the check fired CRITICAL on every
review, which is the one failure mode a self-integrity check must not have.
"""

from __future__ import annotations

from pathlib import Path

from conductor.candidate_review.engine import _package_hash
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
