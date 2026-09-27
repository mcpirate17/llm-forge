"""Tests for the slim-receipt codec bridge (slice L).

One decoder owns the format; these tests pin the contract every reader leans
on: the summary block survives byte-identically, detail round-trips exactly
(including float tokens read from file text), legacy receipts pass through
untouched, superseded pointers fail loud, and the one-write-pass compactor
keeps exactly the receipt the patch audit would read.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from conductor.mutation_receipt_slim import (
    ReceiptDetailError,
    compact_directory,
    expand_receipt,
    expand_receipt_field,
    slim_receipt,
    write_slim_receipt,
)


def _full_receipt(mutants: int, *, status: str = "RATCHET_HELD") -> dict:
    return {
        "campaign_id": "c-slim",
        "status": status,
        "generated_at": "2026-09-13T00:00:00+00:00",
        "mutation_score": 0.9459459459459459,
        "mutants": [
            {"id": f"m{i}", "outcome": "KILLED", "timing_ms": 0.5}
            for i in range(mutants)
        ],
    }


def test_python_codec_returns_dict_and_maps_native_error() -> None:
    receipt = _full_receipt(5)
    slim = slim_receipt(receipt)
    assert isinstance(slim, dict)
    assert expand_receipt(slim) == receipt
    pointer = {
        "campaign_id": "c",
        "status": "PASS",
        "detail": {"encoding": "superseded", "superseded_by": "newer.json"},
    }
    with pytest.raises(ReceiptDetailError, match="superseded by newer.json"):
        expand_receipt(pointer)


def test_the_field_reader_decompresses_only_what_is_asked_for() -> None:
    receipt = _full_receipt(80)
    slim = slim_receipt(receipt)
    # Present-in-summary fields never touch the detail block.
    assert expand_receipt_field(slim, "status") == "RATCHET_HELD"
    # A legacy receipt answers from its own keys.
    assert expand_receipt_field(receipt, "mutants") == receipt["mutants"]
    # A detail field decodes; an absent one is None, not an error.
    assert expand_receipt_field(slim, "mutants") == receipt["mutants"]
    assert expand_receipt_field(slim, "no_such_key") is None
    assert expand_receipt_field({"campaign_id": "c"}, "mutants") is None


def test_write_slim_receipt_lands_canonical_bytes(tmp_path: Path) -> None:
    receipt = _full_receipt(80)
    out = tmp_path / "slim.json"
    write_slim_receipt(out, receipt)
    text = out.read_text(encoding="utf-8")
    disk = json.loads(text)
    assert disk["detail"]["encoding"] == "zstd+base64"
    assert expand_receipt(disk) == receipt
    # The canonical receipt shape: two-space indent, sorted keys, one newline.
    assert text == json.dumps(disk, indent=2, sort_keys=True) + "\n"


def test_compaction_error_maps_to_python_detail_error(tmp_path: Path) -> None:
    with pytest.raises(ReceiptDetailError, match="not a receipt"):
        compact_directory(tmp_path, {"c-slim": "nope.json"})
