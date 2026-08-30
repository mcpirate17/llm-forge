"""Tests for module-level import ablation.

The two oracles exist because either one alone gives a wrong answer, and both wrong
answers were reached by hand before the tool existed:

* the test oracle alone called a live re-export dead, because a re-export's consumers
  are in OTHER modules by construction;
* a hand-written single-line grep for those consumers missed every parenthesised
  multi-line ``from x import (\\n a,\\n b,\\n)``, which is how most of them are written.

Ablation must also never touch the working tree: an earlier throwaway version edited
files in place and left one half-edited when it was interrupted.
"""

from __future__ import annotations

import pathlib
import subprocess
import textwrap

import pytest

from conductor import import_ablation as ia

MODULE = '''
import os  # noqa: F401 - side effect
import sys
from collections import (  # noqa: F401
    OrderedDict,
    defaultdict,
)

VALUE = 1
'''


@pytest.fixture
def module(tmp_path: pathlib.Path) -> pathlib.Path:
    path = tmp_path / "sample.py"
    path.write_text(textwrap.dedent(MODULE).lstrip())
    return path


def test_only_silenced_imports_are_candidates(module: pathlib.Path) -> None:
    """`import sys` is plainly used or plainly unused; ruff decides those already."""
    silenced = [s.statement for s in ia.import_sites(module)]
    every = [s.statement for s in ia.import_sites(module, silenced_only=False)]
    assert any("import os" in s for s in silenced)
    assert not any(s == "import sys" for s in silenced)
    assert any(s == "import sys" for s in every)


def test_a_multiline_import_is_removed_whole(module: pathlib.Path) -> None:
    """Removing by line number would leave `OrderedDict,` dangling and a SyntaxError."""
    site = next(s for s in ia.import_sites(module) if "collections" in s.statement)
    ablated = ia.ablate_source(module.read_text(), site)
    assert "OrderedDict" not in ablated
    assert "defaultdict" not in ablated
    compile(ablated, "<ablated>", "exec")


def test_ablation_never_writes_to_the_module(module: pathlib.Path) -> None:
    before = module.read_bytes()
    site = next(iter(ia.import_sites(module)))
    ia.ablate_source(module.read_text(), site)
    assert module.read_bytes() == before


def test_the_finder_serves_ablated_source_for_one_module_only(
    module: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    site = next(s for s in ia.import_sites(module) if "collections" in s.statement)
    finder = ia.AblatedFinder("sample", module, ia.ablate_source(module.read_text(), site))
    assert finder.find_spec("sample") is not None
    assert finder.find_spec("something_else") is None


def test_consumers_sees_a_parenthesised_multiline_import(
    tmp_path: pathlib.Path
) -> None:
    """The regression that made a hand analysis call three live re-exports dead."""
    subprocess.run(["git", "init", "-q"], cwd=tmp_path, check=True)
    (tmp_path / "pkg").mkdir()
    (tmp_path / "pkg" / "leaf.py").write_text("NAME = 1\n")
    (tmp_path / "pkg" / "user.py").write_text(
        "from pkg.leaf import (\n    NAME,\n)\n"
    )
    subprocess.run(["git", "add", "-A"], cwd=tmp_path, check=True)
    found = ia.consumers("pkg/leaf.py", ["NAME"], tmp_path)
    assert "pkg/user.py" in found


def test_no_driver_tests_is_reported_not_treated_as_clean(
    tmp_path: pathlib.Path
) -> None:
    site = ia.ImportSite("m.py", 1, "import os  # noqa: F401", ("os",), True)
    record = ia.classify(site, "m.py", [], tmp_path)
    assert record["verdict"] == ia.NOT_EXERCISED


def test_failing_drivers_mean_the_import_is_load_bearing(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    (tmp_path / "m.py").write_text("import os  # noqa: F401\n")
    monkeypatch.setattr(ia, "ablate_source", lambda src, site: "")
    monkeypatch.setattr(ia, "consumers", lambda m, n, r: ["other.py"])
    monkeypatch.setattr(
        subprocess, "run",
        lambda *a, **k: subprocess.CompletedProcess(a, 1, "", ""),
    )
    site = ia.ImportSite("m.py", 1, "import os  # noqa: F401", ("os",), True)
    record = ia.classify(site, "m.py", ["test_m.py"], tmp_path)
    assert record["verdict"] == ia.LOAD_BEARING


def test_passing_drivers_with_no_consumers_is_a_dead_candidate(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    (tmp_path / "m.py").write_text("import os  # noqa: F401\n")
    monkeypatch.setattr(ia, "ablate_source", lambda src, site: "")
    monkeypatch.setattr(ia, "consumers", lambda m, n, r: [])
    monkeypatch.setattr(
        subprocess, "run",
        lambda *a, **k: subprocess.CompletedProcess(a, 0, "", ""),
    )
    site = ia.ImportSite("m.py", 1, "import os  # noqa: F401", ("os",), True)
    record = ia.classify(site, "m.py", ["test_m.py"], tmp_path)
    assert record["verdict"] == ia.UNVERIFIED
