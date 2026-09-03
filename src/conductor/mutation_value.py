"""Per-test mutation attribution and high-value test-set selection.

The mutation runner remains responsible for applying reviewed mutants. This
module turns one batch pytest report per baseline/mutant into an auditable kill
matrix, then recommends a deterministic retained set that covers every
required scientific contract and every killed mutant.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path, PurePosixPath
import statistics
from typing import Any, Callable, Mapping, Sequence, TypeVar
from xml.etree.ElementTree import ParseError

from defusedxml.ElementTree import parse as parse_xml


VALUE_SCHEMA = "llm.mutation-testing.test-value.v1"
ADAPTER = "pytest-junit"
ALLOWED_CRITICALITIES = frozenset({"critical", "high"})
ADMITTED_CLASSIFICATIONS = frozenset({"CORE", "INTENTIONAL_REDUNDANCY"})
FAILED_OUTCOMES = frozenset({"FAILED", "ERROR"})
BatchResultT = TypeVar("BatchResultT")


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


def _mapping(value: object, label: str) -> Mapping[str, Any]:
    if not isinstance(value, dict):
        raise ValueEvidenceError(f"{label} must be an object")
    return value


def _string(value: object, label: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValueEvidenceError(f"{label} must be a non-empty string")
    return value


def _safe_path(value: object, label: str) -> str:
    text = _string(value, label).replace("\\", "/")
    path = PurePosixPath(text)
    if path.is_absolute() or ".." in path.parts or text.startswith("./"):
        raise ValueEvidenceError(f"{label} must be repository-relative")
    return path.as_posix()


def _load_contracts(
    value: object, *, source_paths: Sequence[str], test_paths: set[str]
) -> tuple[ValueContract, ...]:
    if not isinstance(value, list) or not value:
        raise ValueEvidenceError("required_contracts must be a non-empty list")
    contracts: list[ValueContract] = []
    for index, raw in enumerate(value):
        row = _mapping(raw, f"required_contracts[{index}]")
        contract_id = _string(row.get("id"), f"required_contracts[{index}].id")
        criticality = _string(
            row.get("criticality"), f"required_contracts[{index}].criticality"
        )
        if criticality not in ALLOWED_CRITICALITIES:
            raise ValueEvidenceError(
                f"required_contracts[{index}].criticality must be critical or high"
            )
        raw_paths = row.get("active_paths")
        if not isinstance(raw_paths, list) or not raw_paths:
            raise ValueEvidenceError(
                f"required_contracts[{index}].active_paths must be non-empty"
            )
        active_paths = tuple(
            _safe_path(path, f"required_contracts[{index}].active_paths")
            for path in raw_paths
        )
        unbound = sorted(set(active_paths) - set(source_paths))
        if unbound:
            raise ValueEvidenceError(
                f"contract {contract_id!r} has unbound active paths: {unbound}"
            )
        test_targets = sorted(set(active_paths) & test_paths)
        if test_targets:
            raise ValueEvidenceError(
                f"contract {contract_id!r} active paths are tests, not production: "
                f"{test_targets}"
            )
        contracts.append(ValueContract(contract_id, criticality, active_paths))
    ids = [contract.contract_id for contract in contracts]
    if len(set(ids)) != len(ids):
        raise ValueEvidenceError("required_contracts contains duplicate ids")
    return tuple(contracts)


def _load_value_tests(
    value: object, *, ranked_nodeids: Sequence[str], contract_ids: set[str]
) -> tuple[ValueTest, ...]:
    if not isinstance(value, list) or not value:
        raise ValueEvidenceError("value_analysis.tests must be a non-empty list")
    tests: list[ValueTest] = []
    for index, raw in enumerate(value):
        row = _mapping(raw, f"value_analysis.tests[{index}]")
        redundancy = row.get("intentional_redundancy", False)
        if not isinstance(redundancy, bool):
            raise ValueEvidenceError(
                f"value_analysis.tests[{index}].intentional_redundancy must be boolean"
            )
        tests.append(
            ValueTest(
                nodeid=_string(
                    row.get("nodeid"), f"value_analysis.tests[{index}].nodeid"
                ),
                contract_id=_string(
                    row.get("contract_id"),
                    f"value_analysis.tests[{index}].contract_id",
                ),
                intentional_redundancy=redundancy,
            )
        )
    if [test.nodeid for test in tests] != list(ranked_nodeids):
        raise ValueEvidenceError(
            "value_analysis.tests must exactly match ranked_tests in rank order"
        )
    unknown = sorted({test.contract_id for test in tests} - contract_ids)
    if unknown:
        raise ValueEvidenceError(f"tests reference unknown contracts: {unknown}")
    return tuple(tests)


def _load_mutation_contracts(
    value: object, *, mutation_ids: Sequence[str], contract_ids: set[str]
) -> dict[str, str]:
    payload = _mapping(value, "value_analysis.mutation_contracts")
    mapping = {
        _string(mutation_id, "mutation contract id"): _string(
            contract_id, f"mutation_contracts[{mutation_id!r}]"
        )
        for mutation_id, contract_id in payload.items()
    }
    if list(mapping) != list(mutation_ids):
        raise ValueEvidenceError(
            "mutation_contracts must exactly match planned mutations in order"
        )
    unknown = sorted(set(mapping.values()) - contract_ids)
    if unknown:
        raise ValueEvidenceError(f"mutations reference unknown contracts: {unknown}")
    return mapping


def load_value_analysis(
    value: object,
    *,
    ranked_nodeids: Sequence[str],
    mutation_ids: Sequence[str],
    source_paths: Sequence[str],
) -> ValueAnalysisSpec | None:
    """Load an optional, complete high-value analysis specification."""

    if value is None:
        return None
    payload = _mapping(value, "value_analysis")
    if payload.get("enabled") is not True:
        raise ValueEvidenceError("value_analysis.enabled must be true when present")
    adapter = _string(payload.get("adapter"), "value_analysis.adapter")
    if adapter != ADAPTER:
        raise ValueEvidenceError(f"value_analysis.adapter must be {ADAPTER!r}")
    repetitions = payload.get("baseline_repetitions")
    if not isinstance(repetitions, int) or not 2 <= repetitions <= 5:
        raise ValueEvidenceError("baseline_repetitions must be an integer in [2, 5]")
    contracts = _load_contracts(
        payload.get("required_contracts"),
        source_paths=source_paths,
        test_paths={nodeid.split("::", 1)[0] for nodeid in ranked_nodeids},
    )
    contract_ids = {contract.contract_id for contract in contracts}
    tests = _load_value_tests(
        payload.get("tests"),
        ranked_nodeids=ranked_nodeids,
        contract_ids=contract_ids,
    )
    mutation_contracts = _load_mutation_contracts(
        payload.get("mutation_contracts"),
        mutation_ids=mutation_ids,
        contract_ids=contract_ids,
    )
    missing_test_contracts = sorted(contract_ids - {test.contract_id for test in tests})
    missing_mutant_contracts = sorted(contract_ids - set(mutation_contracts.values()))
    if missing_test_contracts or missing_mutant_contracts:
        raise ValueEvidenceError(
            "every contract needs tests and mutants; "
            f"missing_tests={missing_test_contracts}, "
            f"missing_mutants={missing_mutant_contracts}"
        )
    return ValueAnalysisSpec(
        adapter=adapter,
        baseline_repetitions=repetitions,
        contracts=contracts,
        tests=tests,
        mutation_contracts=mutation_contracts,
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
        if outcome == "ERROR" or (outcome == "FAILED" and row["outcome"] != "ERROR"):
            row["outcome"] = outcome
        elif outcome == "SKIPPED" and row["cases"] == 0:
            row["outcome"] = outcome
        elif outcome == "PASSED" and row["outcome"] == "SKIPPED":
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
        "missing_nodeids": missing,
        "unmapped_cases": unmapped,
    }


def collect_pytest_junit_batch(
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
            "missing_nodeids": list(ranked_nodeids),
            "unmapped_cases": [],
            "error": str(exc),
        }
    return result, report


def _greedy_core_set(
    coverage: Mapping[str, frozenset[str]],
    runtimes: Mapping[str, float],
    universe: frozenset[str],
    rank: Mapping[str, int],
) -> tuple[str, ...]:
    """Return a deterministic irredundant set covering the required universe."""

    uncovered = set(universe)
    selected: list[str] = []
    while uncovered:
        candidates = [nodeid for nodeid, items in coverage.items() if items & uncovered]
        if not candidates:
            return ()
        best = min(
            candidates,
            key=lambda nodeid: (
                -len(coverage[nodeid] & uncovered),
                runtimes.get(nodeid, float("inf")),
                rank[nodeid],
                nodeid,
            ),
        )
        selected.append(best)
        uncovered.difference_update(coverage[best])
    for nodeid in tuple(reversed(selected)):
        reduced = [item for item in selected if item != nodeid]
        covered = (
            frozenset().union(*(coverage[item] for item in reduced))
            if reduced
            else frozenset()
        )
        if universe <= covered:
            selected = reduced
    return tuple(selected)


def _baseline_measurements(
    spec: ValueAnalysisSpec, reports: Sequence[Mapping[str, Any]]
) -> tuple[dict[str, list[str]], dict[str, list[float]], list[str]]:
    nodeids = [test.nodeid for test in spec.tests]
    outcomes: dict[str, list[str]] = {nodeid: [] for nodeid in nodeids}
    durations: dict[str, list[float]] = {nodeid: [] for nodeid in nodeids}
    errors: list[str] = []
    if len(reports) != spec.baseline_repetitions:
        errors.append(
            "baseline repetition count mismatch: "
            f"expected={spec.baseline_repetitions}, actual={len(reports)}"
        )
    for index, report in enumerate(reports, start=1):
        if report.get("status") != "COMPLETE":
            errors.append(f"baseline report {index} is incomplete")
        tests = report.get("tests")
        if not isinstance(tests, dict):
            errors.append(f"baseline report {index} has no test map")
            continue
        for nodeid in nodeids:
            row = tests.get(nodeid)
            if not isinstance(row, dict):
                continue
            if isinstance(row.get("outcome"), str):
                outcomes[nodeid].append(row["outcome"])
            if isinstance(row.get("duration_seconds"), (int, float)):
                durations[nodeid].append(float(row["duration_seconds"]))
    flaky = [
        nodeid
        for nodeid in nodeids
        if len(outcomes[nodeid]) != spec.baseline_repetitions
        or set(outcomes[nodeid]) != {"PASSED"}
    ]
    if flaky:
        errors.append(f"baseline instability or non-pass outcomes: {flaky}")
    return outcomes, durations, errors


def _mutant_kills(
    spec: ValueAnalysisSpec,
    reports: Mapping[str, Mapping[str, Any]],
    outcomes: Mapping[str, str],
) -> tuple[dict[str, set[str]], dict[str, list[str]], list[str]]:
    nodeids = [test.nodeid for test in spec.tests]
    tests_by_nodeid = {test.nodeid: test for test in spec.tests}
    kills_by_test = {nodeid: set() for nodeid in nodeids}
    killers_by_mutant: dict[str, list[str]] = {}
    errors: list[str] = []
    for mutation_id, contract_id in spec.mutation_contracts.items():
        if outcomes.get(mutation_id) != "KILLED":
            errors.append(
                f"mutant {mutation_id!r} outcome is {outcomes.get(mutation_id)!r}"
            )
        report = reports.get(mutation_id)
        if not isinstance(report, dict) or report.get("status") != "COMPLETE":
            errors.append(f"mutant {mutation_id!r} attribution is incomplete")
            killers_by_mutant[mutation_id] = []
            continue
        tests = report.get("tests")
        if not isinstance(tests, dict):
            errors.append(f"mutant {mutation_id!r} has no test map")
            killers_by_mutant[mutation_id] = []
            continue
        killers = [
            nodeid
            for nodeid in nodeids
            if isinstance(tests.get(nodeid), dict)
            and tests[nodeid].get("outcome") in FAILED_OUTCOMES
        ]
        killers_by_mutant[mutation_id] = killers
        for nodeid in killers:
            kills_by_test[nodeid].add(mutation_id)
        if not any(
            tests_by_nodeid[nodeid].contract_id == contract_id for nodeid in killers
        ):
            errors.append(
                f"mutant {mutation_id!r} has no killer bound to "
                f"contract {contract_id!r}"
            )
    return kills_by_test, killers_by_mutant, errors


def _classification_rows(
    spec: ValueAnalysisSpec,
    *,
    coverage: Mapping[str, frozenset[str]],
    runtimes: Mapping[str, float],
    baseline_outcomes: Mapping[str, list[str]],
    kills_by_test: Mapping[str, set[str]],
    killers_by_mutant: Mapping[str, list[str]],
    core: Sequence[str],
) -> list[dict[str, Any]]:
    nodeids = [test.nodeid for test in spec.tests]
    core_set = set(core)
    rows: list[dict[str, Any]] = []
    for test in spec.tests:
        kills = sorted(kills_by_test[test.nodeid])
        if test.nodeid in core_set:
            classification = "CORE"
        elif test.intentional_redundancy and kills:
            classification = "INTENTIONAL_REDUNDANCY"
        elif kills:
            classification = "MERGE"
        else:
            classification = "DELETE_CANDIDATE"
        rows.append(
            {
                "nodeid": test.nodeid,
                "contract_id": test.contract_id,
                "classification": classification,
                "killed_mutants": kills,
                "unique_kills": sorted(
                    mutation_id
                    for mutation_id in kills
                    if killers_by_mutant.get(mutation_id) == [test.nodeid]
                ),
                "runtime_seconds_median": (
                    round(runtimes[test.nodeid], 6)
                    if runtimes[test.nodeid] != float("inf")
                    else None
                ),
                "baseline_outcomes": baseline_outcomes[test.nodeid],
                "dominated_by": sorted(
                    other
                    for other in nodeids
                    if other != test.nodeid
                    and coverage[test.nodeid] <= coverage[other]
                    and runtimes[other] <= runtimes[test.nodeid]
                ),
            }
        )
    return rows


def analyze_test_value(
    spec: ValueAnalysisSpec,
    *,
    baseline_reports: Sequence[Mapping[str, Any]],
    mutant_reports: Mapping[str, Mapping[str, Any]],
    mutant_outcomes: Mapping[str, str],
) -> dict[str, Any]:
    """Build kill attribution, value classifications, and a retained core set."""

    nodeids = [test.nodeid for test in spec.tests]
    baseline_outcomes, baseline_durations, baseline_errors = _baseline_measurements(
        spec, baseline_reports
    )
    kills_by_test, killers_by_mutant, mutant_errors = _mutant_kills(
        spec, mutant_reports, mutant_outcomes
    )
    errors = [*baseline_errors, *mutant_errors]

    universe = frozenset(
        [f"contract:{contract.contract_id}" for contract in spec.contracts]
        + [f"mutant:{mutation_id}" for mutation_id in spec.mutation_contracts]
    )
    coverage = {
        test.nodeid: frozenset(
            {f"contract:{test.contract_id}"}
            | {f"mutant:{mutation_id}" for mutation_id in kills_by_test[test.nodeid]}
        )
        for test in spec.tests
    }
    runtimes = {
        nodeid: (
            statistics.median(baseline_durations[nodeid])
            if baseline_durations[nodeid]
            else float("inf")
        )
        for nodeid in nodeids
    }
    rank = {test.nodeid: index for index, test in enumerate(spec.tests, start=1)}
    core = _greedy_core_set(coverage, runtimes, universe, rank)
    if not core:
        errors.append("no retained test set covers every contract and mutant")
    rows = _classification_rows(
        spec,
        coverage=coverage,
        runtimes=runtimes,
        baseline_outcomes=baseline_outcomes,
        kills_by_test=kills_by_test,
        killers_by_mutant=killers_by_mutant,
        core=core,
    )
    return {
        "schema_version": VALUE_SCHEMA,
        "status": "PASS" if not errors else "FAIL_CLOSED",
        "adapter": spec.adapter,
        "baseline_repetitions": spec.baseline_repetitions,
        "subprocess_scaling": "baseline_repetitions + mutants",
        "errors": errors,
        "required_contracts": [
            {
                "id": contract.contract_id,
                "criticality": contract.criticality,
                "active_paths": list(contract.active_paths),
            }
            for contract in spec.contracts
        ],
        "killers_by_mutant": killers_by_mutant,
        "retained_core": list(core),
        "tests": rows,
        "classification_counts": {
            classification: sum(row["classification"] == classification for row in rows)
            for classification in (
                "CORE",
                "INTENTIONAL_REDUNDANCY",
                "MERGE",
                "DELETE_CANDIDATE",
            )
        },
    }


def admission_errors(
    value_evidence: object, required_nodeids: Sequence[str]
) -> list[str]:
    """Return fail-closed reasons for newly introduced test definitions."""

    if not isinstance(value_evidence, dict):
        return ["receipt has no test_value evidence"]
    errors: list[str] = []
    if value_evidence.get("schema_version") != VALUE_SCHEMA:
        errors.append("test_value schema is not current")
    if value_evidence.get("status") != "PASS":
        errors.append(f"test_value status={value_evidence.get('status')!r}")
    rows = value_evidence.get("tests")
    if not isinstance(rows, list):
        return [*errors, "test_value.tests must be a list"]
    by_nodeid = {
        row.get("nodeid"): row
        for row in rows
        if isinstance(row, dict) and isinstance(row.get("nodeid"), str)
    }
    for nodeid in required_nodeids:
        row = by_nodeid.get(nodeid)
        if row is None:
            errors.append(f"new test {nodeid!r} has no value classification")
            continue
        classification = row.get("classification")
        if classification not in ADMITTED_CLASSIFICATIONS:
            errors.append(
                f"new test {nodeid!r} is classified {classification!r}; "
                "only CORE or INTENTIONAL_REDUNDANCY may be added"
            )
    return errors


def test_value_receipt_errors(
    value: object,
    *,
    expected_nodeids: Sequence[str],
    expected_repetitions: int,
) -> list[str]:
    """Validate the value-specific portion of a mutation receipt."""

    if not isinstance(value, dict):
        return ["test_value evidence is missing"]
    errors: list[str] = []
    if value.get("schema_version") != VALUE_SCHEMA:
        errors.append("test_value schema is not current")
    if value.get("status") != "PASS":
        errors.append(f"test_value status={value.get('status')!r}")
    rows = value.get("tests")
    actual_nodeids = (
        [row.get("nodeid") for row in rows if isinstance(row, dict)]
        if isinstance(rows, list)
        else []
    )
    if actual_nodeids != list(expected_nodeids):
        errors.append("test_value nodeids do not match ranked tests")
    if value.get("baseline_repetitions") != expected_repetitions:
        errors.append("test_value baseline repetitions mismatch")
    return errors
