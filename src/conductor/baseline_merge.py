"""Union two governance baselines, refusing any merge that drops an entry.

Carrying a baseline between branches is a recurring source of silent damage. The
tempting move -- regenerate with the tool, commit the result -- prunes entries
for files the current tree does not contain yet. Those files reappear higher in a
stack and are then re-flagged by entries the merge deleted, so the failure
surfaces somewhere else, later, with no trace back to the merge.

That happened three times on 2026-08-29 across two sessions and three different
baselines. Twice it was caught only because someone diffed the *removed* count
rather than the added one, which is not something to rely on remembering.

So this refuses. A baseline merge may only gain entries; a removal is an error
with the lost keys named, never a warning.

Handles the shapes actually in the repo:

* ``.secrets.baseline``          -- ``results``: path -> list of entry dicts
* ``jscpd`` / ``pmd_cpd``        -- ``entries``: key -> value
* ``vulture``                    -- ``entries``: key -> value
* ``radon_complexity``           -- ``findings``: list of entry dicts
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

#: Container keys in priority order. The first one present is the merged body.
CONTAINER_KEYS: tuple[str, ...] = ("results", "entries", "findings")


class BaselineMergeError(RuntimeError):
    """The merge would lose entries, or the baselines are not comparable."""


def container_key(document: dict[str, Any]) -> str:
    for key in CONTAINER_KEYS:
        if key in document:
            return key
    raise BaselineMergeError(
        f"no known container key in baseline; expected one of {CONTAINER_KEYS}"
    )


def _entry_identity(entry: Any) -> str:
    """A stable identity for one entry, so reordering is not mistaken for loss."""
    return json.dumps(entry, sort_keys=True)


def signature(document: dict[str, Any], key: str) -> set[tuple[str, str]]:
    """Every entry as (bucket, identity), for exact loss detection."""
    body = document[key]
    if isinstance(body, dict):
        out: set[tuple[str, str]] = set()
        for bucket, value in body.items():
            if isinstance(value, list):
                out.update((bucket, _entry_identity(item)) for item in value)
            else:
                out.add((bucket, _entry_identity(value)))
        return out
    if isinstance(body, list):
        return {("", _entry_identity(item)) for item in body}
    raise BaselineMergeError(
        f"container {key!r} is {type(body).__name__}, not dict/list"
    )


def assert_no_loss(base: dict[str, Any], merged: dict[str, Any], key: str) -> None:
    """Raise if ``merged`` lost any entry ``base`` had, naming the buckets.

    Separate from ``merge`` so it is directly exercisable: within ``merge`` this
    is defensive and unreachable, and an unreachable guard is an untested one.
    Callers assembling a baseline by other means should use it too -- that is the
    path every one of the three 2026-08-29 incidents actually took.
    """
    lost = signature(base, key) - signature(merged, key)
    if lost:
        names = sorted({bucket for bucket, _ in lost})[:5]
        raise BaselineMergeError(
            f"merge would drop {len(lost)} entr(y/ies) from the base: {names}"
        )


def merge(base: dict[str, Any], incoming: dict[str, Any]) -> dict[str, Any]:
    """Union ``incoming`` into ``base``. Never drops or overwrites an entry."""
    key = container_key(base)
    if key != container_key(incoming):
        raise BaselineMergeError("baselines use different container keys")
    merged = dict(base)
    body, other = base[key], incoming[key]

    if isinstance(body, dict) and isinstance(other, dict):
        out = {bucket: value for bucket, value in body.items()}
        for bucket, value in other.items():
            if bucket not in out:
                out[bucket] = value
                continue
            if isinstance(out[bucket], list) and isinstance(value, list):
                seen = {_entry_identity(item) for item in out[bucket]}
                out[bucket] = out[bucket] + [
                    item for item in value if _entry_identity(item) not in seen
                ]
            # A scalar/dict bucket present on both sides keeps the base's value:
            # the base is the tree being merged INTO, and silently adopting the
            # other side's value is an overwrite, not a union.
        merged[key] = out
    elif isinstance(body, list) and isinstance(other, list):
        seen = {_entry_identity(item) for item in body}
        merged[key] = body + [
            item for item in other if _entry_identity(item) not in seen
        ]
    else:
        raise BaselineMergeError("container types differ between the two baselines")

    assert_no_loss(base, merged, key)
    if "count" in merged and isinstance(merged[key], (dict, list)):
        merged["count"] = len(merged[key])
    return merged


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("base", type=Path, help="baseline to merge INTO")
    parser.add_argument("incoming", type=Path, help="baseline to take entries FROM")
    parser.add_argument("--out", type=Path, help="write here instead of over base")
    parser.add_argument(
        "--check", action="store_true", help="report the delta, write nothing"
    )
    args = parser.parse_args(argv)

    base = json.loads(args.base.read_text(encoding="utf-8"))
    incoming = json.loads(args.incoming.read_text(encoding="utf-8"))
    key = container_key(base)
    merged = merge(base, incoming)
    added = len(signature(merged, key) - signature(base, key))
    removed = len(signature(base, key) - signature(merged, key))
    print(f"added {added}  removed {removed}")
    if not args.check:
        target = args.out or args.base
        target.write_text(json.dumps(merged, indent=2, sort_keys=True) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
