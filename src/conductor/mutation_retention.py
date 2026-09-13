"""Retention for published mutation receipts.

`mutation_evidence.rs` indexes every receipt a campaign has ever published, keeps
the ones that validate against the current runner and anchor, sorts them by
`(generated_at, name)` and cites exactly one: the last. Every other receipt on
disk is loaded, validated and discarded on every gate run. Nothing reads it, and
nothing ever will, because the gate's own ordering can never reach it.

`_campaign_receipts` documents that as a lookup rule -- "is ANY receipt good" --
and it was read as a retention rule, so the directory only ever grew: 1318
receipts, 104.8 MB, of which the gate can cite at most one per campaign.

This module deletes what the gate cannot reach, and it asks the gate rather than
predicting it. "Newest PASS per campaign" is the obvious rule and it is wrong: a
receipt is citable only if `receipt_errors` also clears it against the current
runner and anchor, so a campaign whose newest PASS has gone stale is still cited
through an older one. Sweeping on the obvious rule cost 11 test files their
evidence on this corpus. So the keep-set starts as the set of receipts the
coverage gate actually cites right now, and the newest PASS per campaign that
still has a manifest is added on top of it -- never in place of it -- to cover
campaigns whose tests the inventory does not currently reach, and to survive a
sweep run while the tree is mid-edit.

The coverage gate is not the only consumer. `mutation_patch_audit` asks a
different question of the same directory -- "does ANY receipt still validate
against the current runner" -- and answers it per campaign rather than per test
file, so it reaches campaigns the coverage inventory never mentions. Sweeping on
the coverage gate alone cost 33 campaigns their audit evidence. Both consumers
are asked, and a receipt either one reaches is kept.
"""

from __future__ import annotations

import argparse
import json
import sys
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from conductor.project_paths import (
    DEFAULT_MUTATION_REGISTRY,
    campaigns_relative,
    receipts_relative,
)

REPO_ROOT = Path(__file__).resolve().parent.parent

# The default layout, for callers that name a directory without holding a root; every
# function below resolves against the root it is given instead.
CAMPAIGN_DIRECTORY = str(DEFAULT_MUTATION_REGISTRY.parent)
RECEIPT_DIRECTORY = f"{CAMPAIGN_DIRECTORY}/receipts"



class RetentionError(Exception):
    """The corpus cannot be read well enough to decide what to delete."""


@dataclass(frozen=True)
class Receipt:
    """One published receipt, reduced to the fields retention reasons about."""

    path: Path
    campaign_id: str
    status: str
    generated_at: str
    size: int
    payload: Mapping[str, Any] = field(default_factory=dict)

    @property
    def order(self) -> tuple[str, str]:
        """The gate's own tie-break: `(generated_at, name)`, ascending."""

        return (self.generated_at, self.path.name)


@dataclass
class Plan:
    """What a sweep would keep, what it would delete, and why."""

    keep: dict[Path, str] = field(default_factory=dict)
    delete: dict[Path, str] = field(default_factory=dict)
    unreadable: list[Path] = field(default_factory=list)

    @property
    def freed_bytes(self) -> int:
        return sum(path.stat().st_size for path in self.delete if path.exists())


def _manifests_by_campaign_id(repo_root: Path) -> dict[str, Path]:
    """Campaign id -> manifest path, for every campaign with a manifest on disk.

    `registry.json` is deliberately not consulted. The evidence gate never reads
    it -- it resolves a campaign through its manifest -- and the two disagree
    badly: on 2026-09-07 the tree carried 488 manifests against 457 registry
    entries, and the gate cited receipts for 29 of the 31 campaigns the registry
    omits. Sweeping on registry membership therefore deleted live evidence.

    Two manifests may declare the same id; the later filename wins, and the
    duplicate is a lane problem the walk does not paper over either way.

    A manifest that cannot be read refuses the whole sweep: a campaign that
    merely looks absent is exactly the campaign whose receipts must not go.
    """

    relative = campaigns_relative(repo_root)
    directory = repo_root / relative.as_posix()
    if not directory.is_dir():
        raise RetentionError(f"no campaign directory at {relative}")

    manifests: dict[str, Path] = {}
    for path in sorted(directory.glob("*.json")):
        if path.name == "registry.json":
            continue
        try:
            payload = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, UnicodeError, json.JSONDecodeError) as exc:
            raise RetentionError(f"manifest {path.name} is unreadable: {exc}") from exc
        campaign_id = (
            payload.get("campaign_id") if isinstance(payload, Mapping) else None
        )
        if isinstance(campaign_id, str):
            manifests[campaign_id] = path
    return manifests


def _load_receipts(directory: Path) -> tuple[list[Receipt], list[Path]]:
    """Every receipt in the directory, plus the ones that would not parse.

    An unreadable receipt is never swept. The gate reports it as malformed and a
    human decides; deleting it here would erase the only evidence of the problem.
    """

    receipts: list[Receipt] = []
    unreadable: list[Path] = []
    for path in sorted(directory.glob("*.json")):
        try:
            payload = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, UnicodeError, json.JSONDecodeError):
            unreadable.append(path)
            continue
        campaign_id = (
            payload.get("campaign_id") if isinstance(payload, Mapping) else None
        )
        if not isinstance(campaign_id, str):
            unreadable.append(path)
            continue
        receipts.append(
            Receipt(
                path=path,
                campaign_id=campaign_id,
                status=str(payload.get("status")),
                generated_at=str(payload.get("generated_at") or ""),
                size=path.stat().st_size,
                payload=payload,
            )
        )
    return receipts, unreadable


def cited_receipts(repo_root: Path = REPO_ROOT) -> set[Path]:
    """The receipts the coverage gate cites for the tree as it stands.

    This is the authority, not an estimate: `verify_evidence` applies the same
    `receipt_errors` validation and the same `(generated_at, name)` ordering the
    gate uses, so a receipt named here is by construction one the gate can reach.
    A gate that cannot run at all refuses the sweep -- retention must never be the
    step that decides evidence is expendable because it could not check.
    """

    from conductor.mutation_coverage import coverage_report

    try:
        report = coverage_report(repo_root=repo_root)
    except Exception as exc:  # the gate is the safety property; no gate, no sweep
        raise RetentionError(f"coverage gate did not run: {exc}") from exc

    cited: set[Path] = set()
    for row in report.get("evidence") or []:
        relative = row.get("receipt")
        if not isinstance(relative, str):
            raise RetentionError(f"evidence row without a receipt path: {row!r}")
        cited.add((repo_root / relative).resolve())
    return cited


def audited_receipts(receipts: Sequence[Receipt], repo_root: Path) -> set[Path]:
    """The receipts `mutation_patch_audit` reads as a campaign's evidence.

    A second authority, and a genuinely different one: the audit accepts a
    campaign if ANY of its receipts still validates against the current runner
    map, and it reports on campaigns, not on tracked test files, so it vouches
    for campaigns the coverage inventory never reaches. The audit's own predicate
    is used rather than a restatement of it -- a copy would drift, and the whole
    point is to keep what that consumer reads. It is reached through the public
    `ReceiptJudge` seam, never by assembling the predicate's arguments here --
    assembling them is how this module crashed for a week after the predicate
    grew `tree` and `campaign`.

    Each receipt's `campaign_id` is resolved to its `Campaign` by loading the
    manifest the same way the evidence gate does (`load_campaign`), never
    through `registry.json` (see `_manifests_by_campaign_id`). A receipt whose
    campaign has no manifest is not audited -- it is already not live, and the
    plan's own rule condemns it. A manifest that exists but will not load
    refuses the sweep, exactly as an unreadable one does.

    Its selection is reproduced exactly, including the tie-break: `max` over a
    filename-sorted list returns the *first* row holding the highest
    `generated_at`, so among receipts generated in the same instant the audit
    reads the one whose filename sorts first, and that is the one kept.
    """

    from conductor.mutation_campaign_model import Campaign, load_campaign
    from conductor.mutation_patch_audit import ReceiptJudge

    try:
        judge = ReceiptJudge(repo_root)
    except Exception as exc:  # same rule as the gate: no authority, no sweep
        raise RetentionError(f"corpus audit did not run: {exc}") from exc

    manifests = _manifests_by_campaign_id(repo_root)
    campaigns: dict[str, Campaign] = {}
    best: dict[str, Receipt] = {}
    for receipt in sorted(receipts, key=lambda item: item.path.name):
        manifest = manifests.get(receipt.campaign_id)
        if manifest is None:
            continue
        if receipt.campaign_id not in campaigns:
            try:
                campaigns[receipt.campaign_id] = load_campaign(
                    manifest, repo_root=repo_root
                )
            except Exception as exc:
                raise RetentionError(
                    f"manifest {manifest.name} cannot be loaded: {exc}"
                ) from exc
        if judge.rejection(receipt.payload, campaigns[receipt.campaign_id]) is not None:
            continue
        held = best.get(receipt.campaign_id)
        if held is None or receipt.generated_at > held.generated_at:
            best[receipt.campaign_id] = receipt
    return {receipt.path.resolve() for receipt in best.values()}


def plan(repo_root: Path = REPO_ROOT, *, protect: Sequence[str] = ()) -> Plan:
    """Decide the fate of every receipt without touching one.

    `protect` names receipt filenames that survive whatever the rule says, for the
    case where a lane has a run in flight the tree does not yet describe.
    """

    live = set(_manifests_by_campaign_id(repo_root))

    relative = receipts_relative(repo_root)
    directory = repo_root / relative.as_posix()
    if not directory.is_dir():
        raise RetentionError(f"no receipt directory at {relative}")

    receipts, unreadable = _load_receipts(directory)
    cited = cited_receipts(repo_root) | audited_receipts(receipts, repo_root)
    protected = set(protect)

    by_campaign: dict[str, list[Receipt]] = {}
    for receipt in receipts:
        by_campaign.setdefault(receipt.campaign_id, []).append(receipt)

    result = Plan(unreadable=unreadable)
    for path in unreadable:
        result.keep[path] = "unparseable: the gate reports it, a human decides"

    for campaign_id, group in by_campaign.items():
        if campaign_id not in live:
            for receipt in group:
                result.delete[receipt.path] = (
                    f"campaign {campaign_id} has no manifest on disk"
                )
            continue
        passing = sorted(
            (receipt for receipt in group if receipt.status == "PASS"),
            key=lambda receipt: receipt.order,
        )
        if not passing:
            # No PASS anywhere: the campaign's evidence is already broken, and
            # which receipt explains that is not this module's call.
            for receipt in group:
                result.keep[receipt.path] = (
                    f"campaign {campaign_id} has no PASS receipt to supersede"
                )
            continue
        newest = passing[-1]
        result.keep[newest.path] = f"newest PASS for {campaign_id}"
        for receipt in group:
            if receipt.path != newest.path:
                result.delete[receipt.path] = (
                    f"superseded by {newest.path.name}; the gate's ordering "
                    "cannot reach it"
                )

    # The consumers' own citations override every deletion above. A receipt either
    # one reaches is evidence in use, whatever the newest PASS happens to be.
    for path in list(result.delete):
        if path.resolve() in cited:
            result.keep[path] = "read by the coverage gate or the corpus audit"
            del result.delete[path]

    for path in list(result.delete):
        if path.name in protected:
            result.keep[path] = "explicitly protected"
            del result.delete[path]
    return result


def apply(plan_: Plan) -> int:
    """Delete what the plan condemned. Returns the number of files removed."""

    removed = 0
    for path in plan_.delete:
        path.unlink(missing_ok=True)
        removed += 1
    return removed


def _report(plan_: Plan) -> dict[str, Any]:
    return {
        "schema_version": "llm.mutation-testing.receipt-retention.v1",
        "kept": len(plan_.keep),
        "deleted": len(plan_.delete),
        "protected": sum(
            1 for reason in plan_.keep.values() if reason == "explicitly protected"
        ),
        "unreadable": [str(path) for path in plan_.unreadable],
        "freed_bytes": plan_.freed_bytes,
    }


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--apply",
        action="store_true",
        help="delete the superseded receipts (default: report only)",
    )
    parser.add_argument(
        "--protect",
        action="append",
        default=[],
        metavar="FILENAME",
        help="a receipt filename that survives regardless of the rule",
    )
    parser.add_argument("--repo-root", type=Path, default=REPO_ROOT)
    args = parser.parse_args(argv)

    try:
        plan_ = plan(args.repo_root, protect=args.protect)
    except RetentionError as exc:
        # 0 = the sweep ran (its plan is the report, deletions and all); 2 = it
        # could not decide (unreadable manifest, a gate that would not run), so
        # nothing was touched. The split is what lets CI run the report as a
        # step that fails on a crash and still tolerates an accumulating
        # uncitable list. Any other exit is an unhandled defect.
        print(f"mutation-retention: {exc}", file=sys.stderr)
        return 2
    report = _report(plan_)
    if args.apply:
        report["removed"] = apply(plan_)
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
