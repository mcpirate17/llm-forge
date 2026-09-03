"""Test selection can see a Rust crate's own tests.

Every selector in ``graph_selection`` matches ``test_<stem>.py`` names or walks
Python import edges, so a change confined to Rust selected nothing and the gate
reported "no targeted tests" over a crate whose suite sat beside it. That is
structural for a project whose compute is deliberately native, and it made the
finding say something false rather than something strict.

Each test here fixes one edge of what counts as a crate's tests, and one holds
the line the fix must not cross: a change that touches Python as well is still
answerable for the Python half.
"""

from __future__ import annotations

from dataclasses import replace
from pathlib import Path

import pytest

from conductor.candidate_review.checks import Change, ReviewContext
from conductor.candidate_review.graph_selection import _rust_crate_tests
from conductor.candidate_review.verification import select_tests
from conductor.test_candidate_review_hardening import _gate_context

CRATE = "research/runtime/native/rust/probe"
SOURCE = f"{CRATE}/src/lib.rs"

TESTED = """pub fn add(a: i64, b: i64) -> i64 {
    a + b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds() {
        assert_eq!(add(1, 2), 3);
    }
}
"""

UNTESTED = "pub fn add(a: i64, b: i64) -> i64 {\n    a + b\n}\n"


def _write(ctx: ReviewContext, rel: str, text: str) -> None:
    path = ctx.snapshot / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def _crate(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, lib: str = TESTED
) -> ReviewContext:
    """A snapshot holding one Rust crate."""

    ctx = _gate_context(monkeypatch, tmp_path, inventory={})
    _write(ctx, f"{CRATE}/Cargo.toml", '[package]\nname = "probe"\n')
    _write(ctx, SOURCE, lib)
    return ctx


def _native_change(path: str) -> Change:
    return Change(
        status="M",
        path=path,
        old_path=None,
        old_mode="100644",
        new_mode="100644",
        old_oid="1" * 40,
        new_oid="2" * 40,
        classes=("native", "source"),
    )


def _python_change(path: str) -> Change:
    return Change(
        status="M",
        path=path,
        old_path=None,
        old_mode="100644",
        new_mode="100644",
        old_oid="1" * 40,
        new_oid="2" * 40,
        classes=("python", "source"),
    )


def _selecting(
    monkeypatch: pytest.MonkeyPatch, ctx: ReviewContext, *changes: Change
) -> ReviewContext:
    """A context whose Python graph selects nothing, so only Rust can answer."""

    monkeypatch.setattr(
        "conductor.candidate_review.verification._graph_test_paths",
        lambda _ctx, _sources: (set(), {"status": "stubbed", "selected_edges": 0}),
    )
    monkeypatch.setattr(
        "conductor.candidate_review.verification._convention_tests",
        lambda _ctx, _sources: set(),
    )
    return replace(ctx, candidate=replace(ctx.candidate, changes=changes))


def test_an_inline_test_module_counts_as_the_crates_tests(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """A `#[cfg(test)]` module is where a Rust crate keeps most of its tests.

    Selecting only files under `tests/` would miss the common case entirely and
    leave the finding just as wrong as before.
    """

    ctx = _crate(monkeypatch, tmp_path)
    assert _rust_crate_tests(ctx, [SOURCE]) == {SOURCE: (SOURCE,)}


def test_an_integration_test_file_counts_without_the_marker(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """Files under `<crate>/tests/` are tests by cargo's own convention.

    They carry no `#[cfg(test)]` marker, so a marker-only scan reports a crate
    tested exclusively that way as having nothing.
    """

    ctx = _crate(monkeypatch, tmp_path, lib=UNTESTED)
    _write(ctx, f"{CRATE}/tests/integration.rs", "#[test]\nfn works() {}\n")
    assert _rust_crate_tests(ctx, [SOURCE]) == {
        SOURCE: (f"{CRATE}/tests/integration.rs",)
    }


def test_build_output_is_not_evidence(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """`target/` holds compiled dependencies, whose test modules are not this
    crate's.

    A crate with no tests of its own would otherwise report coverage borrowed
    from whatever it happened to have built.
    """

    ctx = _crate(monkeypatch, tmp_path, lib=UNTESTED)
    _write(ctx, f"{CRATE}/target/debug/build/dep/src/lib.rs", TESTED)
    assert _rust_crate_tests(ctx, [SOURCE]) == {}


def test_a_source_outside_any_crate_is_not_covered(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """Without a Cargo.toml above it there is no crate whose tests could answer
    for the change, and inventing one would suppress the finding on nothing."""

    ctx = _crate(monkeypatch, tmp_path)
    _write(ctx, "research/scratch/loose.rs", TESTED)
    assert _rust_crate_tests(ctx, ["research/scratch/loose.rs"]) == {}


def test_crate_tests_answer_the_finding_without_being_run(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """The whole point: a Rust-only change is no longer reported as untested.

    The crate's files must not join the executed set either -- `select_tests`
    output is sharded and handed to pytest, which cannot collect a .rs file.
    """

    ctx = _selecting(monkeypatch, _crate(monkeypatch, tmp_path), _native_change(SOURCE))
    selection = select_tests(ctx)
    assert selection.findings == ()
    assert selection.tests == ()
    assert selection.graph["native_test_files"] == 1


def test_an_untested_crate_is_still_reported(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """A crate with no tests anywhere must keep failing the check, or the fix
    is just an exemption for every Rust change."""

    ctx = _selecting(
        monkeypatch,
        _crate(monkeypatch, tmp_path, lib=UNTESTED),
        _native_change(SOURCE),
    )
    selection = select_tests(ctx)
    assert [finding.rule_id for finding in selection.findings] == ["no-targeted-tests"]
    assert selection.findings[0].evidence == {"source_paths": [SOURCE]}


def test_a_covered_crate_does_not_answer_for_python(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """A change touching both languages is still answerable for the Python half.

    Suppressing the whole finding whenever any Rust crate is covered would let a
    Python module with no tests ride in beside a well-tested crate.
    """

    ctx = _selecting(
        monkeypatch,
        _crate(monkeypatch, tmp_path),
        _native_change(SOURCE),
        _python_change("conductor/probe_module.py"),
    )
    selection = select_tests(ctx)
    assert [finding.rule_id for finding in selection.findings] == ["no-targeted-tests"]
    assert selection.findings[0].evidence == {
        "source_paths": ["conductor/probe_module.py"]
    }
