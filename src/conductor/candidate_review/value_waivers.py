"""Nodeid-exact, base-bound waivers for the new-test value gate.

``_new_test_value_findings`` admits a new test only as CORE or INTENTIONAL_REDUNDANCY,
and both are earned by killing a mutant. A test whose subject lives beyond the
python-patch engine's reach -- a Rust engine behind a thin Python wrapper -- kills only
the wrapper mutants a sibling already owns, classifies DELETE_CANDIDATE or MERGE on
every run, and the three ways to make it "pass" (relabel, delete, contrive a mutant
against code it never reaches) are all dishonest.

``[[value_waivers]]`` in the candidate policy names such tests exactly. A waiver
applies to a finding only while every one of these holds, checked on every gate run:

1. the candidate's base commit equals the waiver's ``integration_base`` -- the rule
   ``[[mutation_waivers]]`` follows, so a rebase or a merge ahead of the waiver
   re-arms the gate rather than carrying the exemption forward silently;
2. an ``expires`` date is declared and is not in the past;
3. the finding names one of the waiver's nodeids exactly -- never a prefix, never a
   pattern, and an empty nodeid list waives nothing.

A waived finding is never dropped. It is re-reported as an INFO ``WAIVED`` line that
carries the reason, the approver and the expiry, so the gate output still says what
was exempted and on whose authority, and every inert waiver reports the condition it
failed. The rule this loosens still bites: each of the three conditions above is a
mutant in ``claude_value_waivers_20260902`` with a test that fails when it is removed.
"""

from __future__ import annotations

from datetime import date
from typing import Iterable, Protocol, Sequence

from conductor.candidate_review.model import Finding, Severity

VALUE_RULE = "new-test-value-not-admitted"
WAIVED_RULE = "new-test-value-waived"


class ValueWaiver(Protocol):
    integration_base: str
    nodeids: tuple[str, ...]
    reason: str
    approved_by: str
    approved_on: date
    expires: date | None


def inert_reason(waiver: ValueWaiver, *, base: str | None, today: date) -> str | None:
    """Why this waiver does not apply to the candidate, or None when it does."""

    if base is None or waiver.integration_base != base:
        return "candidate base commit is not the pinned integration base"
    if waiver.expires is None:
        return "no expiry is declared"
    if waiver.expires < today:
        return f"expired on {waiver.expires.isoformat()}"
    return None


def waiver_states(
    waivers: Sequence[ValueWaiver], *, base: str | None, today: date
) -> list[dict[str, object]]:
    """One auditable activation row per declared waiver."""

    states: list[dict[str, object]] = []
    for waiver in waivers:
        reason = inert_reason(waiver, base=base, today=today)
        states.append(
            {
                "integration_base": waiver.integration_base,
                "nodeids": list(waiver.nodeids),
                "active": reason is None,
                "reason": reason or "",
            }
        )
    return states


def apply_value_waivers(
    findings: Iterable[Finding],
    waivers: Sequence[ValueWaiver],
    *,
    base: str | None,
    today: date,
) -> list[Finding]:
    """Re-report exactly-waived value findings as WAIVED lines; pass the rest through."""

    active = [w for w in waivers if inert_reason(w, base=base, today=today) is None]
    result: list[Finding] = []
    for finding in findings:
        waiver = _waiver_for(finding, active)
        result.append(finding if waiver is None else _waived(finding, waiver))
    return result


def _waiver_for(finding: Finding, active: Sequence[ValueWaiver]) -> ValueWaiver | None:
    if finding.rule_id != VALUE_RULE:
        return None
    nodeid = finding.evidence.get("nodeid")
    if not isinstance(nodeid, str):
        return None
    for waiver in active:
        if nodeid in waiver.nodeids:
            return waiver
    return None


def _waived(finding: Finding, waiver: ValueWaiver) -> Finding:
    expires = waiver.expires.isoformat() if waiver.expires is not None else "never"
    return Finding(
        check_id=finding.check_id,
        rule_id=WAIVED_RULE,
        severity=Severity.INFO,
        message=(
            f"WAIVED {finding.message} -- {waiver.reason} (approved by "
            f"{waiver.approved_by} on {waiver.approved_on.isoformat()}, expires "
            f"{expires}, base {waiver.integration_base[:12]})"
        ),
        path=finding.path,
        help=finding.help,
        evidence={
            **finding.evidence,
            "waived_by": {
                "integration_base": waiver.integration_base,
                "approved_by": waiver.approved_by,
                "approved_on": waiver.approved_on.isoformat(),
                "expires": expires,
                "reason": waiver.reason,
            },
        },
    )
