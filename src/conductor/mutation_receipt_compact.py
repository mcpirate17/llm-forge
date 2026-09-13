"""Compact a receipt directory: slim kept receipts, supersede the rest.

``conductor mutation_receipt_compact campaigns/receipts`` runs the native
one-write-pass compactor: every receipt keeps its summary block byte-identical,
one receipt per campaign keeps its detail (slim -- inline or one zstd+base64
blob), and every other receipt of that campaign has its detail replaced by a
``{"superseded_by": "<kept file>"}`` pointer.

Which receipt keeps its detail is the audit's decision, not the clock's:
``mutation_patch_audit._acceptable_receipt`` can reject a NEWER receipt (its
runner components may match neither this runner nor any lineage entry -- a
parallel slice's clone) while accepting an older sibling, so this CLI runs
that same predicate first and hands the per-campaign keep-set to the native
pass. The patch audit afterwards reads, for every campaign, exactly the
receipt whose detail was kept.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

from conductor.mutation_receipt_slim import ReceiptDetailError, compact_directory


def audit_keep_set(root: Path) -> dict[str, str]:
    """Filename of the receipt the patch audit would read, per campaign.

    Delegates to ``mutation_patch_audit``'s own rejection predicate (and its
    newest-acceptable rule: greatest ``generated_at``, first sorted filename
    on a tie), so the compactor and the audit cannot drift into two different
    definitions of "the receipt that counts". Campaigns whose manifests fail
    to load are simply absent: the native pass falls back to its
    (passing-status, newest-stamp) heuristic for them.
    """

    from conductor.mutation_patch_audit import (
        REPO_ROOT,
        _TreeHasher,
        _receipt_rejection,
        load_registered_campaigns,
    )
    from conductor.mutation_receipt_slim import receipt_files
    from conductor.mutation_testing import (
        _runner_components_sha256,
        runner_component_root,
    )

    # REPO_ROOT (derived from this package's location), not host_root(): the
    # receipts being compacted belong to the tree this code runs from.
    repo_root = REPO_ROOT
    # Indexed with filenames because the audit's index drops them, and a
    # receipt's own `receipt_path` field is engine-era, not guaranteed.
    receipts: dict[str, list[tuple[str, dict]]] = {}
    for path, payload in receipt_files(root):
        campaign_id = payload.get("campaign_id")
        if isinstance(campaign_id, str):
            receipts.setdefault(campaign_id, []).append((path.name, payload))
    registry_path = repo_root / "campaigns/registry.json"
    _payload, campaigns, _unloadable = load_registered_campaigns(
        registry_path, repo_root=repo_root
    )
    current = _runner_components_sha256()
    package_root = runner_component_root()
    tree = _TreeHasher(repo_root)
    keep: dict[str, str] = {}
    for campaign in campaigns:
        accepted = [
            (name, payload)
            for name, payload in receipts.get(campaign.campaign_id, [])
            if _receipt_rejection(payload, current, package_root, tree, campaign) is None
        ]
        if not accepted:
            continue
        newest = max(accepted, key=lambda pair: str(pair[1].get("generated_at") or ""))
        keep[campaign.campaign_id] = newest[0]
    return keep


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path, help="receipt directory to compact")
    parser.add_argument(
        "--no-audit-keep-set",
        action="store_true",
        help="skip the patch-audit predicate and keep the newest passing-status "
        "receipt per campaign (right only when no receipt was lineage-rejected)",
    )
    args = parser.parse_args(argv)
    keep = None if args.no_audit_keep_set else audit_keep_set(args.directory)
    try:
        stats = compact_directory(args.directory, keep)
    except ReceiptDetailError as exc:
        print(json.dumps({"status": "REFUSED", "error": str(exc)}))
        return 4
    stats["status"] = "COMPACTED"
    stats["audit_keep_campaigns"] = 0 if keep is None else len(keep)
    print(json.dumps(stats, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
