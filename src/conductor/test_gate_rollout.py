"""Tests for the required-check promotion and demotion policy.

Both thresholds get a fixture on each side. A policy whose tests only prove the
happy path is how `candidate-review` became required without ever having been green.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from conductor import gate_rollout
from conductor.gate_rollout import CheckRun, RolloutError


def _run(
    *,
    check: str = "candidate-review",
    pr: int = 1,
    conclusion: str = "success",
    required: bool = False,
    merged_at: str = "2026-08-01T00:00:00+00:00",
    unrelated: bool = False,
) -> CheckRun:
    return CheckRun(
        check=check,
        pr_number=pr,
        conclusion=conclusion,
        required=required,
        merged_at=merged_at,
        unrelated_to_diff=unrelated,
    )


def _greens(count: int, *, required: bool = False) -> list[CheckRun]:
    return [
        _run(pr=index, merged_at=f"2026-08-{index + 1:02d}T00:00:00+00:00", required=required)
        for index in range(1, count + 1)
    ]


# ---------------------------------------------------------------------------
# Promotion threshold -- both sides of >= PROMOTION_GREEN_RUNS
# ---------------------------------------------------------------------------


def test_promotion_is_refused_one_run_below_the_threshold() -> None:
    runs = _greens(gate_rollout.PROMOTION_GREEN_RUNS - 1)
    allowed, reason = gate_rollout.may_promote(runs, "candidate-review")
    assert allowed is False
    assert str(gate_rollout.PROMOTION_GREEN_RUNS) in reason


def test_promotion_is_allowed_exactly_at_the_threshold() -> None:
    runs = _greens(gate_rollout.PROMOTION_GREEN_RUNS)
    allowed, _reason = gate_rollout.may_promote(runs, "candidate-review")
    assert allowed is True


def test_promotion_is_allowed_above_the_threshold() -> None:
    runs = _greens(gate_rollout.PROMOTION_GREEN_RUNS + 3)
    allowed, _reason = gate_rollout.may_promote(runs, "candidate-review")
    assert allowed is True


def test_a_red_resets_the_promotion_count() -> None:
    """Five greens with a red among them is flakiness, not readiness."""
    runs = _greens(gate_rollout.PROMOTION_GREEN_RUNS)
    runs.append(_run(pr=99, conclusion="failure", merged_at="2026-08-20T00:00:00+00:00"))
    runs.append(_run(pr=100, merged_at="2026-08-21T00:00:00+00:00"))
    assert gate_rollout.consecutive_green_as_advisory(runs, "candidate-review") == 1
    allowed, _reason = gate_rollout.may_promote(runs, "candidate-review")
    assert allowed is False


def test_required_runs_do_not_count_toward_promotion() -> None:
    """The threshold is about ADVISORY history; required greens are a different thing."""
    runs = _greens(gate_rollout.PROMOTION_GREEN_RUNS, required=True)
    assert gate_rollout.consecutive_green_as_advisory(runs, "candidate-review") == 0


def test_promotion_counts_are_per_check() -> None:
    runs = _greens(gate_rollout.PROMOTION_GREEN_RUNS)
    allowed, _reason = gate_rollout.may_promote(runs, "some-other-check")
    assert allowed is False


# ---------------------------------------------------------------------------
# Demotion threshold -- both sides of >= DEMOTION_CONSECUTIVE_REDS
# ---------------------------------------------------------------------------


def test_one_unrelated_red_does_not_demote() -> None:
    runs = [_run(pr=1, conclusion="failure", required=True, unrelated=True)]
    needed, _reason = gate_rollout.must_demote(runs, "candidate-review")
    assert needed is False


def test_two_consecutive_unrelated_reds_demote() -> None:
    runs = [
        _run(pr=1, conclusion="failure", required=True, unrelated=True, merged_at="2026-08-01T00:00:00+00:00"),
        _run(pr=2, conclusion="failure", required=True, unrelated=True, merged_at="2026-08-02T00:00:00+00:00"),
    ]
    needed, reason = gate_rollout.must_demote(runs, "candidate-review")
    assert needed is True
    assert "#2" in reason


def test_a_red_related_to_its_own_diff_does_not_count() -> None:
    """The other side of `unrelated_to_diff`: a real defect must keep blocking."""
    runs = [
        _run(pr=1, conclusion="failure", required=True, unrelated=True, merged_at="2026-08-01T00:00:00+00:00"),
        _run(pr=2, conclusion="failure", required=True, unrelated=False, merged_at="2026-08-02T00:00:00+00:00"),
    ]
    needed, _reason = gate_rollout.must_demote(runs, "candidate-review")
    assert needed is False


def test_a_green_between_reds_breaks_the_demotion_streak() -> None:
    runs = [
        _run(pr=1, conclusion="failure", required=True, unrelated=True, merged_at="2026-08-01T00:00:00+00:00"),
        _run(pr=2, conclusion="success", required=True, merged_at="2026-08-02T00:00:00+00:00"),
        _run(pr=3, conclusion="failure", required=True, unrelated=True, merged_at="2026-08-03T00:00:00+00:00"),
    ]
    needed, _reason = gate_rollout.must_demote(runs, "candidate-review")
    assert needed is False


def test_advisory_reds_never_demote() -> None:
    """A non-required check cannot be demoted; it is already where demotion lands."""
    runs = [
        _run(pr=1, conclusion="failure", required=False, unrelated=True, merged_at="2026-08-01T00:00:00+00:00"),
        _run(pr=2, conclusion="failure", required=False, unrelated=True, merged_at="2026-08-02T00:00:00+00:00"),
    ]
    needed, _reason = gate_rollout.must_demote(runs, "candidate-review")
    assert needed is False


# ---------------------------------------------------------------------------
# "Unrelated to the diff" -- the mechanised judgement
# ---------------------------------------------------------------------------


def test_findings_naming_a_changed_path_are_related() -> None:
    findings = [{"severity": "high", "path": "conductor/gate.py"}]
    assert gate_rollout.findings_are_unrelated_to_diff(findings, {"conductor/gate.py"}) is False


def test_findings_outside_the_diff_are_unrelated() -> None:
    """The jscpd shape: 600 duplicate pairs the candidate never opened."""
    findings = [{"severity": "high", "path": "aria_core/src/kernel.cpp"}]
    assert gate_rollout.findings_are_unrelated_to_diff(findings, {"conductor/gate.py"}) is True


def test_a_pathless_governance_finding_is_unrelated() -> None:
    findings = [{"severity": "critical", "path": None}]
    assert gate_rollout.findings_are_unrelated_to_diff(findings, {"conductor/gate.py"}) is True


def test_one_related_finding_makes_the_whole_red_related() -> None:
    findings = [
        {"severity": "high", "path": "aria_core/src/kernel.cpp"},
        {"severity": "high", "path": "conductor/gate.py"},
    ]
    assert gate_rollout.findings_are_unrelated_to_diff(findings, {"conductor/gate.py"}) is False


def test_excepted_and_low_findings_are_not_blocking() -> None:
    findings = [
        {"severity": "high", "path": "aria_core/x.cpp", "exception_id": "exc-1"},
        {"severity": "medium", "path": "aria_core/y.cpp"},
    ]
    assert gate_rollout.findings_are_unrelated_to_diff(findings, set()) is False


# ---------------------------------------------------------------------------
# Ledger round-trip
# ---------------------------------------------------------------------------


def test_ledger_round_trips(tmp_path: Path) -> None:
    ledger = tmp_path / "ledger.json"
    runs = _greens(3)
    gate_rollout.save_ledger(ledger, runs)
    assert gate_rollout.load_ledger(ledger) == runs


def test_missing_ledger_is_empty_not_an_error(tmp_path: Path) -> None:
    assert gate_rollout.load_ledger(tmp_path / "absent.json") == []


def test_unknown_ledger_schema_is_refused(tmp_path: Path) -> None:
    ledger = tmp_path / "ledger.json"
    ledger.write_text(json.dumps({"schema_version": 999, "runs": []}), encoding="utf-8")
    with pytest.raises(RolloutError):
        gate_rollout.load_ledger(ledger)


def test_promote_refuses_without_the_owner_acknowledgement(tmp_path: Path, capsys) -> None:
    """Meeting the threshold makes a check eligible, never promoted."""
    ledger = tmp_path / "ledger.json"
    gate_rollout.save_ledger(ledger, _greens(gate_rollout.PROMOTION_GREEN_RUNS))
    exit_code = gate_rollout.main(
        [
            "--ledger",
            str(ledger),
            "promote",
            "--check",
            "candidate-review",
            "--ruleset",
            "1",
        ]
    )
    assert exit_code == 1
    assert "owner's decision" in capsys.readouterr().err
