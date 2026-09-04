"""Per-test mutation attribution and high-value test-set selection.

The mutation runner remains responsible for applying reviewed mutants. This
module turns one batch pytest report per baseline/mutant into an auditable kill
matrix, then recommends a deterministic retained set that covers every
required scientific contract and every killed mutant.
"""

from __future__ import annotations

import json
import re
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from xml.etree.ElementTree import ParseError

from defusedxml.ElementTree import parse as parse_xml

from conductor._native import (
    admission_errors_native,
    analyze_test_value_native,
    load_value_analysis_native,
)
from conductor._native import (
    test_value_receipt_errors_native as _test_value_receipt_errors_native,
)

VALUE_SCHEMA = "llm.mutation-testing.test-value.v1"
ADAPTER = "pytest-junit"


class ValueEvidenceError(ValueError):
    """Raised when value-analysis input is incomplete or unsafe."""


@dataclass(frozen=True, slots=True)
class ValueContract:
    """One high-impact behavior protected by tests and reviewed mutants."""

    contract_id: str
    criticality: str
    active_paths: tuple[str, ...]


@dataclass(frozen=True, slots=True)
class ValueTest:
    """One ranked test's binding to a required behavioral contract."""

    nodeid: str
    contract_id: str
    intentional_redundancy: bool


@dataclass(frozen=True, slots=True)
class ValueAnalysisSpec:
    """Validated value-analysis configuration embedded in a campaign."""

    adapter: str
    baseline_repetitions: int
    contracts: tuple[ValueContract, ...]
    tests: tuple[ValueTest, ...]
    mutation_contracts: Mapping[str, str]


def load_value_analysis(
    value: object,
    *,
    ranked_nodeids: Sequence[str],
    mutation_ids: Sequence[str],
    source_paths: Sequence[str],
) -> ValueAnalysisSpec | None:
    """Load an optional, complete high-value analysis specification."""

    if value is not None and not isinstance(value, dict):
        raise ValueEvidenceError("value_analysis must be an object")
    mutation_contracts = value.get("mutation_contracts") if value else None
    ordered_pairs = (
        list(mutation_contracts.items()) if isinstance(mutation_contracts, dict) else []
    )
    try:
        native = load_value_analysis_native(
            json.dumps(value),
            json.dumps(ordered_pairs),
            list(ranked_nodeids),
            list(mutation_ids),
            list(source_paths),
        )
    except (TypeError, ValueError) as exc:
        raise ValueEvidenceError(str(exc)) from exc
    if native is None:
        return None
    payload = json.loads(native)
    return ValueAnalysisSpec(
        adapter=payload["adapter"],
        baseline_repetitions=payload["baseline_repetitions"],
        contracts=tuple(
            ValueContract(
                contract["id"],
                contract["criticality"],
                tuple(contract["active_paths"]),
            )
            for contract in payload["contracts"]
        ),
        tests=tuple(
            ValueTest(
                test["nodeid"],
                test["contract_id"],
                test["intentional_redundancy"],
            )
            for test in payload["tests"]
        ),
        mutation_contracts=dict(payload["mutation_contracts"]),
    )


def value_inspection_payload(spec: ValueAnalysisSpec | None) -> dict[str, Any]:
    """Return the public inspection summary for optional value analysis."""

    if spec is None:
        return {"status": "NOT_CONFIGURED"}
    return {
        "status": "READY",
        "adapter": spec.adapter,
        "baseline_repetitions": spec.baseline_repetitions,
        "required_contracts": [
            {
                "id": contract.contract_id,
                "criticality": contract.criticality,
                "active_paths": list(contract.active_paths),
            }
            for contract in spec.contracts
        ],
    }


def pytest_junit_argv(argv: Sequence[str], report_path: Path) -> tuple[str, ...]:
    """Add a unique JUnit report path to one existing pytest invocation."""

    if any(arg == "--junitxml" or arg.startswith("--junitxml=") for arg in argv):
        raise ValueEvidenceError("baseline.argv must not set --junitxml")
    return (*argv, f"--junitxml={report_path}")


def _pytest_identity(nodeid: str) -> tuple[str, str]:
    parts = nodeid.split("::")
    module = parts[0]
    if not module.endswith(".py") or len(parts) < 2:
        raise ValueEvidenceError(f"pytest-junit requires a Python nodeid: {nodeid}")
    classname = module[:-3].replace("/", ".")
    if len(parts) > 2:
        classname = f"{classname}.{'.'.join(parts[1:-1])}"
    return classname, parts[-1]


def pytest_attribution_supported(
    argv: Sequence[str], ranked_nodeids: Sequence[str]
) -> bool:
    """Report whether this batch can be run under JUnit and mapped to nodeids."""

    if not ranked_nodeids:
        return False
    if any(arg == "--junitxml" or arg.startswith("--junitxml=") for arg in argv):
        return False
    for nodeid in ranked_nodeids:
        try:
            _pytest_identity(nodeid)
        except ValueEvidenceError:
            return False
    return True


def parse_pytest_junit(
    report_path: Path, ranked_nodeids: Sequence[str]
) -> dict[str, Any]:
    """Parse one pytest JUnit report into function-level outcomes and runtimes."""

    try:
        root = parse_xml(report_path).getroot()
    except (OSError, ParseError) as exc:
        raise ValueEvidenceError(f"cannot parse pytest JUnit report: {exc}") from exc
    if root is None:
        raise ValueEvidenceError("pytest JUnit report has no document root")
    identities = {_pytest_identity(nodeid): nodeid for nodeid in ranked_nodeids}
    aggregate: dict[str, dict[str, Any]] = {}
    unmapped: list[dict[str, str]] = []
    for case in root.iter("testcase"):
        classname = case.attrib.get("classname", "")
        raw_name = case.attrib.get("name", "")
        name = raw_name.split("[", 1)[0]
        nodeid = identities.get((classname, name))
        if nodeid is None:
            unmapped.append({"classname": classname, "name": raw_name})
            continue
        if case.find("error") is not None:
            outcome = "ERROR"
        elif case.find("failure") is not None:
            outcome = "FAILED"
        elif case.find("skipped") is not None:
            outcome = "SKIPPED"
        else:
            outcome = "PASSED"
        try:
            duration = float(case.attrib.get("time", "0"))
        except ValueError:
            duration = 0.0
        row = aggregate.setdefault(
            nodeid,
            {"outcome": "PASSED", "duration_seconds": 0.0, "cases": 0},
        )
        if (
            outcome == "ERROR"
            or (outcome == "FAILED" and row["outcome"] != "ERROR")
            or (outcome == "SKIPPED" and row["cases"] == 0)
            or (outcome == "PASSED" and row["outcome"] == "SKIPPED")
        ):
            row["outcome"] = outcome
        row["duration_seconds"] += duration
        row["cases"] += 1
    missing = [nodeid for nodeid in ranked_nodeids if nodeid not in aggregate]
    tests = {
        nodeid: {
            "outcome": row["outcome"],
            "duration_seconds": round(row["duration_seconds"], 6),
            "cases": row["cases"],
        }
        for nodeid, row in aggregate.items()
    }
    return {
        "status": "COMPLETE" if not missing and not unmapped else "INCOMPLETE",
        "tests": tests,
        "failed_nodeids": [
            nodeid
            for nodeid in ranked_nodeids
            if tests.get(nodeid, {}).get("outcome") in {"FAILED", "ERROR"}
        ],
        "missing_nodeids": missing,
        "unmapped_cases": unmapped,
    }


# ------------------------------------------------------------ cargo / libtest

# libtest prints exactly one line per test it ran, on stdout, in the stable
# format `test <module path>::<fn> ... ok|FAILED|ignored`. There is no machine
# format on stable: `--format json` needs `-Z unstable-options`, `--report-time`
# is nightly-only, and `--logfile` is deprecated AND is truncated by the last
# binary cargo runs (the doc-test pass rewrites it with zero results). Parsing
# the printed line is the only stable per-test signal, so that is what this reads.
CARGO_TEST_LINE = re.compile(
    r"^test\s+(?P<name>\S+)\s+\.\.\.\s+(?P<outcome>ok|FAILED|ignored)\b"
)
_CARGO_OUTCOMES = {"ok": "PASSED", "FAILED": "FAILED", "ignored": "SKIPPED"}


def _cargo_identity(nodeid: str) -> str:
    """Return the test function name a `<path>.rs::<fn>` nodeid names."""

    path, separator, function = nodeid.partition("::")
    if not separator or not path.endswith(".rs") or "::" in function or not function:
        raise ValueEvidenceError(f"cargo-libtest requires a Rust nodeid: {nodeid}")
    return function


def cargo_attribution_supported(ranked_nodeids: Sequence[str]) -> bool:
    """Report whether this batch's ranked tests can be mapped to libtest output.

    Shape is read from the nodeids, not from argv: a campaign may wrap its run in
    `sh -c` or a cargo alias, and refusing attribution because argv[0] is not
    literally `cargo` would leave exactly those campaigns unattributed.
    """

    if not ranked_nodeids:
        return False
    try:
        names = [_cargo_identity(nodeid) for nodeid in ranked_nodeids]
    except ValueEvidenceError:
        return False
    # libtest prints the module path, not the file, so two ranked tests that share
    # a function name are indistinguishable in the output. Guessing would attribute
    # a kill to the wrong contract, which is worse than reporting no attribution.
    return len(set(names)) == len(names)


def parse_cargo_libtest(stdout: str, ranked_nodeids: Sequence[str]) -> dict[str, Any]:
    """Map one libtest run's printed outcomes onto the campaign's ranked nodeids."""

    by_name = {_cargo_identity(nodeid): nodeid for nodeid in ranked_nodeids}
    if len(by_name) != len(ranked_nodeids):
        raise ValueEvidenceError(
            "ranked tests share a function name; libtest output cannot separate them"
        )
    tests: dict[str, dict[str, Any]] = {}
    unranked_failures: list[str] = []
    printed_names: dict[str, set[str]] = {}
    for line in stdout.splitlines():
        match = CARGO_TEST_LINE.match(line.strip())
        if match is None:
            continue
        printed = match.group("name")
        outcome = _CARGO_OUTCOMES[match.group("outcome")]
        nodeid = by_name.get(printed.rpartition("::")[2])
        if nodeid is None:
            # A batch usually runs more than the ranked set -- a crate-wide `cargo
            # test` runs every test in the binary. Those are not a defect, but a
            # mutant that fails many of them is a blunt mutant, and that is worth
            # recording even though it cannot enter `tests` (which is keyed by
            # nodeid and is what the killer verdict reads).
            if outcome == "FAILED":
                unranked_failures.append(printed)
            continue
        # libtest reports no per-test duration on stable, so no duration key is
        # written rather than a fabricated 0.0.
        row = tests.setdefault(nodeid, {"outcome": outcome, "cases": 0})
        if outcome == "FAILED" and row["outcome"] != "FAILED":
            row["outcome"] = outcome
        row["cases"] += 1
        printed_names.setdefault(nodeid, set()).add(printed)
    # A ranked nodeid carries no module path, so it matches on the last segment
    # alone. Two DIFFERENT tests in the binary can therefore land on one nodeid --
    # `crate::a::test_roundtrip` and `crate::b::test_roundtrip` both end
    # `::test_roundtrip`. Merging them attributes one test's failure to the other's
    # contract, which is the misattribution this adapter exists to prevent, so the
    # nodeid is dropped rather than resolved by guesswork. The ambiguity is visible
    # in the output itself, which is why it is caught here and not in the
    # supported-shape check.
    ambiguous = sorted(
        nodeid for nodeid, names in printed_names.items() if len(names) > 1
    )
    for nodeid in ambiguous:
        del tests[nodeid]
    missing = [nodeid for nodeid in ranked_nodeids if nodeid not in tests]
    return {
        "status": "COMPLETE" if not missing else "INCOMPLETE",
        "tests": tests,
        "missing_nodeids": missing,
        "ambiguous_nodeids": ambiguous,
        "unmapped_cases": [],
        "unranked_failures": sorted(unranked_failures),
    }


def collect_cargo_libtest_batch[BatchResultT](
    *,
    argv: Sequence[str],
    ranked_nodeids: Sequence[str],
    run_command: Callable[[Sequence[str], Callable[[str], None]], BatchResultT],
) -> tuple[BatchResultT, Mapping[str, Any]]:
    """Run one cargo batch and return fail-closed attribution from its stdout."""

    captured: list[str] = []
    result = run_command(argv, captured.append)
    try:
        report = parse_cargo_libtest("".join(captured), ranked_nodeids)
    except ValueEvidenceError as exc:
        report = {
            "status": "INCOMPLETE",
            "tests": {},
            "missing_nodeids": list(ranked_nodeids),
            "ambiguous_nodeids": [],
            "unmapped_cases": [],
            "unranked_failures": [],
            "error": str(exc),
        }
    return result, report


def collect_pytest_junit_batch[BatchResultT](
    *,
    argv: Sequence[str],
    report_path: Path,
    ranked_nodeids: Sequence[str],
    run_command: Callable[[Sequence[str]], BatchResultT],
) -> tuple[BatchResultT, Mapping[str, Any]]:
    """Run one instrumented pytest batch and return fail-closed attribution."""

    report_path.parent.mkdir(parents=True, exist_ok=True)
    result = run_command(pytest_junit_argv(argv, report_path))
    try:
        report = parse_pytest_junit(report_path, ranked_nodeids)
    except ValueEvidenceError as exc:
        report = {
            "status": "INCOMPLETE",
            "tests": {},
            "failed_nodeids": [],
            "missing_nodeids": list(ranked_nodeids),
            "unmapped_cases": [],
            "error": str(exc),
        }
    return result, report


def _spec_payload(spec: ValueAnalysisSpec) -> dict[str, Any]:
    return {
        "adapter": spec.adapter,
        "baseline_repetitions": spec.baseline_repetitions,
        "contracts": [
            {
                "id": contract.contract_id,
                "criticality": contract.criticality,
                "active_paths": list(contract.active_paths),
            }
            for contract in spec.contracts
        ],
        "tests": [
            {
                "nodeid": test.nodeid,
                "contract_id": test.contract_id,
                "intentional_redundancy": test.intentional_redundancy,
            }
            for test in spec.tests
        ],
        "mutation_contracts": list(spec.mutation_contracts.items()),
    }


def analyze_test_value(
    spec: ValueAnalysisSpec,
    *,
    baseline_reports: Sequence[Mapping[str, Any]],
    mutant_reports: Mapping[str, Mapping[str, Any]],
    mutant_outcomes: Mapping[str, str],
) -> dict[str, Any]:
    """Build kill attribution, value classifications, and a retained core set."""

    nodeids = [test.nodeid for test in spec.tests]
    mutant_evidence = []
    for mutation_id in spec.mutation_contracts:
        report = mutant_reports.get(mutation_id)
        if not isinstance(report, dict) or report.get("status") != "COMPLETE":
            report_state = "INCOMPLETE"
            killers: list[str] = []
        elif not isinstance(report.get("tests"), dict):
            report_state = "NO_TEST_MAP"
            killers = []
        else:
            report_state = "COMPLETE"
            tests = report["tests"]
            failed_nodeids = report.get("failed_nodeids")
            if isinstance(failed_nodeids, list) and all(
                isinstance(nodeid, str) for nodeid in failed_nodeids
            ):
                failed = set(failed_nodeids)
                killers = [nodeid for nodeid in nodeids if nodeid in failed]
            else:
                killers = [
                    nodeid
                    for nodeid in nodeids
                    if isinstance(tests.get(nodeid), dict)
                    and tests[nodeid].get("outcome") in {"FAILED", "ERROR"}
                ]
        mutant_evidence.append(
            {
                "mutation_id": mutation_id,
                "outcome": mutant_outcomes.get(mutation_id),
                "report_state": report_state,
                "killers": killers,
            }
        )
    try:
        return json.loads(
            analyze_test_value_native(
                json.dumps(_spec_payload(spec)),
                json.dumps(baseline_reports),
                json.dumps(mutant_evidence),
            )
        )
    except (TypeError, ValueError) as exc:
        raise ValueEvidenceError(str(exc)) from exc


def admission_errors(
    value_evidence: object, required_nodeids: Sequence[str]
) -> list[str]:
    """Return fail-closed reasons for newly introduced test definitions."""

    try:
        return admission_errors_native(
            json.dumps(value_evidence), list(required_nodeids)
        )
    except (TypeError, ValueError) as exc:
        raise ValueEvidenceError(str(exc)) from exc


def test_value_receipt_errors(
    value: object,
    *,
    expected_nodeids: Sequence[str],
    expected_repetitions: int,
) -> list[str]:
    """Validate the value-specific portion of a mutation receipt."""

    try:
        return _test_value_receipt_errors_native(
            json.dumps(value), list(expected_nodeids), expected_repetitions
        )
    except (TypeError, ValueError) as exc:
        raise ValueEvidenceError(str(exc)) from exc


test_value_receipt_errors.__test__ = False
