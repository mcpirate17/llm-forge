"""Manifest paths a campaign registry declares, in the native loader's order.

The native loader (`mutation_manifest.rs`) reads the `campaigns` array first and then
`registry.d/*.json` sorted by filename, each fragment `{"manifest": "<path>"}`,
dropping duplicates and keeping the first. Readers that tolerate a malformed registry
cannot call that strict loader, so they share this merge instead of each parsing the
array alone and never seeing a fragment-registered campaign.
"""

from __future__ import annotations

import json
from collections.abc import Iterable
from pathlib import Path, PurePosixPath

from conductor.mutation_scope import CampaignError

FRAGMENT_DIRECTORY = "registry.d"


def is_fragment_name(name: str) -> bool:
    return PurePosixPath(name).suffix == ".json"


def pending_fragment(
    registry: Path, campaign_id: str, manifest: str, *, repo_root: Path
) -> tuple[Path, str] | None:
    """The fragment that registers `manifest` and its bytes, or None when it stands.

    Raises CampaignError when `registry.d/<campaign_id>.json` is unreadable or
    already registers a different manifest.
    """
    fragment = registry.parent / FRAGMENT_DIRECTORY / f"{campaign_id}.json"
    if fragment.exists():
        try:
            declared = json.loads(fragment.read_text(encoding="utf-8"))
        except (OSError, UnicodeError, json.JSONDecodeError) as exc:
            raise CampaignError(f"cannot read fragment {fragment}: {exc}") from exc
        if declared != {"manifest": manifest}:
            raise CampaignError(
                f"{fragment.relative_to(repo_root)} already registers a "
                "different manifest"
            )
        return None
    return fragment, json.dumps({"manifest": manifest}, indent=2) + "\n"


def merge_registry_manifests(rows: object, fragments: Iterable[object]) -> list[str]:
    """Array rows first, then fragments in the order given, first occurrence kept."""
    if not isinstance(rows, list):
        raise CampaignError("registry.campaigns must be a list")
    declared = [
        row["manifest"]
        for row in rows
        if isinstance(row, dict) and isinstance(row.get("manifest"), str)
    ]
    for fragment in fragments:
        if not isinstance(fragment, dict) or not isinstance(
            fragment.get("manifest"), str
        ):
            raise CampaignError("registry.d fragment has no manifest string")
        declared.append(fragment["manifest"])
    return list(dict.fromkeys(declared))


def registry_manifests(registry: Path) -> list[str]:
    """Manifests declared by a registry file and the `registry.d/` beside it.

    Raises OSError, UnicodeError, JSONDecodeError or CampaignError on an unreadable
    or malformed registry or fragment; an absent `registry.d/` is no fragments.
    """
    payload = json.loads(registry.read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        raise CampaignError(f"{registry} is not a registry object")
    directory = registry.parent / FRAGMENT_DIRECTORY
    names = (
        sorted(
            entry.name for entry in directory.iterdir() if is_fragment_name(entry.name)
        )
        if directory.is_dir()
        else []
    )
    return merge_registry_manifests(
        payload.get("campaigns"),
        (json.loads((directory / name).read_text(encoding="utf-8")) for name in names),
    )
