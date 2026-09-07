"""Hunk parsing and range merging for the line-scoped clang checks.

These decide which lines clang-format and clang-tidy are allowed to complain
about, so an off-by-one here silently narrows or widens the gate. Each test
probes one branch with an input no other test uses, so a mutant that breaks
that branch has exactly one killer.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from conductor.candidate_review.diff_ranges import (
    DiffError,
    changed_line_ranges,
    merge_ranges,
    parse_hunks,
)


def test_hunk_without_a_count_is_a_single_line() -> None:
    assert parse_hunks("@@ -4 +7 @@ void f()") == [(7, 7)]


def test_explicit_count_is_inclusive_of_the_first_line() -> None:
    # +10,3 covers 10, 11 and 12 -- not 10..13.
    assert parse_hunks("@@ -1,1 +10,3 @@") == [(10, 12)]


def test_pure_deletion_contributes_no_range() -> None:
    assert parse_hunks("@@ -5,4 +4,0 @@") == []


def test_hunk_syntax_inside_a_source_line_is_not_a_hunk() -> None:
    # The added line contains something that looks exactly like a hunk header.
    # Anchoring the pattern at the start of the line is the only thing keeping
    # line 99 -- which the file does not have -- out of the ranges.
    diff = '@@ -1,1 +1,1 @@\n-int a;\n+const char *s = "@@ -9,1 +99,1 @@";\n'
    assert (99, 99) not in parse_hunks(diff)


def test_every_hunk_is_reported_not_just_the_first() -> None:
    hunks = parse_hunks("@@ -1,2 +1,2 @@\n@@ -9,2 +20,2 @@")
    assert [start for start, _ in hunks] == [1, 20]


def test_touching_ranges_merge_but_a_gap_survives() -> None:
    # (1,3) and (4,5) touch; (9,9) is separated by lines 6..8.
    assert merge_ranges([(1, 3), (4, 5), (9, 9)]) == [(1, 5), (9, 9)]


def test_nested_range_does_not_shorten_the_enclosing_one() -> None:
    assert merge_ranges([(1, 20), (5, 6)]) == [(1, 20)]


def test_unsorted_input_is_ordered_before_merging() -> None:
    assert merge_ranges([(9, 9), (1, 3)]) == [(1, 3), (9, 9)]


def _git(cwd: Path, *args: str) -> None:
    subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True)


@pytest.fixture()
def repo(tmp_path: Path) -> Path:
    _git(tmp_path, "init", "-q")
    _git(tmp_path, "config", "user.email", "t@example.com")
    _git(tmp_path, "config", "user.name", "t")
    (tmp_path / "f.c").write_text("a\nb\nc\nd\ne\nf\ng\n", encoding="utf-8")
    _git(tmp_path, "add", "f.c")
    _git(tmp_path, "commit", "-qm", "base")
    return tmp_path


def test_only_the_changed_lines_are_reported_not_their_context(repo: Path) -> None:
    """Zero context. One diff line of slack here is a line clang may reformat."""
    (repo / "f.c").write_text("a\nB\nC\nd\ne\nf\ng\n", encoding="utf-8")
    ranges = changed_line_ranges("f.c", base="HEAD", cwd=repo)
    # Line 1 is untouched: with any context at all the range would start there.
    assert [start for start, _ in ranges] == [2]


def test_unresolvable_base_raises_rather_than_reporting_no_changes(repo: Path) -> None:
    with pytest.raises(DiffError):
        changed_line_ranges("f.c", base="does-not-exist", cwd=repo)
