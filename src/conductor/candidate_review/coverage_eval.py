"""Changed-line coverage: what the targeted run actually measured, judged by risk.

Split out of `verification` on 2026-09-03: coverage adjudication shares nothing
with the mutation-evidence and grandfather machinery it used to sit beside, and
the file had reached the 1250-line ceiling.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Mapping

from conductor.candidate_review.checks import ReviewContext
from conductor.candidate_review.command_runner import _run_process
from conductor.candidate_review.git_source import changed_line_numbers
from conductor.candidate_review.model import Finding, Severity


def _risk_buckets(
    per_file: Mapping[str, Mapping[str, int]], risk_of: Mapping[str, str]
) -> dict[str, dict[str, int]]:
    """Split measured changed lines into high-risk and everything else.

    A path with no recorded risk counts as other-risk: it is never silently
    promoted to the lenient side by being unknown, nor held to the strict bar it
    was never classified into.
    """
    buckets = {
        "high": {"covered": 0, "measurable": 0},
        "other": {"covered": 0, "measurable": 0},
    }
    for path, counts in per_file.items():
        bucket = "high" if risk_of.get(path) == "high" else "other"
        buckets[bucket]["covered"] += counts["covered"]
        buckets[bucket]["measurable"] += counts["measurable"]
    return buckets


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
    # Evaluate each risk class against its own bar, using the lines actually
    # MEASURED rather than whether any changed path happens to be high-risk.
    # The old rule let a single conductor/** file raise the bar for every changed
    # line in the candidate, so a shortfall in low-risk code was judged at 90 --
    # measured 2026-08-29 on the W7 slices, where the bar and the shortfall came
    # from different paths entirely.
    risk_of = {change.path: change.risk for change in ctx.live_changes}
    buckets = _risk_buckets(per_file, risk_of)

    findings: list[Finding] = []
    metrics: dict[str, object] = {
        "changed_coverage_percent": round(percent, 2),
        "changed_lines_measurable": measurable,
        "changed_lines_covered": covered,
    }
    for bucket, threshold in (
        ("high", ctx.policy.high_risk_coverage_threshold),
        ("other", ctx.policy.coverage_threshold),
    ):
        counts = buckets[bucket]
        if counts["measurable"] == 0:
            continue
        pct = counts["covered"] * 100.0 / counts["measurable"]
        metrics[f"changed_coverage_percent_{bucket}"] = round(pct, 2)
        metrics[f"changed_coverage_threshold_{bucket}"] = threshold
        metrics[f"changed_lines_measurable_{bucket}"] = counts["measurable"]
        metrics[f"changed_lines_covered_{bucket}"] = counts["covered"]
        if pct < threshold:
            findings.append(
                Finding(
                    check_id="targeted-tests-full",
                    rule_id="changed-code-coverage",
                    severity=Severity.HIGH,
                    message=(
                        f"changed-code coverage for {bucket}-risk lines is "
                        f"{pct:.1f}%, below {threshold:.1f}%"
                    ),
                    evidence={
                        "risk_class": bucket,
                        "covered": counts["covered"],
                        "measurable": counts["measurable"],
                        "per_file": {
                            path: c
                            for path, c in per_file.items()
                            if ("high" if risk_of.get(path) == "high" else "other")
                            == bucket
                        },
                    },
                )
            )
    return findings, metrics


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
