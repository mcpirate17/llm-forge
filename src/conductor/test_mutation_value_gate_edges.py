"""Fail-closed edges of the new-test value gate's evidence-envelope handling."""

from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace

from conductor.candidate_review.model import Severity
from conductor.candidate_review.verification import _new_test_value_findings


def test_mutation_value_evidence_envelope_fails_closed(tmp_path: Path) -> None:
    """A non-list evidence envelope blocks admission evaluation per gated path."""

    ctx = SimpleNamespace(snapshot=tmp_path)
    gated = {"conductor/probe_a.py": ("conductor/probe_a.py::test_x",)}
    payload = {"evidence": "envelope-not-a-list"}

    findings = _new_test_value_findings(ctx, payload, gated)

    assert len(findings) == 1
    finding = findings[0]
    assert finding.rule_id == "test-value-receipt-unavailable"
    assert finding.severity == Severity.CRITICAL
    assert finding.path == "conductor/probe_a.py"
    assert "::test_x" in finding.message


def test_mutation_value_evidence_receipt_field_fails_closed(tmp_path: Path) -> None:
    """A non-string receipt field fails closed instead of granting admission."""

    ctx = SimpleNamespace(snapshot=tmp_path)
    gated = {"conductor/probe_a.py": ("conductor/probe_a.py::test_x",)}
    row = {"path": "conductor/probe_a.py", "receipt": None}
    payload = {"evidence": [row]}

    findings = _new_test_value_findings(ctx, payload, gated)

    assert len(findings) == 1
    finding = findings[0]
    assert finding.rule_id == "test-value-receipt-unavailable"
    assert finding.severity == Severity.CRITICAL
    assert finding.path == "conductor/probe_a.py"
    assert "::test_x" in finding.message
