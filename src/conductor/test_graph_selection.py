"""Re-export resolution in changed-code test selection.

The gate's selector (``candidate_review/graph_selection.py``) had no test: the
existing ``test_graph_test_select.py`` covers ``conductor/graph_test_select.py``,
which its own docstring calls "an advisory subset ... never a governance gate".

These pin the re-export hop. A test reaching a changed module through its package
``__init__`` produces no code-review-graph edge into that module (the graph does
not resolve ``from .module import name``) and does not match the
``test_<stem>.py`` convention, so before this hop such tests were invisible and
the changed lines read as uncovered.
"""

from __future__ import annotations

from pathlib import Path

from conductor.candidate_review.checks import ReviewContext
from conductor.candidate_review.graph_selection import (
    _convention_tests,
    _public_names,
    _reexport_surfaces,
)
from conductor.candidate_review.model import Candidate
from conductor.candidate_review.policy import load_policy
from conductor.candidate_review.policy_path import resolve_policy_path

SOURCE = "component_fab/equations/adaptation.py"


def _context(tmp_path: Path) -> ReviewContext:
    """A ReviewContext built through the production policy loader."""
    return ReviewContext(
        repo=tmp_path,
        snapshot=tmp_path / "snapshot",
        candidate=Candidate(
            kind="index",
            tree_oid="a" * 40,
            base_tree_oid="b" * 40,
            base_commit_oid="c" * 40,
            commit_oid=None,
            target_ref="HEAD",
            changes=(),
        ),
        entries=(),
        policy=load_policy(resolve_policy_path()),
        surface="manual",
        profile="fast",
        owner=None,
        runtime_dir=tmp_path / "runtime",
    )


def _write(root: Path, relative: str, text: str) -> None:
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def _snapshot(tmp_path: Path, *, init: str, module: str, test: str) -> ReviewContext:
    ctx = _context(tmp_path)
    _write(ctx.snapshot, SOURCE, module)
    _write(ctx.snapshot, "component_fab/equations/__init__.py", init)
    _write(ctx.snapshot, "component_fab/tests/test_equation_adaptation.py", test)
    return ctx


MODULE = "def adapt_equation_distribution():\n    return 1\n"
INIT = "from .adaptation import adapt_equation_distribution\n"
SELECTED = "component_fab/tests/test_equation_adaptation.py"


def test_reexported_import_selects_the_test(tmp_path: Path) -> None:
    """The blind spot this hop exists to close."""
    ctx = _snapshot(
        tmp_path,
        init=INIT,
        module=MODULE,
        test=(
            "from component_fab.equations import adapt_equation_distribution\n"
            "def test_x():\n    assert adapt_equation_distribution()\n"
        ),
    )
    assert SELECTED in _convention_tests(ctx, [SOURCE])


def test_unrelated_package_import_is_not_selected(tmp_path: Path) -> None:
    """Guards over-selection: importing some other name must not pull the test in."""
    ctx = _snapshot(
        tmp_path,
        init=INIT,
        module=MODULE,
        test=(
            "from component_fab.equations import something_else\n"
            "def test_x():\n    assert something_else\n"
        ),
    )
    assert _convention_tests(ctx, [SOURCE]) == set()


def test_aliased_reexport_matches_the_alias(tmp_path: Path) -> None:
    ctx = _snapshot(
        tmp_path,
        init="from .adaptation import adapt_equation_distribution as adapt_eq\n",
        module=MODULE,
        test=(
            "from component_fab.equations import adapt_eq\n"
            "def test_x():\n    assert adapt_eq()\n"
        ),
    )
    assert SELECTED in _convention_tests(ctx, [SOURCE])


def test_reexport_inside_try_except_is_found(tmp_path: Path) -> None:
    """ast.walk must descend into Try bodies, not just module top level."""
    ctx = _snapshot(
        tmp_path,
        init=(
            "try:\n"
            "    from .adaptation import adapt_equation_distribution\n"
            "except ImportError:\n"
            "    adapt_equation_distribution = None\n"
        ),
        module=MODULE,
        test=(
            "from component_fab.equations import adapt_equation_distribution\n"
            "def test_x():\n    assert adapt_equation_distribution\n"
        ),
    )
    assert SELECTED in _convention_tests(ctx, [SOURCE])


def test_star_reexport_uses_module_public_names(tmp_path: Path) -> None:
    """Star re-exports name nothing; missing them would under-select and read as a pass."""
    ctx = _snapshot(
        tmp_path,
        init="from .adaptation import *\n",
        module=MODULE,
        test=(
            "from component_fab.equations import adapt_equation_distribution\n"
            "def test_x():\n    assert adapt_equation_distribution()\n"
        ),
    )
    assert SELECTED in _convention_tests(ctx, [SOURCE])


def test_star_reexport_respects_dunder_all(tmp_path: Path) -> None:
    """A name excluded from __all__ is not re-exported by a star import."""
    ctx = _snapshot(
        tmp_path,
        init="from .adaptation import *\n",
        module='__all__ = ["kept"]\n\ndef kept():\n    return 1\n\ndef dropped():\n    return 2\n',
        test=(
            "from component_fab.equations import dropped\n"
            "def test_x():\n    assert dropped()\n"
        ),
    )
    assert _convention_tests(ctx, [SOURCE]) == set()


def test_relative_import_in_test_does_not_match(tmp_path: Path) -> None:
    """`from .equations import x` is not an import of component_fab.equations."""
    ctx = _snapshot(
        tmp_path,
        init=INIT,
        module=MODULE,
        test=(
            "from .equations import adapt_equation_distribution\n"
            "def test_x():\n    assert adapt_equation_distribution\n"
        ),
    )
    assert _convention_tests(ctx, [SOURCE]) == set()


def test_filename_convention_still_selects(tmp_path: Path) -> None:
    """Pre-existing mechanism must survive: test_<stem>.py matches with no import."""
    ctx = _context(tmp_path)
    _write(ctx.snapshot, "component_fab/foo.py", "x = 1\n")
    _write(ctx.snapshot, "component_fab/tests/test_foo.py", "def test_x():\n    pass\n")
    assert "component_fab/tests/test_foo.py" in _convention_tests(
        ctx, ["component_fab/foo.py"]
    )


def test_dotted_module_string_still_selects(tmp_path: Path) -> None:
    """Pre-existing mechanism must survive: the full dotted path in the test text."""
    ctx = _context(tmp_path)
    _write(ctx.snapshot, "component_fab/foo.py", "x = 1\n")
    _write(
        ctx.snapshot,
        "component_fab/tests/test_other.py",
        "import component_fab.foo\ndef test_x():\n    pass\n",
    )
    assert "component_fab/tests/test_other.py" in _convention_tests(
        ctx, ["component_fab/foo.py"]
    )


def test_surfaces_skip_non_python_and_packageless_sources(tmp_path: Path) -> None:
    """Only a Python file inside a package can carry a re-export surface."""
    ctx = _context(tmp_path)
    _write(ctx.snapshot, "research/tools/loose.py", "def helper():\n    return 1\n")
    assert _reexport_surfaces(ctx, ["research/tools/loose.py"]) == {}
    _write(ctx.snapshot, "component_fab/equations/config.json", "{}\n")
    _write(
        ctx.snapshot,
        "component_fab/equations/__init__.py",
        "from .config import SETTINGS\n",
    )
    assert _reexport_surfaces(ctx, ["component_fab/equations/config.json"]) == {}


def test_public_names_excludes_underscored(tmp_path: Path) -> None:
    module = tmp_path / "m.py"
    module.write_text(
        "def public():\n    return 1\n\ndef _private():\n    return 2\n\n"
        "class Klass:\n    pass\n\nCONST = 3\n_HIDDEN = 4\n",
        encoding="utf-8",
    )
    assert _public_names(module) == {"public", "Klass", "CONST"}


def test_public_names_prefers_dunder_all(tmp_path: Path) -> None:
    module = tmp_path / "m.py"
    module.write_text(
        '__all__ = ["only"]\n\ndef only():\n    return 1\n\ndef other():\n    return 2\n',
        encoding="utf-8",
    )
    assert _public_names(module) == {"only"}
