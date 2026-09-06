"""The interned nodeid table that keeps mutation receipts under the artifact limit.

A campaign records every ranked test's outcome for every mutant, so before interning
the same ~70-character nodeid was written once per (test, mutant) pair. The campaign
`claude_equivalence_probe_20260830` reached 3591 such entries and a 1,126,240-byte
receipt against `max_file_bytes = 1000000`. These tests pin the property that makes
that safe to fix: the table removes repeated strings and nothing else.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from conductor import mutation_testing
from conductor.mutation_testing_support import (
    ReceiptEncodingError,
    expand_test_attribution,
    intern_test_attribution,
)

# The native verifier's fixture lives with the framework's own tests. Reusing it
# keeps one definition of what a PASS receipt looks like; a local copy would drift
# from the shape the gate actually reads.
from conductor.test_mutation_testing import (
    _temporary_campaign,
    _write_pass_receipt,
    _write_registry,
)

ALPHA = "conductor/test_equivalence_probe.py::test_alpha"
BETA = "conductor/test_equivalence_probe.py::test_beta"
GAMMA = "conductor/test_equivalence_probe.py::test_gamma"


def _report(**overrides: Any) -> dict[str, Any]:
    report: dict[str, Any] = {
        "status": "COMPLETE",
        "tests": {
            ALPHA: {"cases": 1, "duration_seconds": 0.259, "outcome": "PASSED"},
            BETA: {"cases": 2, "duration_seconds": 1.488, "outcome": "FAILED"},
        },
        "failed_nodeids": [BETA],
        "missing_nodeids": [],
        "unmapped_cases": [],
    }
    report.update(overrides)
    return report


def test_one_table_is_shared_by_every_mutant_in_the_receipt() -> None:
    """A per-mutant table saves nothing: the repetition IS across mutants."""

    receipt: dict[str, Any] = {"mutants": []}
    first = intern_test_attribution(receipt, _report())
    second = intern_test_attribution(
        receipt,
        _report(
            tests={
                BETA: {"cases": 2, "duration_seconds": 1.5, "outcome": "PASSED"},
                GAMMA: {"cases": 1, "duration_seconds": 0.1, "outcome": "PASSED"},
            },
            failed_nodeids=[],
        ),
    )

    assert sorted(first["tests"]) == ["0", "1"]
    assert first["failed_nodeids"] == [1]
    assert sorted(second["tests"]) == ["1", "2"]


def test_interning_preserves_every_outcome_case_count_and_duration() -> None:
    """`test_value` computes domination from the matrix and ties on the runtime."""

    receipt: dict[str, Any] = {}
    report = _report()
    interned = intern_test_attribution(receipt, report)

    assert interned != report
    assert expand_test_attribution(receipt, interned) == report


def test_a_receipt_written_before_interning_expands_unchanged() -> None:
    """One reader has to serve ~460 receipts recorded under the old encoding."""

    legacy = _report()

    assert expand_test_attribution({"mutants": []}, legacy) == legacy


def test_an_index_the_table_cannot_resolve_refuses_instead_of_guessing() -> None:
    """A short table is a corrupt receipt, never a test that did not run."""

    receipt: dict[str, Any] = {}
    intern_test_attribution(
        receipt,
        {
            "status": "COMPLETE",
            "tests": {
                ALPHA: {"cases": 1, "duration_seconds": 0.1, "outcome": "PASSED"}
            },
            "failed_nodeids": [],
            "missing_nodeids": [],
        },
    )

    with pytest.raises(ReceiptEncodingError, match="outside a table of 1"):
        expand_test_attribution(receipt, {"failed_nodeids": [1]})


def test_cases_outside_the_ranked_scope_are_never_interned() -> None:
    """`unmapped_cases` is an unbounded namespace, not the campaign's ranked set."""

    receipt: dict[str, Any] = {}
    interned = intern_test_attribution(
        receipt,
        _report(
            unmapped_cases=["tests/other.py::test_stray"],
            unranked_failures=["tests/other.py::test_loud"],
        ),
    )

    assert interned["failed_nodeids"] == [1]
    assert interned["unmapped_cases"] == ["tests/other.py::test_stray"]
    assert interned["unranked_failures"] == ["tests/other.py::test_loud"]


def test_the_native_verifier_accepts_an_interned_receipt(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """The table is a receipt-level field of its own, and Rust reads receipts too.

    `verify_evidence` refuses outright when `conductor._native` is unimportable and
    has no Python fallback, so a PASS here is the native verifier's verdict on a
    receipt carrying the new encoding.
    """

    campaign = _temporary_campaign(tmp_path)
    registry = _write_registry(tmp_path)
    monkeypatch.setattr(mutation_testing, "load_campaign", lambda *_a, **_k: campaign)
    _write_pass_receipt(tmp_path, campaign)
    receipt_path = tmp_path / "receipts/pass.json"
    payload = json.loads(receipt_path.read_text(encoding="utf-8"))
    nodeid = campaign.ranked_tests[0].nodeid
    for row in payload["mutants"]:
        row["test_attribution"] = intern_test_attribution(
            payload,
            {
                "status": "COMPLETE",
                "tests": {
                    nodeid: {"cases": 1, "duration_seconds": 0.5, "outcome": "PASSED"}
                },
                "failed_nodeids": [],
                "missing_nodeids": [],
                "unmapped_cases": [],
            },
        )
    assert payload["test_nodeid_table"] == [nodeid]
    receipt_path.write_text(json.dumps(payload, sort_keys=True), encoding="utf-8")

    result = mutation_testing.verify_evidence(
        registry, ["test_one.py"], repo_root=tmp_path
    )

    assert result["status"] == "PASS"
    assert result["missing_evidence"] == []
