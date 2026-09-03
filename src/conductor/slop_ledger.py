"""The slop backlog: what the sweep found, what is new, and what got fixed.

A sweep emits one finding per ablation, which is the right unit for the probe and the
wrong one for a person. The first real sweep (231 modules, 2026-08-31) produced 2,776
findings over 931 distinct functions, and 86% of them were in ``research/tools/`` --
one-off experiment scripts, where "no test names this function" is the expected state.
Reported flat, the 61 untested functions in shipped code sat underneath 408 nobody
should act on.

The aggregation, tiering and diff are native (``slop_core::ledger``); this module owns
the policy -- which prefixes ship -- and the two artifacts:

* ``conductor/slop_ledger.json``  the tool's own record, one entry per work item
* ``research/reports/slop_backlog.md``  the rendered report

Each run reports what changed rather than restating the backlog, so the number that
moves is the number worth watching.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import json
import pathlib
from collections.abc import Sequence
from typing import Any

from conductor._native import slop_core

# Decided once when conductor._native loaded: the extension, or SlopCoreUnavailable
# (an ImportError naming the build step). No per-call fallback.
_core = slop_core()

LEDGER = pathlib.Path("conductor/slop_ledger.json")
REPORT = pathlib.Path("research/reports/slop_backlog.md")
# Where `make gate`'s equivalence-probe check leaves the summary it already computed.
# The check must not write the ledger itself: it runs against a candidate snapshot, it
# runs concurrently with other reviews, and a review that mutates a TRACKED file
# dirties the tree it is reviewing. So it drops a run artifact here -- gitignored,
# auto-pruned with the rest of research/reports -- and the ledger folds them in.
GATE_FINDINGS = pathlib.Path("research/reports/gate_findings")

# Paths whose code ships, and whose findings are therefore worth acting on.
#
# `research/tools/` is deliberately absent: those are one-off experiment scripts, and a
# helper in a factorial-sweep script that no test names is not a defect. Excluding them
# is what makes the report readable -- they were 86% of the first sweep.
SHIPPED_PREFIXES = (
    "conductor/",
    "aria_core/",
    "aria_designer/api/",
    "aria_designer/runtime/",
    "aria_designer/components/",
    "component_fab/",
    "research/scientist/",
    "research/synthesis/",
)

VERDICT_MEANING = {
    "REACHABLE_BUT_UNTESTED": "changes behaviour in a regime no test drives",
    "NOT_EXERCISED": "no test file anywhere names this function",
    "NOT_REACHED_BY_DRIVERS": "named by a test the driver selection did not pick",
    "NO_DIFFERENCE_OBSERVED": "removing it changed nothing this probe could observe",
    "WITHIN_NUMERIC_NOISE": "removing it moved only the last bits",
    "NONDETERMINISTIC": "the function disagrees with itself, so nothing is attributable",
}


def merge_summaries(summaries: Sequence[dict[str, Any]]) -> dict[str, Any]:
    """One summary out of many, so a sweep and every gate run fold in together.

    The gate probes the two or three modules a change touched; a sweep probes
    hundreds. Both produce the same shape, and the ledger wants their union -- that is
    what keeps the backlog current between sweeps instead of only at one.
    """
    merged: dict[str, Any] = {
        "blocking": [],
        "advisory": [],
        "untested": [],
        "modules_without_drivers": [],
        "modules_probed": 0,
    }
    without: set[str] = set()
    for summary in summaries:
        for bucket in ("blocking", "advisory", "untested"):
            merged[bucket].extend(summary.get(bucket, []))
        without.update(summary.get("modules_without_drivers", []))
        merged["modules_probed"] += summary.get("modules_probed", 0)
    merged["modules_without_drivers"] = sorted(without)
    return merged


def findings_from_summary(summary: dict[str, Any]) -> list[dict[str, Any]]:
    """Every finding a sweep produced, across all three buckets."""
    return [
        f
        for bucket in ("blocking", "advisory", "untested")
        for f in summary.get(bucket, [])
    ]


def aggregate(
    findings: Sequence[dict[str, Any]], shipped: Sequence[str] = SHIPPED_PREFIXES
) -> list[dict[str, Any]]:
    """Collapse findings to one ranked item per function and verdict."""
    return list(_core.aggregate_findings(list(findings), list(shipped)))


def load(path: pathlib.Path = LEDGER) -> dict[str, Any]:
    if not path.is_file() or not path.stat().st_size:
        return {"schema_version": 1, "generated_at": None, "scope": [], "items": []}
    return json.loads(path.read_text())


def diff(
    previous: dict[str, Any], items: Sequence[dict[str, Any]], scope: Sequence[str]
) -> tuple[list, list, list]:
    """`(new, carried, fixed)` -- with `fixed` restricted to modules this sweep covered.

    Without that restriction a narrower sweep reports every module it skipped as fixed,
    and the burndown becomes a function of what you happened to scan.
    """
    in_scope = set(scope)
    known = [i["id"] for i in previous.get("items", []) if i.get("module") in in_scope]
    new, carried, fixed_ids = _core.diff_against(known, list(items))
    by_id = {i["id"]: i for i in previous.get("items", [])}
    fixed = [by_id[i] for i in fixed_ids if i in by_id]
    return list(new), list(carried), fixed


def save(
    items: Sequence[dict[str, Any]],
    scope: Sequence[str],
    path: pathlib.Path = LEDGER,
    previous: dict[str, Any] | None = None,
) -> None:
    """Write the ledger, keeping items from modules this sweep did not cover.

    A sweep of one directory must not erase the backlog for everywhere else.
    """
    previous = previous if previous is not None else load(path)
    in_scope = set(scope)
    kept = [i for i in previous.get("items", []) if i.get("module") not in in_scope]
    merged = sorted(kept + list(items), key=lambda i: (i["module"], i["qualname"]))
    payload = {
        "schema_version": 1,
        "generated_at": _dt.datetime.now(_dt.UTC).isoformat(),
        # Every module the ledger speaks for, not just the ones this sweep touched.
        # Recording the sweep's scope here let a two-module run overwrite the record
        # of a 164-module one, so the file claimed to cover 1 module while holding
        # 1,543 items from 164. `diff` needs the narrower fact, so it is kept too.
        "scope": sorted({i["module"] for i in merged}),
        "last_sweep_scope": sorted(in_scope),
        "items": merged,
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2) + "\n")


def _row(item: dict[str, Any]) -> str:
    why = VERDICT_MEANING.get(item["verdict"], item["verdict"])
    rules = ", ".join(item["rules"][:3])
    more = f" +{len(item['rules']) - 3}" if len(item["rules"]) > 3 else ""
    return (
        f"- [ ] `{item['module']}:{item['line']}` **{item['qualname']}** — {why}  \n"
        f"      `{item['id']}` · {rules}{more}"
    )


def render(
    new: Sequence[dict],
    carried: Sequence[dict],
    fixed: Sequence[dict],
    scope: Sequence[str],
    limit: int = 40,
) -> str:
    """The human-facing report. Shipped code first; exploratory kept as a count."""
    now = _dt.datetime.now(_dt.UTC).strftime("%Y-%m-%d %H:%M UTC")
    ship = [i for i in new if i["tier"] == "shipped"]
    explore = [i for i in new if i["tier"] != "shipped"]
    carried_ship = [i for i in carried if i["tier"] == "shipped"]

    out = [
        "# Slop backlog",
        "",
        f"Generated {now} over {len(scope)} module(s).",
        "",
        "Items are functions, not findings: a sweep emits one finding per ablation, and",
        "the same function usually appears under several rules. `shipped` is the tier",
        "worth acting on; `exploratory` is one-off scripts, counted but not listed.",
        "",
        "| | new | carried | fixed |",
        "|---|---|---|---|",
        f"| shipped | {len(ship)} | {len(carried_ship)} | "
        f"{len([i for i in fixed if i.get('tier') == 'shipped'])} |",
        f"| exploratory | {len(explore)} | {len(carried) - len(carried_ship)} | "
        f"{len([i for i in fixed if i.get('tier') != 'shipped'])} |",
        "",
    ]
    if ship:
        out += [f"## New in shipped code ({len(ship)})", ""]
        out += [_row(i) for i in ship[:limit]]
        if len(ship) > limit:
            out.append(f"\n_...and {len(ship) - limit} more._")
        out.append("")
    if carried_ship:
        out += [f"## Carried in shipped code ({len(carried_ship)})", ""]
        out += [_row(i) for i in carried_ship[:limit]]
        if len(carried_ship) > limit:
            out.append(f"\n_...and {len(carried_ship) - limit} more._")
        out.append("")
    if fixed:
        out += [f"## Fixed since the last sweep ({len(fixed)})", ""]
        out += [
            f"- `{i['module']}` **{i['qualname']}** — {i['verdict']}"
            for i in fixed[:limit]
        ]
        out.append("")
    if explore:
        out += [
            f"## Exploratory ({len(explore)} new)",
            "",
            "One-off scripts. Listed as a count deliberately: a helper in a factorial",
            "sweep that no test names is the expected state, not a defect.",
            "",
        ]
    if not new and not fixed:
        out += ["Nothing new and nothing fixed since the last sweep.", ""]
    return "\n".join(out) + "\n"


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="conductor.slop_ledger")
    parser.add_argument(
        "findings",
        type=pathlib.Path,
        nargs="+",
        help="one or more slop_gate --json summaries; a sweep and any gate run "
        "artifacts from research/reports/gate_findings are folded together",
    )
    parser.add_argument("--ledger", type=pathlib.Path, default=LEDGER)
    parser.add_argument("--report", type=pathlib.Path, default=REPORT)
    parser.add_argument(
        "--dry-run", action="store_true", help="render to stdout and write nothing"
    )
    args = parser.parse_args(argv)

    summary = merge_summaries([json.loads(f.read_text()) for f in args.findings])
    findings = findings_from_summary(summary)
    items = aggregate(findings)
    scope = sorted(
        {f["module"] for f in findings}
        | set(summary.get("modules_without_drivers", []))
    )
    previous = load(args.ledger)
    new, carried, fixed = diff(previous, items, scope)
    report = render(new, carried, fixed, scope)

    if args.dry_run:
        print(report, end="")
        return 0
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(report)
    save(items, scope, args.ledger, previous)
    print(
        f"{len(findings)} findings -> {len(items)} items "
        f"({len(new)} new, {len(carried)} carried, {len(fixed)} fixed)"
    )
    print(f"  {args.report}")
    print(f"  {args.ledger}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
