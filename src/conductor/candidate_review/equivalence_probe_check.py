"""Tier-1 equivalence probe, as a code-review check.

Split out of `checks.py` on 2026-09-05: that module sat at 1,245 of its 1,250-line
budget, which made it unextendable -- any addition tripped the god-file check. The
probe is the natural piece to lift, because it is the one check that shells out to
a separate engine and carries its own backlog bookkeeping.

`check_import_declaration` is the same shape and is the pattern this follows,
including the deferred `_result` import that keeps the two modules acyclic.
"""

from __future__ import annotations

import json
import sys
import time
from datetime import UTC, datetime
from pathlib import Path
from typing import TYPE_CHECKING

from conductor.candidate_review.model import CheckResult, Finding, Severity

if TYPE_CHECKING:
    from conductor.candidate_review.model import ReviewContext


def _record_for_backlog(summary: dict, ctx: ReviewContext) -> None:
    """Drop this run's summary where `make slop-backlog` will find it.

    Best effort by design: a full disk or a read-only checkout must not fail a code
    review over bookkeeping. Nothing downstream reads a partial write, because the
    file is renamed into place only once it is complete.

    `TypeError` is caught alongside `OSError` because `json.dumps` raises it, not
    `OSError`, when a finding carries a value it cannot serialize -- and on
    2026-09-05 that took the entire review down from inside a function documented
    as best-effort (a `bytes` `stderr_tail` from `TimeoutExpired`). Caught, but
    never silent: a summary that could not be recorded is reported on stderr, so
    the bookkeeping failure is visible without being fatal.
    """
    from conductor.slop_ledger import GATE_FINDINGS

    try:
        GATE_FINDINGS.mkdir(parents=True, exist_ok=True)
        stamp = datetime.now(UTC).strftime("%Y%m%dT%H%M%S%f")
        final = GATE_FINDINGS / f"gate-{stamp}.json"
        tmp = final.with_suffix(".json.part")
        tmp.write_text(json.dumps(summary, indent=2) + "\n")
        tmp.rename(final)
    except (OSError, TypeError) as exc:
        print(
            f"slop-backlog: this run's summary was not recorded: {exc!r}",
            file=sys.stderr,
        )


def _findings_from_summary(summary: dict) -> list[Finding]:
    """Turn one probe summary into this check's findings.

    Split out of `check_equivalence_probe` on 2026-09-05 to keep that function
    inside the 100-line budget. The seam is measurement (there) against its
    interpretation (here): everything below reads `summary` and nothing else.
    """
    from conductor import slop_ledger

    # Severity follows the tier, which is what makes this check enforceable: a
    # finding in shipped code blocks, one in a one-off research script reports. The
    # tier comes from slop_ledger.aggregate rather than a second copy of the prefix
    # list, so the gate and the backlog can never disagree about what ships.
    tiers = {
        (item["module"], item["qualname"]): item["tier"]
        for item in slop_ledger.aggregate(
            slop_ledger.findings_from_summary({"blocking": summary["blocking"]})
        )
    }
    findings = [
        Finding(
            check_id="equivalence-probe",
            rule_id=item["rule"],
            severity=(
                Severity.HIGH
                if tiers.get((item["module"], item["qualname"])) == "shipped"
                else Severity.LOW
            ),
            path=item["module"],
            line=item["lineno"],
            message=(
                f"{item['qualname']}: {item['description']} changes the result only "
                f"under {item.get('amplifier')} (relative change "
                f"{item.get('max_diff_amplified'):.3e}); no test drives that regime. "
                "Cover it or remove the construct."
            ),
            evidence={
                k: item[k] for k in ("qualname", "verdict", "amplifier") if k in item
            },
        )
        for item in summary["blocking"]
    ]
    # A module that timed out or crashed produced no evidence either way. Reported
    # at MEDIUM -- visible in the report, not blocking -- because the missing
    # measurement is a cost and coverage signal about the probe, not a defect in the
    # change. Silence here is what let a 180s timeout read as a clean sweep.
    findings.extend(
        Finding(
            check_id="equivalence-probe",
            rule_id=f"probe-{item['verdict'].lower()}",
            severity=Severity.MEDIUM,
            path=item["module"],
            line=0,
            message=(
                f"{item['description']}; a PASS here is the absence of a "
                "measurement, not the absence of a finding"
            ),
            help=(
                "Re-run `python -m conductor.slop_gate --module <module>` to see the "
                "child's own output, or narrow the module's drivers."
            ),
            evidence={
                k: item[k]
                for k in ("verdict", "rule", "duration_s", "stderr_tail")
                if k in item
            },
        )
        for item in summary.get("incomplete", ())
    )
    return findings


def check_equivalence_probe(ctx: ReviewContext) -> CheckResult:
    """Tier-1 gate: no change ships with a reachable branch no test drives.

    The mutation gate asks whether a test notices a corrupted line. This asks what
    that cannot answer -- whether the changed code does anything, and whether the
    tests can tell. Only REACHABLE_BUT_UNTESTED is a finding: a construct that
    changes behaviour in a regime the tests never reach. Sampling cannot prove the
    converse, so "nothing moved" is reported by the CLI and never blocks here.

    It runs against the candidate snapshot, not the working tree, because that is
    the tree that ships.
    """
    from conductor.candidate_review.checks import _result

    started = time.perf_counter()
    from conductor import _native

    if _native.SLOP_CORE_UNAVAILABLE:  # decided once at import, never per call
        finding = Finding(
            check_id="equivalence-probe",
            rule_id="slop-core-unavailable",
            severity=Severity.CRITICAL,
            message=f"required native engine is unavailable: {_native.SLOP_CORE_UNAVAILABLE}",
            help="Install the crate; the probe never skips on a missing engine.",
        )
        return _result("equivalence-probe", started, (finding,), files=())
    from conductor import slop_gate

    modules = [
        change.path
        for change in ctx.live_changes
        if change.path.endswith(".py")
        and "test" not in change.classes
        and not Path(change.path).name.startswith("test_")
    ]
    if not modules:
        return _result("equivalence-probe", started, (), files=())

    _, summary = slop_gate.run("HEAD", ctx.snapshot, only=modules)

    # The gate already paid for this measurement, so hand it to the backlog rather
    # than discarding it. A run ARTIFACT, not a ledger write: this check runs against
    # a candidate snapshot and concurrently with other reviews, and a review that
    # mutates a tracked file dirties the tree it is reviewing. `make slop-backlog`
    # folds these in, which is what keeps the backlog current between full sweeps.
    _record_for_backlog(summary, ctx)

    findings = _findings_from_summary(summary)
    incomplete = summary.get("incomplete", ())
    return _result(
        "equivalence-probe",
        started,
        findings,
        files=modules,
        metrics={
            "modules_probed": summary["modules_probed"],
            "advisory": len(summary["advisory"]),
            "without_drivers": len(summary["modules_without_drivers"]),
            "timed_out": sum(1 for f in incomplete if f["verdict"] == "TIMEOUT"),
            "probe_failed": sum(
                1 for f in incomplete if f["verdict"] == "PROBE_FAILED"
            ),
        },
    )
