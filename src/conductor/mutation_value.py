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

from conductor._native import (
    admission_errors_native,
    analyze_test_value_reports_native,
    load_value_analysis_native,
    mutation_value_attribution_supported_native,
    mutation_value_cargo_identity_native,
    mutation_value_ctest_identity_native,
    mutation_value_parse_cargo_native,
    mutation_value_parse_junit_native,
    mutation_value_pytest_identity_native,
)
from conductor._native import (
    test_value_receipt_errors_native as _test_value_receipt_errors_native,
)

VALUE_SCHEMA = "llm.mutation-testing.test-value.v1"
ADAPTER = "pytest-junit"
CTEST_ADAPTER = "ctest-junit"
CARGO_ADAPTER = "cargo-libtest"


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
    if native is not None:
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
    try:
        return mutation_value_pytest_identity_native(nodeid)
    except ValueError as exc:
        raise ValueEvidenceError(str(exc)) from exc


def pytest_attribution_supported(
    argv: Sequence[str], ranked_nodeids: Sequence[str]
) -> bool:
    """Report whether this batch can be run under JUnit and mapped to nodeids."""

    if not ranked_nodeids:
        return False
    if any(arg == "--junitxml" or arg.startswith("--junitxml=") for arg in argv):
        return False
    return mutation_value_attribution_supported_native(ADAPTER, list(ranked_nodeids))


def parse_pytest_junit(
    report_path: Path, ranked_nodeids: Sequence[str]
) -> dict[str, Any]:
    """Parse one pytest JUnit report into function-level outcomes and runtimes."""

    try:
        return json.loads(
            mutation_value_parse_junit_native(
                str(report_path), ADAPTER, list(ranked_nodeids)
            )
        )
    except ValueError as exc:
        raise ValueEvidenceError(str(exc)) from exc


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


def _cargo_identity(nodeid: str) -> str:
    """Return the test function name a `<path>.rs::<fn>` nodeid names."""

    try:
        return mutation_value_cargo_identity_native(nodeid)
    except ValueError as exc:
        raise ValueEvidenceError(str(exc)) from exc


def cargo_attribution_supported(ranked_nodeids: Sequence[str]) -> bool:
    """Report whether this batch's ranked tests can be mapped to libtest output.

    Shape is read from the nodeids, not from argv: a campaign may wrap its run in
    `sh -c` or a cargo alias, and refusing attribution because argv[0] is not
    literally `cargo` would leave exactly those campaigns unattributed.
    """

    return mutation_value_attribution_supported_native(
        CARGO_ADAPTER, list(ranked_nodeids)
    )


def parse_cargo_libtest(stdout: str, ranked_nodeids: Sequence[str]) -> dict[str, Any]:
    """Map one libtest run's printed outcomes onto the campaign's ranked nodeids."""

    try:
        return json.loads(
            mutation_value_parse_cargo_native(stdout, list(ranked_nodeids))
        )
    except ValueError as exc:
        raise ValueEvidenceError(str(exc)) from exc


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


# --------------------------------------------------------------- ctest / CMake
C_TEST_SUFFIXES = (".c", ".cc", ".cpp", ".cxx")
# The batch writes here and the runner reads here. A relative, campaign-visible
# path rather than a runner-injected flag: a C batch has to build before it can
# test, so its argv is a shell line (`cmake --build ... && ctest ...`) that no
# argv rewrite can safely reach into.
CTEST_JUNIT_RELATIVE = Path(".mutation-value") / "ctest.xml"


def _ctest_identity(nodeid: str) -> str:
    """Return the ctest name a `<path>.c::<fn>` nodeid names.

    CTest names are flat and project-global while nodeids are path-qualified,
    so the registration in CMakeLists is `<binary stem>.<function>` and the
    stem of the nodeid's file is what carries the binary.
    """

    try:
        return mutation_value_ctest_identity_native(nodeid)
    except ValueError as exc:
        raise ValueEvidenceError(str(exc)) from exc


def ctest_attribution_supported(ranked_nodeids: Sequence[str]) -> bool:
    """Report whether this batch's ranked tests can be mapped to ctest names.

    Read from the nodeids, not from argv, for the same reason the cargo adapter
    does: a C batch builds before it tests, so its argv is a shell line and
    `ctest` is not argv[0].
    """

    return mutation_value_attribution_supported_native(
        CTEST_ADAPTER, list(ranked_nodeids)
    )


def ctest_junit_path(snapshot_root: Path) -> Path:
    """Return the report path a ctest batch must write, inside one snapshot."""

    return snapshot_root / CTEST_JUNIT_RELATIVE


def parse_ctest_junit(
    report_path: Path, ranked_nodeids: Sequence[str]
) -> dict[str, Any]:
    """Parse one ctest JUnit report into per-test outcomes and runtimes."""

    try:
        return json.loads(
            mutation_value_parse_junit_native(
                str(report_path), CTEST_ADAPTER, list(ranked_nodeids)
            )
        )
    except ValueError as exc:
        raise ValueEvidenceError(str(exc)) from exc


def collect_ctest_junit_batch[BatchResultT](
    *,
    argv: Sequence[str],
    report_path: Path,
    ranked_nodeids: Sequence[str],
    run_command: Callable[[Sequence[str]], BatchResultT],
) -> tuple[BatchResultT, Mapping[str, Any]]:
    """Run one ctest batch and read fail-closed attribution from its report."""

    report_path.parent.mkdir(parents=True, exist_ok=True)
    # A mutant that breaks the build leaves ctest unrun. Without this removal
    # the previous mutant's report is still on disk and would be read as this
    # one's evidence -- a kill attributed to a run that never happened. Absent
    # afterwards has to mean "no attribution", which is what fail-closed needs.
    report_path.unlink(missing_ok=True)
    result = run_command(argv)
    try:
        report = parse_ctest_junit(report_path, ranked_nodeids)
    except ValueEvidenceError as exc:
        report = {
            "status": "INCOMPLETE",
            "tests": {},
            "failed_nodeids": [],
            "missing_nodeids": list(ranked_nodeids),
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

    try:
        return json.loads(
            analyze_test_value_reports_native(
                json.dumps(_spec_payload(spec)),
                json.dumps(baseline_reports),
                json.dumps(mutant_reports),
                json.dumps(mutant_outcomes),
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
