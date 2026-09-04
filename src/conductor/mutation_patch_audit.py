"""Ask whether the registered mutant corpus can still be applied to this tree.

``repin`` re-pins source digests and, with ``--run``, regenerates receipts. What
neither it nor the evidence gate ever asks is whether a campaign's *patches* still
land. A mutant patch is anchored to three lines of context; any edit that moves
that anchor rots it silently. The campaign keeps its PASS receipt, keeps matching
candidate paths, and keeps unblocking the tests it claims to protect -- while not
one of its mutants can be applied. The receipt describes a tree that no longer
exists and cannot be reproduced.

Nothing detects this today because the rot is only visible at run time, and a
campaign whose sources never drifted is never re-run. This audit applies every
registered patch against the working tree and reports the ones that fail, so
corpus rot is a number instead of a surprise mid-campaign REFUSAL.

The work is one ``git apply --check`` process per mutant -- fork/exec bound, not
compute -- so it is spread across a thread pool rather than ported native: the
interpreter is idle in ``waitpid`` either way.
"""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
import subprocess
from typing import Any

from conductor.mutation_scope import CampaignError, _safe_relative_path
from conductor.mutation_testing import (
    REPO_ROOT,
    Campaign,
    _load_registry,
    _sha256,
    load_campaign,
)


def _patch_verdict(
    campaign: Campaign, mutation: Any, repo_root: Path
) -> dict[str, str] | None:
    """Why this mutant cannot be applied, or ``None`` when it applies cleanly."""

    patch = mutation.patch_file
    row = {"campaign_id": campaign.campaign_id, "mutation_id": mutation.mutation_id}
    if not patch.is_file():
        return row | {"reason": "MISSING", "detail": f"no patch file at {patch}"}
    actual = _sha256(patch)
    if actual != mutation.patch_sha256:
        return row | {
            "reason": "HASH_DRIFT",
            "detail": f"expected {mutation.patch_sha256}, got {actual}",
        }
    proc = subprocess.run(
        ["git", "apply", "--check", str(patch)],
        cwd=repo_root,
        check=False,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        return row | {
            "reason": "DOES_NOT_APPLY",
            "detail": (proc.stderr or proc.stdout).strip()[:400],
        }
    return None


def audit_patches(
    registry_path: Path,
    *,
    repo_root: Path = REPO_ROOT,
    max_workers: int = 16,
) -> dict[str, Any]:
    """Report every registered mutant that can no longer be applied.

    Unloadable manifests are reported rather than raised: one lane's half-written
    campaign must not hide the corpus health of every other lane.
    """

    payload = _load_registry(registry_path, repo_root)
    campaigns: list[Campaign] = []
    unloadable: list[dict[str, str]] = []
    for index, row in enumerate(payload["campaigns"]):
        manifest = repo_root / _safe_relative_path(
            row["manifest"], f"registry.campaigns[{index}].manifest"
        )
        try:
            campaigns.append(load_campaign(manifest, repo_root=repo_root))
        except CampaignError as exc:
            unloadable.append(
                {
                    "manifest": manifest.relative_to(repo_root).as_posix(),
                    "detail": str(exc)[:400],
                }
            )

    jobs = [
        (campaign, mutation)
        for campaign in campaigns
        for mutation in campaign.mutations
    ]
    with ThreadPoolExecutor(max_workers=max_workers) as pool:
        verdicts = list(
            pool.map(
                lambda job: _patch_verdict(job[0], job[1], repo_root),
                jobs,
            )
        )
    stale = [verdict for verdict in verdicts if verdict is not None]
    by_campaign: dict[str, int] = {}
    for verdict in stale:
        campaign_id = verdict["campaign_id"]
        by_campaign[campaign_id] = by_campaign.get(campaign_id, 0) + 1
    return {
        "status": "STALE" if stale or unloadable else "CLEAN",
        "repo_root": str(repo_root),
        "campaigns": len(campaigns),
        "mutations": len(jobs),
        "stale_mutations": len(stale),
        "stale_campaigns": dict(sorted(by_campaign.items())),
        "stale": sorted(
            stale, key=lambda row: (row["campaign_id"], row["mutation_id"])
        ),
        "unloadable": sorted(unloadable, key=lambda row: row["manifest"]),
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--registry",
        type=Path,
        default=Path("conductor/mutation_campaigns/registry.json"),
    )
    parser.add_argument(
        "--max-workers",
        type=int,
        default=16,
        help="concurrent `git apply --check` processes",
    )
    parser.add_argument(
        "--summary",
        action="store_true",
        help="omit the per-mutant rows and report only the counts",
    )
    args = parser.parse_args(argv)
    result = audit_patches(args.registry, max_workers=args.max_workers)
    if args.summary:
        result = {key: value for key, value in result.items() if key != "stale"}
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0 if result["status"] == "CLEAN" else 6


if __name__ == "__main__":
    raise SystemExit(main())
