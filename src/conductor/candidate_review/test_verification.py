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


def test_malformed_missing_evidence_row_stays_critical() -> None:
    """Unchanged by #398: a non-dict row is still a hard failure, not debt."""

    findings = _missing_evidence_findings(
        {"missing_evidence": ["not-a-dict"]},
        waived=set(),
    )

    assert len(findings) == 1
    assert findings[0].severity == Severity.CRITICAL
    assert findings[0].rule_id == "malformed-mutation-receipt"


def test_malformed_container_and_missing_fields_remain_blocking() -> None:
    """Malformed and missing evidence are both blocking admission failures."""

    malformed = _missing_evidence_findings({"missing_evidence": {}}, waived=set())
    assert len(malformed) == 1
    assert malformed[0].severity == Severity.CRITICAL
    assert malformed[0].rule_id == "malformed-evidence-container"
    assert malformed[0].message.startswith("missing_evidence: evidence container")
    assert "not a list" in malformed[0].message

    findings = _missing_evidence_findings(
        {"missing_evidence": [{"receipt_rejections": ["stale runner pin"]}]},
        waived=set(),
    )
    assert len(findings) == 1
    finding = findings[0]
    assert finding.severity == Severity.CRITICAL
    assert finding.path is None
    assert (
        "missing mutation evidence -- current automatic PASS evidence is required"
        in finding.message
    )
    assert finding.evidence == {"receipt_rejections": ["stale runner pin"]}

    missing = _missing_evidence_findings(
        {
            "missing_evidence": [
                {"path": "conductor/example.py", "reason": "no PASS receipt"}
            ]
        },
        waived=set(),
    )
    assert len(missing) == 1
    missing_finding = missing[0]
    assert missing_finding.severity == Severity.CRITICAL
    assert missing_finding.rule_id == "missing-mutation-receipt"
    assert (
        missing_finding.message
        == "conductor/example.py: no PASS receipt -- current automatic PASS evidence is required"
    )
    assert (
        "make mutation-generate MUTATION_GENERATE_ARGS='--only SRC'"
        in missing_finding.help
    )
    assert "MUTATION_SOURCE" not in missing_finding.help
    assert "Hand-authored" in missing_finding.help
    assert "forbidden" in missing_finding.help

    waived_findings = _missing_evidence_findings(
        {
            "missing_evidence": [
                {"path": "conductor/example.py", "reason": "no receipt"}
            ]
        },
        waived={"conductor/example.py"},
    )
    assert waived_findings == []


def test_invalid_or_waived_rows_do_not_hide_later_actionable_debt() -> None:
    """Each row is independent: later evidence must survive an earlier short circuit."""

    findings = _missing_evidence_findings(
        {
            "missing_evidence": [
                "malformed",
                {"path": "waived.py", "reason": "old debt"},
                {"path": "active.py", "reason": "needs generated evidence"},
            ]
        },
        waived={"waived.py"},
    )

    assert [(finding.rule_id, finding.path) for finding in findings] == [
        ("malformed-mutation-receipt", None),
        ("missing-mutation-receipt", "active.py"),
    ]
