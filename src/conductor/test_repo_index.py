"""The native test index answers the two questions the gate's scans used to answer.

The equivalence checks here live in Python deliberately: they compare the index
against the genuine ``ast`` matcher and the genuine ``git grep``, which is the only
comparison that means anything. A reference implementation written in Rust would be
a line scan, and a line scan cannot tell an import from an import quoted inside a
test fixture.
"""

from __future__ import annotations

import ast
import pathlib
import random
import subprocess

import pytest

ri = pytest.importorskip(
    "conductor.repo_index",
    reason="slop_core not built; run `make -C research/runtime/native slop-core`",
    exc_type=ImportError,
)

REPO = pathlib.Path(__file__).resolve().parents[1]


@pytest.fixture(scope="module")
def index():
    return ri.build(REPO)


def _old_matcher_targets(tree: ast.AST) -> set[str]:
    """Exactly what the scan this replaces matched on: a dotted module, nothing else."""
    out: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.ImportFrom) and node.module and node.level == 0:
            out.add(node.module)
        elif isinstance(node, ast.Import):
            out.update(a.name for a in node.names)
    return out


def test_the_index_resolves_every_import_the_ast_matcher_did(index):
    """The property that makes the replacement safe: it can add a driver, never lose one.

    Binding the imported names as well as the module widens the match. If it ever
    narrowed it, a module would silently lose its drivers and the gate would skip it
    while reporting a clean run.
    """
    checked = 0
    for test in REPO.rglob("test_*.py"):
        if ".git" in test.parts or ".venv" in test.parts:
            continue
        try:
            tree = ast.parse(test.read_text())
        except (OSError, SyntaxError):
            continue
        rel = str(test.relative_to(REPO))
        for dotted in _old_matcher_targets(tree):
            checked += 1
            assert rel in index.drivers_for(dotted), (
                f"{rel} imports {dotted}; the index does not resolve it"
            )
    assert checked > 1000, f"only {checked} imports checked -- wrong tree?"


def test_from_package_import_module_is_resolved(index):
    """The defect. `from conductor import slop_gate` was invisible to the old matcher.

    It is the dominant import idiom in this repository, so the gate was reporting
    "no driver tests" for modules whose tests were sitting right there and skipping
    them entirely.
    """
    drivers = index.drivers_for("conductor/slop_gate.py")
    assert "conductor/test_slop_gate.py" in drivers

    src = (REPO / "conductor" / "test_slop_gate.py").read_text()
    assert "from conductor import slop_gate" in src, (
        "the test file no longer uses the idiom this regression is about"
    )


def test_named_by_agrees_with_the_git_grep_it_replaces(index):
    """`refine_unexercised` spawned one of these per unreached function; 405 last sweep.

    Compared over tracked files only, because the two disagree on purpose. `git grep`
    sees what git has; the index walks the filesystem, as `drivers_for` always has.
    An untracked test file you just wrote genuinely does name the function it tests,
    and calling that an unnamed coverage hole would be wrong -- but the difference is
    real, so it is pinned here rather than left to surface as a mystery.
    """
    names = sorted(
        n.name
        for p in (REPO / "conductor").glob("*.py")
        if not p.name.startswith("test_")
        for n in ast.walk(ast.parse(p.read_text()))
        if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))
    )
    tracked = set(
        subprocess.run(["git", "ls-files", "--", "*/test_*.py", "test_*.py"],
                       cwd=REPO, capture_output=True, text=True, check=True).stdout.split()
    )
    random.seed(11)
    for name in random.sample(names, 25):
        grep = subprocess.run(
            ["git", "grep", "-l", "-w", "-F", name, "--", "*/test_*.py", "test_*.py"],
            cwd=REPO,
            capture_output=True,
            text=True,
        ).stdout.split()
        assert set(index.named_by(name)) & tracked == set(grep), name


def test_a_package_is_named_by_its_directory(index):
    assert ri.dotted_for("conductor/__init__.py") == "conductor"
    assert ri.dotted_for("conductor/slop_gate.py") == "conductor.slop_gate"


def test_the_index_is_not_degenerate(index):
    # A tool that walks the wrong tree returns a clean, meaningless answer, so the
    # scale of what it found is part of the contract.
    assert index.file_count > 200
    assert index.import_key_count > 1000
    assert index.name_key_count > 10000


def test_the_gate_asks_the_index_and_gets_the_same_answer(index):
    from conductor import slop_gate

    assert slop_gate.drivers_for(
        "conductor/slop_gate.py", REPO, index
    ) == index.drivers_for("conductor/slop_gate.py")


def test_the_cli_reports_the_root_it_actually_resolved(capsys):
    assert ri.main(["--root", str(REPO)]) == 0
    out = capsys.readouterr().out
    assert str(REPO) in out and "TestIndex" in out


def test_the_index_sees_untracked_tests_and_git_grep_does_not(index, tmp_path):
    """The one deliberate difference, stated as a contract.

    `drivers_for` has always walked the filesystem, so a test file written but not yet
    committed already selects its module for probing. Having `named_by` answer from
    git instead left the two halves of the gate disagreeing about which files exist.
    """
    scratch = REPO / "conductor" / "test_zz_index_untracked_probe.py"
    # Assembled at runtime so the literal appears in no tracked file -- including this
    # one. Spelled out, `git grep` finds it here and the test proves nothing.
    marker = "zz_untracked" + "_marker_" + "name"
    scratch.write_text(f"def {marker}():\n    pass\n")
    try:
        fresh = ri.build(REPO)
        assert scratch.name in " ".join(fresh.named_by(marker))
        grep = subprocess.run(
            ["git", "grep", "-l", "-w", "-F", marker, "--", "*/test_*.py", "test_*.py"],
            cwd=REPO, capture_output=True, text=True,
        ).stdout.split()
        assert grep == [], "the probe file was committed; this test no longer isolates"
    finally:
        scratch.unlink()


def test_the_cli_answers_the_driver_question_it_was_asked(capsys):
    assert ri.main(["--root", str(REPO), "--drivers-for", "conductor/slop_gate.py"]) == 0
    assert "conductor/test_slop_gate.py" in capsys.readouterr().out


def test_the_cli_answers_the_naming_question_it_was_asked(capsys):
    # A distinct question from --drivers-for: this one is a whole-word search, not an
    # import resolution, and the two must not answer each other.
    assert ri.main(["--root", str(REPO), "--named-by", "refine_unexercised"]) == 0
    out = capsys.readouterr().out
    assert "conductor/test_slop_gate.py" in out
    # The two questions differ: slop_gate.py imports nothing named `refine_unexercised`,
    # so answering with drivers_for instead would return the module's importers.
    assert "conductor/test_equivalence_probe.py" not in out


def test_an_impossible_root_is_refused_not_answered_emptily(tmp_path):
    """An index over a directory that does not exist would report every module as
    having no driver tests -- a clean sweep that measured nothing."""
    with pytest.raises(NotADirectoryError):
        ri.build(tmp_path / "no-such-tree")
