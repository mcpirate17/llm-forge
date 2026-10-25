"""The equivalence probe, as a code-review check.

slop_core runs it: `equivalence_gate` probes each changed unit against the recorded
tests that ran its lines, or, on a host with no record, the test files that import its
module, and `equivalence_findings` turns the summary into findings and metrics and
leaves it where the backlog folds it in. This module gathers the inputs.

The probe runs against a separate materialization of the candidate tree, not the
shared review snapshot: a replay can write through a module-relative default path,
and later checks read that snapshot. `check_import_declaration` is the same shape,
including the deferred `_result` import that keeps this module and `checks` acyclic.
"""

from __future__ import annotations

import json
import time
from pathlib import Path
from typing import TYPE_CHECKING

from conductor.candidate_review.git_source import materialize_tree
from conductor.candidate_review.model import CheckResult, Finding, Severity

if TYPE_CHECKING:
    from conductor.candidate_review.checks import ReviewContext


def check_equivalence_probe(ctx: ReviewContext) -> CheckResult:
    """No change ships with a construct that matters only where no test looks.

    Only REACHABLE_BUT_UNTESTED blocks: a construct that changes behaviour in a regime
    the driver tests never reach. A probe that timed out or failed is a MEDIUM finding,
    because a pass there is the absence of a measurement.
    """
    from conductor import _native
    from conductor.candidate_review.checks import _result
    from conductor.candidate_review.coverage_eval import changed_source_lines
    from conductor.equivalence_gate import host, probe

    started = time.perf_counter()
    if _native.SLOP_CORE_UNAVAILABLE:  # decided once at import, never per call
        finding = Finding(
            check_id="equivalence-probe",
            rule_id="slop-core-unavailable",
            severity=Severity.CRITICAL,
            message=f"required native engine is unavailable: {_native.SLOP_CORE_UNAVAILABLE}",
            help="Install the crate; the probe never skips on a missing engine.",
        )
        return _result("equivalence-probe", started, (finding,), files=())
    changed = {
        path: lines for path, lines in changed_source_lines(ctx).items() if lines
    }
    if not changed:
        return _result("equivalence-probe", started, (), files=())
    snapshot = Path(ctx.snapshot).resolve()
    found = host(snapshot)
    within = found.root.relative_to(snapshot)
    changes = [
        ((snapshot / path).relative_to(found.root).as_posix(), sorted(lines))
        for path, lines in sorted(changed.items())
    ]
    with materialize_tree(ctx.repo, ctx.candidate.tree_oid) as (probe_snapshot, _):
        summary = probe(found, probe_snapshot / within, changes)
    rows, metrics = _native.slop_core().equivalence_findings(
        summary, found.path("run_artifacts", Path(ctx.repo) / within)
    )
    findings = [
        Finding(
            check_id="equivalence-probe", severity=Severity(row.pop("severity")), **row
        )
        for row in json.loads(rows)
    ]
    return _result(
        "equivalence-probe",
        started,
        findings,
        files=sorted(changed),
        metrics=json.loads(metrics),
    )
