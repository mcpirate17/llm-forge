"""The value gate scopes to what the candidate changed, not to what shares its file.

`_value_gated_nodeids` used to parse the candidate's copy of a changed test file and
gate every definition in it. A file that landed after the grandfather anchor therefore
put its whole contents through admission on every later edit, so a two-test change to an
83-test file demanded value evidence for 81 definitions it never touched. These tests
pin the base-relative scope and, just as importantly, pin that it still bites: added and
modified definitions are gated exactly as before, and every case where the base cannot be
established falls back to gating the whole file.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from conductor.candidate_review.checks import ReviewContext
from conductor.candidate_review.model import Candidate, Change
from conductor.candidate_review.policy import load_policy
from conductor.candidate_review.policy_path import resolve_policy_path
from conductor.candidate_review.verification import ZERO_OID, _value_gated_nodeids

TEST_PATH = "conductor/test_probe_scope.py"


def _git(repo: Path, *args: str) -> str:
    completed = subprocess.run(
        ["git", *args], cwd=repo, capture_output=True, text=True, check=True
    )
    return completed.stdout.strip()


def _repo(tmp_path: Path) -> Path:
    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init", "--quiet", "--initial-branch=main")
    return repo


def _blob(repo: Path, source: str) -> str:
    """Write `source` into the object store and return its oid."""

    completed = subprocess.run(
        ["git", "hash-object", "-w", "--stdin"],
        cwd=repo,
        input=source.encode("utf-8"),
        capture_output=True,
        check=True,
    )
    return completed.stdout.decode().strip()


def _context(
    tmp_path: Path,
    *,
    candidate_source: str,
    base_oid: str,
    repo: Path,
    path: str = TEST_PATH,
    old_path: str | None = None,
) -> ReviewContext:
    snapshot = tmp_path / "snapshot"
    target = snapshot / path
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(candidate_source, encoding="utf-8")
    change = Change(
        status="M",
        path=path,
        old_path=old_path,
        old_mode="000000" if base_oid == ZERO_OID else "100644",
        new_mode="100644",
        old_oid=base_oid,
        new_oid="d" * 40,
        classes=("test",),
    )
    return ReviewContext(
        repo=repo,
        snapshot=snapshot,
        candidate=Candidate(
            kind="index",
            tree_oid="a" * 40,
            base_tree_oid="b" * 40,
            base_commit_oid="c" * 40,
            commit_oid=None,
            target_ref="HEAD",
            changes=(change,),
        ),
        entries=(),
        policy=load_policy(resolve_policy_path()),
        surface="manual",
        profile="fast",
        owner=None,
        runtime_dir=tmp_path / "runtime",
    )


def _gated(ctx: ReviewContext, grandfathered: dict[str, frozenset[str]] | None = None):
    return _value_gated_nodeids(ctx, grandfathered or {})


BASE = "def test_kept():\n    assert True\n\n\ndef test_edited():\n    assert 1 == 1\n"


def test_an_added_file_gates_every_definition(tmp_path: Path) -> None:
    repo = _repo(tmp_path)
    ctx = _context(tmp_path, candidate_source=BASE, base_oid=ZERO_OID, repo=repo)
    assert _gated(ctx) == {
        TEST_PATH: (f"{TEST_PATH}::test_edited", f"{TEST_PATH}::test_kept")
    }


def test_a_definition_added_to_an_existing_file_is_gated(tmp_path: Path) -> None:
    repo = _repo(tmp_path)
    candidate = BASE + "\n\ndef test_new():\n    assert True\n"
    ctx = _context(
        tmp_path, candidate_source=candidate, base_oid=_blob(repo, BASE), repo=repo
    )
    assert _gated(ctx) == {TEST_PATH: (f"{TEST_PATH}::test_new",)}


def test_a_modified_definition_is_gated(tmp_path: Path) -> None:
    repo = _repo(tmp_path)
    candidate = BASE.replace("assert 1 == 1", "assert 2 == 2")
    ctx = _context(
        tmp_path, candidate_source=candidate, base_oid=_blob(repo, BASE), repo=repo
    )
    assert _gated(ctx) == {TEST_PATH: (f"{TEST_PATH}::test_edited",)}


def test_an_untouched_definition_sharing_the_file_is_not_gated(tmp_path: Path) -> None:
    """The fix: `test_kept` is byte-identical to the base, so it is not this
    candidate's debt even though the file changed around it."""

    repo = _repo(tmp_path)
    candidate = BASE.replace("assert 1 == 1", "assert 2 == 2")
    ctx = _context(
        tmp_path, candidate_source=candidate, base_oid=_blob(repo, BASE), repo=repo
    )
    assert f"{TEST_PATH}::test_kept" not in _gated(ctx)[TEST_PATH]


def test_a_file_changed_only_outside_its_tests_gates_nothing(tmp_path: Path) -> None:
    repo = _repo(tmp_path)
    candidate = "import os  # noqa: F401\n\n\n" + BASE
    ctx = _context(
        tmp_path, candidate_source=candidate, base_oid=_blob(repo, BASE), repo=repo
    )
    assert _gated(ctx) == {}


def test_a_decorator_only_change_still_counts_as_modified(tmp_path: Path) -> None:
    """A parametrize list is part of what the test asserts, so it is part of the
    definition text."""

    repo = _repo(tmp_path)
    base = "import pytest\n\n\n@pytest.mark.parametrize('n', [1])\ndef test_p(n):\n    assert n\n"
    candidate = base.replace("[1]", "[1, 2]")
    ctx = _context(
        tmp_path, candidate_source=candidate, base_oid=_blob(repo, base), repo=repo
    )
    assert _gated(ctx) == {TEST_PATH: (f"{TEST_PATH}::test_p",)}


def test_a_grandfathered_definition_stays_excluded_when_modified(
    tmp_path: Path,
) -> None:
    repo = _repo(tmp_path)
    candidate = BASE.replace("assert 1 == 1", "assert 2 == 2")
    ctx = _context(
        tmp_path, candidate_source=candidate, base_oid=_blob(repo, BASE), repo=repo
    )
    assert _gated(ctx, {TEST_PATH: frozenset({"test_edited"})}) == {}


def test_an_unreadable_base_blob_gates_the_whole_file(tmp_path: Path) -> None:
    """Fail closed: an oid the object store does not have is not evidence that
    nothing changed."""

    repo = _repo(tmp_path)
    ctx = _context(tmp_path, candidate_source=BASE, base_oid="e" * 40, repo=repo)
    assert _gated(ctx) == {
        TEST_PATH: (f"{TEST_PATH}::test_edited", f"{TEST_PATH}::test_kept")
    }


def test_a_base_that_does_not_parse_gates_the_whole_file(tmp_path: Path) -> None:
    repo = _repo(tmp_path)
    ctx = _context(
        tmp_path,
        candidate_source=BASE,
        base_oid=_blob(repo, "def test_kept(:\n"),
        repo=repo,
    )
    assert _gated(ctx) == {
        TEST_PATH: (f"{TEST_PATH}::test_edited", f"{TEST_PATH}::test_kept")
    }


def test_a_renamed_file_is_compared_against_its_old_path(tmp_path: Path) -> None:
    repo = _repo(tmp_path)
    candidate = BASE + "\n\ndef test_new():\n    assert True\n"
    ctx = _context(
        tmp_path,
        candidate_source=candidate,
        base_oid=_blob(repo, BASE),
        repo=repo,
        old_path="conductor/test_probe_scope_old.py",
    )
    assert _gated(ctx) == {TEST_PATH: (f"{TEST_PATH}::test_new",)}


def test_class_nested_tests_are_scoped_by_their_qualified_label(tmp_path: Path) -> None:
    repo = _repo(tmp_path)
    base = (
        "class TestThing:\n"
        "    def test_kept(self):\n        assert True\n\n"
        "    def test_edited(self):\n        assert 1 == 1\n"
    )
    candidate = base.replace("assert 1 == 1", "assert 2 == 2")
    ctx = _context(
        tmp_path, candidate_source=candidate, base_oid=_blob(repo, base), repo=repo
    )
    assert _gated(ctx) == {TEST_PATH: (f"{TEST_PATH}::TestThing::test_edited",)}


def test_a_candidate_that_does_not_parse_is_refused_not_skipped(tmp_path: Path) -> None:
    repo = _repo(tmp_path)
    ctx = _context(
        tmp_path,
        candidate_source="def test_kept(:\n",
        base_oid=_blob(repo, BASE),
        repo=repo,
    )
    with pytest.raises(RuntimeError, match="cannot parse test definitions"):
        _gated(ctx)


def test_a_mutant_patch_fragment_is_never_a_test_definition(tmp_path: Path) -> None:
    repo = _repo(tmp_path)
    ctx = _context(
        tmp_path,
        candidate_source="--- a/x\n+++ b/x\n",
        base_oid=ZERO_OID,
        repo=repo,
        path="conductor/mutation_campaigns/patches/c/test_thing_dropped.patch",
    )
    assert _gated(ctx) == {}


def test_a_new_non_python_test_file_is_gated_as_a_whole_path(tmp_path: Path) -> None:
    repo = _repo(tmp_path)
    ctx = _context(
        tmp_path,
        candidate_source="it('works', () => {});\n",
        base_oid=ZERO_OID,
        repo=repo,
        path="conductor/ui/test_thing.js",
    )
    assert _gated(ctx) == {
        "conductor/ui/test_thing.js": ("conductor/ui/test_thing.js",)
    }
