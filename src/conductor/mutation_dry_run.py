#!/usr/bin/env python3
"""Dry-run a mutation campaign's reviewed patches without writing a receipt.

Applies each materialized mutation of a READY campaign in a disposable snapshot
and runs the campaign's own test argv, reporting killed/survived per mutation.
Nothing is written under ``conductor/mutation_campaigns/receipts/``, so the
result can never be mistaken for evidence: it exists so an author can see whether
the reviewed first-order patch is caught before asking for an authorized
``make mutation-run``. Mutants are still executed, so the same
``--allow-mutations`` authority applies.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any, Sequence

import conductor.mutation_testing as mt


def dry_run(
    campaign: mt.Campaign,
    *,
    allow_mutations: bool,
    mutation_ids: Sequence[str] | None = None,
    repo_root: Path = mt.REPO_ROOT,
) -> dict[str, Any]:
    """Run baseline and every selected mutation in snapshots; report, no receipt."""
    inspection = mt.inspect_campaign(campaign, repo_root=repo_root)
    if inspection["status"] != "READY":
        raise mt.CampaignError(
            "campaign is NOT_READY: " + "; ".join(inspection["readiness_reasons"])
        )
    if not allow_mutations:
        raise mt.CampaignError(
            "refusing mutant execution without --allow-mutations "
            "(a dry-run still runs mutants)"
        )
    selected = mt._select_mutations(campaign, mutation_ids)  # noqa: SLF001

    with mt.isolated_snapshot(repo_root) as snapshot:
        if drift := mt.source_drift(campaign, snapshot.worktree):
            raise mt.CampaignError(f"snapshot source hashes drifted: {drift}")
        mt._link_mutation_patches(campaign, snapshot.worktree, repo_root)  # noqa: SLF001
        mt._link_host_dependencies(campaign, snapshot.worktree, repo_root)  # noqa: SLF001
        baseline = mt._run_command(  # noqa: SLF001
            campaign.test_argv,
            cwd=snapshot.worktree,
            timeout_seconds=campaign.timeout_seconds,
            environment=campaign.environment,
        )
    if baseline.timed_out or baseline.returncode != 0:
        raise mt.CampaignError(
            "unmutated baseline failed; fix the tests before previewing mutants"
        )

    mutants: list[dict[str, Any]] = []
    for mutation in selected:
        with mt.isolated_snapshot(repo_root) as snapshot:
            if drift := mt.source_drift(campaign, snapshot.worktree):
                raise mt.CampaignError(f"snapshot source hashes drifted: {drift}")
            mt._link_mutation_patches(campaign, snapshot.worktree, repo_root)  # noqa: SLF001
            mt._link_host_dependencies(campaign, snapshot.worktree, repo_root)  # noqa: SLF001
            mt._apply_mutation(mutation, snapshot.worktree)  # noqa: SLF001
            result = mt._run_command(  # noqa: SLF001
                campaign.test_argv,
                cwd=snapshot.worktree,
                timeout_seconds=campaign.timeout_seconds,
                environment=campaign.environment,
            )
        outcome = (
            "TIMED_OUT"
            if result.timed_out
            else "SURVIVED"
            if result.returncode == 0
            else "KILLED"
        )
        mutants.append(
            {
                "id": mutation.mutation_id,
                "outcome": outcome,
                "expected_killers": list(mutation.expected_killers),
                "returncode": result.returncode,
            }
        )

    return {
        "campaign_id": campaign.campaign_id,
        "dry_run": True,
        "receipt_written": False,
        "baseline_returncode": baseline.returncode,
        "mutants": mutants,
        "survivors": [row["id"] for row in mutants if row["outcome"] == "SURVIVED"],
        "timed_out": [row["id"] for row in mutants if row["outcome"] == "TIMED_OUT"],
    }


def format_report(report: dict[str, Any]) -> str:
    lines = [
        f"mutation-dry-run | campaign={report['campaign_id']} | NO RECEIPT WRITTEN",
    ]
    for row in report["mutants"]:
        lines.append(f"  [{row['outcome']}] {row['id']}")
    if report["survivors"] or report["timed_out"]:
        lines.append(
            "NOT READY for mutation-run: survivors="
            f"{report['survivors']} timed_out={report['timed_out']}"
        )
    else:
        lines.append("every mutant killed; request an authorized `make mutation-run`")
    return "\n".join(lines) + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path, help="campaign manifest path")
    parser.add_argument("--allow-mutations", action="store_true")
    parser.add_argument(
        "--mutation",
        action="append",
        dest="mutation_ids",
        help="limit to one mutation id",
    )
    parser.add_argument("--json", action="store_true", help="machine-readable report")
    parser.add_argument("--repo", type=Path, default=mt.REPO_ROOT)
    args = parser.parse_args(argv)
    repo_root = args.repo.resolve()
    try:
        campaign = mt.load_campaign(args.manifest, repo_root=repo_root)
        report = dry_run(
            campaign,
            allow_mutations=args.allow_mutations,
            mutation_ids=args.mutation_ids,
            repo_root=repo_root,
        )
    except mt.CampaignError as exc:
        print(f"mutation-dry-run FAILED: {exc}", file=sys.stderr)
        return 2
    if args.json:
        print(json.dumps(report, indent=2))
    else:
        sys.stdout.write(format_report(report))
    return 0 if not report["survivors"] and not report["timed_out"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
