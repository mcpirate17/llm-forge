"""`cost-budget-audit`: the five-metric budget ratchet (design step 5,
Phase 3 step 4).

`docs/design/cost_ledger.md` section 4 names three numbers that must never
silently regress: median hook `elapsed_ms`, resend bytes per session, and
tokens per landed PR. Phase 3 step 4 (`docs/roadmap.md`) added two more on
the routing side: `cap_breach_rate` and `cheap_tier_rework_rate`. This
module does not name any of the five itself -- `AuditResult.metrics` is a
plain `dict[str, MetricResult]` keyed by whatever `forge ledger audit`
reports, so a metric count change is a native-side and baseline-file
change only; nothing here hardcodes "three" or enumerates metric names.
This module is the Python side of that ratchet -- it
shells out to the native computation (`forge ledger audit`,
`native/forge/src/ledger/audit.rs`) the same way `project_init.py` already
finds a `forge` binary for hook wiring, parses its JSON with Pydantic v2
models, and turns the verdict into a `conductor.gate.PhaseResult`.

Baseline discipline mirrors `conductor.mutation_patch_audit` exactly
(KB-MUT-02): a baseline is a tracked file (`ledger/cost_budget_baseline.json`)
that changes only when someone deliberately re-records it, never as a side
effect of a check. `RATCHET_HELD` is reported as exactly that, never rounded
up to `PASS` -- see `audit.rs`'s module doc for the per-metric status rules
this module trusts the native side to have already applied; this module only
maps that verdict onto a gate phase.

That mapping is deliberately lenient: the gate phase's `ok` is `False` only
when a metric is `REGRESSION`. `NO_BASELINE` and `NO_DATA` (including the
hard-empty exit-3 case) are `ok`, because a fresh clone or CI runner with no
recorded baseline and no ledger rows yet has nothing to regress against --
`make gate` must not go permanently red before a first baseline exists. The
non-PASS statuses still show up verbatim in the phase `detail` string so
they stay visible; only the direct CLI (`make cost-budget-audit`, `forge
ledger audit` run by a human) keeps failing loud on anything but `PASS`/
`RATCHET_HELD`, exit 3 included.

The design also names a `ledger/registry.d/<scope>.json` convention (mirroring
`campaigns/registry.d/` -- `mutation_registry_split.py`) for concurrent-safe
baseline pointers. That split is coupled to mutation-campaign manifests (a
`{"manifest": ...}` fragment plus a native registry loader that only knows
how to merge *that* shape); it does not generalize to an arbitrary metric
baseline, so this module uses one tracked baseline file instead, as the
design permits when the registry.d code path does not already generalize.
Every real command run against this repo produces at most one PR at a time
for this metric set, so one file is not yet a bottleneck; splitting it is a
follow-on bet if concurrent cost-ledger PRs ever collide on it.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path
from typing import Literal

from pydantic import BaseModel, ConfigDict, Field

from conductor.gate import PhaseResult
from conductor.project_init import resolve_forge_binary
from conductor.project_paths import host_root

DEFAULT_WINDOW_DAYS = 7
DEFAULT_TIMEOUT_SECONDS = 60
PHASE_NAME = "cost-budget-audit"

# `make cost-budget-audit` / `forge ledger audit` themselves (the human-facing
# CLI path, `main()` below) still fail loud on anything but these two -- a
# person asking for the audit explicitly gets `RATCHET_HELD` as not-quite-PASS
# but everything else as a real failure, unchanged from the original design.
OK_STATUSES = frozenset({"PASS", "RATCHET_HELD"})

MetricStatus = Literal["PASS", "RATCHET_HELD", "REGRESSION", "NO_BASELINE", "NO_DATA"]


class CostBudgetAuditError(RuntimeError):
    """The audit did not produce a verdict at all.

    Distinct from a failing metric: this means the tool could not run
    (no `forge` binary, no ledger data in the window at all, malformed
    output). `gate.py`'s wrapper turns this into a `GateRefusal`, matching
    `mutation_corpus_audit`'s own refusal on a missing registry.
    """


class Window(BaseModel):
    model_config = ConfigDict(extra="forbid", populate_by_name=True)

    from_: str = Field(alias="from")
    to: str
    days: int


class MetricResult(BaseModel):
    model_config = ConfigDict(extra="forbid")

    value: float | None
    n: int
    baseline: float | None
    delta_pct: float | None
    status: MetricStatus


class AuditResult(BaseModel):
    """`forge ledger audit`'s JSON, one-to-one with `audit.rs::AuditOutput`."""

    model_config = ConfigDict(extra="forbid", populate_by_name=True)

    window: Window
    metrics: dict[str, MetricResult]
    status: str


def default_baseline_path(repo_root: Path) -> Path:
    return repo_root / "ledger" / "cost_budget_baseline.json"


def run_forge_ledger_audit(
    *,
    forge_binary: Path,
    baseline: Path,
    ledger_root: Path | None = None,
    window_days: int = DEFAULT_WINDOW_DAYS,
    record: bool = False,
    timeout: int = DEFAULT_TIMEOUT_SECONDS,
) -> subprocess.CompletedProcess[str]:
    """Shell to `forge ledger audit`, returning the raw completed process.

    Never raises on a nonzero exit -- exit 3 (`NO_DATA`, the whole window
    empty) and exit 1 (a well-formed but not-`ok` verdict) are both
    meaningful outputs, not tool failures; only a `subprocess` error (e.g.
    the binary is not executable) escapes as an exception.
    """

    command = [
        str(forge_binary),
        "ledger",
        "audit",
        "--baseline",
        str(baseline),
        "--window-days",
        str(window_days),
    ]
    if ledger_root is not None:
        command += ["--ledger-root", str(ledger_root)]
    if record:
        command.append("--record")
    return subprocess.run(
        command, capture_output=True, text=True, timeout=timeout, check=False
    )


def phase(
    export_root: Path,
    *,
    ledger_root: Path | None = None,
    window_days: int = DEFAULT_WINDOW_DAYS,
) -> PhaseResult:
    """`cost-budget-audit`: ratchets hook latency, resend bytes, tokens/PR.

    Reads the baseline from ``export_root`` (the exported candidate tree),
    never the live working tree, matching `gate.py::run_gate`'s rule that
    policy and evidence come from the export so an uncommitted edit cannot
    change the verdict. The ledger data itself (`hook_rollup`/
    `session_rollup`/`agent_rollup` day files) is not part of the git tree --
    it is read live from ``ledger_root`` (default resolution: `LEDGER_ROOT`
    env var, else the native default in `forge ledger rollup`'s own help).

    `ok` is `False` only when at least one metric's status is `REGRESSION`.
    `PASS`, `RATCHET_HELD`, `NO_BASELINE` and `NO_DATA` -- including the
    hard-empty case where every table is empty (`forge ledger audit` exits
    3) -- are all `ok`: a fresh clone or CI runner with no ledger rows yet
    must not turn `make gate` permanently red with no baseline to improve
    against (there is nothing to regress against yet, so nothing here is a
    gate-blocking finding). `detail` still names every metric's status
    verbatim, never rounded up to `PASS`, so the non-PASS statuses stay
    visible in gate output even though they do not fail the phase. A human
    running `forge ledger audit` or `make cost-budget-audit` directly still
    gets a real failure (exit 3, or a nonzero exit on anything but
    `PASS`/`RATCHET_HELD`) -- only this gate phase's `ok` rule is lenient.

    Still refuses loudly (raises `CostBudgetAuditError`, which `gate.py`
    turns into a `GateRefusal`) when the `forge` binary itself is missing or
    its output cannot be parsed at all -- those are tool failures, not
    verdicts, same shape as `mutation_corpus_audit`'s refusal on a missing
    registry.
    """

    forge_binary = resolve_forge_binary(export_root)
    if forge_binary is None:
        raise CostBudgetAuditError(
            "no forge binary found (.tools/bin/forge or PATH) -- "
            "cannot compute cost-ledger metrics"
        )
    baseline = default_baseline_path(export_root)
    completed = run_forge_ledger_audit(
        forge_binary=forge_binary,
        baseline=baseline,
        ledger_root=ledger_root,
        window_days=window_days,
    )
    if completed.returncode == 3:
        message = completed.stderr.strip() or completed.stdout.strip()
        return PhaseResult(
            name=PHASE_NAME,
            ok=True,
            detail=f"NO_DATA: {message}",
            evidence={"status": "NO_DATA", "metrics": {}, "message": message},
        )
    try:
        payload = json.loads(completed.stdout)
    except json.JSONDecodeError as exc:
        raise CostBudgetAuditError(
            f"forge ledger audit produced no JSON (exit {completed.returncode}): "
            f"{completed.stderr.strip() or exc}"
        ) from exc
    result = AuditResult.model_validate(payload)

    ok = not any(metric.status == "REGRESSION" for metric in result.metrics.values())
    detail = "; ".join(
        f"{name}={metric.status}" for name, metric in sorted(result.metrics.items())
    )
    return PhaseResult(
        name=PHASE_NAME,
        ok=ok,
        detail=f"{result.status}: {detail}",
        evidence=result.model_dump(by_alias=True),
    )


def main(argv: list[str] | None = None) -> int:
    """`make cost-budget-audit` / `make cost-budget-record`'s entry point.

    Runs against the live repo (not an export -- there is no candidate
    review happening here, just a direct check or a deliberate re-record),
    mirroring `mutation_patch_audit.main`'s `--registry`/`--write-baseline`
    shape with `--baseline`/`--record`.
    """

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", type=Path, default=host_root())
    parser.add_argument("--ledger-root", type=Path, default=None)
    parser.add_argument("--baseline", type=Path, default=None)
    parser.add_argument("--window-days", type=int, default=DEFAULT_WINDOW_DAYS)
    parser.add_argument(
        "--record",
        action="store_true",
        help="record this window's metrics as the new baseline instead of checking it",
    )
    args = parser.parse_args(argv)

    forge_binary = resolve_forge_binary(args.repo_root)
    if forge_binary is None:
        print(
            "cost_budget_audit: no forge binary found (.tools/bin/forge or PATH)",
            file=sys.stderr,
        )
        return 2
    baseline = args.baseline or default_baseline_path(args.repo_root)

    completed = run_forge_ledger_audit(
        forge_binary=forge_binary,
        baseline=baseline,
        ledger_root=args.ledger_root,
        window_days=args.window_days,
        record=args.record,
    )
    sys.stdout.write(completed.stdout)
    if completed.stdout and not completed.stdout.endswith("\n"):
        sys.stdout.write("\n")
    sys.stderr.write(completed.stderr)

    if args.record:
        return completed.returncode
    if completed.returncode == 3:
        return 3
    try:
        payload = json.loads(completed.stdout)
    except json.JSONDecodeError:
        return completed.returncode or 1
    return 0 if payload.get("status") in OK_STATUSES else 1


if __name__ == "__main__":
    raise SystemExit(main())
