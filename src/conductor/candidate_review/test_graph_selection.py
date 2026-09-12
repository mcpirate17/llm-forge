"""Paired tests for graph_selection's convention-matched test discovery.

`_convention_tests` walks a fixed set of test roots looking for the tests that
exercise a candidate's changed sources. One of those roots is the conductor package
itself, and spelling it ``conductor`` meant that under any other layout the walk
found no file at all -- which the caller reads as "this change has no convention
test", not as "the wrong directory was searched".
"""

from __future__ import annotations

from pathlib import Path

from conductor.candidate_review.checks import ReviewContext
from conductor.candidate_review.graph_selection import (
    _convention_tests,
    _module_names,
)
from conductor.candidate_review.model import Candidate

SRC_LAYOUT = '[tool.conductor]\npackage_root = "src/conductor"\n'


def _ctx(snapshot: Path) -> ReviewContext:
    return ReviewContext(
        repo=snapshot,
        snapshot=snapshot,
        candidate=Candidate(
            kind="pr",
            tree_oid="a" * 40,
            base_tree_oid="b" * 40,
            base_commit_oid="c" * 40,
            commit_oid="d" * 40,
            target_ref="refs/heads/main",
            changes=(),
        ),
        entries=(),
        policy=None,  # type: ignore[arg-type]
        surface="test",
        profile="fast",
        owner=None,
        runtime_dir=snapshot,
    )


def _tree(snapshot: Path, relative: str, layout: str | None) -> Path:
    if layout is not None:
        (snapshot / "pyproject.toml").write_text(layout, encoding="utf-8")
    package = snapshot / relative
    package.mkdir(parents=True)
    (package / "widget.py").write_text("def go():\n    return 1\n", encoding="utf-8")
    (package / "test_widget.py").write_text(
        "from conductor.widget import go\n\n\ndef test_go():\n    assert go()\n",
        encoding="utf-8",
    )
    return package


def test_convention_test_is_found_under_a_declared_src_layout(tmp_path: Path) -> None:
    _tree(tmp_path, "src/conductor", SRC_LAYOUT)
    found = _convention_tests(_ctx(tmp_path), ["src/conductor/widget.py"])
    assert found == {"src/conductor/test_widget.py"}


def test_convention_test_is_found_under_the_unconfigured_default(
    tmp_path: Path,
) -> None:
    """No configuration is the monorepo, and it keeps answering as before."""
    _tree(tmp_path, "conductor", None)
    found = _convention_tests(_ctx(tmp_path), ["conductor/widget.py"])
    assert found == {"conductor/test_widget.py"}


def test_a_src_tree_is_not_searched_under_the_default_declaration(
    tmp_path: Path,
) -> None:
    """The regression this closes: the package's tests found nowhere at all."""
    _tree(tmp_path, "src/conductor", None)
    assert _convention_tests(_ctx(tmp_path), ["src/conductor/widget.py"]) == set()


def test_a_test_naming_the_changed_module_is_matched(tmp_path: Path) -> None:
    package = _tree(tmp_path, "src/conductor", SRC_LAYOUT)
    (package / "test_elsewhere.py").write_text(
        "import conductor.widget\n", encoding="utf-8"
    )
    found = _convention_tests(_ctx(tmp_path), ["src/conductor/widget.py"])
    assert found == {
        "src/conductor/test_widget.py",
        "src/conductor/test_elsewhere.py",
    }


def test_an_unrelated_test_is_not_matched(tmp_path: Path) -> None:
    package = _tree(tmp_path, "src/conductor", SRC_LAYOUT)
    (package / "test_other.py").write_text("def test_other():\n    pass\n", "utf-8")
    found = _convention_tests(_ctx(tmp_path), ["src/conductor/widget.py"])
    assert "src/conductor/test_other.py" not in found


def test_absent_roots_are_skipped(tmp_path: Path) -> None:
    (tmp_path / "pyproject.toml").write_text(SRC_LAYOUT, encoding="utf-8")
    assert _convention_tests(_ctx(tmp_path), ["src/conductor/widget.py"]) == set()


def test_module_names_strip_the_declared_package_prefix(tmp_path: Path) -> None:
    """``src/conductor/widget.py`` imports as ``conductor.widget``, not ``src.*``."""
    (tmp_path / "pyproject.toml").write_text(SRC_LAYOUT, encoding="utf-8")
    assert _module_names(tmp_path, ["src/conductor/widget.py"]) == ["conductor.widget"]
    assert _module_names(tmp_path, ["src/tooling/hooks/a.py"]) == ["tooling.hooks.a"]


def test_module_names_are_unchanged_without_a_prefix(tmp_path: Path) -> None:
    assert _module_names(tmp_path, ["conductor/widget.py"]) == ["conductor.widget"]
    assert _module_names(tmp_path, ["native/x/y.py"]) == ["native.x.y"]


def test_module_names_leave_a_path_outside_the_prefix_alone(tmp_path: Path) -> None:
    (tmp_path / "pyproject.toml").write_text(SRC_LAYOUT, encoding="utf-8")
    assert _module_names(tmp_path, ["native/build.py"]) == ["native.build"]
