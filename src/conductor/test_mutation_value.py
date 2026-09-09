from __future__ import annotations

from collections.abc import Sequence
import copy
from pathlib import Path
import sys
from types import SimpleNamespace
from typing import Any

import pytest

from conductor import mutation_testing, mutation_testing_support, mutation_value


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


def test_cargo_attribution_refuses_ambiguity_rather_than_guessing() -> None:
    """libtest attribution must fail closed on every shape it cannot separate.

    Each branch here is a way to attribute a Rust kill to the wrong contract, which
    is worse than reporting no attribution at all: a MISATTRIBUTED verdict that
    should have been UNAVAILABLE reads as a contract that held.
    """

    alpha = "tooling/native/conductor-native/src/mutation_receipt.rs::test_alpha"
    beta = "tooling/native/conductor-native/src/mutation_manifest.rs::test_beta"

    assert mutation_value.cargo_attribution_supported([alpha, beta])
    # No ranked tests is not "trivially attributable"; there is nothing to map.
    assert not mutation_value.cargo_attribution_supported([])
    # A Python nodeid must never take the cargo path -- pytest attribution is
    # richer, and silently downgrading it would lose per-test durations.
    assert not mutation_value.cargo_attribution_supported(
        ["conductor/test_mutation_value.py::test_alpha"]
    )
    # libtest prints a module path, not a file, so two ranked tests sharing a
    # function name are indistinguishable in its output.
    twin = "tooling/native/conductor-native/src/mutation_evidence.rs::test_alpha"
    assert not mutation_value.cargo_attribution_supported([alpha, twin])
    with pytest.raises(mutation_value.ValueEvidenceError, match="share a function"):
        mutation_value.parse_cargo_libtest("", [alpha, twin])
    with pytest.raises(mutation_value.ValueEvidenceError, match="Rust nodeid"):
        mutation_value.parse_cargo_libtest("", ["src/lib.rs::mods::test_alpha"])

    stdout = (
        "running 4 tests\n"
        "test receipt::tests::test_alpha ... FAILED\n"
        "test manifest::tests::test_beta ... ok\n"
        "test manifest::tests::test_unranked ... FAILED\n"
        "test manifest::tests::test_skipped ... ignored\n"
        "test result: FAILED. 1 passed; 2 failed; 1 ignored\n"
    )
    parsed = mutation_value.parse_cargo_libtest(stdout, [alpha, beta])
    assert parsed["status"] == "COMPLETE"
    assert parsed["tests"][alpha] == {"outcome": "FAILED", "cases": 1}
    assert parsed["tests"][beta]["outcome"] == "PASSED"
    # stable libtest reports no per-test time; a fabricated 0.0 would be read as a
    # measurement by the value analysis, so no duration key is written at all.
    assert "duration_seconds" not in parsed["tests"][alpha]
    # A failure outside the ranked set is evidence of a blunt mutant. It must be
    # recorded, and it must not enter `tests`, which the killer verdict reads.
    assert parsed["unranked_failures"] == ["manifest::tests::test_unranked"]
    assert set(parsed["tests"]) == {alpha, beta}

    # An `ignored` test ran nothing, so it can never be a killer.
    ignored = mutation_value.parse_cargo_libtest(
        "test receipt::tests::test_alpha ... ignored\n", [alpha]
    )
    assert ignored["tests"][alpha]["outcome"] == "SKIPPED"

    # A ranked test the binary never printed is INCOMPLETE, never a silent PASSED.
    missing = mutation_value.parse_cargo_libtest(stdout, [alpha, beta, "x.rs::test_x"])
    assert missing["status"] == "INCOMPLETE"
    assert missing["missing_nodeids"] == ["x.rs::test_x"]

    # Two DIFFERENT binaries' tests can end in the same segment as one ranked
    # nodeid. Merging them attributes one test's failure to the other's contract.
    collided = mutation_value.parse_cargo_libtest(
        "test receipt::tests::test_alpha ... ok\n"
        "test other::tests::test_alpha ... FAILED\n",
        [alpha],
    )
    assert collided["ambiguous_nodeids"] == [alpha]
    assert alpha not in collided["tests"]
    assert collided["status"] == "INCOMPLETE"


def test_cargo_attribution_reads_the_full_stdout_not_the_stored_tail() -> None:
    """The sink must see the whole run even though the receipt stores a tail."""

    alpha = "tooling/native/conductor-native/src/mutation_receipt.rs::test_alpha"
    tail = mutation_testing.OUTPUT_TAIL_CHARS

    def run(script: str, *, timeout_seconds: int, sink: list[str]) -> dict[str, object]:
        return mutation_testing_support.run_command(
            [sys.executable, "-c", script],
            cwd=Path.cwd(),
            timeout_seconds=timeout_seconds,
            environment={},
            pin_argv=list,
            result_factory=lambda **fields: fields,
            output_tail_chars=tail,
            stdout_sink=sink.append,
        )

    # The verdict line is printed first and the chatter after it, so a sink fed
    # the stored tail rather than the full stdout loses the one line attribution
    # depends on -- which is why the tail cannot be the attribution source.
    captured: list[str] = []
    result = run(
        "import sys\n"
        "sys.stdout.write('test receipt::tests::test_alpha ... FAILED\\n')\n"
        f"sys.stdout.write('c' * {tail * 2})\n",
        timeout_seconds=120,
        sink=captured,
    )
    assert "test_alpha" not in str(result["stdout_tail"])
    report = mutation_value.parse_cargo_libtest("".join(captured), [alpha])
    assert report["status"] == "COMPLETE"
    assert report["tests"][alpha]["outcome"] == "FAILED"

    # A run that exhausts its budget still attributes whatever it reported before
    # the kill, so a slow mutant is adjudicated instead of silently unattributed.
    timed: list[str] = []
    expired = run(
        "import sys, time\n"
        "sys.stdout.write('test receipt::tests::test_alpha ... FAILED\\n')\n"
        "sys.stdout.flush()\n"
        "time.sleep(120)\n",
        timeout_seconds=2,
        sink=timed,
    )
    assert expired["timed_out"] is True
    partial = mutation_value.parse_cargo_libtest("".join(timed), [alpha])
    assert partial["tests"][alpha]["outcome"] == "FAILED"

    # A shape the parser refuses must come back as no attribution, never as an
    # exception that aborts a campaign mid-walk.
    _, refused = mutation_value.collect_cargo_libtest_batch(
        argv=("cargo", "test"),
        ranked_nodeids=["conductor/test_mutation_value.py::test_alpha"],
        run_command=lambda argv, sink: sink("test a::b ... ok\n"),
    )
    assert refused["status"] == "INCOMPLETE" and "Rust nodeid" in refused["error"]


def test_a_rust_kill_by_an_undeclared_test_is_misattributed() -> None:
    """The whole point of Rust attribution: expected_killers becomes enforceable."""

    alpha = "tooling/native/conductor-native/src/mutation_receipt.rs::test_alpha"
    beta = "tooling/native/conductor-native/src/mutation_manifest.rs::test_beta"
    mutation = SimpleNamespace(expected_killers=[alpha])

    report = mutation_value.parse_cargo_libtest(
        "test receipt::tests::test_alpha ... ok\n"
        "test manifest::tests::test_beta ... FAILED\n"
        "test manifest::tests::test_blunt ... FAILED\n",
        [alpha, beta],
    )
    verdict = mutation_testing.killer_verdict(mutation, report, "KILLED")
    assert verdict["status"] == "MISATTRIBUTED"
    assert verdict["observed_failures"] == [beta]
    assert verdict["collateral"] == [beta]
    # Bluntness survives into the receipt rather than being discarded.
    assert verdict["unranked_failures"] == ["manifest::tests::test_blunt"]

    declared_failed = mutation_value.parse_cargo_libtest(
        "test receipt::tests::test_alpha ... FAILED\n"
        "test manifest::tests::test_beta ... ok\n",
        [alpha, beta],
    )
    confirmed = mutation_testing.killer_verdict(mutation, declared_failed, "KILLED")
    assert confirmed["status"] == "CONFIRMED" and confirmed["matched"] == [alpha]
    assert "unranked_failures" not in confirmed

    # An ambiguous mapping must not be adjudicated at all.
    ambiguous = mutation_value.parse_cargo_libtest(
        "test receipt::tests::test_alpha ... FAILED\n"
        "test other::tests::test_alpha ... ok\n"
        "test manifest::tests::test_beta ... ok\n",
        [alpha, beta],
    )
    # A batch that CAN be attributed and did not attribute this kill is a
    # refusal, not the same "no attribution here" as a harness that never
    # produces per-test evidence -- the usual cause is a kill that came from
    # the toolchain rather than from a test.
    assert mutation_testing.killer_verdict(mutation, ambiguous, "KILLED") == {
        "status": "UNATTRIBUTED",
        "declared": [alpha],
        "reason": "attribution is INCOMPLETE",
        "missing_nodeids": [alpha],
        "error": None,
    }


# Everything a routing probe never varies. Held as a mapping rather than spelled
# out in the call so the helper stays short enough not to duplicate the campaign
# builders the other probes in this module keep inline.
_ROUTING_PROBE_FIELDS: dict[str, Any] = {
    "manifest_sha256": "0" * 64,
    "campaign_id": "routing_probe",
    "title": "routing probe",
    "language": "rust",
    "mutation_engine": "reviewed_unified_diff",
    "expected_mutations": 0,
    "source_sha256": {},
    "planned_mutations": (),
    "mutations": (),
    "timeout_seconds": 10,
    "blocked_process_substrings": (),
    "poll_seconds": 1,
    "environment": {},
    "host_read_dependencies": (),
}


def _routing_campaign(
    manifest: Path,
    nodeids: Sequence[str],
    argv: Sequence[str],
    value_analysis: mutation_value.ValueAnalysisSpec | None = None,
) -> mutation_testing.Campaign:
    ranked = tuple(
        mutation_testing.RankedTest(i + 1, n, "c", "r") for i, n in enumerate(nodeids)
    )
    return mutation_testing.Campaign(
        manifest_path=manifest,
        ranked_tests=ranked,
        test_argv=tuple(argv),
        value_analysis=value_analysis,
        **_ROUTING_PROBE_FIELDS,
    )


def _routing_spec(adapter: str, nodeid: str) -> mutation_value.ValueAnalysisSpec:
    return mutation_value.ValueAnalysisSpec(
        adapter=adapter,
        baseline_repetitions=2,
        contracts=(mutation_value.ValueContract("c", "critical", ("src/lib.rs",)),),
        tests=(mutation_value.ValueTest(nodeid, "c", False),),
        mutation_contracts={"m": "c"},
    )


def test_value_analysis_reaches_the_native_collectors(tmp_path: Path) -> None:
    """Value analysis is not a pytest feature; it is a per-test kill matrix.

    The classifier reads `report["tests"][nodeid]["outcome"]`, which the cargo and
    ctest collectors produce in exactly the shape the pytest one does. Refusing
    every non-pytest batch made value analysis unreachable for every Rust, C and
    C++ campaign in the repo -- the tests those campaigns ship could never be
    classified, so their receipts could never say whether the tests were worth
    keeping. What still has to refuse is a batch that attributes NOTHING, and a
    manifest whose adapter names a harness that will not run.
    """

    rust = "tooling/native/conductor-native/src/mutation_manifest.rs::test_alpha"
    manifest = tmp_path / "campaign.json"
    manifest.write_text("{}", encoding="utf-8")

    def fake_run_command(argv, *, cwd, timeout_seconds, environment, stdout_sink=None):
        if stdout_sink is not None:
            stdout_sink("test manifest::tests::test_alpha ... FAILED\n")
        return "result"

    original = mutation_testing._run_command  # noqa: SLF001
    mutation_testing._run_command = fake_run_command  # noqa: SLF001
    try:
        # A cargo batch that declares value analysis reaches the libtest collector
        # and comes back with the matrix, instead of being refused.
        campaign = _routing_campaign(
            manifest, [rust], ("cargo", "test"), _routing_spec("cargo-libtest", rust)
        )
        _, report = mutation_testing._run_campaign_command(  # noqa: SLF001
            campaign, snapshot_root=tmp_path, report_name="batch"
        )
        assert report["tests"][rust]["outcome"] == "FAILED"

        # The adapter label must name the collector that actually runs, or the
        # receipt reads as evidence from a harness that never executed.
        mislabelled = _routing_campaign(
            manifest, [rust], ("cargo", "test"), _routing_spec("pytest-junit", rust)
        )
        with pytest.raises(mutation_testing.CampaignError, match="cargo-libtest"):
            mutation_testing._run_campaign_command(  # noqa: SLF001
                mislabelled, snapshot_root=tmp_path, report_name="batch"
            )

        # A batch no collector can attribute still refuses: value analysis without
        # a kill matrix would classify every test off an empty report.
        opaque = "tests/suite.js"
        blind = _routing_campaign(
            manifest, [opaque], ("npm", "test"), _routing_spec("pytest-junit", opaque)
        )
        with pytest.raises(mutation_testing.CampaignError, match="attributes failures"):
            mutation_testing._run_campaign_command(  # noqa: SLF001
                blind, snapshot_root=tmp_path, report_name="batch"
            )
    finally:
        mutation_testing._run_command = original  # noqa: SLF001


def test_a_rust_batch_is_routed_to_the_libtest_collector(tmp_path: Path) -> None:
    """The adapter only pays off if the batch actually reaches it.

    Lives beside the adapter rather than in test_mutation_testing.py because what it
    pins is the adapter-selection contract: which collector a batch gets, and what a
    batch that fits none of them still returns.
    """

    alpha = "tooling/native/conductor-native/src/mutation_manifest.rs::test_alpha"
    manifest = tmp_path / "campaign.json"
    manifest.write_text("{}", encoding="utf-8")

    def build(nodeids: Sequence[str], argv: Sequence[str]):
        ranked = tuple(
            mutation_testing.RankedTest(i + 1, n, "c", "r")
            for i, n in enumerate(nodeids)
        )
        return mutation_testing.Campaign(
            manifest_path=manifest,
            manifest_sha256="0" * 64,
            campaign_id="routing_probe",
            title="routing probe",
            language="rust",
            mutation_engine="reviewed_unified_diff",
            expected_mutations=0,
            source_sha256={},
            ranked_tests=ranked,
            planned_mutations=(),
            mutations=(),
            test_argv=tuple(argv),
            timeout_seconds=10,
            blocked_process_substrings=(),
            poll_seconds=1,
            environment={},
            host_read_dependencies=(),
        )

    calls: list[tuple[Sequence[str], bool]] = []

    def fake_run_command(argv, *, cwd, timeout_seconds, environment, stdout_sink=None):
        calls.append((tuple(argv), stdout_sink is not None))
        if stdout_sink is not None:
            stdout_sink("test manifest::tests::test_alpha ... FAILED\n")
        return "result"

    original = mutation_testing._run_command  # noqa: SLF001
    mutation_testing._run_command = fake_run_command  # noqa: SLF001
    try:
        rust = build([alpha], ("cargo", "test"))
        result, report = mutation_testing._run_campaign_command(  # noqa: SLF001
            rust, snapshot_root=tmp_path, report_name="batch"
        )
        assert result == "result"
        assert report["tests"][alpha]["outcome"] == "FAILED"
        # The cargo path must not rewrite argv the way the pytest one does.
        assert calls[-1] == (("cargo", "test"), True)

        # A batch that fits neither collector still runs, and reports NO
        # attribution rather than an empty one that would read as COMPLETE.
        other = build(["tests/suite.js"], ("npm", "test"))
        result, report = mutation_testing._run_campaign_command(  # noqa: SLF001
            other, snapshot_root=tmp_path, report_name="batch"
        )
        assert (result, report) == ("result", None)
        assert calls[-1] == (("npm", "test"), False)
    finally:
        mutation_testing._run_command = original  # noqa: SLF001


_CTEST_JUNIT = """<?xml version="1.0" encoding="UTF-8"?>
<testsuite name="(empty)" tests="4" failures="1" disabled="1" skipped="0">
  <testcase name="test_profiler.test_memory_events" classname="c" time="0.03" status="run"/>
  <testcase name="test_profiler.test_reset_clears_all" classname="c" time="0.02" status="fail">
    <failure message="Failed"/>
  </testcase>
  <testcase name="test_profiler.test_clock_ns_monotonic" classname="c" time="0" status="disabled"/>
  <testcase name="test_kernels.test_relu" classname="c" time="0.01" status="fail">
    <failure message="Failed"/>
  </testcase>
</testsuite>
"""


def test_a_ctest_nodeid_maps_onto_the_name_cmake_registers() -> None:
    """The nodeid and the ctest name are two spellings of one test.

    CTest names are flat and project-global while a nodeid is path-qualified, so
    the mapping is `<file stem>.<function>`. If this drifts from what CMakeLists
    registers, every kill lands as unattributed and the campaign refuses -- loud,
    but only because the shape is pinned here.
    """

    nodeid = "research/runtime/native/tests/test_profiler.c::test_memory_events"
    assert (
        mutation_value._ctest_identity(nodeid)  # noqa: SLF001
        == "test_profiler.test_memory_events"
    )
    for rejected in (
        "research/runtime/native/src/profiler.c",  # no function
        "conductor/test_mutation_value.py::test_x",  # not a C file
        "tests/test_a.c::mod::test_x",  # ctest names carry no module path
        "tests/test_a.c::",  # empty function
    ):
        with pytest.raises(mutation_value.ValueEvidenceError):
            mutation_value._ctest_identity(rejected)  # noqa: SLF001


def test_ctest_attribution_refuses_names_it_cannot_separate() -> None:
    """Two files with the same stem collide in ctest's flat namespace.

    Attributing a kill to whichever contract sorted first is worse than
    reporting none, so the collision is refused at the shape check rather than
    resolved by guesswork.
    """

    assert mutation_value.ctest_attribution_supported(
        ["a/test_profiler.c::test_reset", "b/test_kernels.c::test_reset"]
    )
    assert not mutation_value.ctest_attribution_supported(
        ["a/test_profiler.c::test_reset", "b/test_profiler.c::test_reset"]
    )
    assert not mutation_value.ctest_attribution_supported([])
    assert not mutation_value.ctest_attribution_supported(["tests/suite.rs::test_x"])


def test_a_ctest_report_separates_the_declared_killer_from_collateral(
    tmp_path: Path,
) -> None:
    """ctest runs the whole project, so most cases in the report are unranked.

    A failure outside the ranked set is not a defect but it is bluntness, and it
    is recorded rather than discarded. A disabled case is not a pass: a test that
    CMake registered and then disabled would otherwise vouch for a mutant it
    never ran.
    """

    report_path = tmp_path / "ctest.xml"
    report_path.write_text(_CTEST_JUNIT, encoding="utf-8")
    memory = "research/runtime/native/tests/test_profiler.c::test_memory_events"
    reset = "research/runtime/native/tests/test_profiler.c::test_reset_clears_all"
    clock = "research/runtime/native/tests/test_profiler.c::test_clock_ns_monotonic"

    report = mutation_value.parse_ctest_junit(report_path, [memory, reset, clock])
    assert report["status"] == "COMPLETE"
    assert report["tests"][memory]["outcome"] == "PASSED"
    assert report["tests"][reset]["outcome"] == "FAILED"
    assert report["tests"][clock]["outcome"] == "SKIPPED"
    assert report["tests"][memory]["duration_seconds"] == 0.03
    assert report["failed_nodeids"] == [reset]
    assert report["unranked_failures"] == ["test_kernels.test_relu"]

    mutation = SimpleNamespace(expected_killers=[reset])
    verdict = mutation_testing.killer_verdict(mutation, report, "KILLED")
    assert verdict["status"] == "CONFIRMED" and verdict["matched"] == [reset]

    # A ranked test ctest never registered leaves the report incomplete rather
    # than silently narrowing the campaign to the cases that happened to run.
    absent = "research/runtime/native/tests/test_profiler.c::test_never_registered"
    partial = mutation_value.parse_ctest_junit(report_path, [reset, absent])
    assert partial["status"] == "INCOMPLETE"
    assert partial["missing_nodeids"] == [absent]


def test_a_ctest_batch_that_never_ran_cannot_inherit_the_last_one_s_report(
    tmp_path: Path,
) -> None:
    """The failure this prevents is a kill attributed to a run that never happened.

    A C batch builds before it tests, so a mutant that breaks the build exits
    non-zero with ctest unrun. The previous mutant's report is still on disk and
    would be read as this one's evidence -- and it would say every ranked test
    passed, so the kill would be attributed to nothing while reading COMPLETE.
    """

    report_path = tmp_path / ".mutation-value" / "ctest.xml"
    report_path.parent.mkdir(parents=True)
    report_path.write_text(_CTEST_JUNIT, encoding="utf-8")
    reset = "research/runtime/native/tests/test_profiler.c::test_reset_clears_all"

    result, report = mutation_value.collect_ctest_junit_batch(
        argv=("sh", "-c", "false"),
        report_path=report_path,
        ranked_nodeids=[reset],
        run_command=lambda argv: "build failed",
    )
    assert result == "build failed"
    assert report["status"] == "INCOMPLETE"
    assert report["missing_nodeids"] == [reset]
    assert report["tests"] == {}
    assert "cannot parse ctest JUnit report" in report["error"]
    assert not report_path.exists()

    mutation = SimpleNamespace(expected_killers=[reset])
    verdict = mutation_testing.killer_verdict(mutation, report, "KILLED")
    assert verdict["status"] == "UNATTRIBUTED"


def test_a_c_batch_is_routed_to_the_ctest_collector(tmp_path: Path) -> None:
    """Routing is read from the ranked nodeids, not argv.

    A C batch has to build before it tests, so its argv is a shell line and
    `ctest` is never argv[0]. Routing on argv would leave every C campaign
    unattributed, which is the state this branch exists to end.
    """

    reset = "research/runtime/native/tests/test_profiler.c::test_reset_clears_all"
    manifest = tmp_path / "campaign.json"
    manifest.write_text("{}", encoding="utf-8")
    campaign = mutation_testing.Campaign(
        manifest_path=manifest,
        manifest_sha256="0" * 64,
        campaign_id="routing_probe",
        title="routing probe",
        language="c",
        mutation_engine="reviewed_unified_diff",
        expected_mutations=0,
        source_sha256={},
        ranked_tests=(mutation_testing.RankedTest(1, reset, "c", "r"),),
        planned_mutations=(),
        mutations=(),
        test_argv=("sh", "-c", "cmake --build build && ctest --test-dir build"),
        timeout_seconds=10,
        blocked_process_substrings=(),
        poll_seconds=1,
        environment={},
        host_read_dependencies=(),
    )

    calls: list[Sequence[str]] = []

    def fake_run_command(argv, *, cwd, timeout_seconds, environment, stdout_sink=None):
        calls.append(tuple(argv))
        mutation_value.ctest_junit_path(tmp_path).write_text(
            _CTEST_JUNIT, encoding="utf-8"
        )
        return "result"

    original = mutation_testing._run_command  # noqa: SLF001
    mutation_testing._run_command = fake_run_command  # noqa: SLF001
    try:
        result, report = mutation_testing._run_campaign_command(  # noqa: SLF001
            campaign, snapshot_root=tmp_path, report_name="batch"
        )
    finally:
        mutation_testing._run_command = original  # noqa: SLF001
    assert result == "result"
    assert report["tests"][reset]["outcome"] == "FAILED"
    # The ctest path must not rewrite argv the way the pytest one does.
    assert calls == [("sh", "-c", "cmake --build build && ctest --test-dir build")]


def test_a_kill_a_capable_batch_did_not_attribute_refuses_the_campaign() -> None:
    """An unattributed kill is a refusal, not a quieter shade of enforced.

    Without this the framework counts a mutant that broke the build as killed,
    reports `killer_enforcement: UNAVAILABLE`, and still passes the campaign --
    a body count with no contract behind it. The distinction that makes it
    actionable is harness-vs-run: a batch with no per-test evidence at all is
    reported and not punished, because no campaign can fix it.
    """

    def row(mutant_id: str, status: str) -> dict[str, object]:
        return {"id": mutant_id, "killer_attribution": {"status": status}}

    assert mutation_testing.killer_enforcement(
        [row("a", "CONFIRMED"), row("b", "CONFIRMED")]
    ) == {
        "status": "ENFORCED",
        "misattributed": [],
        "unattributed": [],
        "unattributed_runs": [],
    }
    harness = mutation_testing.killer_enforcement(
        [row("a", "CONFIRMED"), row("b", "UNAVAILABLE")]
    )
    assert harness["status"] == "UNAVAILABLE" and harness["unattributed"] == ["b"]
    run = mutation_testing.killer_enforcement(
        [row("a", "CONFIRMED"), row("b", "UNATTRIBUTED")]
    )
    assert run["status"] == "REFUSED" and run["unattributed_runs"] == ["b"]
    wrong = mutation_testing.killer_enforcement(
        [row("a", "MISATTRIBUTED"), row("b", "UNAVAILABLE")]
    )
    assert wrong["status"] == "REFUSED" and wrong["misattributed"] == ["a"]
