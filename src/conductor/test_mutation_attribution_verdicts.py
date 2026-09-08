"""Killer attribution: which declared test actually killed a mutant, and what a
receipt keeps once a kill is routine enough to fold.

Split out of `test_mutation_testing` so neither module carries the whole
framework; the runner these exercise still lives in `conductor.mutation_testing`.
"""

from __future__ import annotations

from pathlib import Path

from conductor import mutation_testing


def _mutation(*killers: str) -> mutation_testing.Mutation:
    return mutation_testing.Mutation(
        mutation_id="m1",
        patch_file=Path("m1.patch"),
        patch_sha256="0" * 64,
        allowed_paths=("src/a.py",),
        expected_killers=killers,
    )


def _report(**rows: str) -> dict[str, object]:
    return {
        "status": "COMPLETE",
        "tests": {nodeid: {"outcome": outcome} for nodeid, outcome in rows.items()},
    }


def test_a_kill_is_confirmed_only_when_a_declared_killer_actually_failed() -> None:
    """`expected_killers` is a prediction; the verdict says whether it held.

    A non-zero exit only proves *something* failed, so the four fields a reader
    acts on -- what was declared, what was observed, the intersection, and the
    two disjoint remainders -- all have to be present and disjointly correct.
    """

    verdict = mutation_testing.killer_verdict(
        _mutation("t::declared", "t::silent"),
        _report(**{"t::declared": "FAILED", "t::stranger": "ERROR", "t::ok": "PASSED"}),
        "KILLED",
    )

    assert verdict["status"] == "CONFIRMED"
    assert verdict["declared"] == ["t::declared", "t::silent"]
    # ERROR counts as a failure beside FAILED, and PASSED never does.
    assert verdict["observed_failures"] == ["t::declared", "t::stranger"]
    assert verdict["matched"] == ["t::declared"]
    # Declared but absent from the matrix entirely -- not merely passing.
    assert verdict["unobservable"] == ["t::silent"]
    assert verdict["collateral"] == ["t::stranger"]
    # No unranked failures were reported, so the key stays off the verdict rather
    # than appearing as an empty list a reader would have to interpret.
    assert "unranked_failures" not in verdict


def test_a_kill_by_nobody_who_was_declared_is_misattributed_not_confirmed() -> None:
    """The contract failing is a finding, not a pass."""

    verdict = mutation_testing.killer_verdict(
        _mutation("t::declared"),
        _report(**{"t::stranger": "FAILED"}),
        "KILLED",
    )

    assert verdict["status"] == "MISATTRIBUTED"
    assert verdict["matched"] == []
    assert verdict["collateral"] == ["t::stranger"]


def test_unranked_failures_are_carried_so_a_blunt_mutant_is_visible() -> None:
    """A mutant that breaks the build fails tests no contract can adjudicate.

    Their count is what separates a precise mutant from one any test would catch,
    so they are recorded rather than discarded with the rest of the transcript.
    """

    report = _report(**{"t::declared": "FAILED"})
    report["unranked_failures"] = ["other::a", "other::b"]

    verdict = mutation_testing.killer_verdict(
        _mutation("t::declared"), report, "KILLED"
    )

    assert verdict["unranked_failures"] == ["other::a", "other::b"]


def test_a_kill_nobody_can_attribute_is_refused_and_says_which_kind() -> None:
    """Three non-verdicts, each a different fact about the run.

    A survivor was never adjudicable; a batch with no report has no attribution
    machinery at all; a report that did not complete had the machinery and still
    could not say. Collapsing them would hide which one happened.
    """

    survived = mutation_testing.killer_verdict(_mutation("t::a"), None, "SURVIVED")
    assert survived == {"status": "NOT_APPLICABLE", "declared": ["t::a"]}

    unavailable = mutation_testing.killer_verdict(_mutation("t::a"), None, "KILLED")
    assert unavailable["status"] == "UNAVAILABLE"
    assert unavailable["reason"] == "campaign batch carries no per-test attribution"

    unattributed = mutation_testing.killer_verdict(
        _mutation("t::a"),
        {"status": "INCOMPLETE", "missing_nodeids": ["t::a"], "error": "collection"},
        "KILLED",
    )
    assert unattributed["status"] == "UNATTRIBUTED"
    assert unattributed["reason"] == "attribution is INCOMPLETE"
    assert unattributed["missing_nodeids"] == ["t::a"]
    assert unattributed["error"] == "collection"


def test_only_a_confirmed_kill_is_routine_enough_to_fold() -> None:
    """Both conjuncts have to hold, because folding discards the transcript.

    A survivor and a kill the contract did not predict are exactly the rows
    anyone ever reads, so neither may take the folded path.
    """

    assert mutation_testing._is_routine_kill("KILLED", {"status": "CONFIRMED"})
    assert not mutation_testing._is_routine_kill("SURVIVED", {"status": "CONFIRMED"})
    assert not mutation_testing._is_routine_kill("KILLED", {"status": "MISATTRIBUTED"})
    assert not mutation_testing._is_routine_kill("KILLED", {})


def test_folding_a_routine_kill_keeps_every_field_a_reader_acts_on() -> None:
    """Only the per-test matrix collapses, and it collapses to its tally.

    The failing nodeids, the ranked tests the batch could not map, the unmapped
    cases and the batch's own status all survive literally; `tests` becomes the
    count per outcome, sorted so two receipts of the same run compare equal.
    """

    summary = mutation_testing._attribution_summary(
        {
            "status": "COMPLETE",
            "failed_nodeids": ["t::a"],
            "missing_nodeids": ["t::b"],
            "unmapped_cases": ["t::c[1]"],
            "tests": {
                "t::a": {"outcome": "FAILED"},
                "t::b": {"outcome": "PASSED"},
                "t::d": {"outcome": "PASSED"},
                "t::e": "not a row",
            },
        }
    )

    assert summary["status"] == "COMPLETE"
    assert summary["failed_nodeids"] == ["t::a"]
    assert summary["missing_nodeids"] == ["t::b"]
    assert summary["unmapped_cases"] == ["t::c[1]"]
    assert summary["ranked_outcome_counts"] == {"FAILED": 1, "PASSED": 2}
    # The matrix itself is gone -- that is the whole point of folding it.
    assert "tests" not in summary

    # A batch that carries no matrix at all, or carries one of these keys with
    # the wrong type, folds to an empty tally rather than crashing: the
    # summary's shape must not depend on what the batch happened to report.
    assert mutation_testing._attribution_summary(
        {"status": "COMPLETE", "tests": ["t::a"], "failed_nodeids": "t::a"}
    ) == {"status": "COMPLETE", "ranked_outcome_counts": {}}
