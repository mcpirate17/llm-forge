"""Nodeid-exact, base-bound value waivers: every inert condition still bites.

Loosening a gate needs a measurement: each test here removes one condition of the
waiver (base binding, expiry, exact match, non-empty list, reporting) and shows the
CRITICAL finding survives, so a mutant that drops that condition is caught.
"""

from __future__ import annotations

import json
from dataclasses import replace
from datetime import date, timedelta
from pathlib import Path

import pytest

from conductor.candidate_review.checks import check_mutation_evidence
from conductor.candidate_review.model import CheckStatus, Finding, Severity
from conductor.candidate_review.policy import (
    PolicyError,
    ValueWaiverPolicy,
    _parse_value_waivers,
)
from conductor.candidate_review.value_waivers import apply_value_waivers
from conductor.mutation_value import VALUE_SCHEMA
from conductor.test_candidate_review_hardening import PROBE_PATH, _gate_context

BASE = "d" * 40
NODEID = "conductor/test_repo_index.py::test_the_index_is_not_degenerate"
TODAY = date(2026, 9, 2)
REASON = "Rust-engine-pinned behaviour; python patch engine cannot discriminate"


def _waiver(**overrides: object) -> ValueWaiverPolicy:
    fields: dict[str, object] = {
        "integration_base": BASE,
        "nodeids": (NODEID,),
        "reason": REASON,
        "approved_by": "Tim",
        "approved_on": TODAY,
        "expires": TODAY + timedelta(days=30),
    }
    fields.update(overrides)
    return ValueWaiverPolicy(**fields)  # type: ignore[arg-type]


def _raw(**overrides: object) -> dict[str, object]:
    raw: dict[str, object] = {
        "integration_base": BASE,
        "nodeids": [NODEID],
        "reason": REASON,
        "approved_by": "Tim",
        "approved_on": "2026-09-02",
        "expires": "2026-10-02",
    }
    raw.update(overrides)
    return raw


def _finding(nodeid: str = NODEID) -> Finding:
    return Finding(
        check_id="mutation-evidence",
        rule_id="new-test-value-not-admitted",
        severity=Severity.CRITICAL,
        message=f"conductor/test_repo_index.py: new test {nodeid!r} is classified "
        "'DELETE_CANDIDATE'; only CORE or INTENTIONAL_REDUNDANCY may be added",
        path="conductor/test_repo_index.py",
        evidence={"nodeid": nodeid},
    )


def _apply(
    waiver: ValueWaiverPolicy, *, base: str = BASE, today: date = TODAY
) -> list[Finding]:
    return apply_value_waivers([_finding()], (waiver,), base=base, today=today)


def _still_critical(findings: list[Finding]) -> None:
    assert [f.rule_id for f in findings] == ["new-test-value-not-admitted"]
    assert findings[0].severity is Severity.CRITICAL


def test_a_waiver_pinned_to_another_base_is_inert() -> None:
    _still_critical(_apply(_waiver(), base="e" * 40))


def test_an_expired_waiver_is_inert() -> None:
    _still_critical(_apply(_waiver(expires=TODAY - timedelta(days=1))))


def test_a_waiver_without_an_expiry_is_inert() -> None:
    _still_critical(_apply(_waiver(expires=None)))


def test_a_nodeid_matches_only_exactly() -> None:
    _still_critical(_apply(_waiver(nodeids=(NODEID[:-4],))))
    _still_critical(_apply(_waiver(nodeids=(NODEID + "_more",))))


def test_an_empty_nodeid_list_waives_nothing() -> None:
    _still_critical(_apply(_waiver(nodeids=())))


def test_a_waived_finding_is_reported_not_dropped() -> None:
    findings = _apply(_waiver())
    assert [f.rule_id for f in findings] == ["new-test-value-waived"]
    waived = findings[0]
    assert waived.severity is Severity.INFO
    assert waived.message.startswith("WAIVED conductor/test_repo_index.py: ")
    assert (
        REASON in waived.message and "approved by Tim on 2026-09-02" in waived.message
    )
    assert waived.evidence["nodeid"] == NODEID
    assert waived.evidence["waived_by"]["integration_base"] == BASE


def test_the_policy_rejects_an_empty_nodeid_list() -> None:
    with pytest.raises(PolicyError, match="non-empty"):
        _parse_value_waivers([_raw(nodeids=[])])


def test_the_policy_rejects_pattern_nodeids() -> None:
    with pytest.raises(PolicyError, match="exact, never patterns"):
        _parse_value_waivers([_raw(nodeids=["conductor/test_repo_index.py::test_*"])])


def test_the_policy_rejects_an_abbreviated_integration_base() -> None:
    with pytest.raises(PolicyError, match="full 40-hex"):
        _parse_value_waivers([_raw(integration_base=BASE[:12])])


def test_a_parsed_waiver_carries_its_expiry() -> None:
    (waiver,) = _parse_value_waivers([_raw()])
    assert waiver.expires == date(2026, 10, 2)
    assert waiver.approved_on == date(2026, 9, 2)
    assert waiver.nodeids == (NODEID,)


def test_the_gate_applies_an_active_policy_waiver(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """End to end: a DELETE_CANDIDATE new test waived in policy is a WAIVED line."""

    context = _gate_context(
        monkeypatch, tmp_path, inventory={PROBE_PATH: ["test_probe_legacy"]}
    )
    nodeid = f"{PROBE_PATH}::test_probe_new"
    receipt = context.snapshot / "receipt.json"
    receipt.write_text(
        json.dumps(
            {
                "test_value": {
                    "schema_version": VALUE_SCHEMA,
                    "status": "PASS",
                    "tests": [{"nodeid": nodeid, "classification": "DELETE_CANDIDATE"}],
                }
            }
        ),
        encoding="utf-8",
    )
    payload = {
        "status": "PASS",
        "checked_test_paths": [PROBE_PATH],
        "evidence": [{"path": PROBE_PATH, "receipt": "receipt.json"}],
        "missing_evidence": [],
        "malformed_receipts": [],
    }
    monkeypatch.setattr(
        "conductor.mutation_testing.verify_evidence", lambda *a, **k: payload
    )
    waiver = _waiver(
        integration_base=context.candidate.base_commit_oid,
        nodeids=(nodeid,),
        expires=date.today() + timedelta(days=30),
    )
    context = replace(context, policy=replace(context.policy, value_waivers=(waiver,)))
    result = check_mutation_evidence(context)
    assert [f.rule_id for f in result.findings] == ["new-test-value-waived"]
    assert result.status is CheckStatus.PASSED
    assert result.metrics["value_waiver_states"] == [
        {
            "integration_base": waiver.integration_base,
            "nodeids": [nodeid],
            "active": True,
            "reason": "",
        }
    ]
