"""Nodeid-exact, base-bound value waivers: every inert condition still bites.

Loosening a gate needs a measurement: each test here removes one condition of the
waiver (base binding, expiry, exact match, non-empty list, reporting) and shows the
CRITICAL finding survives, so a mutant that drops that condition is caught.
"""

from __future__ import annotations

import json
import time
from dataclasses import replace
from datetime import date, timedelta
from pathlib import Path

import pytest

from conductor.candidate_review.checks import ReviewContext, check_mutation_evidence
from conductor.candidate_review.git_source import resolve_candidate, run_git
from conductor.candidate_review.model import (
    CheckStatus,
    Finding,
    ReviewReceipt,
    Severity,
)
from conductor.candidate_review.policy import (
    PolicyError,
    ValueWaiverPolicy,
    _parse_value_waivers,
)
from conductor.candidate_review.reporters import human_summary
from conductor.candidate_review.value_waivers import (
    WAIVED_RULE,
    apply_value_waivers,
)
from conductor.candidate_review.verification import _mutation_evidence_result
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


def _git(repo: Path, *args: str) -> str:
    return run_git(repo, list(args)).stdout.decode().strip()


def _branched_repo(tmp_path: Path) -> tuple[Path, str, str]:
    """A repo whose lane branch carries one commit past the `master` integration line."""

    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init", "-q", "-b", "master")
    _git(repo, "config", "user.email", "gate@example.invalid")
    _git(repo, "config", "user.name", "gate")
    _git(repo, "config", "commit.gpgsign", "false")
    _git(repo, "config", "core.hooksPath", str(tmp_path / "no-hooks"))
    (repo / "a.txt").write_text("one\n", encoding="utf-8")
    _git(repo, "add", "a.txt")
    _git(repo, "commit", "-qm", "integration base")
    integration = _git(repo, "rev-parse", "HEAD")
    _git(repo, "checkout", "-q", "-b", "lane")
    (repo / "a.txt").write_text("two\n", encoding="utf-8")
    _git(repo, "commit", "-qam", "lane commit")
    head = _git(repo, "rev-parse", "HEAD")
    (repo / "b.txt").write_text("staged\n", encoding="utf-8")
    _git(repo, "add", "b.txt")
    return repo, integration, head


def test_an_index_candidate_binds_waivers_to_the_integration_base(
    tmp_path: Path,
) -> None:
    """The staged diff is still taken against HEAD; the waiver base is the merge base."""

    repo, integration, head = _branched_repo(tmp_path)
    assert integration != head
    candidate = resolve_candidate(repo, kind="index")
    assert candidate.base_commit_oid == head
    assert candidate.integration_base_oid == integration
    assert candidate.waiver_base == integration
    assert "master" in candidate.integration_base_detail
    assert [change.path for change in candidate.changes] == ["b.txt"]


def test_an_integration_base_that_is_not_an_ancestor_is_refused(
    tmp_path: Path,
) -> None:
    """A base off this history binds nothing: no waiver base, so every waiver is inert."""

    repo, _integration, _head = _branched_repo(tmp_path)
    _git(repo, "checkout", "-q", "--orphan", "unrelated")
    (repo / "c.txt").write_text("elsewhere\n", encoding="utf-8")
    _git(repo, "add", "c.txt")
    _git(repo, "commit", "-qm", "unrelated root")
    unrelated = _git(repo, "rev-parse", "HEAD")
    _git(repo, "checkout", "-q", "lane")
    candidate = resolve_candidate(repo, kind="index", base_ref=unrelated)
    assert candidate.base_commit_oid == unrelated
    assert candidate.integration_base_oid is None
    assert "is not an ancestor of" in candidate.integration_base_detail
    assert candidate.waiver_base is None
    _still_critical(
        apply_value_waivers(
            [_finding()],
            (_waiver(integration_base=unrelated),),
            base=candidate.waiver_base,
            today=TODAY,
        )
    )


def _gate_context_with_receipt(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> tuple[ReviewContext, str]:
    """A gate context whose one new test is classified DELETE_CANDIDATE."""

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
    return context, nodeid


def _with_waiver(
    context: ReviewContext, waiver: ValueWaiverPolicy, **candidate_fields: object
) -> ReviewContext:
    return replace(
        context,
        candidate=replace(context.candidate, **candidate_fields),
        policy=replace(context.policy, value_waivers=(waiver,)),
    )


def test_the_gate_binds_waivers_to_the_integration_base_not_head(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """A waiver pinned to the integration base applies while HEAD has moved past it."""

    context, nodeid = _gate_context_with_receipt(monkeypatch, tmp_path)
    integration = "f" * 40
    assert context.candidate.base_commit_oid != integration
    waiver = _waiver(
        integration_base=integration,
        nodeids=(nodeid,),
        expires=date.today() + timedelta(days=30),
    )
    result = check_mutation_evidence(
        _with_waiver(
            context,
            waiver,
            integration_base_oid=integration,
            integration_base_detail="merge base with origin/master",
        )
    )
    assert [f.rule_id for f in result.findings] == ["new-test-value-waived"]
    assert result.status is CheckStatus.PASSED
    assert result.metrics["value_waiver_base"] == {
        "commit": integration,
        "resolved_by": "merge base with origin/master",
    }


def test_an_expired_waiver_is_inert_at_the_integration_base(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """Rebinding the base does not loosen the expiry: an expired waiver still blocks."""

    context, nodeid = _gate_context_with_receipt(monkeypatch, tmp_path)
    integration = "f" * 40
    waiver = _waiver(
        integration_base=integration,
        nodeids=(nodeid,),
        expires=date.today() - timedelta(days=1),
    )
    result = check_mutation_evidence(
        _with_waiver(
            context,
            waiver,
            integration_base_oid=integration,
            integration_base_detail="merge base with origin/master",
        )
    )
    assert [f.rule_id for f in result.findings] == ["new-test-value-not-admitted"]
    assert result.status is CheckStatus.FAILED
    assert result.metrics["value_waiver_states"][0]["active"] is False


def test_the_gate_applies_an_active_policy_waiver(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """End to end: a DELETE_CANDIDATE new test waived in policy is a WAIVED line."""

    context, nodeid = _gate_context_with_receipt(monkeypatch, tmp_path)
    waiver = _waiver(
        integration_base=context.candidate.base_commit_oid,
        nodeids=(nodeid,),
        expires=date.today() + timedelta(days=30),
    )
    result = check_mutation_evidence(_with_waiver(context, waiver))
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


def test_a_range_candidate_keeps_its_merge_base_as_the_waiver_base(
    tmp_path: Path,
) -> None:
    """The ancestor check leaves a legitimately derived base alone."""

    repo, integration, _head = _branched_repo(tmp_path)
    ranged = resolve_candidate(repo, kind="range", target_ref="lane", base_ref="master")
    assert ranged.base_commit_oid == integration
    assert ranged.waiver_base == integration
    assert ranged.integration_base_detail == "range candidate base"
    committed = resolve_candidate(repo, kind="commit", target_ref="lane")
    assert committed.waiver_base == integration
    assert committed.integration_base_detail == "commit candidate base"


def test_a_commit_candidate_refuses_a_base_off_another_history(tmp_path: Path) -> None:
    """`--base-ref` on a commit candidate cannot smuggle in an unrelated waiver base."""

    repo, _integration, _head = _branched_repo(tmp_path)
    _git(repo, "checkout", "-q", "--orphan", "unrelated")
    (repo / "c.txt").write_text("elsewhere\n", encoding="utf-8")
    _git(repo, "add", "c.txt")
    _git(repo, "commit", "-qm", "unrelated root")
    unrelated = _git(repo, "rev-parse", "HEAD")
    _git(repo, "checkout", "-q", "lane")
    candidate = resolve_candidate(
        repo, kind="commit", target_ref="lane", base_ref=unrelated
    )
    assert candidate.base_commit_oid == unrelated
    assert candidate.integration_base_oid is None
    assert "is not an ancestor of" in candidate.integration_base_detail
    assert candidate.waiver_base is None
    _still_critical(
        apply_value_waivers(
            [_finding()],
            (_waiver(integration_base=unrelated),),
            base=candidate.waiver_base,
            today=TODAY,
        )
    )


def _report_receipt(findings: list[dict[str, object]]) -> ReviewReceipt:
    return ReviewReceipt(
        schema_version=1,
        receipt_id="rcpt-0001",
        receipt_digest="0" * 64,
        surface="manual",
        profile="fast",
        decision="pass",
        candidate={"tree_oid": "a" * 40},
        policy={},
        engine={},
        graph={},
        bypass={},
        timings={"duration_ms": 12},
        cache={"hits": 0},
        baselines=[],
        checks=[],
        findings=findings,
        binding="candidate",
    )


def _report_finding(rule_id: str, severity: str, message: str) -> dict[str, object]:
    return {
        "check_id": "mutation-evidence",
        "rule_id": rule_id,
        "severity": severity,
        "message": message,
        "path": "conductor/test_repo_index.py",
        "line": None,
    }


def _waived_report_findings() -> list[dict[str, object]]:
    return [
        _report_finding("policy-drift", "critical", "policy fingerprint drifted"),
        _report_finding(WAIVED_RULE, "info", f"WAIVED {NODEID} -- {REASON}"),
        _report_finding(WAIVED_RULE, "info", "WAIVED a second value finding"),
    ]


def test_the_human_report_prints_every_waived_line() -> None:
    """A waived finding a developer cannot see is a gate they cannot audit."""

    summary = human_summary(_report_receipt(_waived_report_findings()))
    assert f"INFO mutation-evidence/{WAIVED_RULE}" in summary
    assert REASON in summary
    assert "WAIVED a second value finding" in summary


def test_the_human_report_counts_the_waived_findings() -> None:
    """`waived=N` counts exactly the findings the waivers suppressed, and no others."""

    findings = _waived_report_findings()
    waived = [f for f in findings if f["rule_id"] == WAIVED_RULE]
    summary = human_summary(_report_receipt(findings))
    assert f"blocking=1 advisory=0 waived={len(waived)} " in summary


def test_the_admission_finding_names_the_test_nodeid(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """The nodeid a waiver matches on is carried as evidence, not reparsed from prose.

    No waiver is configured, so only the admission site is under test: this fails when
    the finding stops naming its nodeid, and for no other reason.
    """

    context, nodeid = _gate_context_with_receipt(monkeypatch, tmp_path)
    result = check_mutation_evidence(_without_waivers(context))
    assert [f.rule_id for f in result.findings] == ["new-test-value-not-admitted"]
    assert result.findings[0].evidence["nodeid"] == nodeid


def _without_waivers(context: ReviewContext) -> ReviewContext:
    return replace(context, policy=replace(context.policy, value_waivers=()))


def test_an_active_waiver_rewrites_the_finding_it_matches(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """Waiver application is a step of its own, separate from how findings are built.

    The seam is fed a ready-made value finding, so the admission site cannot matter;
    the integration base is deliberately the review base, so which of the two the
    activation reads cannot matter; the status is not asserted, so the pass promotion
    cannot matter; and only the disappearance of the un-waived finding is asserted, so
    how it is re-reported cannot matter. What is left is the call itself.
    """

    context = _gate_context(
        monkeypatch, tmp_path, inventory={PROBE_PATH: ["test_probe_legacy"]}
    )
    base = context.candidate.base_commit_oid
    assert base is not None
    waiver = _waiver(integration_base=base, expires=date.today() + timedelta(days=30))
    result = _mutation_evidence_result(
        _with_waiver(
            context,
            waiver,
            integration_base_oid=base,
            integration_base_detail="merge base with origin/master",
        ),
        time.perf_counter(),
        [_finding()],
        [PROBE_PATH],
        {},
    )
    assert "new-test-value-not-admitted" not in [f.rule_id for f in result.findings]


def test_a_result_made_only_of_waived_lines_passes(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """A finding already re-reported as WAIVED must not still fail the check.

    No waiver is configured and the finding arrives already waived, so neither the
    admission site nor the application step can change the outcome: this fails only
    when the pass promotion is gone.
    """

    context = _gate_context(
        monkeypatch, tmp_path, inventory={PROBE_PATH: ["test_probe_legacy"]}
    )
    already_waived = Finding(
        check_id="mutation-evidence",
        rule_id=WAIVED_RULE,
        severity=Severity.INFO,
        message=f"WAIVED {NODEID} -- {REASON}",
        path="conductor/test_repo_index.py",
    )
    result = _mutation_evidence_result(
        _without_waivers(context),
        time.perf_counter(),
        [already_waived],
        [PROBE_PATH],
        {},
    )
    assert [f.rule_id for f in result.findings] == ["new-test-value-waived"]
    assert result.status is CheckStatus.PASSED
