"""Targeted tests reuse the host's recorded passes.

A host that declares ``[tool.forge.test_record]`` records every pytest run with the
``forge_test_record`` plugin. On every profile but ``nightly`` the targeted shards ask
the plugin to skip a test whose newest run against identical inputs passed; the
nightly sweep runs everything, so a skip resting on an incomplete record is caught
within a day. A host without the table ignores the switch and writes no report.
"""

from __future__ import annotations

import json
import shutil
from pathlib import Path

from conductor.candidate_review.checks import ReviewContext


def _report_dir(ctx: ReviewContext, check_id: str) -> Path:
    return ctx.runtime_dir / "test-record-reuse" / check_id


def reuse_environment(ctx: ReviewContext, check_id: str) -> dict[str, str]:
    """The shard variables that turn reuse on; clears an earlier run's reports."""
    directory = _report_dir(ctx, check_id)
    if directory.exists():
        shutil.rmtree(directory)
    if ctx.profile == "nightly":
        return {}
    return {
        "FORGE_TEST_RECORD_REUSE": "1",
        "FORGE_TEST_RECORD_REUSE_REPORT": str(directory),
    }


def reuse_metrics(ctx: ReviewContext, check_id: str) -> dict[str, object]:
    """How many collected tests the shards skipped on a recorded pass."""
    directory = _report_dir(ctx, check_id)
    if not directory.is_dir():
        return {}
    reports = [
        json.loads(path.read_text(encoding="utf-8"))
        for path in sorted(directory.glob("*.json"))
    ]
    return {
        "reused_tests": sum(report["reused"] for report in reports),
        "reused_of_collected": sum(report["collected"] for report in reports),
        "reused_recorded_seconds": round(
            sum(report["recorded_seconds"] for report in reports), 1
        ),
    }
