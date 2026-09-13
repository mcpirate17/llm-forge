"""Slim mutation receipts: one Python seam over the native codec.

A receipt's summary block (status, campaign id, score, counts, source hashes,
engine, base commit, timestamp) stays plain JSON -- every existing reader keys
off it without change. The bulky per-mutant detail (``mutants`` and
``test_value``, ~24 of the ~26 MiB the tracked receipts held before slice L)
moves under a single ``detail`` key: inline JSON for small campaigns, one
zstd+base64 blob inside the same file otherwise, and a
``{"encoding": "superseded", "superseded_by": <file>}`` pointer for receipts a
newer run of the same campaign replaced.

The codec lives in ``conductor-native`` (``receipt_slim.rs``); this module is
the only importer, mirroring ``conductor._native``'s single-importer rule.
Readers that need detail rows expand lazily through :func:`expand_receipt` --
a summary-only reader never decompresses anything, and a legacy receipt
without a ``detail`` key passes through unchanged.
"""

from __future__ import annotations

import json
from collections.abc import Mapping
from pathlib import Path
from typing import Any

from conductor._native import (
    receipt_compact_directory_native,
    receipt_expand_detail_native,
    receipt_slim_detail_native,
)


class ReceiptDetailError(ValueError):
    """A receipt's detail block cannot be decoded (superseded or malformed)."""


def slim_receipt(receipt: Mapping[str, Any]) -> dict[str, Any]:
    """The receipt with its detail lists folded under one slim ``detail`` key.

    Every other key is carried over untouched, so the summary block a reader
    compares stays byte-identical. A receipt with no detail lists comes back
    as a plain copy.
    """

    expanded = json.loads(receipt_slim_detail_native(json.dumps(dict(receipt))))
    if not isinstance(expanded, dict):
        raise ReceiptDetailError("slim receipt must be a JSON object")
    return expanded


def expand_receipt(receipt: Mapping[str, Any]) -> dict[str, Any]:
    """The receipt with its detail lists restored (legacy receipts unchanged).

    Raises :class:`ReceiptDetailError` for a superseded pointer (naming the
    newer file) or an unknown encoding -- decoding must fail loud, not guess.
    """

    try:
        expanded = json.loads(receipt_expand_detail_native(json.dumps(dict(receipt))))
    except (ValueError, RuntimeError) as exc:
        raise ReceiptDetailError(str(exc)) from exc
    if not isinstance(expanded, dict):
        raise ReceiptDetailError("expanded receipt must be a JSON object")
    return expanded


def expand_receipt_field(receipt: Mapping[str, Any], key: str) -> Any:
    """One detail field of a receipt, or ``None`` when it is absent.

    The lazy-read seam for disk readers: nothing is decompressed unless the
    asked-for field actually lives under the detail block.
    """

    if key in receipt:
        return receipt[key]
    if "detail" not in receipt:
        return None
    return expand_receipt(receipt).get(key)


def expand_receipt_file(path: Path) -> dict[str, Any]:
    """Read one receipt file and expand it (the debug/human path)."""

    return expand_receipt(json.loads(Path(path).read_text(encoding="utf-8")))


def write_slim_receipt(path: Path, receipt: Mapping[str, Any]) -> dict[str, Any]:
    """Atomically write the receipt slim, in the canonical receipt format.

    The write mechanics are the pinned runner's own ``atomic_json`` (called,
    never edited -- every tracked receipt pins that module's hash), so a slim
    receipt lands on disk byte-shaped exactly like a legacy one.
    """

    from conductor.mutation_testing_support import atomic_json

    slim = slim_receipt(receipt)
    atomic_json(Path(path), slim)
    return slim


def receipt_files(root: Path) -> list[tuple[Path, dict[str, Any]]]:
    """Every parseable receipt under ``root`` as ``(path, payload)`` pairs.

    Sorted by filename, ``*.json`` only, non-recursive -- the same walk every
    receipt indexer in this package performs. One iterator so the audit's
    index and the compactor's keep-set builder cannot drift apart.
    """

    rows: list[tuple[Path, dict[str, Any]]] = []
    for path in sorted(root.glob("*.json")):
        try:
            payload = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            continue
        if isinstance(payload, dict):
            rows.append((path, payload))
    return rows


def compact_directory(root: Path, keep: Mapping[str, str] | None = None) -> dict[str, Any]:
    """One compaction pass over a receipt directory (see the Rust docs).

    Slims every kept receipt, points superseded ones at the kept file of
    their campaign, returns the before/after stats. ``keep`` maps campaign id
    to the filename whose detail must survive -- the audit's own acceptance
    predicate decides it, not the clock (a newer receipt can be rejected for
    foreign runner components while an older sibling is accepted).
    """

    try:
        stats = json.loads(
            receipt_compact_directory_native(
                str(root), None if keep is None else json.dumps(dict(keep))
            )
        )
    except (ValueError, RuntimeError) as exc:
        raise ReceiptDetailError(str(exc)) from exc
    if not isinstance(stats, dict):
        raise ReceiptDetailError("compaction stats must be a JSON object")
    return stats
