#!/usr/bin/env python3
"""One-shot migration: the registry array becomes one fragment per campaign.

`registry.json`'s shared `campaigns` array made every registration in the
fleet a write to one file: two concurrent PRs each appending a row conflicted
every time (PRs #13, #16, #20 and #22 each needed a merge-in for that alone).
A row in `registry.d/<campaign-id>.json` is a distinct path, so lanes register
in parallel without touching each other's files.

The reader has accepted fragments alongside the array since the native
registry loader learned `registry.d/`; this module is the migration that
empties the array. `registry.json` itself stays behind as the read-only
envelope the loader still requires -- schema, enforcement and the canonical
test patterns -- and nothing appends to it again; a later PR relocates that
envelope and removes the file.
"""

from __future__ import annotations

import json
from pathlib import Path, PurePosixPath

from conductor.mutation_campaign_model import REPO_ROOT
from conductor.mutation_scope import CampaignError
from conductor.project_paths import registry_path


def split_registry_array(repo_root: Path = REPO_ROOT) -> list[str]:
    """Move every array row into `registry.d/`, leaving the envelope behind.

    Idempotent: a registry whose array is already empty changes nothing, and a
    fragment identical to its row is left as it is. A fragment naming a
    different manifest, or two rows sharing one campaign id, refuses loudly
    before anything is written -- half a split registry is worse than none.
    """

    path = registry_path(repo_root)
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise CampaignError(f"cannot read registry {path}: {exc}") from exc
    if not isinstance(payload, dict):
        raise CampaignError(f"{path} is not a registry object")
    rows = payload.get("campaigns")
    if not isinstance(rows, list):
        raise CampaignError(f"{path} has no campaigns array to split")
    pairs: list[tuple[str, str]] = []
    seen: set[str] = set()
    for index, row in enumerate(rows):
        if not isinstance(row, dict) or not isinstance(row.get("manifest"), str):
            raise CampaignError(f"{path} campaigns[{index}] has no manifest string")
        campaign_id = PurePosixPath(row["manifest"]).stem
        if not campaign_id:
            raise CampaignError(
                f"{path} campaigns[{index}] manifest has no campaign id to name a fragment"
            )
        if campaign_id in seen:
            raise CampaignError(
                f"two array rows share the campaign id {campaign_id!r}"
            )
        seen.add(campaign_id)
        pairs.append((campaign_id, row["manifest"]))
    if not pairs:
        return []  # already split: the array is empty and the fragments stand

    fragments = path.parent / "registry.d"
    pending: list[tuple[Path, str]] = []
    for campaign_id, manifest in pairs:
        fragment = fragments / f"{campaign_id}.json"
        body = json.dumps({"manifest": manifest}, indent=2) + "\n"
        if fragment.exists():
            if fragment.read_text(encoding="utf-8") != body:
                raise CampaignError(
                    f"{fragment.relative_to(repo_root)} already registers a "
                    "different manifest"
                )
            continue
        pending.append((fragment, body))
    fragments.mkdir(parents=True, exist_ok=True)
    for fragment, body in pending:
        fragment.write_text(body, encoding="utf-8")
    payload["campaigns"] = []
    path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    return [fragment.relative_to(repo_root).as_posix() for fragment, _ in pending]


def main(argv: list[str] | None = None) -> int:
    """CLI: split this tree's registry array into one fragment per campaign."""

    _ = argv  # no flags: the tree to split is the one this module is run in
    try:
        written = split_registry_array()
    except CampaignError as exc:
        print(json.dumps({"status": "REFUSED", "error": str(exc)}, indent=2))
        return 4
    print(
        json.dumps(
            {"status": "SPLIT", "fragments": len(written), "written": written},
            indent=2,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
