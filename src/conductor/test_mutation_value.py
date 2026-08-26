from __future__ import annotations

import copy
from contextlib import contextmanager
from pathlib import Path
from types import SimpleNamespace

import pytest

from conductor import mutation_testing, mutation_value


def _spec(
    *,
    tests: list[mutation_value.ValueTest] | None = None,
    contracts: list[mutation_value.ValueContract] | None = None,
    mutation_contracts: dict[str, str] | None = None,
) -> mutation_value.ValueAnalysisSpec:
    return mutation_value.ValueAnalysisSpec(
        adapter=mutation_value.ADAPTER,
        baseline_repetitions=2,
        contracts=tuple(
            contracts
            or [
                mutation_value.ValueContract(
                    "contract", "critical", ("conductor/mutation_value.py",)
                )
            ]
        ),
        tests=tuple(
            tests
            or [
                mutation_value.ValueTest(
                    "conductor/test_mutation_value.py::test_contract",
                    "contract",
                    False,
                )
            ]
        ),
        mutation_contracts=mutation_contracts or {"mutant": "contract"},
    )


def _report(outcomes: dict[str, tuple[str, float]]) -> dict[str, object]:
    return {
        "status": "COMPLETE",
        "tests": {
            nodeid: {
                "outcome": outcome,
                "duration_seconds": duration,
                "cases": 1,
            }
            for nodeid, (outcome, duration) in outcomes.items()
        },
        "missing_nodeids": [],
        "unmapped_cases": [],
    }


def _invalid_value_payloads(
    valid: dict[str, object], nodeid: str
) -> list[tuple[dict[str, object], str, list[str]]]:
    cases: list[tuple[dict[str, object], str, list[str]]] = []

    def add(payload: dict[str, object], match: str) -> None:
        cases.append((payload, match, ["mutant"]))

    for field, value, match in (
        ("enabled", False, "enabled must be true"),
        ("adapter", "unsupported", "adapter must be"),
        ("baseline_repetitions", 1, "integer in"),
        ("required_contracts", [], "non-empty list"),
        ("tests", [], "non-empty list"),
        ("mutation_contracts", [], "must be an object"),
    ):
        payload = copy.deepcopy(valid)
        payload[field] = value
        add(payload, match)
    contract_cases = (
        ("id", "", "non-empty string"),
        ("active_paths", [], "must be non-empty"),
        ("active_paths", ["/absolute.py"], "repository-relative"),
        ("active_paths", ["missing.py"], "unbound active paths"),
    )
    for field, value, match in contract_cases:
        payload = copy.deepcopy(valid)
        payload["required_contracts"][0][field] = value
        add(payload, match)
    payload = copy.deepcopy(valid)
    payload["required_contracts"].append(
        copy.deepcopy(payload["required_contracts"][0])
    )
    add(payload, "duplicate ids")
    payload = copy.deepcopy(valid)
    payload["tests"][0]["intentional_redundancy"] = "yes"
    add(payload, "must be boolean")
    payload = copy.deepcopy(valid)
    payload["tests"][0]["nodeid"] = "conductor/test_mutation_value.py::other"
    add(payload, "exactly match ranked_tests")
    payload = copy.deepcopy(valid)
    payload["tests"][0]["contract_id"] = "unknown"
    add(payload, "unknown contracts")
    payload = copy.deepcopy(valid)
    payload["mutation_contracts"] = {"other": "contract"}
    add(payload, "exactly match planned mutations")
    payload = copy.deepcopy(valid)
    payload["mutation_contracts"] = {"mutant": "unknown"}
    add(payload, "unknown contracts")
    payload = copy.deepcopy(valid)
    payload["required_contracts"].append(
        {"id": "second", "criticality": "high", "active_paths": ["second.py"]}
    )
    payload["tests"].append({"nodeid": nodeid, "contract_id": "second"})
    payload["tests"][0]["nodeid"] = "conductor/test_mutation_value.py::first"
    cases.append((payload, "every contract needs", ["mutant"]))
    return cases


def test_value_spec_requires_high_risk_contracts_bound_to_production() -> None:
    nodeid = "conductor/test_mutation_value.py::test_contract"
    payload = {
        "enabled": True,
        "adapter": "pytest-junit",
        "baseline_repetitions": 2,
        "required_contracts": [
            {
                "id": "contract",
                "criticality": "critical",
                "active_paths": ["conductor/mutation_value.py"],
            }
        ],
        "tests": [{"nodeid": nodeid, "contract_id": "contract"}],
        "mutation_contracts": {"mutant": "contract"},
    }
    spec = mutation_value.load_value_analysis(
        payload,
        ranked_nodeids=[nodeid],
        mutation_ids=["mutant"],
        source_paths=[
            "conductor/mutation_value.py",
            "conductor/test_mutation_value.py",
        ],
    )
    assert spec is not None
    assert spec.contracts[0].active_paths == ("conductor/mutation_value.py",)
    assert (
        mutation_value.load_value_analysis(
            None, ranked_nodeids=[], mutation_ids=[], source_paths=[]
        )
        is None
    )

    low_risk = copy.deepcopy(payload)
    low_risk["required_contracts"][0]["criticality"] = "low"
    with pytest.raises(mutation_value.ValueEvidenceError, match="critical or high"):
        mutation_value.load_value_analysis(
            low_risk,
            ranked_nodeids=[nodeid],
            mutation_ids=["mutant"],
            source_paths=[
                "conductor/mutation_value.py",
                "conductor/test_mutation_value.py",
            ],
        )
    test_bound = copy.deepcopy(payload)
    test_bound["required_contracts"][0]["active_paths"] = [
        "conductor/test_mutation_value.py"
    ]
    with pytest.raises(
        mutation_value.ValueEvidenceError, match="tests, not production"
    ):
        mutation_value.load_value_analysis(
            test_bound,
            ranked_nodeids=[nodeid],
            mutation_ids=["mutant"],
            source_paths=[
                "conductor/mutation_value.py",
                "conductor/test_mutation_value.py",
            ],
        )
    with pytest.raises(mutation_value.ValueEvidenceError, match="must be an object"):
        mutation_value.load_value_analysis(
            [], ranked_nodeids=[nodeid], mutation_ids=["mutant"], source_paths=[]
        )
    for invalid, match, mutation_ids in _invalid_value_payloads(payload, nodeid):
        source_paths = [
            "conductor/mutation_value.py",
            "conductor/test_mutation_value.py",
            "second.py",
        ]
        ranked = [
            test["nodeid"]
            for test in invalid.get("tests", [])
            if isinstance(test, dict)
        ]
        if not any(item.endswith("::first") for item in ranked):
            ranked = [nodeid]
        with pytest.raises(mutation_value.ValueEvidenceError, match=match):
            mutation_value.load_value_analysis(
                invalid,
                ranked_nodeids=ranked,
                mutation_ids=mutation_ids,
                source_paths=source_paths,
            )


def test_junit_attribution_maps_parameterized_failures_and_incomplete_reports(
    tmp_path: Path,
) -> None:
    nodeids = [
        "conductor/test_mutation_value.py::test_alpha",
        "conductor/test_mutation_value.py::TestGroup::test_beta",
    ]
    report = tmp_path / "report.xml"
    report.write_text(
        """<?xml version="1.0" encoding="utf-8"?>
<testsuites><testsuite tests="3">
  <testcase classname="conductor.test_mutation_value" name="test_alpha[a]" time="0.2" />
  <testcase classname="conductor.test_mutation_value" name="test_alpha[b]" time="0.3"><failure /></testcase>
  <testcase classname="conductor.test_mutation_value.TestGroup" name="test_beta" time="0.1" />
</testsuite></testsuites>""",
        encoding="utf-8",
    )
    parsed = mutation_value.parse_pytest_junit(report, nodeids)
    assert parsed["status"] == "COMPLETE"
    assert parsed["tests"][nodeids[0]] == {
        "outcome": "FAILED",
        "duration_seconds": 0.5,
        "cases": 2,
    }
    assert parsed["tests"][nodeids[1]]["outcome"] == "PASSED"

    incomplete = mutation_value.parse_pytest_junit(report, [*nodeids, "x.py::test_x"])
    assert incomplete["status"] == "INCOMPLETE"
    assert incomplete["missing_nodeids"] == ["x.py::test_x"]
    assert mutation_value.pytest_junit_argv(("pytest",), report)[-1].startswith(
        "--junitxml="
    )
    with pytest.raises(mutation_value.ValueEvidenceError, match="must not set"):
        mutation_value.pytest_junit_argv(("pytest", "--junitxml=old.xml"), report)
    with pytest.raises(mutation_value.ValueEvidenceError, match="Python nodeid"):
        mutation_value.parse_pytest_junit(report, ["native_test.cpp"])
    with pytest.raises(mutation_value.ValueEvidenceError, match="cannot parse"):
        mutation_value.parse_pytest_junit(tmp_path / "missing.xml", nodeids)

    edge_nodeid = "conductor/test_mutation_value.py::test_edge"
    report.write_text(
        """<testsuite>
<testcase classname="conductor.test_mutation_value" name="test_edge[a]" time="bad"><skipped /></testcase>
<testcase classname="conductor.test_mutation_value" name="test_edge[b]" time="0"><error /></testcase>
<testcase classname="unmapped" name="test_other" time="0" />
</testsuite>""",
        encoding="utf-8",
    )
    edge = mutation_value.parse_pytest_junit(report, [edge_nodeid])
    assert edge["status"] == "INCOMPLETE"
    assert edge["tests"][edge_nodeid]["outcome"] == "ERROR"
    result, attribution = mutation_value.collect_pytest_junit_batch(
        argv=("pytest",),
        report_path=tmp_path / "absent" / "report.xml",
        ranked_nodeids=nodeids,
        run_command=lambda _argv: "ran",
    )
    assert result == "ran"
    assert attribution["status"] == "INCOMPLETE"


def test_value_analysis_selects_core_and_flags_merge_and_delete_candidates() -> None:
    fast = "conductor/test_mutation_value.py::test_fast"
    slow = "conductor/test_mutation_value.py::test_slow"
    merge = "conductor/test_mutation_value.py::test_merge"
    empty = "conductor/test_mutation_value.py::test_empty"
    spec = _spec(
        tests=[
            mutation_value.ValueTest(fast, "contract", False),
            mutation_value.ValueTest(slow, "contract", True),
            mutation_value.ValueTest(merge, "contract", False),
            mutation_value.ValueTest(empty, "contract", False),
        ]
    )
    baseline = {
        fast: ("PASSED", 0.1),
        slow: ("PASSED", 0.4),
        merge: ("PASSED", 0.5),
        empty: ("PASSED", 0.2),
    }
    mutant = {
        fast: ("FAILED", 0.1),
        slow: ("FAILED", 0.4),
        merge: ("FAILED", 0.5),
        empty: ("PASSED", 0.2),
    }
    result = mutation_value.analyze_test_value(
        spec,
        baseline_reports=[_report(baseline), _report(baseline)],
        mutant_reports={"mutant": _report(mutant)},
        mutant_outcomes={"mutant": "KILLED"},
    )
    assert result["status"] == "PASS"
    assert result["retained_core"] == [fast]
    rows = {row["nodeid"]: row for row in result["tests"]}
    assert rows[fast]["classification"] == "CORE"
    assert rows[slow]["classification"] == "INTENTIONAL_REDUNDANCY"
    assert rows[slow]["dominated_by"] == [fast]
    assert rows[merge]["classification"] == "MERGE"
    assert rows[empty]["classification"] == "DELETE_CANDIDATE"


def test_value_analysis_fails_closed_on_flakes_and_cross_contract_kills() -> None:
    first = "conductor/test_mutation_value.py::test_first"
    second = "conductor/test_mutation_value.py::test_second"
    spec = _spec(
        contracts=[
            mutation_value.ValueContract(
                "first", "critical", ("conductor/mutation_value.py",)
            ),
            mutation_value.ValueContract(
                "second", "high", ("conductor/mutation_testing.py",)
            ),
        ],
        tests=[
            mutation_value.ValueTest(first, "first", False),
            mutation_value.ValueTest(second, "second", False),
        ],
        mutation_contracts={"first_mutant": "first", "second_mutant": "second"},
    )
    clean = {first: ("PASSED", 0.1), second: ("PASSED", 0.1)}
    flaky = {first: ("FAILED", 0.1), second: ("PASSED", 0.1)}
    wrong = {first: ("PASSED", 0.1), second: ("FAILED", 0.1)}
    result = mutation_value.analyze_test_value(
        spec,
        baseline_reports=[_report(clean), _report(flaky)],
        mutant_reports={
            "first_mutant": _report(wrong),
            "second_mutant": _report(wrong),
        },
        mutant_outcomes={"first_mutant": "KILLED", "second_mutant": "KILLED"},
    )
    assert result["status"] == "FAIL_CLOSED"
    assert any("baseline instability" in error for error in result["errors"])
    assert any(
        "first_mutant" in error and "contract 'first'" in error
        for error in result["errors"]
    )
    incomplete = mutation_value.analyze_test_value(
        spec,
        baseline_reports=[{"status": "INCOMPLETE", "tests": None}],
        mutant_reports={
            "first_mutant": {"status": "INCOMPLETE"},
            "second_mutant": {"status": "COMPLETE", "tests": None},
        },
        mutant_outcomes={"first_mutant": "SURVIVED", "second_mutant": "KILLED"},
    )
    assert incomplete["status"] == "FAIL_CLOSED"
    assert incomplete["retained_core"] == []
    assert any("repetition count mismatch" in error for error in incomplete["errors"])
    assert any("attribution is incomplete" in error for error in incomplete["errors"])
    assert any("has no test map" in error for error in incomplete["errors"])


def test_runner_value_analysis_scales_with_mutants_not_tests(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    nodeid = "conductor/test_mutation_value.py::test_contract"
    spec = _spec()
    manifest = tmp_path / "campaign.json"
    manifest.write_text("{}\n", encoding="utf-8")
    patch = tmp_path / "mutant.patch"
    patch.write_text("patch\n", encoding="utf-8")
    campaign = mutation_testing.Campaign(
        manifest_path=manifest,
        manifest_sha256="0" * 64,
        campaign_id="value",
        title="value",
        language="python",
        mutation_engine="reviewed_unified_diff",
        expected_mutations=1,
        source_sha256={},
        ranked_tests=(mutation_testing.RankedTest(1, nodeid, "contract", "critical"),),
        planned_mutations=(
            mutation_testing.PlannedMutation(
                "mutant", "source.py", "contract", "defect", (nodeid,)
            ),
        ),
        mutations=(
            mutation_testing.Mutation(
                "mutant", patch, "0" * 64, ("source.py",), (nodeid,)
            ),
        ),
        test_argv=("python", "-m", "pytest", nodeid),
        timeout_seconds=10,
        blocked_process_substrings=(),
        poll_seconds=1,
        environment={},
        host_read_dependencies=(),
        value_analysis=spec,
    )
    calls: list[str] = []

    @contextmanager
    def snapshot(_repo: Path):
        yield SimpleNamespace(worktree=tmp_path)

    def run_batch(
        _campaign: mutation_testing.Campaign,
        *,
        snapshot_root: Path,
        report_name: str,
    ):
        del snapshot_root
        calls.append(report_name)
        outcome = "FAILED" if report_name.startswith("mutant") else "PASSED"
        return (
            mutation_testing.CommandResult(
                returncode=1 if outcome == "FAILED" else 0,
                timed_out=False,
                duration_seconds=0.01,
                stdout_tail="",
                stderr_tail="",
            ),
            _report({nodeid: (outcome, 0.01)}),
        )

    monkeypatch.setattr(
        mutation_testing, "inspect_campaign", lambda *_a, **_k: {"status": "READY"}
    )
    monkeypatch.setattr(mutation_testing, "_wait_for_idle", lambda *_a, **_k: [])
    monkeypatch.setattr(mutation_testing, "isolated_snapshot", snapshot)
    monkeypatch.setattr(mutation_testing, "source_drift", lambda *_a, **_k: [])
    monkeypatch.setattr(mutation_testing, "_link_mutation_patches", lambda *_a: None)
    monkeypatch.setattr(mutation_testing, "_link_host_dependencies", lambda *_a: None)
    monkeypatch.setattr(mutation_testing, "_apply_mutation", lambda *_a: None)
    monkeypatch.setattr(mutation_testing, "_run_campaign_command", run_batch)
    result = mutation_testing.run_campaign(
        campaign,
        allow_mutations=True,
        receipt_path=Path("receipt.json"),
        repo_root=tmp_path,
    )
    assert calls == ["baseline-1", "baseline-2", "mutant-1"]
    assert result["status"] == "PASS"
    assert result["test_value"]["retained_core"] == [nodeid]
    assert result["test_value"]["subprocess_scaling"] == (
        "baseline_repetitions + mutants"
    )


def test_value_admission_rejects_unmeasured_and_low_value_new_tests() -> None:
    core = "conductor/test_mutation_value.py::test_core"
    redundant = "conductor/test_mutation_value.py::test_redundant"
    delete = "conductor/test_mutation_value.py::test_delete"
    evidence = {
        "schema_version": mutation_value.VALUE_SCHEMA,
        "status": "PASS",
        "tests": [
            {"nodeid": core, "classification": "CORE"},
            {
                "nodeid": redundant,
                "classification": "INTENTIONAL_REDUNDANCY",
            },
            {"nodeid": delete, "classification": "DELETE_CANDIDATE"},
        ],
    }
    assert mutation_value.admission_errors(evidence, [core, redundant]) == []
    errors = mutation_value.admission_errors(evidence, [delete, "missing::test"])
    assert any("DELETE_CANDIDATE" in error for error in errors)
    assert any("no value classification" in error for error in errors)
    assert mutation_value.admission_errors(None, [core]) == [
        "receipt has no test_value evidence"
    ]
    assert mutation_value.admission_errors(
        {"schema_version": "old", "status": "FAIL", "tests": None}, [core]
    ) == [
        "test_value schema is not current",
        "test_value status='FAIL'",
        "test_value.tests must be a list",
    ]
    assert mutation_value.test_value_receipt_errors(
        None, expected_nodeids=[core], expected_repetitions=2
    ) == ["test_value evidence is missing"]
    receipt_errors = mutation_value.test_value_receipt_errors(
        {"schema_version": "old", "status": "FAIL", "tests": []},
        expected_nodeids=[core],
        expected_repetitions=2,
    )
    assert set(receipt_errors) == {
        "test_value schema is not current",
        "test_value status='FAIL'",
        "test_value nodeids do not match ranked tests",
        "test_value baseline repetitions mismatch",
    }
