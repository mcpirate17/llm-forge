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
            {"id": f"m{i}", "outcome": "KILLED", "timing_ms": 0.5} for i in range(mutants)
        ],
    }


def test_small_campaigns_stay_inline_and_round_trip() -> None:
    receipt = _full_receipt(5)
    slim = slim_receipt(receipt)
    assert slim["detail"]["encoding"] == "json"
    assert "mutants" not in slim
    # Every summary key is carried untouched.
    assert all(slim[key] == value for key, value in receipt.items() if key != "mutants")
    assert expand_receipt(slim) == receipt


def test_big_campaigns_become_one_blob_with_verbatim_numbers() -> None:
    receipt = _full_receipt(80)
    slim = slim_receipt(receipt)
    assert slim["detail"]["encoding"] == "zstd+base64"
    assert isinstance(slim["detail"]["blob"], str)
    assert expand_receipt(slim) == receipt
    # A float token parsed from file text keeps its digits through
    # compress-and-back, because both codecs preserve number tokens.
    fussy = json.loads(
        '{"campaign_id": "c", "mutants": [{"timing_ms": 0.9459459459459459}]}'
    )
    assert expand_receipt(slim_receipt(fussy)) == fussy


def test_legacy_receipts_pass_through_unchanged() -> None:
    receipt = _full_receipt(3)
    assert expand_receipt(receipt) == receipt


def test_superseded_pointers_fail_loud_naming_the_replacement() -> None:
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


def test_compaction_honours_the_audit_keep_set(tmp_path: Path) -> None:
    """The kept receipt is the audit's choice, not the clock's."""

    older, newer = _full_receipt(80), _full_receipt(80)
    older["generated_at"] = "2026-09-01T00:00:00+00:00"
    newer["generated_at"] = "2026-09-05T00:00:00+00:00"
    for name, payload in (("a_old.json", older), ("z_new.json", newer)):
        (tmp_path / name).write_text(
            json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
    stats = compact_directory(tmp_path, {"c-slim": "a_old.json"})
    assert stats["superseded"] == 1 and stats["slimmed"] == 1
    kept = json.loads((tmp_path / "a_old.json").read_text(encoding="utf-8"))
    dead = json.loads((tmp_path / "z_new.json").read_text(encoding="utf-8"))
    assert kept["detail"]["encoding"] == "zstd+base64"
    assert kept["status"] == "RATCHET_HELD"  # summary kept
    assert dead["detail"]["superseded_by"] == "a_old.json"
    assert expand_receipt(kept) == older
    # A keep-set naming a file that is not there refuses rather than
    # writing a dangling pointer.
    with pytest.raises(ReceiptDetailError, match="not a receipt"):
        compact_directory(tmp_path, {"c-slim": "nope.json"})
