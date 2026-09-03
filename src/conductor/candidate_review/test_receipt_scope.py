"""Receipt scope: only a new test definition demands a mutation campaign.

Historical mutation debt was declared complete on 2026-08-31, so touching a test
file the anchored inventory already covers must not re-open a campaign obligation
for whoever edited it. Each test here removes one condition of that scoping and
shows the CRITICAL finding either survives (new work) or is correctly absent
(historical work), so a mutant that drops the condition is caught.
"""

from __future__ import annotations

from dataclasses import replace
from pathlib import Path
from typing import Any, Sequence

import pytest

from conductor.candidate_review import verification as review_verification
from conductor.candidate_review.checks import ReviewContext, check_mutation_evidence
from conductor.candidate_review.model import CheckResult, CheckStatus
from conductor.candidate_review.verification import _receipt_required_paths
from conductor.test_candidate_review import _probe_source
from conductor.test_candidate_review_hardening import PROBE_PATH, _change, _gate_context

LEGACY = ["test_probe_legacy", "test_probe_older"]
OTHER_PATH = "research/tests/test_probe_other.py"


def _record_calls(monkeypatch: pytest.MonkeyPatch) -> list[list[str]]:
    """Capture exactly which paths the gate asks the evidence check about."""

    seen: list[list[str]] = []

    def _verify(
        _registry: Path, paths: Sequence[str], **_kwargs: Any
    ) -> dict[str, Any]:
        seen.append(list(paths))
        return {
            "status": "PASS" if not paths else "FAIL",
            "checked_test_paths": list(paths),
            "evidence": [],
            "missing_evidence": [
                {"path": path, "reason": "no registered campaign ranks this test file"}
                for path in paths
            ],
            "malformed_receipts": [],
        }

    monkeypatch.setattr("conductor.mutation_testing.verify_evidence", _verify)
    return seen


def _historical(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, labels: list[str] | None = None
) -> ReviewContext:
    """A candidate that edits one test file whose definitions are all inventoried."""

    ctx = _gate_context(monkeypatch, tmp_path, inventory={PROBE_PATH: LEGACY})
    (ctx.snapshot / PROBE_PATH).write_text(
        _probe_source(LEGACY if labels is None else labels), encoding="utf-8"
    )
    return ctx


def _rules(result: CheckResult) -> list[str]:
    return [finding.rule_id for finding in result.findings]


def test_a_touched_historical_test_file_needs_no_campaign(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """The whole point: editing an inventoried test file demands no receipt."""

    seen = _record_calls(monkeypatch)
    result = check_mutation_evidence(_historical(monkeypatch, tmp_path))
    assert _rules(result) == []
    assert result.status is CheckStatus.PASSED
    assert seen == [[]]
    assert result.metrics["receipt_exempt_tests"] == [PROBE_PATH]


def test_a_new_test_definition_still_demands_a_receipt(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """New work is still gated: one uninventoried def re-arms the whole file."""

    seen = _record_calls(monkeypatch)
    context = _historical(monkeypatch, tmp_path, labels=[*LEGACY, "test_probe_new"])
    result = check_mutation_evidence(context)
    assert _rules(result) == [
        "missing-mutation-receipt",
        "new-test-value-not-admitted",
    ]
    assert result.status is CheckStatus.FAILED
    assert seen == [[PROBE_PATH]]
    assert result.metrics["receipt_exempt_tests"] == []


def test_an_unreadable_inventory_fails_closed(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """Nothing is historical without a readable anchor: demand every receipt."""

    seen = _record_calls(monkeypatch)
    context = _historical(monkeypatch, tmp_path)
    monkeypatch.setattr(review_verification, "GRANDFATHER_INVENTORY_SHA256", "0" * 64)
    result = check_mutation_evidence(context)
    assert _rules(result) == [
        "missing-mutation-receipt",
        "grandfather-inventory-invalid",
    ]
    assert seen == [[PROBE_PATH]]
    assert result.metrics["receipt_exempt_tests"] == []


def test_exempt_and_gated_files_are_separated_in_one_candidate(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """A mixed candidate asks about the new file only, and reports the other exempt."""

    seen = _record_calls(monkeypatch)
    context = _gate_context(
        monkeypatch, tmp_path, inventory={PROBE_PATH: LEGACY, OTHER_PATH: LEGACY}
    )
    (context.snapshot / PROBE_PATH).write_text(_probe_source(LEGACY), encoding="utf-8")
    other = context.snapshot / OTHER_PATH
    other.parent.mkdir(parents=True, exist_ok=True)
    other.write_text(_probe_source([*LEGACY, "test_probe_new"]), encoding="utf-8")
    context = replace(
        context,
        candidate=replace(
            context.candidate, changes=(_change(PROBE_PATH), _change(OTHER_PATH))
        ),
    )
    result = check_mutation_evidence(context)
    assert seen == [[OTHER_PATH]]
    assert _rules(result) == [
        "missing-mutation-receipt",
        "new-test-value-not-admitted",
    ]
    assert {finding.path for finding in result.findings} == {OTHER_PATH}
    assert result.metrics["receipt_exempt_tests"] == [PROBE_PATH]


def test_only_paths_with_gated_nodeids_are_required() -> None:
    """Selection is by gated nodeids, not by membership in the changed set."""

    gated = {"a/test_new.py": ("a/test_new.py::test_x",)}
    assert _receipt_required_paths(["a/test_new.py", "b/test_old.py"], gated) == [
        "a/test_new.py"
    ]


def test_an_unevaluable_inventory_requires_every_changed_test() -> None:
    """``None`` is the fail-closed signal, not "nothing is gated"."""

    paths = ["a/test_new.py", "b/test_old.py"]
    assert _receipt_required_paths(paths, None) == paths


def test_an_empty_nodeid_tuple_does_not_gate_a_path() -> None:
    """A present key with no gated nodeids is still historical."""

    assert _receipt_required_paths(["b/test_old.py"], {"b/test_old.py": ()}) == []
