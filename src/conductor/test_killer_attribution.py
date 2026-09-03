"""Enforcement of `expected_killers`: a mutant must die to the test it named."""

from __future__ import annotations

from pathlib import Path

from conductor.mutation_testing import Mutation, killer_verdict
from conductor.mutation_value import pytest_attribution_supported

DECLARED = "pkg/test_thing.py::test_declared"
OTHER = "pkg/test_thing.py::test_other"


def _mutation(*killers: str) -> Mutation:
    return Mutation(
        mutation_id="m1",
        patch_file=Path("patches/m1.patch"),
        patch_sha256="0" * 64,
        allowed_paths=("pkg/thing.py",),
        expected_killers=killers,
    )


def _report(*, status: str = "COMPLETE", **outcomes: str) -> dict[str, object]:
    lookup = {DECLARED: outcomes.get("declared"), OTHER: outcomes.get("other")}
    return {
        "status": status,
        "tests": {
            nodeid: {"outcome": outcome, "duration_seconds": 0.1, "cases": 1}
            for nodeid, outcome in lookup.items()
            if outcome is not None
        },
        "missing_nodeids": [],
        "unmapped_cases": [],
    }


def test_a_mutant_killed_by_its_declared_test_is_confirmed() -> None:
    verdict = killer_verdict(
        _mutation(DECLARED),
        _report(declared="FAILED", other="PASSED"),
        "KILLED",
    )
    assert verdict["status"] == "CONFIRMED"
    assert verdict["matched"] == [DECLARED]


def test_a_mutant_killed_only_by_another_test_is_misattributed() -> None:
    verdict = killer_verdict(
        _mutation(DECLARED),
        _report(declared="PASSED", other="FAILED"),
        "KILLED",
    )
    assert verdict["status"] == "MISATTRIBUTED"
    assert verdict["matched"] == []
    assert verdict["collateral"] == [OTHER]


def test_an_erroring_declared_test_still_counts_as_the_killer() -> None:
    verdict = killer_verdict(
        _mutation(DECLARED),
        _report(declared="ERROR"),
        "KILLED",
    )
    assert verdict["status"] == "CONFIRMED"


def test_a_declared_killer_absent_from_the_report_is_named_unobservable() -> None:
    verdict = killer_verdict(
        _mutation(DECLARED),
        _report(other="FAILED"),
        "KILLED",
    )
    assert verdict["status"] == "MISATTRIBUTED"
    assert verdict["unobservable"] == [DECLARED]


def test_incomplete_attribution_never_confirms_a_contract() -> None:
    verdict = killer_verdict(
        _mutation(DECLARED),
        _report(status="INCOMPLETE", declared="FAILED"),
        "KILLED",
    )
    assert verdict["status"] == "UNAVAILABLE"


def test_a_batch_without_attribution_is_unavailable_not_confirmed() -> None:
    verdict = killer_verdict(_mutation(DECLARED), None, "KILLED")
    assert verdict["status"] == "UNAVAILABLE"


def test_a_survivor_is_adjudicated_by_the_outcome_not_the_killers() -> None:
    verdict = killer_verdict(
        _mutation(DECLARED),
        _report(declared="PASSED"),
        "SURVIVED",
    )
    assert verdict["status"] == "NOT_APPLICABLE"


def test_python_nodeids_without_a_junitxml_flag_are_attributable() -> None:
    assert pytest_attribution_supported(("pytest", "-q"), (DECLARED, OTHER))


def test_a_preexisting_junitxml_flag_blocks_attribution() -> None:
    assert not pytest_attribution_supported(
        ("pytest", "--junitxml=out.xml"), (DECLARED,)
    )


def test_a_non_python_nodeid_blocks_attribution() -> None:
    assert not pytest_attribution_supported(("pytest",), ("crate::tests::case",))


def test_a_batch_with_no_ranked_tests_is_not_attributable() -> None:
    assert not pytest_attribution_supported(("pytest",), ())
