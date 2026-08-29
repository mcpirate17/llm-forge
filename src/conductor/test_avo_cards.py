from __future__ import annotations

import json
from pathlib import Path

import pytest

from conductor import avo_cards


def test_load_empty_dir(tmp_path: Path) -> None:
    assert avo_cards.load_receipts(tmp_path) == []


def test_render_and_write_pass_receipt(tmp_path: Path) -> None:
    receipts_dir = tmp_path / "receipts"
    receipts_dir.mkdir()
    (receipts_dir / "r1.json").write_text(
        json.dumps(
            {
                "status": "PASS",
                "is_valid": True,
                "score": 167.0,
                "mode": "paired",
                "metrics": {
                    "provenance": {
                        "config": "research/tools/battery.py",
                        "fingerprint": "abc123def456",
                        "compile_mode": "default",
                    }
                },
                "diagnostics": [],
            }
        ),
        encoding="utf-8",
    )
    rows = avo_cards.load_receipts(receipts_dir)
    assert len(rows) == 1
    assert rows[0]["status"] == "PASS"
    card = tmp_path / "kb_avo_receipts.md"
    avo_cards.write_card(rows, path=card)
    text = card.read_text(encoding="utf-8")
    assert "KB-AVO-RECEIPTS-01" in text
    assert "PASS" in text
    assert "battery.py" in text


def test_rejects_non_receipt(tmp_path: Path) -> None:
    (tmp_path / "junk.json").write_text(json.dumps({"hello": 1}), encoding="utf-8")
    with pytest.raises(avo_cards.AvoCardsError, match="not an avo_eval receipt"):
        avo_cards.load_receipts(tmp_path)


def test_rejects_invalid_pass_receipt(tmp_path: Path) -> None:
    (tmp_path / "bad-pass.json").write_text(
        json.dumps({"status": "PASS", "is_valid": False, "metrics": {}}),
        encoding="utf-8",
    )
    with pytest.raises(avo_cards.AvoCardsError, match="not an avo_eval receipt"):
        avo_cards.load_receipts(tmp_path)
