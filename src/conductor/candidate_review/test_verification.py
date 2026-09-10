"""`_missing_evidence_findings` reports missing mutation receipts as debt.

#398 dropped `missing-mutation-receipt` from CRITICAL (blocking) to INFO
(debt, not a block) because the manual mutation-authoring tooling that used
to close the gap was removed outright -- a changed test with no current PASS
receipt can no longer hold a branch back. Nothing pinned that severity or the
"debt, not a block" message before this test, so a mutant that reverted the
finding back to CRITICAL, or dropped the debt framing, would pass unnoticed.
"""

from __future__ import annotations

from conductor.candidate_review.model import Severity
from conductor.candidate_review.verification import _missing_evidence_findings


def test_missing_receipt_is_info_debt_not_a_block() -> None:
    findings = _missing_evidence_findings(
        {
            "missing_evidence": [
                {"path": "conductor/example.py", "reason": "no PASS receipt"}
            ]
        },
        waived=set(),
    )

    assert len(findings) == 1
    finding = findings[0]
    assert finding.severity == Severity.INFO
    assert finding.rule_id == "missing-mutation-receipt"
    assert (
        finding.message == "conductor/example.py: no PASS receipt -- debt, not a block"
    )
    assert "make mutation-generate" in finding.help
    assert "Hand-authored" in finding.help and "forbidden" in finding.help

    # The waiver still short-circuits the same path to an empty finding list --
    # folded into this test rather than its own nodeid, which the mutation
    # engine classified MERGE (fully dominated by the assertions above).
    waived_findings = _missing_evidence_findings(
        {
            "missing_evidence": [
                {"path": "conductor/example.py", "reason": "no receipt"}
            ]
        },
        waived={"conductor/example.py"},
    )
    assert waived_findings == []


def test_malformed_missing_evidence_row_stays_critical() -> None:
    """Unchanged by #398: a non-dict row is still a hard failure, not debt."""

    findings = _missing_evidence_findings(
        {"missing_evidence": ["not-a-dict"]},
        waived=set(),
    )

    assert len(findings) == 1
    assert findings[0].severity == Severity.CRITICAL
    assert findings[0].rule_id == "malformed-mutation-receipt"
