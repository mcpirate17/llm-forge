"""Dependency-graph test selection, targeted execution, and changed-line coverage."""

from __future__ import annotations

import json
import re
import sqlite3
import subprocess
import sys
import time
from pathlib import Path, PurePosixPath
from typing import Sequence

from conductor.candidate_review.checks import (
    ReviewContext,
    TestSelection,
    _result,
)
from conductor.candidate_review.command_runner import _run_process, _tail
from conductor.candidate_review.git_source import changed_line_numbers
from conductor.candidate_review.model import (
    CheckResult,
    CheckStatus,
    Finding,
    Severity,
    sha256_json,
)
from conductor.candidate_review.policy import CheckPolicy

TEST_PROPERTY_TEXT = re.compile(
    r"hypothesis|@(?:pytest\.)?mark\.parametrize|@pytest\.mark\.(?:property|invariant)|"
    r"def test_.*(?:property|invariant|boundary|roundtrip|adversarial)"
)


def _graph_database(repo: Path) -> Path:
    return repo / ".code-review-graph" / "graph.db"


def _graph_test_paths(
    ctx: ReviewContext, source_paths: Sequence[str]
) -> tuple[set[str], dict[str, object]]:
    database = _graph_database(ctx.repo)
    if not database.is_file():
        raise RuntimeError("code-review graph database is missing")
    uri = f"file:{database.as_posix()}?mode=ro&immutable=1"
    connection = sqlite3.connect(uri, uri=True, timeout=2.0)
    try:
        metadata = dict(connection.execute("SELECT key, value FROM metadata"))
        expected = (
            ctx.candidate.base_commit_oid
            if ctx.candidate.kind == "index"
            else ctx.candidate.commit_oid
        )
        if not expected or metadata.get("git_head_sha") != expected:
            raise RuntimeError(
                "stale code-review graph: "
                f"expected {expected}, found {metadata.get('git_head_sha')}"
            )
        absolute = [str((ctx.repo / path).resolve()) for path in source_paths]
        if not absolute:
            return set(), {
                "head_sha": expected,
                "schema_version": metadata.get("schema_version"),
            }
        placeholders = ",".join("?" for _ in absolute)
        rows = connection.execute(
            f"""
            SELECT DISTINCT source.file_path, edge.kind, target.qualified_name
            FROM nodes AS target
            JOIN edges AS edge ON edge.target_qualified = target.qualified_name
            JOIN nodes AS source ON source.qualified_name = edge.source_qualified
            WHERE target.file_path IN ({placeholders}) AND source.is_test = 1
            ORDER BY source.file_path, edge.kind, target.qualified_name
            """,
            absolute,
        ).fetchall()
        tests: set[str] = set()
        evidence_rows: list[tuple[str, str, str]] = []
        for file_path, edge_kind, target in rows:
            try:
                relative = Path(file_path).resolve().relative_to(ctx.repo).as_posix()
            except ValueError:
                continue
            if (ctx.snapshot / relative).is_file():
                tests.add(relative)
                evidence_rows.append((relative, edge_kind, target))
        graph = {
            "head_sha": expected,
            "schema_version": metadata.get("schema_version"),
            "last_updated": metadata.get("last_updated"),
            "selected_edges": len(evidence_rows),
            "evidence_sha256": sha256_json(evidence_rows),
        }
        return tests, graph
    finally:
        connection.close()


def _convention_tests(ctx: ReviewContext, source_paths: Sequence[str]) -> set[str]:
    names = {f"test_{PurePosixPath(path).stem}.py" for path in source_paths}
    tests: set[str] = set()
    for base in (
        "conductor",
        "research/tests",
        "component_fab/tests",
        "aria_core/tests",
        "aria_designer/tests",
    ):
        root = ctx.snapshot / base
        if not root.is_dir():
            continue
        for path in root.rglob("test*.py"):
            rel = path.relative_to(ctx.snapshot).as_posix()
            if path.name in names:
                tests.add(rel)
                continue
            try:
                text = path.read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError):
                continue
            for source in source_paths:
                module = source.removesuffix(".py").replace("/", ".")
                if module in text:
                    tests.add(rel)
                    break
    return tests


def select_tests(ctx: ReviewContext) -> TestSelection:
    sources = [
        change.path
        for change in ctx.live_changes
        if ({"python", "native"} & set(change.classes)) and "test" not in change.classes
    ]
    changed_tests = {
        change.path
        for change in ctx.live_changes
        if "test" in change.classes and change.path.endswith(".py")
    }
    findings: list[Finding] = []
    graph: dict[str, object] = {}
    graph_tests: set[str] = set()
    if sources:
        try:
            graph_tests, graph = _graph_test_paths(ctx, sources)
        except (RuntimeError, sqlite3.Error) as exc:
            findings.append(
                Finding(
                    check_id="test-evidence",
                    rule_id="graph-evidence-incomplete",
                    severity=Severity.CRITICAL,
                    message=f"dependency/call-graph test selection failed closed: {exc}",
                )
            )
    else:
        graph = {"status": "not-required", "selected_edges": 0}
    tests = graph_tests | _convention_tests(ctx, sources) | changed_tests
    if sources and not tests:
        findings.append(
            Finding(
                check_id="test-evidence",
                rule_id="no-targeted-tests",
                severity=Severity.HIGH,
                message="changed production code has no graph-selected or convention-matched tests",
                evidence={"source_paths": sources},
            )
        )
    high_risk = any(
        change.risk == "high" and "test" not in change.classes
        for change in ctx.live_changes
    )
    if high_risk and tests and not _has_property_evidence(ctx, tests):
        findings.append(
            Finding(
                check_id="test-evidence",
                rule_id="missing-property-or-mutation-evidence",
                severity=Severity.HIGH,
                message="high-risk logic lacks property/parameterized/mutation-style test evidence",
            )
        )
    return TestSelection(
        tuple(sorted(tests)), graph, tuple(finding.finalize() for finding in findings)
    )


def _has_property_evidence(ctx: ReviewContext, tests: set[str]) -> bool:
    for test in tests:
        try:
            if TEST_PROPERTY_TEXT.search(
                (ctx.snapshot / test).read_text(encoding="utf-8")
            ):
                return True
        except (OSError, UnicodeDecodeError):
            continue
    return False


def check_mutation_evidence(ctx: ReviewContext) -> CheckResult:
    """Require current mutation PASS receipts for every changed test file."""

    started = time.perf_counter()
    test_paths = [
        change.path for change in ctx.live_changes if "test" in change.classes
    ]
    if not test_paths:
        return CheckResult(
            check_id="mutation-evidence",
            status=CheckStatus.SKIPPED,
            duration_ms=0,
            skipped_reason="no matching candidate test changes",
        )
    registry = ctx.snapshot / "conductor/mutation_campaigns/registry.json"
    if not registry.is_file():
        finding = Finding(
            check_id="mutation-evidence",
            rule_id="mutation-registry-missing",
            severity=Severity.CRITICAL,
            message=(
                "candidate snapshot lacks conductor/mutation_campaigns/registry.json; "
                "changed tests cannot prove mutation evidence"
            ),
            help=(
                "Include the mutation registry, campaign, and PASS receipt in the "
                "same candidate as the test file."
            ),
        )
        return _result("mutation-evidence", started, [finding], files=test_paths)
    from conductor.mutation_testing import CampaignError, verify_evidence

    try:
        payload = verify_evidence(registry, test_paths, repo_root=ctx.snapshot)
    except CampaignError as exc:
        finding = Finding(
            check_id="mutation-evidence",
            rule_id="mutation-evidence-unavailable",
            severity=Severity.CRITICAL,
            message=f"mutation evidence could not be verified: {exc}",
        )
        return _result("mutation-evidence", started, [finding], files=test_paths)
    findings: list[Finding] = []
    for missing in payload.get("missing_evidence", []):
        if not isinstance(missing, dict):
            continue
        path_name = str(missing.get("path", ""))
        findings.append(
            Finding(
                check_id="mutation-evidence",
                rule_id="missing-mutation-receipt",
                severity=Severity.CRITICAL,
                message=(
                    f"{path_name}: {missing.get('reason', 'missing mutation evidence')}"
                ),
                path=path_name or None,
                help=(
                    "Scaffold with `python -m conductor.mutation_coverage scaffold "
                    "PATH --source SRC`, register the campaign, obtain Tim's "
                    "authority, then `make mutation-run` and keep the PASS receipt."
                ),
                evidence={
                    "receipt_rejections": missing.get("receipt_rejections", []),
                },
            )
        )
    for malformed in payload.get("malformed_receipts", []):
        findings.append(
            Finding(
                check_id="mutation-evidence",
                rule_id="malformed-mutation-receipt",
                severity=Severity.CRITICAL,
                message=f"malformed mutation receipt: {malformed}",
            )
        )
    return _result(
        "mutation-evidence",
        started,
        findings,
        files=test_paths,
        metrics={
            "checked_test_paths": payload.get("checked_test_paths", []),
            "covered_tests": len(payload.get("evidence", [])),
            "missing_tests": len(payload.get("missing_evidence", [])),
        },
    )


def check_test_evidence(ctx: ReviewContext) -> tuple[CheckResult, TestSelection]:
    started = time.perf_counter()
    selection = select_tests(ctx)
    result = _result(
        "test-evidence",
        started,
        selection.findings,
        files=selection.tests,
        metrics={"selected_tests": len(selection.tests), **selection.graph},
    )
    return result, selection


def run_targeted_tests(
    ctx: ReviewContext,
    selection: TestSelection,
    check: CheckPolicy,
    *,
    coverage: bool,
) -> CheckResult:
    started = time.perf_counter()
    if not selection.tests:
        return CheckResult(
            check_id=check.check_id,
            status=CheckStatus.SKIPPED,
            duration_ms=0,
            skipped_reason="no selected tests",
        )
    coverage_file = ctx.runtime_dir / ".coverage-targeted"
    command = _pytest_command(selection, coverage_file if coverage else None)
    try:
        completed = _run_process(
            command,
            ctx=ctx,
            timeout_seconds=check.timeout_seconds,
            memory_mb=check.memory_mb,
            include_git_metadata=False,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        finding = Finding(
            check_id=check.check_id,
            rule_id="targeted-test-crash",
            severity=Severity.CRITICAL,
            message=f"targeted test execution did not complete: {type(exc).__name__}: {exc}",
        )
        return _result(check.check_id, started, [finding], files=selection.tests)
    findings: list[Finding] = []
    metrics: dict[str, object] = {"selected_tests": len(selection.tests)}
    if completed.returncode:
        findings.append(
            Finding(
                check_id=check.check_id,
                rule_id="targeted-test-failure",
                severity=Severity.HIGH,
                message=_tail(
                    completed.stdout + "\n" + completed.stderr,
                    check.max_output_chars,
                ).strip(),
                evidence={"exit_code": completed.returncode},
            )
        )
    elif coverage:
        coverage_findings, coverage_metrics = _evaluate_changed_coverage(
            ctx, coverage_file
        )
        findings.extend(coverage_findings)
        metrics.update(coverage_metrics)
    result = _result(
        check.check_id, started, findings, files=selection.tests, metrics=metrics
    )
    result.command = command
    result.exit_code = completed.returncode
    result.stdout_tail = _tail(completed.stdout, check.max_output_chars)
    result.stderr_tail = _tail(completed.stderr, check.max_output_chars)
    return result


def _pytest_command(selection: TestSelection, coverage_file: Path | None) -> list[str]:
    prefix = [sys.executable]
    if coverage_file is not None:
        prefix.extend(
            [
                "-m",
                "coverage",
                "run",
                "--branch",
                f"--data-file={coverage_file}",
            ]
        )
    prefix.extend(
        [
            "-m",
            "pytest",
            "-q",
            "-o",
            "addopts=",
            "-p",
            "no:cacheprovider",
            *selection.tests,
        ]
    )
    return prefix


def _evaluate_changed_coverage(
    ctx: ReviewContext, coverage_file: Path
) -> tuple[list[Finding], dict[str, object]]:
    output = ctx.runtime_dir / "coverage.json"
    command = [
        sys.executable,
        "-m",
        "coverage",
        "json",
        f"--data-file={coverage_file}",
        "-o",
        str(output),
        "--quiet",
    ]
    completed = _run_process(
        command,
        ctx=ctx,
        timeout_seconds=60,
        memory_mb=2048,
        limit_resources=False,
        include_git_metadata=False,
    )
    if completed.returncode or not output.is_file():
        return [
            Finding(
                check_id="targeted-tests-full",
                rule_id="coverage-incomplete",
                severity=Severity.CRITICAL,
                message=(
                    completed.stderr
                    or completed.stdout
                    or "coverage JSON was not produced"
                ).strip(),
            )
        ], {}
    payload = json.loads(output.read_text(encoding="utf-8"))
    source_paths = [
        change.path
        for change in ctx.live_changes
        if "python" in change.classes and "test" not in change.classes
    ]
    changed = changed_line_numbers(ctx.repo, ctx.candidate, source_paths)
    covered, measurable, per_file = _coverage_counts(ctx, payload, changed)
    percent = 100.0 if measurable == 0 else covered * 100.0 / measurable
    threshold = (
        ctx.policy.high_risk_coverage_threshold
        if any(change.risk == "high" for change in ctx.live_changes)
        else ctx.policy.coverage_threshold
    )
    findings: list[Finding] = []
    if percent < threshold:
        findings.append(
            Finding(
                check_id="targeted-tests-full",
                rule_id="changed-code-coverage",
                severity=Severity.HIGH,
                message=f"changed-code coverage is {percent:.1f}%, below {threshold:.1f}%",
                evidence={
                    "covered": covered,
                    "measurable": measurable,
                    "per_file": per_file,
                },
            )
        )
    return findings, {
        "changed_coverage_percent": round(percent, 2),
        "changed_coverage_threshold": threshold,
        "changed_lines_measurable": measurable,
        "changed_lines_covered": covered,
    }


def _coverage_counts(
    ctx: ReviewContext,
    payload: object,
    changed: dict[str, set[int]],
) -> tuple[int, int, dict[str, dict[str, int]]]:
    if not isinstance(payload, dict) or not isinstance(payload.get("files"), dict):
        raise ValueError("coverage JSON has no files object")
    coverage_files = payload["files"]
    covered = 0
    measurable = 0
    per_file: dict[str, dict[str, int]] = {}
    for rel, changed_lines in changed.items():
        record = coverage_files.get(rel) or coverage_files.get(str(ctx.snapshot / rel))
        if not isinstance(record, dict):
            continue
        executed = set(record.get("executed_lines", []))
        missing = set(record.get("missing_lines", []))
        relevant = changed_lines & (executed | missing)
        hits = relevant & executed
        measurable += len(relevant)
        covered += len(hits)
        per_file[rel] = {"measurable": len(relevant), "covered": len(hits)}
    return covered, measurable, per_file
